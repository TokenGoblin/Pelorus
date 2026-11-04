//! `cargo xtask flex-grid` -- the Phase 7 gate (build-spec section 9). Port of
//! `ci/gate-flex-grid.sh`.
//!
//!   - WPT css-flexbox subset at threshold
//!   - WPT css-grid subset at threshold
//!   - crashtests for both (ADR 031, item 5)
//!
//! ADR 031 fixes what the items mean: references are paired by reading
//! `rel="match"`, grid-lanes is out of scope, each reftest threshold is a pinned
//! ratchet that may not fall, flex and grid are graded separately, and the
//! crashtests are a hard pass rather than a threshold.
//!
//! Each named suite must (1) match at least one test in `cargo test --list`,
//! (2) live in its home file as `#[test]` functions, and (3) have none of its
//! tests `#[ignore]`d. Then the whole workspace test run must pass, and the
//! vendored corpora, read from the git index, may not shrink.
//!
//! The "named suite" check is duplicated in the `gate-layout` port; fold both
//! into a shared module once both have landed.

use crate::Gate;
use crate::git;
use std::path::Path;
use std::process::{Command, Stdio};

/// name, test-name prefix, file that must hold them. Every prefix carries
/// `layout_flex`/`layout_grid` so it cannot match another crate's tests.
const SUITES: &[(&str, &str, &str)] = &[
    (
        "flexbox-reftests",
        "layout_flex_reftest_",
        "crates/px-layout/tests/flex_reftest.rs",
    ),
    (
        "grid-reftests",
        "layout_grid_reftest_",
        "crates/px-layout/tests/grid_reftest.rs",
    ),
    (
        "flex-grid-crashtests",
        "layout_flexgrid_crashtest_",
        "crates/px-layout/tests/crashtest_flexgrid.rs",
    ),
];

/// Number of `libtest --list` entries (lines ending `: test`).
pub fn count_listed_tests(list_output: &str) -> usize {
    list_output
        .lines()
        .filter(|l| l.ends_with(": test"))
        .count()
}

/// Number of `--list --ignored` entries whose line contains `filter`.
pub fn count_ignored_matching(ignored_output: &str, filter: &str) -> usize {
    ignored_output
        .lines()
        .filter(|l| l.ends_with(": test") && l.contains(filter))
        .count()
}

/// Lines holding `#[test]` among each `fn <filter>` line and the two lines
/// before it (what `grep -B2 "fn $filter" | grep -c '#\[test\]'` counted:
/// each line once, however many matches share it).
pub fn count_test_attrs(source: &str, filter: &str) -> usize {
    let lines: Vec<&str> = source.lines().collect();
    let needle = format!("fn {filter}");
    let mut keep = vec![false; lines.len()];
    for (i, l) in lines.iter().enumerate() {
        if l.contains(&needle) {
            for k in keep.iter_mut().take(i + 1).skip(i.saturating_sub(2)) {
                *k = true;
            }
        }
    }
    lines
        .iter()
        .zip(&keep)
        .filter(|(l, k)| **k && l.contains("#[test]"))
        .count()
}

/// Decide one suite. `Ok` is the `ok` line, `Err` the `FAIL` line. `home_src`
/// is `None` when the home file does not exist.
pub fn judge_suite(
    item: &str,
    filter: &str,
    home: &str,
    matched: usize,
    home_src: Option<&str>,
    ignored: usize,
) -> Result<String, String> {
    if matched == 0 {
        return Err(format!(
            "gate item '{item}' has no tests matching '{filter}'"
        ));
    }
    let Some(src) = home_src else {
        return Err(format!(
            "gate item '{item}' expects tests in {home}, which does not exist"
        ));
    };
    let in_home = count_test_attrs(src, filter);
    if in_home == 0 {
        return Err(format!(
            "gate item '{item}' has no #[test] fn matching '{filter}' in {home}"
        ));
    }
    if ignored > 0 {
        return Err(format!(
            "gate item '{item}' has {ignored} #[ignore]d test(s)"
        ));
    }
    Ok(format!(
        "{item}: {matched} test(s), {in_home} in {home}, none ignored"
    ))
}

fn is_html_exact(p: &str) -> bool {
    p.ends_with(".html")
}

fn is_html_or_htm(p: &str) -> bool {
    p.ends_with(".html") || p.ends_with(".htm")
}

fn check_corpus(
    g: &mut Gate,
    root: &Path,
    label: &str,
    dir: &str,
    is_match: fn(&str) -> bool,
    floor: usize,
) {
    let files = git::ls_files(root, &[], &[dir]).unwrap_or_default();
    if files.is_empty() {
        g.fail(format!("{label} is not vendored at {dir}"));
        return;
    }
    let count = files.iter().filter(|f| is_match(f)).count();
    if count < floor {
        g.fail(format!("{label} has {count} files, floor is {floor}"));
    } else {
        g.ok(format!(
            "{label} has {count} files in the index (floor {floor})"
        ));
    }
}

fn check_corpora(g: &mut Gate, root: &Path) {
    // 27 + 35 vendored; the floor leaves room for a re-vendor to drop a few.
    check_corpus(
        g,
        root,
        "the flexbox crashtests",
        "tests/wpt/css-flexbox-crashtests",
        is_html_exact,
        20,
    );
    check_corpus(
        g,
        root,
        "the grid crashtests",
        "tests/wpt/css-grid-crashtests",
        is_html_exact,
        25,
    );
    // The reftest subsets (ADR 031): tests, references and the few support
    // files they need. The pair counts are pinned by the harnesses (540 and
    // 400); these floors catch the files themselves going missing.
    check_corpus(
        g,
        root,
        "the flexbox reftest subset",
        "tests/wpt/css-flexbox",
        is_html_or_htm,
        950,
    );
    check_corpus(
        g,
        root,
        "the grid reftest subset",
        "tests/wpt/css-grid",
        is_html_or_htm,
        700,
    );

    // ADR 031 item 2: grid-lanes must stay out of the graded subset.
    let grid = git::ls_files(root, &[], &["tests/wpt/css-grid"]).unwrap_or_default();
    if grid.iter().any(|p| p.contains("grid-lanes")) {
        g.fail("grid-lanes files are in tests/wpt/css-grid; ADR 031 excludes them");
    } else {
        g.ok("grid-lanes is not in the graded subset");
    }
}

/// stdout of `cargo <args>`, stderr discarded, exit status ignored (as
/// `cmd 2>/dev/null | grep` did). `None` if cargo cannot be started.
fn cargo_stdout(root: &Path, args: &[&str]) -> Option<String> {
    let prog = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let o = Command::new(prog)
        .args(args)
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn list(root: &Path, extra: &[&str]) -> Option<String> {
    let mut a = vec!["test", "--workspace", "--locked", "--", "--list"];
    a.extend_from_slice(extra);
    cargo_stdout(root, &a)
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if cargo_stdout(root, &["--version"]).is_none() {
        g.fail("cargo is not on PATH");
        return;
    }

    let ignored_tests = list(root, &["--ignored"]).unwrap_or_default();

    for (item, filter, home) in SUITES {
        let matched = count_listed_tests(&list(root, &[filter]).unwrap_or_default());
        let src = std::fs::read(root.join(home))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let ignored = count_ignored_matching(&ignored_tests, filter);
        match judge_suite(item, filter, home, matched, src.as_deref(), ignored) {
            Ok(m) => g.ok(m),
            Err(m) => g.fail(m),
        }
    }

    g.info("cargo test --workspace --locked");
    match crate::proc::cargo(root, &["test", "--workspace", "--locked"]) {
        Ok(s) if s.success() => {}
        _ => g.fail("tests failed"),
    }

    // The corpora are committed and cannot quietly shrink (ADR 019's pattern).
    // Read from the git index: an empty directory git cannot store and
    // `git status` does not report is how Phase 4 lost five commits.
    check_corpora(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const LIST: &str = "a::b: test\nc::d: test\nsome bench: benchmark\n\n3 tests, 0 benchmarks\n";

    #[test]
    fn counts_only_test_lines() {
        assert_eq!(count_listed_tests(LIST), 2);
        assert_eq!(count_listed_tests(""), 0);
    }

    #[test]
    fn ignored_counts_filter_matches() {
        let out = "layout_flex_reftest_x: test\nother::layout_flex_reftest_y: test\nlayout_grid_reftest_z: test\n";
        assert_eq!(count_ignored_matching(out, "layout_flex_reftest_"), 2);
        assert_eq!(count_ignored_matching(out, "layout_flexgrid_crashtest_"), 0);
    }

    #[test]
    fn test_attr_window_is_two_lines_before() {
        let f = "layout_flex_reftest_";
        let src = "#[test]\nfn layout_flex_reftest_a() {}\n\n\n#[test]\n// c\n// d\nfn layout_flex_reftest_b() {}\n";
        // second fn: window is "// c", "// d" -- its attribute is three back.
        assert_eq!(count_test_attrs(src, f), 1);
        let src2 = "#[test]\n#[ignore]\nfn layout_flex_reftest_a() {}\n";
        assert_eq!(count_test_attrs(src2, f), 1);
        assert_eq!(count_test_attrs("fn layout_flex_reftest_a() {}\n", f), 0);
        // overlapping windows count a line once
        let src3 = "#[test]\nfn layout_flex_reftest_a() {}\nfn layout_flex_reftest_b() {}\n";
        assert_eq!(count_test_attrs(src3, f), 1);
    }

    #[test]
    fn suite_judging_order_and_messages() {
        let f = "layout_flex_reftest_";
        let src = "#[test]\nfn layout_flex_reftest_a() {}\n";
        assert_eq!(
            judge_suite("i", f, "h.rs", 0, Some(src), 0).unwrap_err(),
            "gate item 'i' has no tests matching 'layout_flex_reftest_'"
        );
        assert_eq!(
            judge_suite("i", f, "h.rs", 1, None, 0).unwrap_err(),
            "gate item 'i' expects tests in h.rs, which does not exist"
        );
        assert_eq!(
            judge_suite("i", f, "h.rs", 1, Some("fn layout_flex_reftest_a() {}"), 0).unwrap_err(),
            "gate item 'i' has no #[test] fn matching 'layout_flex_reftest_' in h.rs"
        );
        assert_eq!(
            judge_suite("i", f, "h.rs", 1, Some(src), 2).unwrap_err(),
            "gate item 'i' has 2 #[ignore]d test(s)"
        );
        assert_eq!(
            judge_suite("i", f, "h.rs", 3, Some(src), 0).unwrap(),
            "i: 3 test(s), 1 in h.rs, none ignored"
        );
    }

    fn corpus_fixture(tag: &str, n: usize) -> Fixture {
        let fx = Fixture::new(tag);
        for i in 0..n {
            fx.write(&format!("tests/wpt/css-grid/t{i}.html"), "x");
        }
        fx.write("tests/wpt/css-grid/support.css", "x");
        fx
    }

    fn corpus_gate(fx: &Fixture, floor: usize) -> Gate {
        let mut g = Gate::new("flex-grid");
        check_corpus(
            &mut g,
            fx.path(),
            "the grid",
            "tests/wpt/css-grid",
            is_html_or_htm,
            floor,
        );
        g
    }

    #[test]
    fn corpus_at_floor_passes_below_fails_and_counts_only_matching() {
        let fx = corpus_fixture("fg-floor", 5);
        fx.write("tests/wpt/css-grid/old.htm", "x").commit_all();
        assert!(corpus_gate(&fx, 6).passed());
        let g = corpus_gate(&fx, 7);
        assert_eq!(g.failures(), ["the grid has 6 files, floor is 7"]);
    }

    #[test]
    fn missing_corpus_fails_as_not_vendored() {
        let fx = Fixture::new("fg-missing");
        fx.write("README.md", "x").commit_all();
        let g = corpus_gate(&fx, 1);
        assert_eq!(
            g.failures(),
            ["the grid is not vendored at tests/wpt/css-grid"]
        );
    }

    #[test]
    fn untracked_corpus_counts_as_missing() {
        let fx = Fixture::new("fg-untracked");
        fx.write("README.md", "x").commit_all();
        fx.write("tests/wpt/css-grid/a.html", "x");
        assert_eq!(corpus_gate(&fx, 1).fail_count(), 1);
    }

    #[test]
    fn corpus_patterns() {
        assert!(is_html_exact("a.html"));
        assert!(!is_html_exact("a.htm"));
        assert!(is_html_or_htm("a.htm") && is_html_or_htm("a.html"));
        assert!(!is_html_or_htm("a.html.orig"));
    }

    #[test]
    fn grid_lanes_in_index_fails() {
        let fx = corpus_fixture("fg-lanes", 2);
        fx.write("tests/wpt/css-grid/grid-lanes/x.html", "x")
            .commit_all();
        let mut g = Gate::new("flex-grid");
        check_corpora(&mut g, fx.path());
        assert!(
            g.failures()
                .iter()
                .any(|f| f == "grid-lanes files are in tests/wpt/css-grid; ADR 031 excludes them"),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn no_grid_lanes_passes_that_check() {
        let fx = corpus_fixture("fg-nolanes", 2);
        fx.commit_all();
        let mut g = Gate::new("flex-grid");
        check_corpora(&mut g, fx.path());
        assert!(!g.failures().iter().any(|f| f.contains("grid-lanes")));
    }
}
