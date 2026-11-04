//! The style layout reads. Plain data; see ADR 029 for why it is not stylo's.

use crate::au::Au;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Display {
    /// Generates no box, and neither do its descendants.
    None,
    #[default]
    Block,
    /// Not laid out yet (inline formatting is a later Phase 6 unit). Boxes with
    /// this display are skipped by tree construction rather than guessed at.
    Inline,
}

/// A used-or-computed length that may depend on the containing block.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum Length {
    #[default]
    Auto,
    Px(Au),
    /// A fraction of the containing block's width (0.5 = 50%).
    Percent(f32),
}

impl Length {
    pub const ZERO: Length = Length::Px(Au::ZERO);

    pub fn px(px: i32) -> Length {
        Length::Px(Au::from_px(px))
    }

    pub fn is_auto(self) -> bool {
        matches!(self, Length::Auto)
    }

    /// Resolve against a containing-block width. `Auto` is `None`.
    pub fn resolve(self, containing_width: Au) -> Option<Au> {
        match self {
            Length::Auto => None,
            Length::Px(au) => Some(au),
            Length::Percent(p) => Some(containing_width.mul_f32(p)),
        }
    }
}

/// Four edges in CSS order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Edges<T> {
    pub top: T,
    pub right: T,
    pub bottom: T,
    pub left: T,
}

impl<T: Copy> Edges<T> {
    pub fn all(v: T) -> Self {
        Edges {
            top: v,
            right: v,
            bottom: v,
            left: v,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BoxStyle {
    pub display: Display,
    pub margin: Edges<Length>,
    pub padding: Edges<Length>,
    pub border: Edges<Au>,
    pub width: Length,
    /// `Percent` heights are treated as `auto` (the containing block's height
    /// is not definite in normal flow).
    pub height: Length,
    /// Whether this box establishes a new block formatting context
    /// (`overflow` other than visible, `display: flow-root`, the root element).
    /// Its margins do not collapse with its children's.
    pub establishes_bfc: bool,
}
