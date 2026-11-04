//! The display list: what to draw, in paint order, with no layout left in it.
//!
//! Layout produces boxes; the rasterizer (software, deterministic, and the
//! `--disable-gpu` fallback) and later the GPU process both consume *this*. Keeping
//! a flat, ordered, `Copy`-able list between them is the whole design: either
//! backend can be killed, replaced or fuzzed without the other noticing, and a
//! display list can be hashed, which is what makes pixel baselines meaningful.
//!
//! Coordinates are [`Au`], in page space with the origin at the top-left of the
//! document. Rounding to device pixels is the rasterizer's job and happens once.
//!
//! Text is [`Item::TextRun`], a placeholder rectangle with a colour, until Phase 9
//! supplies shaping and glyphs. A page whose text paints as blocks is honest about
//! where the engine is; a page whose text is silently missing is not.

use px_layout::geom::Au;

/// A rectangle in page space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: Au,
    pub y: Au,
    pub width: Au,
    pub height: Au,
}

/// Non-premultiplied sRGB with 8-bit alpha.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    pub const TRANSPARENT: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
    pub const WHITE: Color = Color {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const BLACK: Color = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 255,
    };
}

/// Border line style. Only what the first rasterizer draws; the rest are added
/// with their tests, not stubbed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BorderStyle {
    None,
    Solid,
}

/// One border side.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BorderSide {
    pub width: Au,
    pub style: BorderStyle,
    pub color: Color,
}

/// One paint operation. `Copy`, so a list never owns another list and dropping or
/// walking one cannot recurse over nesting depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Item {
    /// Fill a rectangle (a background).
    FillRect {
        rect: Rect,
        color: Color,
    },
    /// A border around `rect`, sides in CSS order: top, right, bottom, left.
    Border {
        rect: Rect,
        sides: [BorderSide; 4],
    },
    /// Placeholder for a run of text until Phase 9: a solid block in the text colour.
    TextRun {
        rect: Rect,
        color: Color,
    },
    /// Restrict everything until the matching [`Item::PopClip`] to `rect`.
    PushClip {
        rect: Rect,
    },
    PopClip,
}

/// Items in paint order, back to front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayList {
    pub items: Vec<Item>,
}

impl DisplayList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, item: Item) {
        self.items.push(item);
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Clip nesting is balanced: every `PushClip` has its `PopClip`, and none is
    /// popped before it is pushed. A rasterizer may assume this holds.
    pub fn clips_balanced(&self) -> bool {
        let mut depth = 0usize;
        for item in &self.items {
            match item {
                Item::PushClip { .. } => depth += 1,
                Item::PopClip => match depth.checked_sub(1) {
                    Some(d) => depth = d,
                    None => return false,
                },
                _ => {}
            }
        }
        depth == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r() -> Rect {
        Rect {
            x: px_layout::geom::px(0),
            y: px_layout::geom::px(0),
            width: px_layout::geom::px(1),
            height: px_layout::geom::px(1),
        }
    }

    #[test]
    fn items_are_copy() {
        fn copy<T: Copy>() {}
        copy::<Item>();
    }

    #[test]
    fn balanced_clips_are_recognised() {
        let mut l = DisplayList::new();
        assert!(l.clips_balanced());
        l.push(Item::PushClip { rect: r() });
        l.push(Item::FillRect {
            rect: r(),
            color: Color::BLACK,
        });
        l.push(Item::PopClip);
        assert!(l.clips_balanced());
    }

    #[test]
    fn unbalanced_clips_are_rejected() {
        let mut l = DisplayList::new();
        l.push(Item::PopClip);
        assert!(!l.clips_balanced());
        let mut l = DisplayList::new();
        l.push(Item::PushClip { rect: r() });
        assert!(!l.clips_balanced());
    }
}
