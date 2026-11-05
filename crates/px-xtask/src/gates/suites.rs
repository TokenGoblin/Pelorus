//! The "named test suite" check shared by the phase gates (layout, flex-grid,
//! dom, style, ipc, sandbox, network).
//!
//! Phases 1-5 each taught the gates one way a green gate had meant nothing, and
//! every named suite is held to all three:
//!
//! 1. the filter must match at least one test in `cargo test -- --list`;
//! 2. the suite's home file must carry `#[test]` functions for it (a `#[test]`
//!    on, or within two lines before, a `fn <filter>` line), so a filter cannot
//!    be satisfied by some other crate's tests;
//! 3. none of the matching tests may be `#[ignore]`d.
//!
//! This is the one tested implementation. It was copied into the layout and
//! flex-grid ports before being folded here.

use crate::Gate;
use crate::proc;
use std::path::Path;
use std::process::Stdio;

/// One gate item: `(item name, test-name filter, file that must carry the
/// tests)`.
pub type Suite = (&'static str, &'static str, &'static str);

/// Lines of `cargo test -- --list` output that name a test (`<name>: test`).
pub fn count_listed_tests(listing: &str) -> usize {
    listing.lines().filter(|l| l.ends_with(": test")).count()
}

/// Test lines of `listing` that contain `filter` (what
/// `grep ": test$" | grep -c "$filter"` counted).
pub fn count_listed_matching(listing: &str, filter: &str) -> usize {
    listing
        .lines()
        .filter(|l| l.ends_with(": test") && l.contains(filter))
        .count()
}

/// How many distinct lines carrying `#[test]` sit on, or within two lines
/// before, a line containing `fn <filter>`. This is what `grep -B2` followed by
/// a count of `#[test]` lines measured: the attribute must be next to the
/// function it belongs to, so a stray attribute elsewhere in the file does not
/// satisfy the check. A line is counted once however many matches share it.
pub fn count_test_attrs(src: &str, filter: &str) -> usize {
    let lines: Vec<&str> = src.lines().collect();
    let needle = format!("fn {filter}");
    let mut shown = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if l.contains(&needle) {
            for s in shown.iter_mut().take(i + 1).skip(i.saturating_sub(2)) {
                *s = true;
            }
        }
    }
    lines
        .iter()
        .zip(&shown)
        .filter(|(l, s)| **s && l.contains("#[test]"))
        .count()
}

/// Why a suite was refused. The shared wording is [`problem_message`]; a gate
/// whose script said more than that (ipc) formats its own.
#[derive(Debug, PartialEq, Eq)]
pub enum Problem {
    /// The filter matches no test in `--list`.
    NoTests,
    /// The home file does not exist.
    NoHome,
    /// No `#[test]` sits next to a `fn <filter>` in the home file.
    NoAttr,
    /// This many matching tests are `#[ignore]`d.
    Ignored(usize),
}

/// The conditions, in the order the scripts applied them. `Ok` is
/// `(matched, in_home)`.
pub fn judge(
    filter: &str,
    listed: &str,
    home_src: Option<&str>,
    ignored: &str,
) -> Result<(usize, usize), Problem> {
    let matched = count_listed_tests(listed);
    if matched == 0 {
        return Err(Problem::NoTests);
    }
    let Some(src) = home_src else {
        return Err(Problem::NoHome);
    };
    let in_home = count_test_attrs(src, filter);
    if in_home == 0 {
        return Err(Problem::NoAttr);
    }
    let skipped = count_listed_matching(ignored, filter);
    if skipped > 0 {
        return Err(Problem::Ignored(skipped));
    }
    Ok((matched, in_home))
}

/// The `FAIL` line for a [`Problem`].
pub fn problem_message(item: &str, filter: &str, home: &str, p: &Problem) -> String {
    match p {
        Problem::NoTests => format!("gate item '{item}' has no tests matching '{filter}'"),
        Problem::NoHome => {
            format!("gate item '{item}' expects tests in {home}, which does not exist")
        }
        Problem::NoAttr => {
            format!("gate item '{item}' has no #[test] fn matching '{filter}' in {home}")
        }
        Problem::Ignored(n) => format!("gate item '{item}' has {n} #[ignore]d test(s)"),
    }
}

/// Decide one suite. `Ok` carries the `ok` line, `Err` the `FAIL` line.
///
/// `listed` is the `--list <filter>` output, `ignored` the `--list --ignored`
/// output, `home_src` the contents of `home` (`None` if it does not exist).
pub fn judge_suite(
    item: &str,
    filter: &str,
    home: &str,
    listed: &str,
    home_src: Option<&str>,
    ignored: &str,
) -> Result<String, String> {
    match judge(filter, listed, home_src, ignored) {
        Ok((matched, in_home)) => Ok(format!(
            "{item}: {matched} test(s), {in_home} in {home}, none ignored"
        )),
        Err(p) => Err(problem_message(item, filter, home, &p)),
    }
}

/// The `--list` lines of tests whose line contains `filter` (what
/// `grep "$filter"` printed from the ignored listing).
pub fn matching_lines<'a>(listing: &'a str, filter: &str) -> Vec<&'a str> {
    listing
        .lines()
        .filter(|l| l.ends_with(": test") && l.contains(filter))
        .collect()
}

/// stdout of `cargo <args>` in `root`, stderr discarded, exit status ignored
/// (as `cmd 2>/dev/null | grep` did). `None` if cargo cannot be started.
pub fn cargo_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let o = proc::cargo_command(root)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

/// `cargo test --workspace --locked -- --list <extra>`. A failed listing is an
/// empty listing, which the checks then report.
pub fn list_tests(root: &Path, extra: &[&str]) -> String {
    let mut a = vec!["test", "--workspace", "--locked", "--", "--list"];
    a.extend_from_slice(extra);
    cargo_stdout(root, &a).unwrap_or_default()
}

/// Whether `cargo` can be started at all (`command -v cargo`).
pub fn cargo_available(root: &Path) -> bool {
    cargo_stdout(root, &["--version"]).is_some()
}

/// Report one refused suite: `(gate, item, filter, home, problem, ignored
/// listing)`.
pub type Reporter = fn(&mut Gate, &str, &str, &str, &Problem, &str);

/// Judge every suite against the workspace's real test listing, using the
/// shared wording.
pub fn check_suites(g: &mut Gate, root: &Path, suites: &[Suite]) {
    check_suites_with(g, root, suites, |g, item, filter, home, p, _| {
        g.fail(problem_message(item, filter, home, p));
    });
}

/// As [`check_suites`], with the gate supplying the failure report.
pub fn check_suites_with(g: &mut Gate, root: &Path, suites: &[Suite], report: Reporter) {
    let ignored = list_tests(root, &["--ignored"]);
    for (item, filter, home) in suites {
        let listed = list_tests(root, &[filter]);
        let src = std::fs::read(root.join(home))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        match judge(filter, &listed, src.as_deref(), &ignored) {
            Ok((matched, in_home)) => g.ok(format!(
                "{item}: {matched} test(s), {in_home} in {home}, none ignored"
            )),
            Err(p) => report(g, item, filter, home, &p, &ignored),
        }
    }
}

/// `cargo test --workspace --locked --no-fail-fast` with inherited stdio; a failure
/// is the gate's `tests failed`.
///
/// `--no-fail-fast` because cargo otherwise stops at the first failing test BINARY,
/// so every crate after it alphabetically is never run. CI on Windows hid whole
/// crates behind one failing `px-gpu` binary that way. The exit status still
/// reports failure; the log now shows all of them.
pub fn run_workspace_tests(g: &mut Gate, root: &Path) {
    g.info("cargo test --workspace --locked --no-fail-fast");
    match proc::cargo(root, &["test", "--workspace", "--locked", "--no-fail-fast"]) {
        Ok(s) if s.success() => {}
        _ => g.fail("tests failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str =
        "a::layout_reftest_one: test\nb::layout_reftest_two: test\n\n2 tests, 0 benchmarks\n";

    #[test]
    fn counts_only_test_lines() {
        assert_eq!(count_listed_tests(LISTING), 2);
        assert_eq!(count_listed_tests("x: benchmark\n"), 0);
        assert_eq!(count_listed_tests(""), 0);
    }

    #[test]
    fn listed_matching_counts_filter_matches() {
        let out = "layout_flex_reftest_x: test\nother::layout_flex_reftest_y: test\nlayout_grid_reftest_z: test\n";
        assert_eq!(count_listed_matching(out, "layout_flex_reftest_"), 2);
        assert_eq!(count_listed_matching(out, "layout_flexgrid_crashtest_"), 0);
        let noise = "z::unrelated: test\nsummary layout_reftest_ not a test line\n";
        assert_eq!(count_listed_matching(noise, "layout_reftest_"), 0);
    }

    #[test]
    fn test_attr_must_sit_within_two_lines_of_the_fn() {
        let f = "layout_depth_";
        assert_eq!(count_test_attrs("#[test]\nfn layout_depth_a() {}\n", f), 1);
        let doc = "#[test]\n/// doc\nfn layout_depth_a() {}\n";
        assert_eq!(count_test_attrs(doc, f), 1);
        let far = "#[test]\n/// a\n/// b\nfn layout_depth_a() {}\n";
        assert_eq!(count_test_attrs(far, f), 0);
        // Overlapping windows do not double count one attribute.
        let two = "#[test]\nfn layout_depth_a() {}\nfn layout_depth_b() {}\n";
        assert_eq!(count_test_attrs(two, f), 1);
        let both = "#[test]\nfn layout_depth_a() {}\n#[test]\nfn layout_depth_b() {}\n";
        assert_eq!(count_test_attrs(both, f), 2);
        assert_eq!(count_test_attrs("fn other() {}\n", f), 0);
        // An #[ignore] between the attribute and the fn still has the attribute.
        let ign = "#[test]\n#[ignore]\nfn layout_depth_a() {}\n";
        assert_eq!(count_test_attrs(ign, f), 1);
        // The window is two lines before: this attribute is three back.
        let w =
            "#[test]\nfn layout_depth_a() {}\n\n\n#[test]\n// c\n// d\nfn layout_depth_b() {}\n";
        assert_eq!(count_test_attrs(w, f), 1);
    }

    #[test]
    fn suite_healthy() {
        let src = "#[test]\nfn layout_reftest_one() {}\n";
        let r = judge_suite("css2", "layout_reftest_", "t.rs", LISTING, Some(src), "");
        assert_eq!(r.unwrap(), "css2: 2 test(s), 1 in t.rs, none ignored");
    }

    #[test]
    fn suite_with_no_matching_test_fails() {
        let src = Some("#[test]\nfn layout_x_a(){}");
        let r = judge_suite("i", "layout_x_", "t.rs", "", src, "");
        assert_eq!(
            r.unwrap_err(),
            "gate item 'i' has no tests matching 'layout_x_'"
        );
    }

    #[test]
    fn suite_with_missing_home_fails() {
        let r = judge_suite("i", "layout_reftest_", "t.rs", LISTING, None, "");
        assert_eq!(
            r.unwrap_err(),
            "gate item 'i' expects tests in t.rs, which does not exist"
        );
    }

    #[test]
    fn suite_whose_home_has_no_test_attribute_fails() {
        let r = judge_suite(
            "i",
            "layout_reftest_",
            "t.rs",
            LISTING,
            Some("fn layout_reftest_one() {}\n"),
            "",
        );
        assert_eq!(
            r.unwrap_err(),
            "gate item 'i' has no #[test] fn matching 'layout_reftest_' in t.rs"
        );
    }

    #[test]
    fn suite_with_ignored_test_fails_but_other_ignores_do_not() {
        let src = Some("#[test]\nfn layout_reftest_one() {}\n");
        let ign = "x::layout_reftest_one: test\ny::layout_reftest_two: test\n";
        let r = judge_suite("i", "layout_reftest_", "t.rs", LISTING, src, ign);
        assert_eq!(r.unwrap_err(), "gate item 'i' has 2 #[ignore]d test(s)");
        let other = "z::unrelated: test\nsummary layout_reftest_ not a test line\n";
        assert!(judge_suite("i", "layout_reftest_", "t.rs", LISTING, src, other).is_ok());
    }

    #[test]
    fn problems_are_distinguished_and_matching_lines_listed() {
        let src = Some("#[test]\nfn layout_reftest_one() {}\n");
        assert_eq!(judge("f", "", src, ""), Err(Problem::NoTests));
        assert_eq!(judge("f", "a: test\n", None, ""), Err(Problem::NoHome));
        assert_eq!(
            judge("f", "a: test\n", Some("fn f_x() {}"), ""),
            Err(Problem::NoAttr)
        );
        let ign = "x::f_a: test\ny::g: test\nf_b: test\n";
        assert_eq!(
            judge("f_", "a: test\n", Some("#[test]\nfn f_x() {}"), ign),
            Err(Problem::Ignored(2))
        );
        assert_eq!(matching_lines(ign, "f_"), ["x::f_a: test", "f_b: test"]);
    }
}
