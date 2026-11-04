//! The engine-bound test262 runner (Phase 10, unit 5, second half).
//!
//! `cargo xtask test262` links no JavaScript engine, so the runner that does
//! lives here. The pure rules (how a test is assembled from its harness and
//! flags, how a run is graded, the results formats, the ratchet) are not
//! re-implemented: `px-xtask/src/test262.rs` and `test262_core.rs` are
//! compiled into this test with `#[path]`, the same way `gates/render.rs`
//! includes `png.rs`. Same source, two crates: they cannot drift, and the core's
//! own unit tests run again in this binary.
//!
//! What runs when:
//!
//! * `cargo test -p px-script --test test262` runs the fast tests (the engine
//!   on hand-written cases, the assembly of a real vendored test, and the
//!   `annexB/` slice of the corpus against the committed baseline).
//! * `script_test262_corpus` runs the WHOLE vendored subset when
//!   `PX_TEST262_OUT=<file>` is set (that is what `cargo xtask test262` does,
//!   with `--release`), writing `path<TAB>status<TAB>class<TAB>detail` lines
//!   sorted by path. With `PX_TEST262_FULL=1` instead it applies the ratchet
//!   itself. `PX_TEST262_FILTER=<prefix>` narrows either, `PX_TEST262_JOBS`
//!   sets the worker count, `PX_TEST262_TIMEOUT_MS` the per-run deadline.
//!
//! Every run is in a fresh [`Host`] with the project's limits and a manual
//! clock (so timers cost no wall time), on its own thread with a hard
//! deadline: a test that does not finish is recorded as a failure of class
//! `timeout` and its thread is abandoned, so one hung test cannot hang the run.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use px_script::clock::ManualClock;
use px_script::queue::Caps;
use px_script::{Host, HostOptions, LoopEnd, Outcome as ScriptOutcome};

#[allow(dead_code)]
#[path = "../../px-xtask/src/test262.rs"]
mod test262;
#[allow(dead_code)]
#[path = "../../px-xtask/src/test262_core.rs"]
mod test262_core;

use test262::{IndexEntry, Phase, parse_index};
use test262_core::{
    CORPUS_FLOOR, Case, CaseResult, Counts, Engine, MATCH_FLOOR, Mode, Outcome, Run, Status,
    check_expected, format_run, grade, parse_expected, parse_results, ratchet, statuses,
};

/// Run and clock seed: runs are reproducible.
const SEED: u64 = 0x07e5_7262;
/// A fixed wall-clock epoch for `Date.now()` (the clock itself is manual).
const WALL_EPOCH_MS: i64 = 1_700_000_000_000;
/// Tasks an async test's loop may run before it is called hung.
const LOOP_TASKS: usize = 10_000;
/// Stack of the thread one run executes on: the parser and the engine's native
/// builtins recurse in Rust, and a stack overflow aborts the whole process.
const RUN_STACK: usize = 256 * 1024 * 1024;

/// What the harness hooks of the engine's global look like. Defined by script,
/// before the test, because the host's globals are exactly what the project
/// gave it plus these.
///
/// * `print` collects lines (the `$DONE` protocol reports through it).
/// * `$262.evalScript` is an indirect `eval`; `gc` is a no-op;
///   `detachArrayBuffer`, `createRealm`, `agent` and `IsHTMLDDA` are not
///   provided, and a test that uses one is ungraded (see [`ungraded_reason`]),
///   never silently failed or passed.
/// * `__px_mark` records that the program started running, which is how an
///   error is told to be a parse/early error (before) or a runtime one (after).
const PRELUDE: &str = r#"
(function () {
  var g = globalThis;
  var lines = [];
  var started = false;
  function def(name, value) {
    Object.defineProperty(g, name, {
      value: value, writable: true, configurable: true, enumerable: false
    });
  }
  def('print', function print() {
    lines.push(Array.prototype.join.call(arguments, ' '));
  });
  def('__px_out', function () { return lines.join('\n'); });
  def('__px_mark', function () { started = true; });
  def('__px_started', function () { return started; });
  def('$262', {
    global: g,
    gc: function () {},
    evalScript: function (src) { return (0, eval)(String(src)); }
  });
})();
"#;

/// Why a test cannot be graded with this host, or `None` if it can. A token,
/// not a sentence: it is the `class` column of an ungraded result.
fn ungraded_reason(entry: &IndexEntry, body: &str) -> Option<String> {
    if entry.features.iter().any(|f| f == "cross-realm") {
        return Some("hook-createRealm".into());
    }
    for (needle, token) in [
        ("$262.createRealm", "hook-createRealm"),
        ("$262.agent", "hook-agent"),
        ("$262.detachArrayBuffer", "hook-detachArrayBuffer"),
        ("$262.IsHTMLDDA", "hook-IsHTMLDDA"),
    ] {
        if body.contains(needle) {
            return Some(token.into());
        }
    }
    if entry.includes.iter().any(|i| i == "detachArrayBuffer.js") {
        return Some("hook-detachArrayBuffer".into());
    }
    // ADR 040: these are absent on purpose.
    if entry
        .features
        .iter()
        .any(|f| matches!(f.as_str(), "SharedArrayBuffer" | "Temporal" | "Atomics"))
        || entry.includes.iter().any(|i| i == "temporalHelpers.js")
    {
        return Some("deliberately-absent-global".into());
    }
    None
}

/// The Boa-backed [`Engine`].
struct BoaEngine {
    deadline: Duration,
}

impl BoaEngine {
    fn new(deadline: Duration) -> Self {
        Self { deadline }
    }
}

/// `TypeError: message` -> `TypeError`; anything that is not a name -> empty.
fn error_kind(described: &str) -> String {
    let head = described.split(':').next().unwrap_or("").trim();
    if !head.is_empty() && head.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        head.to_string()
    } else {
        String::new()
    }
}

/// Put the start marker after the strict prologue, or first.
fn with_marker(source: &str, mode: Mode) -> String {
    match mode {
        Mode::Raw => source.to_string(),
        Mode::Strict => match source.strip_prefix("\"use strict\";\n") {
            Some(rest) => format!("\"use strict\";\n__px_mark();\n{rest}"),
            None => format!("__px_mark();\n{source}"),
        },
        Mode::Sloppy => format!("__px_mark();\n{source}"),
    }
}

fn run_on_host(source: &str, mode: Mode, is_async: bool) -> Run {
    let host = Host::with_options(HostOptions {
        caps: Caps::default(),
        clock: Rc::new(ManualClock::at(0)),
        wall_epoch_ms: WALL_EPOCH_MS,
        ..HostOptions::new(SEED)
    });
    let mut host = match host {
        Ok(h) => h,
        Err(_) => return Run::Unsupported("host-not-built".into()),
    };
    if !matches!(host.eval(PRELUDE), ScriptOutcome::Value(_)) {
        return Run::Unsupported("prelude-failed".into());
    }
    let program = with_marker(source, mode);
    let first = host.eval(&program);
    let threw = |host: &mut Host, text: &str| {
        let started = matches!(
            host.eval("__px_started()"),
            ScriptOutcome::Value(ref v) if v == "true"
        );
        let kind = error_kind(text);
        let phase = if (mode == Mode::Raw || !started) && kind == "SyntaxError" {
            Phase::Parse
        } else {
            Phase::Runtime
        };
        Run::Threw { phase, kind }
    };
    match first {
        ScriptOutcome::Terminated(_) => {
            return Run::Threw {
                phase: Phase::Runtime,
                kind: "Terminated".into(),
            };
        }
        ScriptOutcome::Error(text) => return threw(&mut host, &text),
        ScriptOutcome::Value(_) => {}
    }
    if is_async {
        match host.run_loop(LOOP_TASKS) {
            LoopEnd::Terminated { .. } => {
                return Run::Threw {
                    phase: Phase::Runtime,
                    kind: "Terminated".into(),
                };
            }
            LoopEnd::Idle { .. } | LoopEnd::TaskLimit { .. } => {}
        }
    }
    let output = match host.eval("__px_out()") {
        ScriptOutcome::Value(text) => text.lines().map(str::to_string).collect(),
        _ => Vec::new(),
    };
    Run::Completed { output }
}

impl Engine for BoaEngine {
    fn name(&self) -> &str {
        "boa-0.22.0"
    }

    fn run(&self, source: &str, mode: Mode, is_async: bool) -> Run {
        let (tx, rx) = mpsc::channel();
        let source = source.to_string();
        let spawned = std::thread::Builder::new()
            .name("test262-run".into())
            .stack_size(RUN_STACK)
            .spawn(move || {
                let _ = tx.send(run_on_host(&source, mode, is_async));
            });
        if spawned.is_err() {
            return Run::Unsupported("no-thread".into());
        }
        match rx.recv_timeout(self.deadline) {
            Ok(run) => run,
            // The sender is gone without a result: the thread panicked.
            Err(mpsc::RecvTimeoutError::Disconnected) => Run::Threw {
                phase: Phase::Runtime,
                kind: "EnginePanic".into(),
            },
            // The thread is abandoned (an engine call cannot be interrupted);
            // it ends by itself when the loop limit stops it, or with the process.
            Err(mpsc::RecvTimeoutError::Timeout) => Run::Threw {
                phase: Phase::Runtime,
                kind: "Timeout".into(),
            },
        }
    }
}

/// Grade one test across its modes, with the ungraded rule first.
fn run_entry(
    engine: &dyn Engine,
    entry: &IndexEntry,
    body: &str,
    harness: &dyn Fn(&str) -> Option<String>,
) -> CaseResult {
    if let Some(why) = ungraded_reason(entry, body) {
        return CaseResult::from_outcomes(&[Outcome::Ungraded(why)]);
    }
    let case = Case {
        entry: entry.clone(),
        body: body.to_string(),
    };
    let mut outcomes = Vec::new();
    for mode in case.modes() {
        match case.source_for(mode, harness) {
            Ok(src) => {
                let run = engine.run(&src, mode, case.is_async());
                outcomes.push(grade(entry, case.is_async(), &run));
            }
            Err(e) => outcomes.push(Outcome::Fail(format!("harness: {e}"))),
        }
    }
    CaseResult::from_outcomes(&outcomes)
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .to_path_buf()
}

fn corpus_dir() -> PathBuf {
    repo_root().join("tests/test262")
}

#[cfg(test)]
fn load_index() -> Vec<IndexEntry> {
    let text = std::fs::read_to_string(corpus_dir().join("INDEX")).expect("read INDEX");
    parse_index(&text).expect("INDEX parses")
}

#[cfg(test)]
fn harness_cache() -> BTreeMap<String, String> {
    let mut m = BTreeMap::new();
    let dir = corpus_dir().join("harness");
    for e in std::fs::read_dir(dir).expect("harness dir") {
        let p = e.expect("entry").path();
        if let (Some(name), Ok(text)) = (
            p.file_name().and_then(|n| n.to_str()),
            std::fs::read_to_string(&p),
        ) {
            m.insert(name.to_string(), text);
        }
    }
    m
}

/// Run `entries` whose path starts with `filter` on `jobs` workers. The result
/// map is ordered by path, so the output does not depend on scheduling.
fn run_corpus(
    engine: &(dyn Engine + Sync),
    entries: &[IndexEntry],
    filter: &str,
    jobs: usize,
) -> BTreeMap<String, CaseResult> {
    let harness = harness_cache();
    let todo: Vec<&IndexEntry> = entries
        .iter()
        .filter(|e| e.path.starts_with(filter))
        .collect();
    let next = AtomicUsize::new(0);
    let out = Arc::new(Mutex::new(BTreeMap::new()));
    let dir = corpus_dir().join("test");
    std::thread::scope(|s| {
        for _ in 0..jobs.max(1) {
            s.spawn(|| {
                let lookup = |n: &str| harness.get(n).cloned();
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(entry) = todo.get(i) else { break };
                    let result = match std::fs::read(dir.join(&entry.path)) {
                        Ok(bytes) => {
                            let body = String::from_utf8_lossy(&bytes);
                            run_entry(engine, entry, &body, &lookup)
                        }
                        Err(e) => CaseResult::from_outcomes(&[Outcome::Fail(format!(
                            "unreadable test: {e}"
                        ))]),
                    };
                    if let Ok(mut m) = out.lock() {
                        m.insert(entry.path.clone(), result);
                    }
                }
            });
        }
    });
    out.lock().map(|m| m.clone()).unwrap_or_default()
}

fn env_usize(name: &str) -> Option<usize> {
    std::env::var(name).ok().and_then(|v| v.parse().ok())
}

fn deadline() -> Duration {
    Duration::from_millis(env_usize("PX_TEST262_TIMEOUT_MS").unwrap_or(30_000) as u64)
}

fn jobs() -> usize {
    env_usize("PX_TEST262_JOBS").unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    })
}

#[cfg(test)]
fn read_baselines() -> (BTreeMap<String, Status>, BTreeMap<String, String>) {
    let dir = corpus_dir();
    let results = std::fs::read_to_string(dir.join("RESULTS")).unwrap_or_default();
    let expected = std::fs::read_to_string(dir.join("EXPECTED_FAILURES")).unwrap_or_default();
    (
        parse_results(&results).expect("RESULTS parses"),
        parse_expected(&expected).expect("EXPECTED_FAILURES parses"),
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

fn harness_lookup(name: &str) -> Option<String> {
    std::fs::read_to_string(corpus_dir().join("harness").join(name)).ok()
}

fn entry(flags: &[&str], neg: Option<(Phase, &str)>, includes: &[&str]) -> IndexEntry {
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect();
    IndexEntry {
        path: "t/x.js".into(),
        flags: v(flags),
        negative: neg.map(|(phase, kind)| test262::Negative {
            phase,
            kind: kind.into(),
        }),
        includes: v(includes),
        features: Vec::new(),
    }
}

fn status_of(e: &IndexEntry, body: &str) -> CaseResult {
    run_entry(
        &BoaEngine::new(Duration::from_secs(60)),
        e,
        body,
        &harness_lookup,
    )
}

#[test]
fn script_test262_engine_passing_and_failing_assertions() {
    let ok = status_of(&entry(&[], None, &[]), "assert.sameValue(1 + 1, 2);");
    assert_eq!(ok.status, Status::Pass, "{ok:?}");
    let bad = status_of(&entry(&[], None, &[]), "assert.sameValue(1, 2);");
    assert_eq!(
        (bad.status, bad.class.as_str()),
        (Status::Fail, "assertion")
    );
    // Both modes run: this one throws only in strict code.
    let strict_only = status_of(
        &entry(&[], None, &[]),
        "if ((function () { return this === undefined; })()) { throw new Test262Error('s'); }",
    );
    assert_eq!(strict_only.status, Status::Fail);
    // `noStrict` and `onlyStrict` pick one mode.
    let sloppy = status_of(
        &entry(&["noStrict"], None, &[]),
        "if ((function () { return this === undefined; })()) { throw new Test262Error('s'); }",
    );
    assert_eq!(sloppy.status, Status::Pass);
}

#[test]
fn script_test262_engine_negative_phases() {
    let syntax = entry(&["raw"], Some((Phase::Parse, "SyntaxError")), &[]);
    assert_eq!(status_of(&syntax, "var 1 = ;").status, Status::Pass);
    // `early` is the same stage as `parse` for an engine.
    let early = entry(&[], Some((Phase::Early, "SyntaxError")), &[]);
    assert_eq!(status_of(&early, "let a; let a;").status, Status::Pass);
    // A SyntaxError thrown while running is a runtime error, not a parse error.
    let runtime = entry(&[], Some((Phase::Runtime, "SyntaxError")), &[]);
    assert_eq!(
        status_of(&runtime, "eval('var 1 = ;');").status,
        Status::Pass
    );
    let wrong_stage = entry(&[], Some((Phase::Parse, "SyntaxError")), &[]);
    assert_eq!(
        status_of(&wrong_stage, "eval('var 1 = ;');").status,
        Status::Fail
    );
    // A negative test whose program runs fine fails.
    let r = status_of(&syntax, "var a = 1;");
    assert_eq!(
        (r.status, r.class.as_str()),
        (Status::Fail, "negative-accepted")
    );
    // A positive test the parser rejects is classed as such.
    let r = status_of(&entry(&[], None, &[]), "var 1 = ;");
    assert_eq!(
        (r.status, r.class.as_str()),
        (Status::Fail, "syntax-rejected")
    );
}

#[test]
fn script_test262_engine_async_protocol() {
    let e = entry(&["async"], None, &[]);
    let done = status_of(&e, "Promise.resolve(1).then(function () { $DONE(); });");
    assert_eq!(done.status, Status::Pass, "{done:?}");
    // A timer-driven completion needs the loop, on the manual clock.
    let timer = status_of(&e, "setTimeout(function () { $DONE(); }, 5000);");
    assert_eq!(timer.status, Status::Pass, "{timer:?}");
    let failed = status_of(
        &e,
        "Promise.resolve().then(function () { $DONE(new Error('x')); });",
    );
    assert_eq!(
        (failed.status, failed.class.as_str()),
        (Status::Fail, "async-failed")
    );
    let never = status_of(&e, "var x = 1;");
    assert_eq!(
        (never.status, never.class.as_str()),
        (Status::Fail, "async-no-done")
    );
}

#[test]
fn script_test262_engine_includes_and_hooks() {
    let inc = entry(&[], None, &["compareArray.js"]);
    assert_eq!(
        status_of(&inc, "assert.compareArray([1, 2], [1, 2]);").status,
        Status::Pass
    );
    assert_eq!(
        status_of(
            &entry(&[], None, &[]),
            "$262.gc(); assert.sameValue($262.evalScript('1+1'), 2);"
        )
        .status,
        Status::Pass
    );
    // Hooks the host does not provide make the test ungraded, with the reason.
    for (body, token) in [
        ("var r = $262.createRealm();", "hook-createRealm"),
        ("$262.agent.start('');", "hook-agent"),
        ("$262.detachArrayBuffer(b);", "hook-detachArrayBuffer"),
        ("$262.IsHTMLDDA();", "hook-IsHTMLDDA"),
    ] {
        let r = status_of(&entry(&[], None, &[]), body);
        assert_eq!((r.status, r.class.as_str()), (Status::Ungraded, token));
    }
    let mut cross = entry(&[], None, &[]);
    cross.features = vec!["cross-realm".into()];
    assert_eq!(status_of(&cross, "1;").class, "hook-createRealm");
    let mut sab = entry(&[], None, &[]);
    sab.features = vec!["SharedArrayBuffer".into()];
    assert_eq!(status_of(&sab, "1;").class, "deliberately-absent-global");
    let dab = entry(&[], None, &["detachArrayBuffer.js"]);
    assert_eq!(status_of(&dab, "1;").class, "hook-detachArrayBuffer");
}

#[test]
fn script_test262_engine_hard_deadline_and_limits() {
    // A run that does not finish by the deadline is a recorded failure, and the
    // run goes on. (`while (true)` ends at the engine's loop limit eventually;
    // the deadline here is far shorter.)
    let engine = BoaEngine::new(Duration::from_millis(50));
    let started = Instant::now();
    let r = run_entry(
        &engine,
        &entry(&["noStrict"], None, &[]),
        "while (true) {}",
        &harness_lookup,
    );
    assert_eq!(
        (r.status, r.class.as_str()),
        (Status::Fail, "timeout"),
        "{r:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(20));
    // A runaway the host's own limit stops is a failure of its own class.
    let engine = BoaEngine::new(Duration::from_secs(120));
    let r = run_entry(
        &engine,
        &entry(&["noStrict"], None, &[]),
        "function f() { return f(); } f();",
        &harness_lookup,
    );
    assert_eq!(r.status, Status::Fail);
    // Each run has a fresh host: a global from the last test is not there.
    let a = status_of(&entry(&["noStrict"], None, &[]), "globalThis.leak = 1;");
    let b = status_of(
        &entry(&["noStrict"], None, &[]),
        "assert.sameValue(typeof leak, 'undefined');",
    );
    assert_eq!((a.status, b.status), (Status::Pass, Status::Pass));
}

#[test]
fn script_test262_assembly_matches_the_documented_rules() {
    // The first vendored test with an include: assembled by the shared code and
    // by hand from INTERPRETING.md, the two must agree byte for byte.
    let entries = load_index();
    let e = entries
        .iter()
        .find(|e| !e.includes.is_empty() && !e.has_flag("raw") && !e.has_flag("async"))
        .expect("an entry with includes");
    let body = std::fs::read_to_string(corpus_dir().join("test").join(&e.path)).expect("body");
    let case = Case {
        entry: e.clone(),
        body: body.clone(),
    };
    let mut by_hand = String::new();
    let mut files = vec!["assert.js".to_string(), "sta.js".to_string()];
    for i in &e.includes {
        if !files.contains(i) {
            files.push(i.clone());
        }
    }
    for f in &files {
        let t = harness_lookup(f).expect("harness file");
        by_hand.push_str(&t);
        if !t.ends_with('\n') {
            by_hand.push('\n');
        }
    }
    by_hand.push_str(&body);
    let sloppy = case.source_for(Mode::Sloppy, &harness_lookup).expect("src");
    if case.modes().contains(&Mode::Sloppy) {
        assert_eq!(sloppy, by_hand);
    }
    let strict = case.source_for(Mode::Strict, &harness_lookup).expect("src");
    assert_eq!(strict, format!("\"use strict\";\n{by_hand}"));
    // And the marker the engine adds keeps the strict prologue first.
    let marked = with_marker(&strict, Mode::Strict);
    assert!(marked.starts_with("\"use strict\";\n__px_mark();\n"));
    assert_eq!(with_marker("x", Mode::Raw), "x");
}

#[test]
fn script_test262_index_is_complete_on_disk() {
    let entries = load_index();
    assert!(
        entries.len() >= CORPUS_FLOOR,
        "{} tests, floor {CORPUS_FLOOR}",
        entries.len()
    );
    let harness = harness_cache();
    for e in &entries {
        assert!(
            corpus_dir().join("test").join(&e.path).is_file(),
            "{} is listed but not vendored",
            e.path
        );
        for i in &e.includes {
            assert!(
                harness.contains_key(i),
                "{}: include {i} not vendored",
                e.path
            );
        }
    }
    for needed in ["assert.js", "sta.js", "doneprintHandle.js"] {
        assert!(harness.contains_key(needed), "{needed}");
    }
}

#[test]
fn script_test262_annexb_slice_matches_the_baseline() {
    // A fast slice of the real corpus against the committed baseline, so an
    // ordinary `cargo test` notices an engine or host change that regresses it.
    // (The whole corpus is `script_test262_corpus`.)
    let (baseline, expected) = read_baselines();
    if baseline.is_empty() {
        return; // no baseline recorded yet
    }
    let entries = load_index();
    let engine = BoaEngine::new(deadline());
    let now = statuses(&run_corpus(&engine, &entries, "annexB/", jobs()));
    assert!(!now.is_empty());
    let mut problems = Vec::new();
    for (p, st) in &now {
        match (baseline.get(p), st) {
            (Some(Status::Pass), Status::Pass) => {}
            (Some(Status::Pass), other) => {
                problems.push(format!("{p}: was pass, now {}", other.as_str()))
            }
            (_, Status::Fail) if !expected.contains_key(p) => {
                problems.push(format!("{p}: fails, not in EXPECTED_FAILURES"))
            }
            _ => {}
        }
    }
    assert!(problems.is_empty(), "{problems:#?}");
}

#[test]
fn script_test262_corpus() {
    let out = std::env::var_os("PX_TEST262_OUT");
    let full = std::env::var_os("PX_TEST262_FULL").is_some();
    if out.is_none() && !full {
        eprintln!(
            "script_test262_corpus: set PX_TEST262_OUT=<file> (or PX_TEST262_FULL=1) to run the whole subset"
        );
        return;
    }
    let filter = std::env::var("PX_TEST262_FILTER").unwrap_or_default();
    let entries = load_index();
    let engine = BoaEngine::new(deadline());
    let started = Instant::now();
    let results = run_corpus(&engine, &entries, &filter, jobs());
    let now = statuses(&results);
    let c = Counts::of(&now);
    eprintln!(
        "test262: {} tests in {:.1}s on {} worker(s): pass {} fail {} ungraded {} (graded {})",
        now.len(),
        started.elapsed().as_secs_f64(),
        jobs(),
        c.pass,
        c.fail,
        c.ungraded,
        c.graded()
    );
    // Per top-level directory.
    let mut dirs: BTreeMap<&str, Counts> = BTreeMap::new();
    for (p, st) in &now {
        let top = p.split('/').next().unwrap_or("");
        let d = dirs.entry(top).or_default();
        match st {
            Status::Pass => d.pass += 1,
            Status::Fail => d.fail += 1,
            Status::Ungraded => d.ungraded += 1,
        }
    }
    for (d, c) in &dirs {
        eprintln!(
            "test262: {d}: pass {} fail {} ungraded {} ({:.2}% of graded)",
            c.pass,
            c.fail,
            c.ungraded,
            100.0 * c.pass as f64 / c.graded().max(1) as f64
        );
    }
    if let Some(path) = out {
        std::fs::write(&path, format_run(&results)).expect("write the results file");
    } else {
        let (baseline, expected) = read_baselines();
        let mut v = ratchet(&baseline, &now, entries.len(), CORPUS_FLOOR, MATCH_FLOOR);
        v.problems.extend(check_expected(&expected, &now));
        assert!(v.problems.is_empty(), "{:#?}", v.problems);
    }
}
