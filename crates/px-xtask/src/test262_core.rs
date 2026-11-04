//! The pure half of the test262 runner: assembly, grading, results and the
//! ratchet. Std only, and no `crate::` path except `crate::test262` (the INDEX
//! format), so that two crates can compile the very same source:
//!
//! * `px-xtask` (`cargo xtask test262`), which holds the ratchet and the
//!   committed baselines but links no JavaScript engine, and
//! * `px-script` (`tests/test262.rs`), which links the engine and runs the
//!   corpus. It includes this file and `test262.rs` with `#[path]`, the way
//!   `gates/render.rs` includes `png.rs`.
//!
//! One copy of the rules, compiled twice, cannot drift: that is the equivalence
//! argument, and the unit tests below run again inside `px-script`'s test
//! binary (where a `script_test262_` test also pins the assembly of a real
//! vendored test).

use crate::test262::{IndexEntry, Phase};
use std::collections::BTreeMap;

/// The vendored corpus may not shrink below this many tests (10356 vendored).
pub const CORPUS_FLOOR: usize = 10_000;

/// Tests that must pass: the number of tests on which the engine matches the
/// behaviour test262 specifies. Raised in the commit that raises the pass count;
/// it never falls.
///
/// Set from the first honest run of the Boa 0.22.0 runner (2026-11-04, debug
/// profile, 4 workers): 10356 tests, 10077 pass, 156 fail, 123 ungraded (hooks
/// the host does not provide, and globals absent by ADR 040); 98.5% of the
/// 10233 graded tests. Two runs gave identical counts. The ratchet already
/// forbids any recorded pass from regressing; this is the second wall, so a
/// `RESULTS` file edited down cannot lower the bar.
pub const MATCH_FLOOR: usize = 10_077;

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
            if same_stage(*phase, n.phase) && *kind == n.kind {
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

/// Whether two phases are the same stage as far as an engine can tell.
///
/// test262 distinguishes `parse` (a syntax error in the grammar) from `early`
/// (an early error the specification lists), and an engine that rejects the
/// program before running any of it cannot say which; INTERPRETING.md requires
/// only that both be reported before evaluation. So the two are one stage,
/// `Runtime` is another, and `Resolution` is its own.
pub fn same_stage(a: Phase, b: Phase) -> bool {
    let stage = |p: Phase| match p {
        Phase::Parse | Phase::Early => 0,
        Phase::Resolution => 1,
        Phase::Runtime => 2,
    };
    stage(a) == stage(b)
}

/// Longest `detail` kept for one result (the file stays one line per test).
pub const DETAIL_MAX: usize = 160;

/// A test's status with the reason, so a failure can be classified and an
/// ungraded test says why it is ungraded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseResult {
    pub status: Status,
    /// Empty for a pass; for a failure, one of [`classify_failure`]'s classes;
    /// for an ungraded test, the reason token the runner gave.
    pub class: String,
    /// The first failing mode's message (empty for pass and ungraded).
    pub detail: String,
}

impl CaseResult {
    pub fn from_outcomes(outcomes: &[Outcome]) -> CaseResult {
        let status = Status::combine(outcomes);
        let (class, detail) = match status {
            Status::Pass => (String::new(), String::new()),
            Status::Fail => {
                let msg = outcomes
                    .iter()
                    .find_map(|o| match o {
                        Outcome::Fail(m) => Some(m.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                (classify_failure(msg).to_string(), clean_detail(msg))
            }
            Status::Ungraded => {
                let why = outcomes
                    .iter()
                    .find_map(|o| match o {
                        Outcome::Ungraded(w) => Some(w.as_str()),
                        _ => None,
                    })
                    .unwrap_or("no-mode");
                (clean_detail(why), String::new())
            }
        };
        CaseResult {
            status,
            class,
            detail,
        }
    }
}

/// One line, no tabs, bounded: safe as a field of the tab-separated files.
pub fn clean_detail(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(DETAIL_MAX)
        .collect()
}

/// The reason class of a failure message from [`grade`]. The class is what
/// `EXPECTED_FAILURES` records, so a reader can tell "the engine rejects this
/// syntax" from "the engine runs it wrongly" without opening a test.
pub fn classify_failure(msg: &str) -> &'static str {
    if msg.contains(":Timeout") {
        "timeout"
    } else if msg.contains(":Terminated") {
        "limit-terminated"
    } else if msg.contains(":EnginePanic") {
        "engine-panic"
    } else if msg.starts_with("threw parse:SyntaxError") {
        "syntax-rejected"
    } else if msg.contains("but the program completed") {
        "negative-accepted"
    } else if msg.starts_with("expected ") {
        "negative-wrong-error"
    } else if msg.starts_with("Test262:AsyncTestFailure") {
        "async-failed"
    } else if msg.starts_with("async test never") {
        "async-no-done"
    } else if msg.starts_with("threw runtime:Test262Error") {
        "assertion"
    } else if msg.starts_with("threw runtime:TypeError") {
        "unexpected-typeerror"
    } else if msg.starts_with("threw runtime:") || msg.starts_with("threw ") {
        "unexpected-throw"
    } else {
        "other"
    }
}

/// `path<TAB>status<TAB>class<TAB>detail`, sorted by path: what the
/// engine-bound runner writes and `cargo xtask test262` reads.
pub fn format_run(results: &BTreeMap<String, CaseResult>) -> String {
    let mut s = String::new();
    for (p, r) in results {
        s.push_str(&format!(
            "{p}\t{}\t{}\t{}\n",
            r.status.as_str(),
            r.class,
            r.detail
        ));
    }
    s
}

pub fn parse_run(text: &str) -> Result<BTreeMap<String, CaseResult>, String> {
    let mut out = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.splitn(4, '\t').collect();
        let [p, st, class, detail] = f.as_slice() else {
            return Err(format!("run line {}: need 4 fields", n + 1));
        };
        let status = Status::parse(st)
            .ok_or_else(|| format!("run line {}: unknown status `{st}`", n + 1))?;
        let r = CaseResult {
            status,
            class: (*class).to_string(),
            detail: (*detail).to_string(),
        };
        if out.insert((*p).to_string(), r).is_some() {
            return Err(format!("run line {}: duplicate path {p}", n + 1));
        }
    }
    Ok(out)
}

/// The statuses alone, for the ratchet.
pub fn statuses(results: &BTreeMap<String, CaseResult>) -> BTreeMap<String, Status> {
    results.iter().map(|(p, r)| (p.clone(), r.status)).collect()
}

/// `EXPECTED_FAILURES`: `path<TAB>class` for every graded failure, sorted.
pub fn format_expected(results: &BTreeMap<String, CaseResult>) -> String {
    let mut s = String::from(
        "# test262 tests known to fail (cargo xtask test262 --update). Do not edit by hand.\n# path<TAB>reason class. A failure not listed here, or a listed test that no longer fails, fails the gate.\n",
    );
    for (p, r) in results {
        if r.status == Status::Fail {
            s.push_str(&format!("{p}\t{}\n", r.class));
        }
    }
    s
}

pub fn parse_expected(text: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (n, l) in text.lines().enumerate() {
        if l.trim().is_empty() || l.starts_with('#') {
            continue;
        }
        let (p, class) = l
            .split_once('\t')
            .ok_or_else(|| format!("EXPECTED_FAILURES line {}: no tab", n + 1))?;
        if class.is_empty() {
            return Err(format!("EXPECTED_FAILURES line {}: no reason class", n + 1));
        }
        if out.insert(p.to_string(), class.to_string()).is_some() {
            return Err(format!("EXPECTED_FAILURES line {}: duplicate {p}", n + 1));
        }
    }
    Ok(out)
}

/// Compare a run with `EXPECTED_FAILURES`: a failure that is not listed, and a
/// listed test that no longer fails, are both problems (ADR 019: the list is
/// kept true in both directions; the corpus is never shrunk to hit a number).
pub fn check_expected(
    expected: &BTreeMap<String, String>,
    now: &BTreeMap<String, Status>,
) -> Vec<String> {
    let mut v = Vec::new();
    for (p, st) in now {
        if *st == Status::Fail && !expected.contains_key(p) {
            v.push(format!("{p}: fails and is not in EXPECTED_FAILURES"));
        }
    }
    for p in expected.keys() {
        match now.get(p) {
            Some(Status::Fail) => {}
            Some(Status::Pass) => {
                v.push(format!("{p}: now passes; it is still in EXPECTED_FAILURES"))
            }
            Some(Status::Ungraded) => v.push(format!(
                "{p}: now ungraded; it is still in EXPECTED_FAILURES"
            )),
            None => v.push(format!("{p}: in EXPECTED_FAILURES but not run")),
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn parse_and_early_are_one_stage() {
        assert!(same_stage(Phase::Parse, Phase::Early));
        assert!(same_stage(Phase::Early, Phase::Parse));
        assert!(!same_stage(Phase::Parse, Phase::Runtime));
        assert!(!same_stage(Phase::Resolution, Phase::Runtime));
        let neg = entry("n.js", &[], Some((Phase::Early, "SyntaxError")), &[]);
        let threw = Run::Threw {
            phase: Phase::Parse,
            kind: "SyntaxError".into(),
        };
        assert_eq!(grade(&neg, false, &threw), Outcome::Pass);
    }

    #[test]
    fn failures_are_classified() {
        for (msg, class) in [
            ("threw parse:SyntaxError", "syntax-rejected"),
            ("threw runtime:Test262Error", "assertion"),
            ("threw runtime:TypeError", "unexpected-typeerror"),
            ("threw runtime:RangeError", "unexpected-throw"),
            ("threw runtime:Timeout", "timeout"),
            ("threw runtime:Terminated", "limit-terminated"),
            ("threw runtime:EnginePanic", "engine-panic"),
            (
                "expected parse:SyntaxError but the program completed",
                "negative-accepted",
            ),
            (
                "expected parse:SyntaxError, got runtime:TypeError",
                "negative-wrong-error",
            ),
            ("Test262:AsyncTestFailure:x", "async-failed"),
            ("async test never called $DONE", "async-no-done"),
            ("???", "other"),
        ] {
            assert_eq!(classify_failure(msg), class, "{msg}");
        }
    }

    #[test]
    fn case_result_keeps_the_first_reason() {
        let r = CaseResult::from_outcomes(&[
            Outcome::Pass,
            Outcome::Fail("threw runtime:Test262Error".into()),
            Outcome::Fail("threw parse:SyntaxError".into()),
        ]);
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.class, "assertion");
        let u = CaseResult::from_outcomes(&[Outcome::Ungraded("hook-createRealm".into())]);
        assert_eq!(
            (u.status, u.class.as_str()),
            (Status::Ungraded, "hook-createRealm")
        );
        let p = CaseResult::from_outcomes(&[Outcome::Pass]);
        assert_eq!(
            (p.status, p.class.as_str(), p.detail.as_str()),
            (Status::Pass, "", "")
        );
        assert_eq!(clean_detail("a\tb\nc"), "a b c");
        assert_eq!(clean_detail(&"x".repeat(500)).len(), DETAIL_MAX);
    }

    #[test]
    fn run_file_and_expected_round_trip() {
        let mut m = BTreeMap::new();
        m.insert(
            "a.js".to_string(),
            CaseResult::from_outcomes(&[Outcome::Pass]),
        );
        m.insert(
            "b.js".to_string(),
            CaseResult::from_outcomes(&[Outcome::Fail("threw runtime:Test262Error".into())]),
        );
        m.insert(
            "c.js".to_string(),
            CaseResult::from_outcomes(&[Outcome::Ungraded("hook-agent".into())]),
        );
        let text = format_run(&m);
        assert_eq!(parse_run(&text).unwrap(), m);
        assert_eq!(statuses(&m)["b.js"], Status::Fail);
        let ex = format_expected(&m);
        assert!(ex.contains("b.js\tassertion\n") && !ex.contains("a.js") && !ex.contains("c.js"));
        let parsed = parse_expected(&ex).unwrap();
        assert_eq!(parsed.len(), 1);
        assert!(parse_run("a.js\tpass\n").is_err());
        assert!(parse_expected("a.js\n").is_err());
        assert!(parse_expected("a.js\tx\na.js\ty\n").is_err());
    }

    #[test]
    fn expected_failures_are_kept_true_both_ways() {
        use Status::*;
        let exp: BTreeMap<String, String> =
            [("b", "assertion"), ("c", "timeout"), ("d", "x"), ("e", "x")]
                .iter()
                .map(|(a, b)| (a.to_string(), b.to_string()))
                .collect();
        let now = map(&[("a", Fail), ("b", Fail), ("c", Pass), ("d", Ungraded)]);
        let p = check_expected(&exp, &now);
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(p[0].starts_with("a:"));
        assert!(
            check_expected(
                &exp,
                &map(&[
                    ("b", Fail),
                    ("c", Fail),
                    ("d", Fail),
                    ("e", Fail),
                    ("z", Pass)
                ])
            )
            .is_empty()
        );
    }
}
