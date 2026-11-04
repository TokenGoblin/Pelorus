//! `cargo xtask test262` -- the test262 runner (Phase 10, unit 5).
//!
//! Reads the vendored subset (`tests/test262/INDEX`), assembles each test with
//! its harness, hands the source to an [`Engine`], grades the answer against the
//! test's own metadata, and compares the result with the committed
//! `tests/test262/RESULTS` under a ratchet.
//!
//! This unit ships the runner and its ratchet but no real engine: the only
//! engine reachable from the command line is `none`, which marks every test
//! ungraded. The binding to the JavaScript engine is a later unit. The fake
//! pass-all and fail-all engines exist so the grading and the ratchet are
//! tested without one; they are not selectable, so they cannot write a bogus
//! baseline.
//!
//! The gate is not part of `all`.
//!
//! Ratchet (all must hold):
//!   * the INDEX lists at least [`CORPUS_FLOOR`] tests and every one is on disk;
//!   * at least [`MATCH_FLOOR`] tests pass;
//!   * no test that passes in `RESULTS` stops passing or disappears;
//!   * a test that newly passes is recorded (`--update`), so the floor only rises.

use crate::Gate;
use crate::test262::{IndexEntry, Phase, parse_index};
use std::collections::BTreeMap;
use std::path::Path;

pub const DIR: &str = "tests/test262";
pub const RESULTS_PATH: &str = "tests/test262/RESULTS";

/// The vendored corpus may not shrink below this many tests (10356 vendored).
pub const CORPUS_FLOOR: usize = 10_000;

/// Tests that must pass: the number of tests on which the engine matches the
/// behaviour test262 specifies. Raised in the commit that raises the pass count;
/// it never falls. Zero until an engine is bound.
pub const MATCH_FLOOR: usize = 0;

/// How a test is run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Sloppy,
    Strict,
    /// `flags: [raw]`: the source exactly as written, no harness, no prologue.
    Raw,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Sloppy => "sloppy",
            Mode::Strict => "strict",
            Mode::Raw => "raw",
        }
    }
}

/// An index entry together with the test's source text.
#[derive(Debug, Clone)]
pub struct Case {
    pub entry: IndexEntry,
    pub body: String,
}

impl Case {
    pub fn is_async(&self) -> bool {
        self.entry.has_flag("async")
    }

    /// The modes this test runs in (INTERPRETING.md: `onlyStrict`, `noStrict`,
    /// `raw`, otherwise both).
    pub fn modes(&self) -> Vec<Mode> {
        if self.entry.has_flag("raw") {
            vec![Mode::Raw]
        } else if self.entry.has_flag("onlyStrict") {
            vec![Mode::Strict]
        } else if self.entry.has_flag("noStrict") {
            vec![Mode::Sloppy]
        } else {
            vec![Mode::Sloppy, Mode::Strict]
        }
    }

    /// The harness files for this test, in load order, without duplicates:
    /// `assert.js`, `sta.js`, `doneprintHandle.js` for async tests, then the
    /// test's own `includes`. A raw test has none.
    pub fn harness_files(&self, mode: Mode) -> Vec<String> {
        if mode == Mode::Raw {
            return Vec::new();
        }
        let mut v: Vec<String> = vec!["assert.js".into(), "sta.js".into()];
        if self.is_async() {
            v.push("doneprintHandle.js".into());
        }
        for i in &self.entry.includes {
            if !v.contains(i) {
                v.push(i.clone());
            }
        }
        v
    }

    /// The complete program text for `mode`. `harness` returns a harness file's
    /// text by name; a missing file is an error, never silently skipped.
    pub fn source_for(
        &self,
        mode: Mode,
        harness: &dyn Fn(&str) -> Option<String>,
    ) -> Result<String, String> {
        let mut out = String::new();
        if mode == Mode::Strict {
            out.push_str("\"use strict\";\n");
        }
        for f in self.harness_files(mode) {
            let text = harness(&f)
                .ok_or_else(|| format!("{}: harness file {f} is not vendored", self.entry.path))?;
            out.push_str(&text);
            if !text.ends_with('\n') {
                out.push('\n');
            }
        }
        out.push_str(&self.body);
        Ok(out)
    }
}

/// What the engine reports for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Run {
    /// The program ran to completion; `output` is what it printed (async tests
    /// report through it).
    Completed { output: Vec<String> },
    /// An error escaped, and in which phase.
    Threw { phase: Phase, kind: String },
    /// The engine cannot run this test at all (not a failure of the engine).
    Unsupported(String),
}

pub trait Engine {
    fn name(&self) -> &str;
    fn run(&self, source: &str, mode: Mode, is_async: bool) -> Run;
}

/// Marks everything ungraded. The only engine reachable from the command line
/// until the real one is bound.
pub struct NoEngine;

impl Engine for NoEngine {
    fn name(&self) -> &str {
        "none"
    }
    fn run(&self, _: &str, _: Mode, _: bool) -> Run {
        Run::Unsupported("no engine bound".into())
    }
}

/// Test double: every program completes (and reports async success).
pub struct PassAll;

impl Engine for PassAll {
    fn name(&self) -> &str {
        "fake-pass-all"
    }
    fn run(&self, _: &str, _: Mode, is_async: bool) -> Run {
        Run::Completed {
            output: if is_async {
                vec!["Test262:AsyncTestComplete".into()]
            } else {
                Vec::new()
            },
        }
    }
}

/// Test double: every program throws a runtime `Test262Error`.
pub struct FailAll;

impl Engine for FailAll {
    fn name(&self) -> &str {
        "fake-fail-all"
    }
    fn run(&self, _: &str, _: Mode, _: bool) -> Run {
        Run::Threw {
            phase: Phase::Runtime,
            kind: "Test262Error".into(),
        }
    }
}

/// The verdict on one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Pass,
    Fail(String),
    /// Not counted either way.
    Ungraded(String),
}

/// Grade what an engine reported against what the test says should happen.
pub fn grade(entry: &IndexEntry, is_async: bool, run: &Run) -> Outcome {
    match (run, &entry.negative) {
        (Run::Unsupported(why), _) => Outcome::Ungraded(why.clone()),
        (Run::Threw { phase, kind }, Some(n)) => {
            if *phase == n.phase && *kind == n.kind {
                Outcome::Pass
            } else {
                Outcome::Fail(format!(
                    "expected {}:{}, got {}:{kind}",
                    n.phase.as_str(),
                    n.kind,
                    phase.as_str()
                ))
            }
        }
        (Run::Completed { .. }, Some(n)) => Outcome::Fail(format!(
            "expected {}:{} but the program completed",
            n.phase.as_str(),
            n.kind
        )),
        (Run::Threw { phase, kind }, None) => {
            Outcome::Fail(format!("threw {}:{kind}", phase.as_str()))
        }
        (Run::Completed { output }, None) => {
            if !is_async {
                return Outcome::Pass;
            }
            if let Some(l) = output
                .iter()
                .find(|l| l.starts_with("Test262:AsyncTestFailure"))
            {
                Outcome::Fail(l.clone())
            } else if output.iter().any(|l| l == "Test262:AsyncTestComplete") {
                Outcome::Pass
            } else {
                Outcome::Fail("async test never called $DONE".into())
            }
        }
    }
}

/// A test's status across all its modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Pass,
    Fail,
    Ungraded,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "pass",
            Status::Fail => "fail",
            Status::Ungraded => "ungraded",
        }
    }

    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "pass" => Some(Status::Pass),
            "fail" => Some(Status::Fail),
            "ungraded" => Some(Status::Ungraded),
            _ => None,
        }
    }

    /// Pass only if every mode passed; any failing mode fails the test;
    /// otherwise (some mode ungraded, none failed) the test is ungraded.
    pub fn combine(outcomes: &[Outcome]) -> Status {
        if outcomes.iter().any(|o| matches!(o, Outcome::Fail(_))) {
            Status::Fail
        } else if outcomes.is_empty() || outcomes.iter().any(|o| matches!(o, Outcome::Ungraded(_)))
        {
            Status::Ungraded
        } else {
            Status::Pass
        }
    }
}

/// Run one case in every mode.
pub fn run_case(
    engine: &dyn Engine,
    case: &Case,
    harness: &dyn Fn(&str) -> Option<String>,
) -> Result<Status, String> {
    let mut outcomes = Vec::new();
    for mode in case.modes() {
        let src = case.source_for(mode, harness)?;
        let run = engine.run(&src, mode, case.is_async());
        outcomes.push(grade(&case.entry, case.is_async(), &run));
    }
    Ok(Status::combine(&outcomes))
}

/// Pass, fail and ungraded counts. A test that is ungraded is neither: it is
/// reported on its own line so a shrinking graded set is visible.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub pass: usize,
    pub fail: usize,
    pub ungraded: usize,
}

impl Counts {
    pub fn of(results: &BTreeMap<String, Status>) -> Counts {
        let mut c = Counts::default();
        for s in results.values() {
            match s {
                Status::Pass => c.pass += 1,
                Status::Fail => c.fail += 1,
                Status::Ungraded => c.ungraded += 1,
            }
        }
        c
    }

    pub fn graded(&self) -> usize {
        self.pass + self.fail
    }
}

/// What the committed `RESULTS` says a test should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expectation {
    /// Passes today; must keep passing.
    Pass,
    /// Does not pass yet (failing, ungraded or unrecorded); passing is progress.
    NotYet,
}

impl Expectation {
    pub fn of(baseline: &BTreeMap<String, Status>, path: &str) -> Expectation {
        match baseline.get(path) {
            Some(Status::Pass) => Expectation::Pass,
            _ => Expectation::NotYet,
        }
    }
}

pub fn format_results(results: &BTreeMap<String, Status>) -> String {
    let c = Counts::of(results);
    let mut s = format!(
        "# test262 results (cargo xtask test262 --update). Do not edit by hand.\n# path<TAB>pass|fail|ungraded ; pass {} fail {} ungraded {}\n",
        c.pass, c.fail, c.ungraded
    );
    for (p, st) in results {
        s.push_str(p);
        s.push('\t');
        s.push_str(st.as_str());
        s.push('\n');
    }
    s
}

pub fn parse_results(text: &str) -> Result<BTreeMap<String, Status>, String> {
    let mut out = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (p, st) = l
            .split_once('\t')
            .ok_or_else(|| format!("RESULTS line {}: no tab", n + 1))?;
        let st = Status::parse(st)
            .ok_or_else(|| format!("RESULTS line {}: unknown status `{st}`", n + 1))?;
        if out.insert(p.to_string(), st).is_some() {
            return Err(format!("RESULTS line {}: duplicate path {p}", n + 1));
        }
    }
    Ok(out)
}

/// The ratchet's verdict.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Verdict {
    /// Reasons the gate fails.
    pub problems: Vec<String>,
    /// Tests that pass now and were not recorded as passing.
    pub improved: Vec<String>,
}

/// Compare a run with the baseline. `corpus_len` is the number of INDEX
/// entries; `now` has one entry per test that was run.
pub fn ratchet(
    baseline: &BTreeMap<String, Status>,
    now: &BTreeMap<String, Status>,
    corpus_len: usize,
    corpus_floor: usize,
    match_floor: usize,
) -> Verdict {
    let mut v = Verdict::default();
    if corpus_len < corpus_floor {
        v.problems.push(format!(
            "the INDEX lists {corpus_len} tests, floor is {corpus_floor}"
        ));
    }
    let c = Counts::of(now);
    if c.pass < match_floor {
        v.problems
            .push(format!("{} tests pass, floor is {match_floor}", c.pass));
    }
    for (p, st) in baseline {
        if *st != Status::Pass {
            continue;
        }
        match now.get(p) {
            None => v
                .problems
                .push(format!("{p}: passed in RESULTS, no longer run")),
            Some(Status::Pass) => {}
            Some(other) => v
                .problems
                .push(format!("{p}: passed in RESULTS, now {}", other.as_str())),
        }
    }
    for (p, st) in now {
        if *st == Status::Pass && Expectation::of(baseline, p) == Expectation::NotYet {
            v.improved.push(p.clone());
        }
    }
    v
}

struct Opts {
    engine: String,
    update: bool,
    filter: String,
}

fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        engine: "none".into(),
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
            "no engine named `{other}` (only `none` until the engine is bound)"
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

fn execute(g: &mut Gate, root: &Path, args: &[String]) -> Result<(), String> {
    let o = parse_args(args)?;
    let engine = engine_named(&o.engine)?;
    let index = std::fs::read_to_string(root.join(DIR).join("INDEX"))
        .map_err(|e| format!("read {DIR}/INDEX: {e}"))?;
    let entries = parse_index(&index)?;
    let baseline = match std::fs::read_to_string(root.join(RESULTS_PATH)) {
        Ok(t) => parse_results(&t)?,
        Err(_) => BTreeMap::new(),
    };
    let mut now = run_all(engine.as_ref(), root, &entries, &o.filter)?;
    if !o.filter.is_empty() {
        // A filtered run says nothing about the tests it skipped: carry them over
        // from the baseline so the ratchet compares like with like.
        for (p, st) in &baseline {
            now.entry(p.clone()).or_insert(*st);
        }
    }
    let c = Counts::of(&now);
    g.info(format!(
        "engine {}: {} tests; pass {}, fail {}, ungraded {} (graded {})",
        engine.name(),
        now.len(),
        c.pass,
        c.fail,
        c.ungraded,
        c.graded()
    ));
    let mut v = ratchet(&baseline, &now, entries.len(), CORPUS_FLOOR, MATCH_FLOOR);
    if !v.improved.is_empty() {
        g.info(format!("{} test(s) newly pass", v.improved.len()));
        if o.update {
            // handled below, after the regressions are known
        } else {
            v.problems.push(format!(
                "{} test(s) newly pass and are not in RESULTS; run `cargo xtask test262 --update`",
                v.improved.len()
            ));
        }
    }
    if o.update {
        // Never record a state the ratchet rejects, other than the floor itself
        // being unmet by a corpus change the person is making on purpose.
        if v.problems.is_empty() {
            let dest = root.join(RESULTS_PATH);
            std::fs::write(&dest, format_results(&now))
                .map_err(|e| format!("write {}: {e}", dest.display()))?;
            g.info(format!("wrote {RESULTS_PATH}"));
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

    fn entry(path: &str, flags: &[&str], neg: Option<(Phase, &str)>, inc: &[&str]) -> IndexEntry {
        let f = |v: &[&str]| v.iter().map(|s| s.to_string()).collect();
        IndexEntry {
            path: path.into(),
            flags: f(flags),
            negative: neg.map(|(phase, k)| crate::test262::Negative {
                phase,
                kind: k.into(),
            }),
            includes: f(inc),
            features: Vec::new(),
        }
    }

    fn case(e: IndexEntry) -> Case {
        Case {
            entry: e,
            body: "BODY;".into(),
        }
    }

    fn harness(n: &str) -> Option<String> {
        Some(format!("/*{n}*/"))
    }

    #[test]
    fn modes_follow_the_flags() {
        let m = |flags: &[&str]| case(entry("a.js", flags, None, &[])).modes();
        assert_eq!(m(&[]), [Mode::Sloppy, Mode::Strict]);
        assert_eq!(m(&["onlyStrict"]), [Mode::Strict]);
        assert_eq!(m(&["noStrict"]), [Mode::Sloppy]);
        assert_eq!(m(&["raw"]), [Mode::Raw]);
    }

    #[test]
    fn source_assembly() {
        let c = case(entry(
            "a.js",
            &["async"],
            None,
            &["x.js", "assert.js", "y.js"],
        ));
        assert_eq!(
            c.harness_files(Mode::Sloppy),
            ["assert.js", "sta.js", "doneprintHandle.js", "x.js", "y.js"]
        );
        let s = c.source_for(Mode::Strict, &harness).unwrap();
        assert!(s.starts_with("\"use strict\";\n/*assert.js*/\n/*sta.js*/\n"));
        assert!(s.ends_with("/*y.js*/\nBODY;"));
        let sloppy = c.source_for(Mode::Sloppy, &harness).unwrap();
        assert!(!sloppy.contains("use strict"));
        // Raw: the body alone.
        let raw = case(entry("r.js", &["raw"], None, &["x.js"]));
        assert_eq!(raw.source_for(Mode::Raw, &harness).unwrap(), "BODY;");
        // A missing harness file is an error.
        assert!(c.source_for(Mode::Sloppy, &|_| None).is_err());
    }

    #[test]
    fn grading() {
        let pos = entry("p.js", &[], None, &[]);
        let asy = entry("a.js", &["async"], None, &[]);
        let neg = entry("n.js", &[], Some((Phase::Parse, "SyntaxError")), &[]);
        let done = |l: &str| Run::Completed {
            output: vec![l.into()],
        };
        let threw = |p, k: &str| Run::Threw {
            phase: p,
            kind: k.into(),
        };
        assert_eq!(grade(&pos, false, &done("x")), Outcome::Pass);
        assert!(matches!(
            grade(&pos, false, &threw(Phase::Runtime, "TypeError")),
            Outcome::Fail(_)
        ));
        assert_eq!(
            grade(&asy, true, &done("Test262:AsyncTestComplete")),
            Outcome::Pass
        );
        assert!(matches!(
            grade(&asy, true, &done("Test262:AsyncTestFailure:Error: x")),
            Outcome::Fail(_)
        ));
        assert!(matches!(grade(&asy, true, &done("hi")), Outcome::Fail(_)));
        assert_eq!(
            grade(&neg, false, &threw(Phase::Parse, "SyntaxError")),
            Outcome::Pass
        );
        // Right type, wrong phase; right phase, wrong type; no error at all.
        assert!(matches!(
            grade(&neg, false, &threw(Phase::Runtime, "SyntaxError")),
            Outcome::Fail(_)
        ));
        assert!(matches!(
            grade(&neg, false, &threw(Phase::Parse, "TypeError")),
            Outcome::Fail(_)
        ));
        assert!(matches!(grade(&neg, false, &done("")), Outcome::Fail(_)));
        assert!(matches!(
            grade(&pos, false, &Run::Unsupported("x".into())),
            Outcome::Ungraded(_)
        ));
    }

    #[test]
    fn status_combines_modes() {
        let p = Outcome::Pass;
        let f = Outcome::Fail("x".into());
        let u = Outcome::Ungraded("x".into());
        assert_eq!(Status::combine(&[p.clone(), p.clone()]), Status::Pass);
        assert_eq!(Status::combine(&[p.clone(), f.clone()]), Status::Fail);
        assert_eq!(Status::combine(&[u.clone(), f]), Status::Fail);
        assert_eq!(Status::combine(&[p, u]), Status::Ungraded);
        assert_eq!(Status::combine(&[]), Status::Ungraded);
    }

    #[test]
    fn results_round_trip() {
        let mut m = BTreeMap::new();
        m.insert("a/b.js".to_string(), Status::Pass);
        m.insert("a/c.js".to_string(), Status::Fail);
        m.insert("d.js".to_string(), Status::Ungraded);
        let t = format_results(&m);
        assert!(t.contains("pass 1 fail 1 ungraded 1"));
        assert_eq!(parse_results(&t).unwrap(), m);
        assert!(parse_results("a.js\tmaybe\n").is_err());
        assert!(parse_results("a.js\tpass\na.js\tfail\n").is_err());
        assert!(parse_results("a.js pass\n").is_err());
    }

    fn map(v: &[(&str, Status)]) -> BTreeMap<String, Status> {
        v.iter().map(|(p, s)| (p.to_string(), *s)).collect()
    }

    #[test]
    fn ratchet_rules() {
        use Status::*;
        let base = map(&[("a", Pass), ("b", Fail), ("c", Pass)]);
        // Holding steady with one improvement.
        let now = map(&[("a", Pass), ("b", Pass), ("c", Pass)]);
        let v = ratchet(&base, &now, 3, 3, 3);
        assert!(v.problems.is_empty());
        assert_eq!(v.improved, ["b"]);
        // A regression, a disappearance, and a floor miss are each reported.
        let now = map(&[("a", Fail), ("b", Fail)]);
        let v = ratchet(&base, &now, 2, 3, 2);
        assert_eq!(v.problems.len(), 4, "{:?}", v.problems);
        // Ungraded after passing is a regression too.
        let v = ratchet(&base, &map(&[("a", Ungraded), ("c", Pass)]), 3, 3, 0);
        assert_eq!(v.problems.len(), 1);
        // Expectation.
        assert_eq!(Expectation::of(&base, "a"), Expectation::Pass);
        assert_eq!(Expectation::of(&base, "b"), Expectation::NotYet);
        assert_eq!(Expectation::of(&base, "zz"), Expectation::NotYet);
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
        execute(&mut g, fx.path(), &[]).unwrap();
        // 3 tests is under the corpus floor.
        assert_eq!(g.fail_count(), 1, "{:?}", g.failures());
        assert!(g.failures()[0].contains("floor"));
        assert!(engine_named("v8").is_err());
        assert!(parse_args(&["--nope".to_string()]).is_err());
    }
}
