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
///
/// History: 70 with flex unimplemented (flex containers laid out as plain blocks);
/// 150 once `flex.rs` landed (+80, 0 panics, 0 pairs lost).
///
/// 151 once real shaped text replaced the half-em stub (Phase 9 unit 3, ADR 036;
/// every family resolves to Ahem, one em per glyph). Two pairs gained and one was
/// lost, all read:
/// - Gained `flexbox_fbfc2.html` and `flexbox_item-clear.html`: a float beside a
///   flex container whose text wraps. With the stub the text broke at a different
///   column from the reference's equivalent; with real one-em glyphs both sides
///   break in the same place.
/// - Lost `percentage-widths-001.html`: an absolutely positioned flex container
///   whose items hold a paragraph of text. The reference fixes the items at 100px
///   and 500px; the test reaches them by shrinking a 20% and a 100% basis, which a
///   flex item may not do below its content-based minimum (css-flexbox-1 section
///   4.5). With Ahem at 16px the widest word ("percentage", ten glyphs) is 160px,
///   so the 100px the reference expects is below the minimum and the item stays
///   at 120px. That is the specified result for a font whose words are that wide;
///   a proportional face has "percentage" at about 80px, which is why browsers
///   pass. It is a consequence of the single-font policy, not a flex defect.
///
/// The 28 pairs that linked `/fonts/ahem.css` are now laid out as written
/// (the harness satisfies that sheet; see `reftest_common`), 27 of them differ in
/// layout and none matches, so they add no passes.
///
/// 156 once inline content had boxes of its own and `inline-flex` / `inline-grid`
/// became atomic inlines that sit on a line like an `inline-block` (Phase 9,
/// `inline.rs`), instead of being laid out as block-level boxes. Eight pairs
/// gained and three lost, all read:
/// - Gained `flex-inline.html`, `flexbox_inline.html`,
///   `flexbox-baseline-empty-001a.html` and `-001b.html`: they are about
///   `inline-flex` sitting inline (shrink-to-fit, next to text, on a baseline).
///   Gained `flexbox_box-clear.html`, `flexbox_flex-natural-mixed-basis.html`,
///   `flexbox_flex-natural-mixed-basis-auto.html` and `percentage-heights-004.html`:
///   text, spans and an `inline-flex` laid out by the new line machinery (the
///   mixed-basis pair puts `<span>` flex items in a flex container; the clear
///   pair has text that does not fit beside a float and now moves below it, §9.5).
///   These four were read for what they exercise but not bisected one by one.
/// - Lost `percentage-heights-015.html`, `percentage-padding-002.html` and
///   `flexbox-with-multi-column-property.html`: the test puts an `inline-flex`
///   on a line and the reference a block (or a top-aligned `inline-block`), so
///   the test's line box is the box plus the strut's descent below the baseline
///   (3px at 16px Ahem), or its inline box adds a line fragment, and the sets of
///   rectangles differ by that. Browsers draw the same picture (the extra is
///   empty space under the box), which a rectangle comparison cannot see: the
///   oracle's blind spot, not a flex defect.
///
/// 157 once tables lay out (`phase6/tables`): a flex test whose items are
/// `display: table` elements stopped being laid out as plain blocks.
const MATCH_FLOOR: usize = 157;

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
