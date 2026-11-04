//! Phase 7 gate item: the WPT css-flexbox reftest subset (ADR 031).
//!
//! Same instrument as Phase 6's `reftest.rs` (ADR 029): both halves of a pair are
//! laid out against the same stub text metric and their box geometry is compared,
//! because a reftest asserts an equality and an equality survives the missing paint
//! and shaping. The shared machinery is in `reftest_common/`; read its header for
//! what differs from Phase 6 (pairing by `rel="match"`, linked stylesheets).
//!
//! # What the numbers mean
//!
//! [`MATCH_FLOOR`] is a ratchet over a pinned corpus, as in Phase 6: it may not fall,
//! the corpus size is pinned by the test below and by `cargo xtask flex-grid`, and so
//! it cannot be met by deleting tests (ADR 019). The report also prints the ungraded
//! figures -- tests dropped at vendoring, from PROVENANCE -- beside the graded one,
//! and splits the non-matches into categories: needs a script host,
//! comparison-method exclusion, linked stylesheet not vendored, did not lay out,
//! panicked, and the remainder, which is "layout differs": an unimplemented feature
//! or a defect. That last bucket is deliberately not subdivided by filename.
//!
//! ```text
//! REPORT=1 cargo test -p px-layout --test flex_reftest -- --nocapture
//! DIAG=<pair name> cargo test -p px-layout --test flex_reftest -- --nocapture
//! ```
//!
//! # Where the floor came from
//!
//! **Seventy of 543, from the first honest measured run on `phase07/reftests`, before any flex layout exists.** `display: flex` is not implemented on this branch, so every flex container lays out as a plain block and a test and its reference agree whenever the property under test does not change the block geometry. That is agreement-in-brokenness, the thing `reftest.rs` documents learning about 17 pairs of: expect some of these 70 to stop matching once flex lands, and expect the number to rise past them as flex lays boxes out for real. Whoever lands flex layout and sees this fall must read the pairs that moved, say in the commit which were agreeing about nothing, and lower the floor deliberately in that commit; it must not be lowered to make a regression pass. Of the 473 not matching, 28 link a stylesheet that is not in the pinned sparse checkout (`/fonts/ahem.css`, `css/support/`) and the rest differ in layout. None needs a script host (scripted tests were dropped at vendoring) and none is yet a comparison-method exclusion.

mod reftest_common;

use std::sync::OnceLock;

use reftest_common::{Category, Outcome, Suite, diagnose, evaluate, stale_exclusions, summary};

const SUITE: Suite = Suite {
    dir: "css-flexbox",
    label: "css-flexbox",
    expected_failures: EXPECTED_FAILURES,
};

/// Pairs the *comparison method* gets wrong, each with the reason, read pair by
/// pair. Empty: no pair has been read yet, and writing reasons by filename would be
/// a guess dressed as documentation. Enforced in both directions.
const EXPECTED_FAILURES: &[(&str, &str)] = &[];

/// How many pairs must match. **It may not fall.** Raising it is a deliberate
/// commit whose diff says the engine improved.
const MATCH_FLOOR: usize = 70;

/// The corpus size this suite is pinned at (kept pairs in PROVENANCE: 543).
const MIN_PAIRS: usize = 540;

/// `(scripted, other)` tests dropped at vendoring, from `tests/wpt/css-flexbox/PROVENANCE`.
const DROPPED: (usize, usize) = (66, 402);

fn outcomes() -> &'static [Outcome] {
    static OUTCOMES: OnceLock<Vec<Outcome>> = OnceLock::new();
    OUTCOMES.get_or_init(|| evaluate(&SUITE))
}

#[test]
fn layout_flex_reftest_the_subset_is_present_and_paired() {
    let found = outcomes().len();
    assert!(
        found >= MIN_PAIRS,
        "cargo xtask flex-grid pins the corpus at {MIN_PAIRS} pairs or more; found {found}"
    );
    assert!(
        outcomes().iter().all(|o| !o.name.contains("grid-lanes")),
        "grid-lanes is out of scope (ADR 031 item 2)"
    );
}

#[test]
fn layout_flex_reftest_matches_at_or_above_the_pinned_floor() {
    let outcomes = outcomes();
    let passed = outcomes.iter().filter(|o| o.matched).count();

    // Printed unconditionally, so the graded and ungraded figures are
    // reconcilable from the output (ADR 019's habit).
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

/// A pair on which layout panics is a defect, never a feature gap, so it is its own
/// assertion rather than one more way of not matching.
#[test]
fn layout_flex_reftest_no_pair_panics() {
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

/// An expected failure that starts passing must be taken off the list: a stale
/// exclusion looks exactly like a test nobody checked.
#[test]
fn layout_flex_reftest_no_expected_failure_has_started_passing() {
    let stale = stale_exclusions(&SUITE, outcomes());
    assert!(
        stale.is_empty(),
        "these expected failures are stale -- they pass now, or are not in the \
         corpus at all:\n  {}",
        stale.join("\n  ")
    );
}
