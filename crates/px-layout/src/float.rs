//! Floats: taking a box out of flow and keeping track of where it landed.
//!
//! CSS 2.1 §9.5. A float is removed from the normal flow and shifted to the
//! inline-start or inline-end edge of its containing block, and the *content*
//! that follows it flows down the side — line boxes shorten to avoid it while
//! block boxes do not.
//!
//! # What this module is
//!
//! A [`FloatContext`] is the list of floats placed so far in one block formatting
//! context, in that context's coordinate space. Two questions are asked of it:
//!
//! - [`FloatContext::band`] — how much inline space is free across a given range
//!   of the block axis. Line boxes ask this.
//! - [`FloatContext::place`] — where does a new float of this size go, given
//!   everything already placed and the earliest block position it may take.
//!
//! Both are `O(floats)` linear scans. A page with a thousand floats in one
//! formatting context would want the interval tree real engines keep; the scan is
//! what makes the rules above legible next to the spec text they implement, and
//! `docs/backlog.md` is where the replacement goes when a profile asks for it.
//!
//! # What it is not, yet
//!
//! A float is placed against the floats in **its own containing block**. A float
//! in an ancestor formatting context does not shorten it, and `clear` is not here
//! at all. Both are named gaps rather than silent ones.

use app_units::Au;

use crate::geom::LogicalSize;

/// Which edge a float is shifted to.
///
/// Named for the *inline* axis rather than left and right, because that is what
/// the rest of this crate is written in — `direction: rtl` swaps which physical
/// edge `Start` means, and nothing here has to change for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FloatSide {
    /// `float: left` in a left-to-right containing block.
    Start,
    /// `float: right` in a left-to-right containing block.
    End,
}

/// Which floats a box must clear (§9.5.2).
///
/// Separate from [`FloatSide`] because `both` has no float counterpart: a float
/// takes one edge, while `clear` names a set of floats to get out from under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClearSide {
    /// `clear: left` — get below every inline-start float.
    Start,
    /// `clear: right` — get below every inline-end float.
    End,
    /// `clear: both` — get below all of them.
    Both,
}

/// One float, already placed, as its **margin box**.
///
/// Margin box rather than border box because §9.5.1's rules are all stated about
/// outer edges: a float's margin is what keeps the next float and the shortened
/// line boxes away from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlacedFloat {
    /// Which edge it was shifted to.
    pub side: FloatSide,
    /// The margin box's inline-start edge.
    pub inline_start: Au,
    /// The margin box's inline-end edge.
    pub inline_end: Au,
    /// The margin box's block-start edge.
    pub block_start: Au,
    /// The margin box's block-end edge.
    pub block_end: Au,
}

/// The floats placed so far in one block formatting context.
///
/// Coordinates are relative to the content box of the container that owns the
/// context, which is the containing block of every float in it.
#[derive(Clone, Debug, Default)]
pub struct FloatContext {
    floats: Vec<PlacedFloat>,
}

impl FloatContext {
    /// A context with no floats in it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any float has been placed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.floats.is_empty()
    }

    /// The free inline range across the block range `[block_start, block_end)`.
    ///
    /// Returns `(start, end)` — the inline offsets between which content may be
    /// placed, given `available` as the container's content width. The
    /// intersection of every band the range crosses, so a caller asking about a
    /// range gets the width that is free for *all* of it.
    ///
    /// A float whose block extent merely touches the range does not narrow it:
    /// the range is half-open at both ends, which is what makes a float ending
    /// exactly where a line begins leave that line full width.
    #[must_use]
    pub fn band(&self, block_start: Au, block_end: Au, available: Au) -> (Au, Au) {
        let mut start = Au(0);
        let mut end = available;

        for float in &self.floats {
            if float.block_end <= block_start || float.block_start >= block_end {
                continue;
            }
            match float.side {
                FloatSide::Start => {
                    if float.inline_end > start {
                        start = float.inline_end;
                    }
                }
                FloatSide::End => {
                    if float.inline_start < end {
                        end = float.inline_start;
                    }
                }
            }
        }

        if end < start {
            (start, start)
        } else {
            (start, end)
        }
    }

    /// Place a float of margin-box `size`, and return its margin box's origin.
    ///
    /// `floor` is the earliest block position it may take — §9.5.1 rules 5 and 6:
    /// a float's outer top may be no higher than the top of any earlier float, nor
    /// than the top of the line box or block it appears in.
    ///
    /// The search walks down through the band boundaries. At each candidate the
    /// float either fits in the free range across its whole height, in which case
    /// it is shifted to its edge and done, or it does not and the next boundary
    /// below is tried. Checking the *whole* height rather than only the top edge
    /// is what rule 3 asks for — "next to" there means vertically overlapping —
    /// and it is why a tall float goes below a short one it cannot fit beside
    /// rather than through it.
    ///
    /// A float too wide for any band is placed at its floor and overflows, which
    /// §9.5 requires: a float is never made narrower than its used width, and
    /// descending forever to look for room that does not exist is the bug that
    /// rule would otherwise produce.
    pub fn place(
        &mut self,
        side: FloatSide,
        size: LogicalSize,
        floor: Au,
        available: Au,
    ) -> PlacedFloat {
        let mut block_start = floor;

        loop {
            let (start, end) = self.band(block_start, block_start + size.block, available);
            if end - start >= size.inline {
                let inline_start = match side {
                    FloatSide::Start => start,
                    FloatSide::End => end - size.inline,
                };
                return self.push(side, inline_start, block_start, size);
            }

            let Some(next) = self.next_edge_below(block_start) else {
                // Nothing below to move past: the float is wider than the
                // container. Place it at its edge and let it overflow.
                let inline_start = match side {
                    FloatSide::Start => Au(0),
                    FloatSide::End => available - size.inline,
                };
                return self.push(side, inline_start, block_start, size);
            };
            block_start = next;
        }
    }

    /// The next band boundary strictly below `block_position`.
    ///
    /// Every float's bottom edge is a boundary, because that is where the free
    /// inline range can change. Strictly below, so a caller stepping through the
    /// bands makes progress on every call and cannot loop — which both
    /// [`FloatContext::place`] and line breaking depend on.
    #[must_use]
    pub fn next_edge_below(&self, block_position: Au) -> Option<Au> {
        self.floats
            .iter()
            .map(|f| f.block_end)
            .filter(|edge| *edge > block_position)
            .min()
    }

    /// Record a float at a position already decided.
    fn push(
        &mut self,
        side: FloatSide,
        inline_start: Au,
        block_start: Au,
        size: LogicalSize,
    ) -> PlacedFloat {
        let placed = PlacedFloat {
            side,
            inline_start,
            inline_end: inline_start + size.inline,
            block_start,
            block_end: block_start + size.block,
        };
        self.floats.push(placed);
        placed
    }

    /// The block position a box with `clear` may not start above (§9.5.2).
    ///
    /// > The top outer edge of the box must be below the bottom outer edge of all
    /// > earlier floats.
    ///
    /// Zero when there is nothing to clear, which makes this composable with a
    /// `max` against wherever the box was going anyway.
    #[must_use]
    pub fn clearance(&self, clear: ClearSide) -> Au {
        self.lowest_edge(match clear {
            ClearSide::Start => Some(FloatSide::Start),
            ClearSide::End => Some(FloatSide::End),
            ClearSide::Both => None,
        })
    }

    /// The lowest block-end edge of the floats on `side`, or of all of them.
    ///
    /// What [`FloatContext::clearance`] resolves against, and what a block
    /// formatting context root uses to contain its own floats (§10.6.7).
    #[must_use]
    pub fn lowest_edge(&self, side: Option<FloatSide>) -> Au {
        self.floats
            .iter()
            .filter(|f| side.is_none_or(|s| f.side == s))
            .map(|f| f.block_end)
            .max()
            .unwrap_or(Au(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    fn size(inline: i32, block: i32) -> LogicalSize {
        LogicalSize::new(px(inline), px(block))
    }

    #[test]
    fn float_an_empty_context_offers_the_whole_width() {
        let context = FloatContext::new();
        assert_eq!(context.band(px(0), px(100), px(800)), (px(0), px(800)));
    }

    #[test]
    fn float_a_start_float_narrows_the_band_from_the_start() {
        let mut context = FloatContext::new();
        assert_eq!(
            context
                .place(FloatSide::Start, size(100, 50), px(0), px(800))
                .block_start,
            px(0)
        );
        assert_eq!(context.band(px(0), px(20), px(800)), (px(100), px(800)));
    }

    #[test]
    fn float_an_end_float_narrows_the_band_from_the_end() {
        let mut context = FloatContext::new();
        context.place(FloatSide::End, size(100, 50), px(0), px(800));
        assert_eq!(context.band(px(0), px(20), px(800)), (px(0), px(700)));
    }

    /// §9.5.1 rule 3: two floats on the same side sit beside each other.
    #[test]
    fn float_two_floats_on_one_side_sit_side_by_side() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(100, 50), px(0), px(800));
        assert_eq!(
            context
                .place(FloatSide::Start, size(100, 50), px(0), px(800))
                .block_start,
            px(0),
            "there is room beside the first, so the second stays at the top"
        );
        assert_eq!(context.band(px(0), px(20), px(800)), (px(200), px(800)));
    }

    /// §9.5.1 rule 3 again: when there is no room beside, the float goes below.
    #[test]
    fn float_a_float_with_no_room_beside_drops_below() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(500, 50), px(0), px(800));
        assert_eq!(
            context
                .place(FloatSide::Start, size(500, 50), px(0), px(800))
                .block_start,
            px(50),
            "500 + 500 does not fit in 800, so the second drops past the first"
        );
        assert_eq!(context.band(px(60), px(70), px(800)), (px(500), px(800)));
    }

    /// A float goes below a float on the *other* side too, if it cannot fit.
    #[test]
    fn float_opposite_sides_share_a_band_until_they_cannot() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(500, 50), px(0), px(800));
        context.place(FloatSide::End, size(200, 50), px(0), px(800));
        assert_eq!(context.band(px(0), px(20), px(800)), (px(500), px(600)));

        // 200 wide does not fit in the 100 that is left.
        assert_eq!(
            context
                .place(FloatSide::End, size(200, 50), px(0), px(800))
                .block_start,
            px(50)
        );
    }

    /// §9.5.1 rule 5: a float never rises above an earlier one.
    #[test]
    fn float_the_floor_is_respected_even_when_there_is_room() {
        let mut context = FloatContext::new();
        assert_eq!(
            context
                .place(FloatSide::Start, size(100, 50), px(200), px(800))
                .block_start,
            px(200),
            "an empty context still honours the floor"
        );
    }

    /// A float wider than its container overflows rather than descending forever.
    #[test]
    fn float_a_float_wider_than_the_container_overflows_at_its_floor() {
        let mut context = FloatContext::new();
        assert_eq!(
            context
                .place(FloatSide::Start, size(900, 50), px(0), px(800))
                .block_start,
            px(0)
        );
    }

    /// The band is half-open, so a float ending where a line starts is clear of it.
    #[test]
    fn float_a_float_that_only_touches_a_range_does_not_narrow_it() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(100, 50), px(0), px(800));
        assert_eq!(
            context.band(px(50), px(70), px(800)),
            (px(0), px(800)),
            "the float ends at 50 and the range starts there"
        );
    }

    /// Stepping through the bands always makes progress.
    ///
    /// The property line breaking relies on to avoid looping when a float leaves
    /// no room at all for a line box.
    #[test]
    fn float_the_next_edge_below_is_strictly_below() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(100, 50), px(0), px(800));
        context.place(FloatSide::Start, size(100, 90), px(0), px(800));
        assert_eq!(context.next_edge_below(px(0)), Some(px(50)));
        assert_eq!(context.next_edge_below(px(50)), Some(px(90)));
        assert_eq!(context.next_edge_below(px(90)), None);
    }

    /// §9.5.2: `clear` names a set of floats, and `both` is not a side.
    #[test]
    fn float_clearance_is_the_bottom_of_the_floats_named() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(100, 50), px(0), px(800));
        context.place(FloatSide::End, size(100, 90), px(0), px(800));
        assert_eq!(context.clearance(ClearSide::Start), px(50));
        assert_eq!(context.clearance(ClearSide::End), px(90));
        assert_eq!(context.clearance(ClearSide::Both), px(90));
    }

    /// Nothing to clear is zero, so a caller can `max` against it unconditionally.
    #[test]
    fn float_clearance_of_an_empty_context_is_zero() {
        let context = FloatContext::new();
        assert_eq!(context.clearance(ClearSide::Both), Au(0));
    }

    #[test]
    fn float_the_lowest_edge_is_per_side() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(100, 50), px(0), px(800));
        context.place(FloatSide::End, size(100, 90), px(0), px(800));
        assert_eq!(context.lowest_edge(Some(FloatSide::Start)), px(50));
        assert_eq!(context.lowest_edge(Some(FloatSide::End)), px(90));
        assert_eq!(context.lowest_edge(None), px(90));
    }

    /// A band narrower than nothing is empty, not negative.
    ///
    /// `place` cannot produce two overlapping floats -- it descends until there is
    /// room -- so the way this arises is the overflow case above: a float wider
    /// than its container sticks out past the container's own end edge, and every
    /// band it crosses is then asked to be narrower than zero. A negative width
    /// reaching `break_into_lines` would make every word "not fit" and put each on
    /// its own line, which looks like a text bug rather than a float one.
    #[test]
    fn float_an_overflowing_float_leaves_an_empty_band_not_a_negative_one() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(900, 50), px(0), px(800));
        let (start, end) = context.band(px(0), px(20), px(800));
        assert!(end >= start, "an empty band is {start:?}..{end:?}");
        assert_eq!(end - start, Au(0));
    }

    /// Two floats placed by `place` never overlap, whatever is asked of it.
    ///
    /// The invariant the test above leans on, asserted rather than assumed.
    #[test]
    fn float_placed_floats_never_overlap() {
        let mut context = FloatContext::new();
        context.place(FloatSide::Start, size(700, 50), px(0), px(800));
        context.place(FloatSide::End, size(700, 50), px(0), px(800));
        let (start, end) = context.band(px(0), px(20), px(800));
        assert_eq!(
            (start, end),
            (px(700), px(800)),
            "the second float dropped below rather than overlapping"
        );
    }
}
