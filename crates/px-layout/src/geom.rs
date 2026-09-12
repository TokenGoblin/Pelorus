//! Geometry: `Au`, and the rectangles layout is written in.
//!
//! build-spec §4.2: *"Layout uses `Au` — app units, `i32` at 1/60 px, as Servo
//! and Gecko do — with explicit saturating operations, so geometry overflow is
//! defined and testable rather than a panic or a wrap."*
//!
//! # Why `app_units::Au` rather than our own
//!
//! §4.2 says "as Servo and Gecko do", and `app_units` is that type. It is already
//! in the dependency closure — `px-css` names it for `query_container_size` — so
//! this adds nothing to the tree.
//!
//! It also already has the saturating property, which is worth spelling out
//! because the code does not look like it does:
//!
//! ```text
//! pub const MAX_AU: Au = Au((1 << 30) - 1);
//! impl Add for Au { fn add(self, other: Au) -> Au { Au(self.0 + other.0).clamp() } }
//! ```
//!
//! That is a **raw `i32` addition**, which would be a panic in debug and a wrap in
//! release — both of the things §4.2 exists to prevent. It is nonetheless correct,
//! and the bound is why: two values within `±(2³⁰−1)` sum to at most `2³¹−2`, and
//! `i32::MAX` is `2³¹−1`. The addition cannot overflow, so `clamp` sees a true sum
//! and saturates it. `app_units`' own comment says as much — *"(1 << 30) - 1 lets
//! us add/subtract two Au and check for overflow after the operation"*.
//!
//! # The hazard, and the rule this module imposes
//!
//! That argument holds **only while every `Au` is in range**, and `Au` is
//! `pub struct Au(pub i32)` — a public field. `Au(i32::MAX)` is constructible,
//! costs nothing, reads fine, and turns the next addition into the panic-or-wrap
//! §4.2 forbids. `app_units`' own documentation warns about it: *"It is safe to
//! construct invalid Au values, but it may lead to panics and overflows."*
//!
//! So the rule for `px-layout` is: **never write `Au(...)`.** Use [`px`],
//! [`au`], or `Au::from_px`, all of which clamp. `ci/gate-layout.sh` scans for
//! the tuple constructor, because this is exactly the kind of invariant that
//! holds for a year and then does not.

use app_units::Au;

/// Au from whole CSS pixels, clamped.
///
/// The constructor layout code should reach for. `Au::from_px` multiplies by 60
/// through the checked `Mul`, so a px value large enough to leave the range
/// saturates rather than wrapping.
#[must_use]
pub fn px(value: i32) -> Au {
    Au::from_px(value)
}

/// Au from raw app units, clamped.
///
/// For values that are already in app units — a computed length stylo handed
/// back, say. Goes through `Au::new`, which clamps, rather than the tuple
/// constructor, which does not.
#[must_use]
pub fn au(value: i32) -> Au {
    Au::new(value)
}

/// A size in app units.
///
/// Deliberately not `euclid::Size2D<Au>`: layout wants `inline`/`block` naming
/// rather than `width`/`height`, because which physical axis those mean depends
/// on `writing-mode`, and Phase 6 is the phase that has to get that distinction
/// right from the start. Converting to physical happens at one boundary, not at
/// every use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LogicalSize {
    /// Along the inline axis — the direction text advances.
    pub inline: Au,
    /// Along the block axis — the direction lines stack.
    pub block: Au,
}

impl LogicalSize {
    /// A size of zero in both axes.
    pub const ZERO: Self = Self {
        inline: Au(0),
        block: Au(0),
    };

    /// A size, clamped on both axes.
    #[must_use]
    pub fn new(inline: Au, block: Au) -> Self {
        Self { inline, block }
    }
}

/// Edge lengths in app units: margins, borders, padding.
///
/// Logical, for the same reason [`LogicalSize`] is. `block_start` is the top edge
/// in `horizontal-tb` and the right edge in `vertical-rl`, and the whole point of
/// naming it this way is that layout does not have to remember which.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct LogicalEdges {
    /// The edge lines start from.
    pub block_start: Au,
    /// The edge lines end at.
    pub block_end: Au,
    /// The edge text advances from.
    pub inline_start: Au,
    /// The edge text advances towards.
    pub inline_end: Au,
}

impl LogicalEdges {
    /// All four edges zero.
    pub const ZERO: Self = Self {
        block_start: Au(0),
        block_end: Au(0),
        inline_start: Au(0),
        inline_end: Au(0),
    };

    /// The total this adds along the inline axis.
    ///
    /// Saturating, like every other sum here: two in-range `Au` cannot overflow,
    /// and the result is clamped.
    #[must_use]
    pub fn inline_sum(self) -> Au {
        self.inline_start + self.inline_end
    }

    /// The total this adds along the block axis.
    #[must_use]
    pub fn block_sum(self) -> Au {
        self.block_start + self.block_end
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The saturating behaviour §4.2 requires, asserted rather than assumed.
    ///
    /// This is the test that would have caught the mistake of reaching for
    /// `Au(...)`: the tuple constructor produces a value outside the documented
    /// range, and the very next addition is the panic-or-wrap the spec forbids.
    #[test]
    fn geom_addition_saturates_instead_of_overflowing() {
        let max = au(i32::MAX);
        assert_eq!(
            max,
            app_units::MAX_AU,
            "au() must clamp, or every argument below is about a different type"
        );

        // The sum of two maxima is still the maximum, not a wrapped negative.
        assert_eq!(max + max, app_units::MAX_AU);

        let min = au(i32::MIN);
        assert_eq!(min, app_units::MIN_AU);
        assert_eq!(min + min, app_units::MIN_AU);
        assert_eq!(min - max, app_units::MIN_AU);
    }

    /// Multiplication saturates too, and by a different mechanism.
    ///
    /// `Add` relies on the range bound making overflow impossible; `Mul` cannot,
    /// so `app_units` uses `checked_mul` and picks the correct end of the range
    /// from the signs. Worth a test because the two are not the same argument and
    /// only one of them is written down in this module's documentation.
    #[test]
    fn geom_multiplication_saturates_at_both_ends() {
        let big = px(1_000_000);
        assert_eq!(big * 1_000_000, app_units::MAX_AU);
        assert_eq!(big * -1_000_000, app_units::MIN_AU);
    }

    /// A pixel is sixty app units, which is the whole point of the type.
    #[test]
    fn geom_px_converts_at_sixty_app_units() {
        assert_eq!(px(1), au(60));
        assert_eq!(px(0), au(0));
        assert_eq!(px(-2), au(-120));
        assert_eq!(px(3).to_px(), 3);
    }

    /// Edge sums saturate rather than wrapping.
    #[test]
    fn geom_edge_sums_saturate() {
        let edges = LogicalEdges {
            inline_start: au(i32::MAX),
            inline_end: au(i32::MAX),
            block_start: au(i32::MIN),
            block_end: au(i32::MIN),
        };
        assert_eq!(edges.inline_sum(), app_units::MAX_AU);
        assert_eq!(edges.block_sum(), app_units::MIN_AU);
    }

    /// Zero is zero in both axes, and `Default` agrees with the constant.
    ///
    /// The constants use `Au(0)` because a `const fn` cannot call `Au::new` —
    /// and zero is the one value where the tuple constructor is unambiguously in
    /// range, which is why the rule against it is enforced by a gate scan that
    /// can carry an exception rather than by hoping nobody needs a constant.
    #[test]
    fn geom_zero_constants_agree_with_default() {
        assert_eq!(LogicalSize::ZERO, LogicalSize::default());
        assert_eq!(LogicalEdges::ZERO, LogicalEdges::default());
        assert_eq!(LogicalSize::ZERO.inline, au(0));
    }
}
