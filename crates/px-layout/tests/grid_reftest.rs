//! Phase 7 gate item: the WPT css-grid reftest subset (ADR 031).
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
//! REPORT=1 cargo test -p px-layout --test grid_reftest -- --nocapture
//! DIAG=<pair name> cargo test -p px-layout --test grid_reftest -- --nocapture
//! ```
//!
//! # Where the floor came from
//!
//! **Seventy-seven of 405, from the first honest measured run on `phase07/reftests`, before any grid layout exists.** The same caveat as `flex_reftest.rs` applies in full: `display: grid` lays out as a block today, so these are pairs that agree about block geometry, and some will stop matching when grid is real (`subgrid` is 34 of 59 for this reason). Of the 328 not matching, 102 link `/css/support/grid.css` or another stylesheet that is not in the pinned sparse checkout, so they cannot match until `css/support/` is vendored; the remaining 226 differ in layout. None needs a script host.

mod reftest_common;

use std::sync::OnceLock;

use reftest_common::{Category, Outcome, Suite, diagnose, evaluate, stale_exclusions, summary};

const SUITE: Suite = Suite {
    dir: "css-grid",
    label: "css-grid",
    expected_failures: EXPECTED_FAILURES,
};

/// Pairs the *comparison method* gets wrong, each with the reason, read pair by
/// pair. Empty: no pair has been read yet, and writing reasons by filename would be
/// a guess dressed as documentation. Enforced in both directions.
const EXPECTED_FAILURES: &[(&str, &str)] = &[];

/// How many pairs must match. **It may not fall.** Raising it is a deliberate
/// commit whose diff says the engine improved.
///
/// History: 77 with grid unimplemented (grid containers laid out as plain blocks,
/// so a pair matched whenever the property under test did not change block
/// geometry); 75 once flex landed. **Lowered deliberately, with the pairs read:**
/// `alignment/grid-align-content-space-between-001.html` and
/// `alignment/grid-justify-content-space-between-001.html` matched by accident as
/// plain blocks and stopped matching when flex's shared alignment code changed
/// their layout; `abspos/grid-abspos-staticpos-align-items-center.html` and its
/// `-large-border-padding` sibling started matching. Neither is a grid defect:
/// grid layout did not exist yet, and these were to be re-measured when it did.
///
/// 71 once grid layout landed (`grid.rs`): **lowered deliberately, user-approved,
/// with every moved pair read.** Grid gained 37 pairs and lost 41 against the
/// plain-block baseline. None of the 41 is a grid defect:
/// - 26 are `subgrid/` pairs that matched only because grid laid out as plain
///   blocks. Subgrid is out of scope (ADR 031).
/// - Text metrics (the stub, no Ahem): the four `grid-percent-*-shrinkwrap` pairs
///   and `grid-intrinsic-maximums`.
/// - ADR 029's superset shape (the grid container is a box the reference lacks):
///   `grid-item-block-axis-content-contribution-001..003` and `auto-margins-008`.
/// - Baselines, fragmentation and vertical writing modes: `grid-baseline-align-
///   cycles-001`, `grid-justify-baseline-001`,
///   `grid-fragmentation-between-rows-001-print` and the `abspos` `vertWM` pairs.
/// - Grid-area placement of absolutely positioned children (not implemented):
///   `grid-abspos-staticpos-align-self-safe-outer-cb-003`.
/// - Flex-in-grid and form controls: `min-size-auto-overflow-clip` (flex's
///   min-size handling) and `stretch-grid-item-text-input-overflow`.
///
/// Measure with a clean build: a stale baseline binary reused through
/// `CARGO_TARGET_DIR` once made this read 75.
///
/// 67 once real shaped text replaced the half-em stub (Phase 9 unit 3, ADR 036):
/// **lowered deliberately, four pairs read, none gained.** 4 of 405 is 1% of the
/// suite. All four matched only because the stub ignored a property the real
/// text path reads, and in all four the document that would show the property
/// working is not the one laid out, because `/css/support/grid.css` is not
/// vendored (so `.grid` is a plain block and `.directionRTL` does nothing):
/// - `grid-items/whitespace-in-grid-item-001.html`: `.grid { white-space: pre }`
///   around a grid item and whitespace-only text. The reference has a real grid
///   container, whose whitespace-only text generates nothing (css-grid-1 section
///   6); the test has a plain block, where `white-space: pre` correctly keeps the
///   newlines and spaces as lines. With the stub those collapsed on both sides.
/// - `grid-model/grid-container-scrollbar-001.html` and its `-vertical-lr` and
///   `-vertical-rl` siblings: the reference sets `direction: rtl` inline and the
///   test relies on the missing `.directionRTL`. `text-align: start` now follows
///   `direction`, so the reference's text is right-aligned and the test's is not.
///
/// The harness now also satisfies WPT's `/fonts/ahem.css` (Ahem comes from the
/// font registry whether or not an `@font-face` names it). That moved 56 pairs
/// from "stylesheet not vendored" to "differ in layout" (113 to 57) and matched
/// none of them: they test baselines, writing modes, subgrid and the like.
const MATCH_FLOOR: usize = 67;

/// The corpus size this suite is pinned at (kept pairs in PROVENANCE: 405).
const MIN_PAIRS: usize = 400;

/// `(scripted, other)` tests dropped at vendoring, from `tests/wpt/css-grid/PROVENANCE`.
const DROPPED: (usize, usize) = (94, 270);

fn outcomes() -> &'static [Outcome] {
    static OUTCOMES: OnceLock<Vec<Outcome>> = OnceLock::new();
    OUTCOMES.get_or_init(|| evaluate(&SUITE))
}

#[test]
fn layout_grid_reftest_the_subset_is_present_and_paired() {
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
fn layout_grid_reftest_matches_at_or_above_the_pinned_floor() {
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
fn layout_grid_reftest_no_pair_panics() {
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
fn layout_grid_reftest_no_expected_failure_has_started_passing() {
    let stale = stale_exclusions(&SUITE, outcomes());
    assert!(
        stale.is_empty(),
        "these expected failures are stale -- they pass now, or are not in the \
         corpus at all:\n  {}",
        stale.join("\n  ")
    );
}
