//! The WPT css-tables reftest subset (CSS 2.1 chapter 17 and CSS Tables 3).
//!
//! Same instrument as `flex_reftest.rs` and ADR 029: both halves of a pair are
//! laid out and their box geometry is compared (`reftest_common/`). [`MATCH_FLOOR`]
//! is a ratchet over a pinned corpus: it may not fall, and the corpus size is
//! pinned by [`MIN_PAIRS`], so it cannot be met by deleting tests.
//!
//! ```text
//! REPORT=1 cargo test -p px-layout --test table_reftest -- --nocapture
//! DIAG=<pair name> cargo test -p px-layout --test table_reftest -- --nocapture
//! ```

mod reftest_common;

use std::sync::OnceLock;

use reftest_common::{Category, Outcome, Suite, diagnose, evaluate, stale_exclusions, summary};

const SUITE: Suite = Suite {
    dir: "css-tables",
    label: "css-tables",
    expected_failures: EXPECTED_FAILURES,
};

/// Pairs the comparison method gets wrong, read pair by pair. Empty.
const EXPECTED_FAILURES: &[(&str, &str)] = &[];

/// How many pairs must match. **It may not fall.**
const MATCH_FLOOR: usize = 30;

/// The vendored corpus has this many pairs or more.
const MIN_PAIRS: usize = 70;

/// Dropped at vendoring (PROVENANCE): scripted, other.
const DROPPED: (usize, usize) = (29, 72);

fn outcomes() -> &'static [Outcome] {
    static OUTCOMES: OnceLock<Vec<Outcome>> = OnceLock::new();
    OUTCOMES.get_or_init(|| evaluate(&SUITE))
}

#[test]
fn layout_table_reftest_the_subset_is_present_and_paired() {
    let found = outcomes().len();
    assert!(
        found >= MIN_PAIRS,
        "cargo xtask layout pins the corpus at {MIN_PAIRS} pairs or more; found {found}"
    );
}

#[test]
fn layout_table_reftest_matches_at_or_above_the_pinned_floor() {
    let outcomes = outcomes();
    let passed = outcomes.iter().filter(|o| o.matched).count();
    summary(&SUITE, outcomes, MATCH_FLOOR, DROPPED);
    if let Ok(name) = std::env::var("DIAG") {
        diagnose(&SUITE, &name);
    }
    assert!(
        passed >= MATCH_FLOOR,
        "only {passed} of {} pairs matched, floor is {MATCH_FLOOR}. A fall is a \
         regression -- the corpus is pinned, so this cannot be a corpus change.",
        outcomes.len()
    );
}

#[test]
fn layout_table_reftest_no_pair_panics() {
    let panicked: Vec<&str> = outcomes()
        .iter()
        .filter(|o| o.category == Some(Category::Panicked))
        .map(|o| o.name.as_str())
        .collect();
    assert!(
        panicked.is_empty(),
        "layout panicked on {} pair(s):\n  {}",
        panicked.len(),
        panicked.join("\n  ")
    );
}

#[test]
fn layout_table_reftest_no_expected_failure_has_started_passing() {
    let stale = stale_exclusions(&SUITE, outcomes());
    assert!(
        stale.is_empty(),
        "these expected failures are stale:\n  {}",
        stale.join("\n  ")
    );
}
