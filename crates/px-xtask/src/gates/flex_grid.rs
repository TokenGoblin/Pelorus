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
//! The named-suite check is shared with the other phase gates: see `suites`.

use super::suites::{self, Suite};
use crate::Gate;
use crate::git;
use std::path::Path;

/// name, test-name prefix, file that must hold them. Every prefix carries
/// `layout_flex`/`layout_grid` so it cannot match another crate's tests.
const SUITES: &[Suite] = &[
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

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }

    suites::check_suites(g, root, SUITES);
    suites::run_workspace_tests(g, root);

    // The corpora are committed and cannot quietly shrink (ADR 019's pattern).
    // Read from the git index: an empty directory git cannot store and
    // `git status` does not report is how Phase 4 lost five commits.
    check_corpora(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

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
