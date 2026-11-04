//! `cargo xtask test262` -- the test262 runner (Phase 10, unit 5).
//!
//! Reads the vendored subset (`tests/test262/INDEX`), runs each test, grades
//! the answer against the test's own metadata, and compares the result with
//! the committed `tests/test262/RESULTS` and `EXPECTED_FAILURES` under a
//! ratchet.
//!
//! Two halves, because this crate links no JavaScript engine:
//!
//! * the pure rules (assembly, grading, results, ratchet) are in
//!   `test262_core.rs`, which `crates/px-script/tests/test262.rs` includes with
//!   `#[path]`, so there is one copy;
//! * the engine-bound runner is that test file. `--engine boa` (the default)
//!   runs it as a child of `cargo test` and reads the per-test results file it
//!   writes, then applies the ratchet here. `--engine none` marks every test
//!   ungraded and runs no child. The fake pass-all and fail-all engines exist
//!   so the grading and the ratchet are tested without an engine; they are not
//!   selectable, so they cannot write a bogus baseline.
//!
//! The gate is not part of `all` (a full run takes minutes).
//!
//! Ratchet (all must hold):
//!   * the INDEX lists at least [`CORPUS_FLOOR`] tests and every one is on disk;
//!   * at least [`MATCH_FLOOR`] tests pass;
//!   * no test that passes in `RESULTS` stops passing or disappears;
//!   * a test that newly passes is recorded (`--update`), so the floor only rises;
//!   * `EXPECTED_FAILURES` is true both ways: a failing test missing from it, and
//!     a listed test that no longer fails, are both reported (ADR 019: the list
//!     is never shortened by dropping tests from the corpus).

use crate::Gate;
use crate::proc;
use crate::test262::{IndexEntry, parse_index};
pub use crate::test262_core::*;
use std::collections::BTreeMap;
use std::path::Path;

pub const DIR: &str = "tests/test262";
pub const RESULTS_PATH: &str = "tests/test262/RESULTS";
pub const EXPECTED_PATH: &str = "tests/test262/EXPECTED_FAILURES";

/// The cargo profile the engine-bound runner is built in, and so the profile in
/// which `MATCH_FLOOR` is reproducible. Debug: the whole corpus takes about
/// three minutes on four cores, and the gate then needs no second build of the
/// engine. (`cargo test -p px-script --test test262 --release` with the same
/// environment variables also works and is faster to run, slower to build.)
pub const ENGINE_PROFILE: &str = "debug";

struct Opts {
    engine: String,
    update: bool,
    filter: String,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        engine: "boa".into(),
        update: false,
        filter: String::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--update" => o.update = true,
            "--engine" => o.engine = it.next().ok_or("--engine needs a name")?.clone(),
            "--filter" => o.filter = it.next().ok_or("--filter needs a path prefix")?.clone(),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    Ok(o)
}

fn engine_named(name: &str) -> Result<Box<dyn Engine>, String> {
    match name {
        "none" => Ok(Box::new(NoEngine)),
        other => Err(format!(
            "no engine named `{other}` (`boa` runs the engine-bound runner; `none` grades nothing)"
        )),
    }
}

/// Run every case in `entries` with `engine`. Tests are read from
/// `<root>/tests/test262/test/`, harness files from `.../harness/`.
pub fn run_all(
    engine: &dyn Engine,
    root: &Path,
    entries: &[IndexEntry],
    filter: &str,
) -> Result<BTreeMap<String, Status>, String> {
    let base = root.join(DIR);
    let mut cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut out = BTreeMap::new();
    for e in entries.iter().filter(|e| e.path.starts_with(filter)) {
        for h in std::iter::once(&"assert.js".to_string())
            .chain(std::iter::once(&"sta.js".to_string()))
            .chain(std::iter::once(&"doneprintHandle.js".to_string()))
            .chain(e.includes.iter())
        {
            cache
                .entry(h.clone())
                .or_insert_with(|| std::fs::read_to_string(base.join("harness").join(h)).ok());
        }
        let body = std::fs::read(base.join("test").join(&e.path))
            .map_err(|er| format!("read test/{}: {er}", e.path))?;
        let case = Case {
            entry: e.clone(),
            body: String::from_utf8_lossy(&body).into_owned(),
        };
        let lookup = |n: &str| cache.get(n).cloned().flatten();
        out.insert(e.path.clone(), run_case(engine, &case, &lookup)?);
    }
    Ok(out)
}

/// Run the engine-bound runner (`crates/px-script/tests/test262.rs`) as a child
/// of `cargo test` and read the per-test results file it writes.
fn run_boa(root: &Path, filter: &str) -> Result<BTreeMap<String, CaseResult>, String> {
    let out = proc::target_dir(root).join("test262-run.tsv");
    let _ = std::fs::remove_file(&out);
    let mut c = proc::cargo_command(root);
    c.args(["test", "--locked", "-p", "px-script", "--test", "test262"]);
    if ENGINE_PROFILE == "release" {
        c.arg("--release");
    }
    c.args(["--", "script_test262_corpus", "--exact", "--nocapture"]);
    c.env("PX_TEST262_OUT", &out)
        .env("PX_TEST262_FILTER", filter);
    let status = c.status().map_err(|e| format!("cannot run cargo: {e}"))?;
    if !status.success() {
        return Err(format!("the engine-bound runner exited with {status}"));
    }
    let text = std::fs::read_to_string(&out)
        .map_err(|e| format!("read {}: {e} (the runner wrote no results)", out.display()))?;
    parse_run(&text)
}

fn read_or_empty(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

fn execute(g: &mut Gate, root: &Path, args: &[String]) -> Result<(), String> {
    let o = parse_args(args)?;
    let index = std::fs::read_to_string(root.join(DIR).join("INDEX"))
        .map_err(|e| format!("read {DIR}/INDEX: {e}"))?;
    let entries = parse_index(&index)?;
    let baseline = parse_results(&read_or_empty(&root.join(RESULTS_PATH)))?;
    let expected = parse_expected(&read_or_empty(&root.join(EXPECTED_PATH)))?;
    let (engine_name, mut detailed) = if o.engine == "boa" {
        ("boa".to_string(), run_boa(root, &o.filter)?)
    } else {
        let engine = engine_named(&o.engine)?;
        let plain = run_all(engine.as_ref(), root, &entries, &o.filter)?;
        let detailed = plain
            .into_iter()
            .map(|(p, status)| {
                let class = if status == Status::Ungraded {
                    "no-engine".to_string()
                } else {
                    String::new()
                };
                (
                    p,
                    CaseResult {
                        status,
                        class,
                        detail: String::new(),
                    },
                )
            })
            .collect();
        (engine.name().to_string(), detailed)
    };
    if !o.filter.is_empty() {
        // A filtered run says nothing about the tests it skipped: carry them over
        // from the baseline so the ratchet compares like with like.
        for (p, st) in &baseline {
            detailed.entry(p.clone()).or_insert_with(|| CaseResult {
                status: *st,
                class: expected.get(p).cloned().unwrap_or_default(),
                detail: String::new(),
            });
        }
    }
    let now = statuses(&detailed);
    let c = Counts::of(&now);
    g.info(format!(
        "engine {engine_name}: {} tests; pass {}, fail {}, ungraded {} (graded {})",
        now.len(),
        c.pass,
        c.fail,
        c.ungraded,
        c.graded()
    ));
    if c.graded() > 0 {
        g.info(format!(
            "pass rate {:.2}% of graded",
            100.0 * c.pass as f64 / c.graded() as f64
        ));
    }
    let mut v = ratchet(&baseline, &now, entries.len(), CORPUS_FLOOR, MATCH_FLOOR);
    if !v.improved.is_empty() {
        g.info(format!("{} test(s) newly pass", v.improved.len()));
        if !o.update {
            v.problems.push(format!(
                "{} test(s) newly pass and are not in RESULTS; run `cargo xtask test262 --update`",
                v.improved.len()
            ));
        }
    }
    if !o.update {
        for p in check_expected(&expected, &now) {
            v.problems.push(p);
        }
    }
    if o.update {
        // Never record a state the ratchet rejects.
        if !v.problems.is_empty() {
            g.info("not recording: the ratchet rejects this run");
        } else if !o.filter.is_empty() || o.engine == "none" {
            g.info("not recording: only a full run with the real engine is recorded");
        } else {
            for (rel, text) in [
                (RESULTS_PATH, format_results(&now)),
                (EXPECTED_PATH, format_expected(&detailed)),
            ] {
                let dest = root.join(rel);
                std::fs::write(&dest, text)
                    .map_err(|e| format!("write {}: {e}", dest.display()))?;
                g.info(format!("wrote {rel}"));
            }
            let mut classes: BTreeMap<&str, usize> = BTreeMap::new();
            for r in detailed.values().filter(|r| r.status == Status::Fail) {
                *classes.entry(r.class.as_str()).or_default() += 1;
            }
            g.info(format!("failure classes: {classes:?}"));
        }
    }
    for p in v.problems {
        g.fail(p);
    }
    if g.passed() {
        g.ok("ratchet holds");
    }
    Ok(())
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    if let Err(e) = execute(g, root, args) {
        g.fail(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn map(v: &[(&str, Status)]) -> BTreeMap<String, Status> {
        v.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    fn fixture() -> Fixture {
        let fx = Fixture::new("t262-run");
        fx.write("tests/test262/harness/assert.js", "var assert;")
            .write("tests/test262/harness/sta.js", "var Test262Error;")
            .write("tests/test262/harness/doneprintHandle.js", "function $DONE(){}")
            .write("tests/test262/test/a/one.js", "1;")
            .write("tests/test262/test/a/async.js", "$DONE();")
            .write("tests/test262/test/b/neg.js", "@@@")
            .write(
                "tests/test262/INDEX",
                "a/async.js\tasync\t-\t-\t-\na/one.js\t-\t-\t-\t-\nb/neg.js\t-\tparse:SyntaxError\t-\t-\n",
            );
        fx
    }

    #[test]
    fn fake_engines_over_a_fixture_corpus() {
        let fx = fixture();
        let idx = std::fs::read_to_string(fx.path().join("tests/test262/INDEX")).unwrap();
        let entries = parse_index(&idx).unwrap();
        let pass = run_all(&PassAll, fx.path(), &entries, "").unwrap();
        // The negative test cannot pass when nothing is ever rejected.
        assert_eq!(
            pass,
            map(&[
                ("a/async.js", Status::Pass),
                ("a/one.js", Status::Pass),
                ("b/neg.js", Status::Fail)
            ])
        );
        let fail = run_all(&FailAll, fx.path(), &entries, "").unwrap();
        assert_eq!(Counts::of(&fail).fail, 3);
        let none = run_all(&NoEngine, fx.path(), &entries, "").unwrap();
        let c = Counts::of(&none);
        assert_eq!((c.pass, c.fail, c.ungraded, c.graded()), (0, 0, 3, 0));
        let only = run_all(&PassAll, fx.path(), &entries, "a/").unwrap();
        assert_eq!(only.len(), 2);
        // Pass-all then fail-all is a regression the ratchet catches.
        let v = ratchet(&pass, &fail, 3, 3, 0);
        assert_eq!(v.problems.len(), 2);
    }

    #[test]
    fn gate_with_no_engine_records_nothing_and_enforces_the_corpus_floor() {
        let fx = fixture();
        let mut g = Gate::new("test262");
        execute(
            &mut g,
            fx.path(),
            &["--engine".to_string(), "none".to_string()],
        )
        .unwrap();
        // 3 tests is under the corpus floor.
        assert_eq!(g.fail_count(), 1, "{:?}", g.failures());
        assert!(g.failures()[0].contains("floor"));
        assert!(engine_named("v8").is_err());
        assert!(parse_args(&["--nope".to_string()]).is_err());
    }
}
