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
//! Text is [`Item::GlyphRun`] (Phase 9 unit 4): a font, a size, a colour and a
//! window `start..start + len` into the list's glyph arena
//! ([`DisplayList::glyphs`]). The arena holds the variable-length data so that
//! [`Item`] stays `Copy`. [`Item::TextRun`], a placeholder rectangle, remains
//! until the builder emits glyph runs. A page whose text paints as blocks is
//! honest about where the engine is; a page whose text is silently missing is
//! not.

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

/// A font as the display list names it. The number is meaningful only between
/// the producer of a list and whoever supplies glyph outlines for it (the
/// [`crate::raster::GlyphSource`]); `px-paint` does not know what a font is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontId(pub u32);

/// One glyph of a run: which glyph, and where its origin (the pen position on
/// the baseline) sits in page space, in [`Au`]. Positions are absolute, not
/// relative to the run, so a run needs no rectangle and a glyph can be drawn
/// without knowing its neighbours.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PositionedGlyph {
    /// The font's glyph index.
    pub glyph_id: u32,
    /// Origin x, page space.
    pub x: Au,
    /// Origin y (baseline), page space.
    pub y: Au,
}

/// Most glyphs in one list's arena. 262,144 glyphs at 12 bytes is 3 MiB: far
/// more than a page of text needs, small enough that a hostile list cannot buy
/// unbounded memory.
pub const MAX_GLYPHS: usize = 1 << 18;

/// Most glyphs one run may name. A single line of text is nowhere near this;
/// the cap bounds the work one item can ask for.
pub const MAX_GLYPHS_PER_RUN: u32 = 1 << 16;

/// Why a list is not well formed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListError {
    /// The glyph arena is longer than [`MAX_GLYPHS`].
    TooManyGlyphs,
    /// A glyph run is longer than [`MAX_GLYPHS_PER_RUN`].
    RunTooLong,
    /// A glyph run's `start + len` lies beyond the arena.
    RunOutOfBounds,
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ListError::TooManyGlyphs => "glyph arena too large",
            ListError::RunTooLong => "glyph run too long",
            ListError::RunOutOfBounds => "glyph run outside the arena",
        })
    }
}

impl std::error::Error for ListError {}

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
    /// Draw `len` glyphs of `font` at `size`, taken from the list's glyph arena
    /// starting at `start`, in `color`. `size` is the em size in [`Au`].
    GlyphRun {
        font: FontId,
        size: Au,
        color: Color,
        start: u32,
        len: u32,
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
    /// The glyphs [`Item::GlyphRun`] windows into. At most [`MAX_GLYPHS`].
    pub glyphs: Vec<PositionedGlyph>,
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

    /// Append a glyph run: the glyphs go into the arena and an
    /// [`Item::GlyphRun`] naming them into the list. Returns `false`, changing
    /// nothing, if either bound would be exceeded.
    pub fn push_glyph_run(
        &mut self,
        font: FontId,
        size: Au,
        color: Color,
        glyphs: &[PositionedGlyph],
    ) -> bool {
        let Ok(len) = u32::try_from(glyphs.len()) else {
            return false;
        };
        let start = self.glyphs.len();
        let Ok(start32) = u32::try_from(start) else {
            return false;
        };
        if len > MAX_GLYPHS_PER_RUN || start + glyphs.len() > MAX_GLYPHS {
            return false;
        }
        self.glyphs.extend_from_slice(glyphs);
        self.items.push(Item::GlyphRun {
            font,
            size,
            color,
            start: start32,
            len,
        });
        true
    }

    /// The glyphs of a run, or `None` if the window is outside the arena.
    pub fn run_glyphs(&self, start: u32, len: u32) -> Option<&[PositionedGlyph]> {
        let start = usize::try_from(start).ok()?;
        let end = start.checked_add(usize::try_from(len).ok()?)?;
        self.glyphs.get(start..end)
    }

    /// The glyph arena and every run in it are within bounds. A rasterizer may
    /// assume this holds after it returns `Ok`.
    pub fn validate(&self) -> Result<(), ListError> {
        if self.glyphs.len() > MAX_GLYPHS {
            return Err(ListError::TooManyGlyphs);
        }
        for item in &self.items {
            if let Item::GlyphRun { start, len, .. } = item {
                if *len > MAX_GLYPHS_PER_RUN {
                    return Err(ListError::RunTooLong);
                }
                if self.run_glyphs(*start, *len).is_none() {
                    return Err(ListError::RunOutOfBounds);
                }
            }
        }
        Ok(())
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

    fn g(id: u32) -> PositionedGlyph {
        PositionedGlyph {
            glyph_id: id,
            x: px_layout::geom::px(id as i32),
            y: px_layout::geom::px(7),
        }
    }

    fn run(start: u32, len: u32) -> Item {
        Item::GlyphRun {
            font: FontId(1),
            size: px_layout::geom::px(16),
            color: Color::BLACK,
            start,
            len,
        }
    }

    #[test]
    fn push_glyph_run_fills_the_arena_and_names_the_window() {
        let mut l = DisplayList::new();
        assert!(l.push_glyph_run(
            FontId(1),
            px_layout::geom::px(16),
            Color::BLACK,
            &[g(1), g(2)]
        ));
        assert!(l.push_glyph_run(FontId(2), px_layout::geom::px(10), Color::WHITE, &[g(3)]));
        assert_eq!(l.glyphs.len(), 3);
        // The second run starts where the first ended.
        assert!(matches!(
            l.items[1],
            Item::GlyphRun {
                start: 2,
                len: 1,
                font: FontId(2),
                ..
            }
        ));
        assert_eq!(l.run_glyphs(1, 2), Some(&[g(2), g(3)][..]));
        assert_eq!(l.validate(), Ok(()));
    }

    #[test]
    fn a_run_beyond_the_arena_is_rejected() {
        let mut l = DisplayList::new();
        l.glyphs.push(g(1));
        l.push(run(0, 1));
        assert_eq!(l.validate(), Ok(()));
        for (s, n) in [(0, 2), (1, 1), (2, 0), (u32::MAX, 1), (1, u32::MAX)] {
            let mut bad = l.clone();
            bad.items = vec![run(s, n)];
            assert!(bad.validate().is_err(), "{s}+{n}");
        }
        // An empty run at the very end is in bounds.
        let mut ok = l.clone();
        ok.items = vec![run(1, 0)];
        assert_eq!(ok.validate(), Ok(()));
        assert_eq!(l.run_glyphs(u32::MAX, u32::MAX), None);
    }

    #[test]
    fn the_caps_hold() {
        let mut l = DisplayList::new();
        l.glyphs = vec![g(0); MAX_GLYPHS + 1];
        assert_eq!(l.validate(), Err(ListError::TooManyGlyphs));

        let mut l = DisplayList::new();
        l.glyphs = vec![g(0); MAX_GLYPHS];
        l.push(run(0, MAX_GLYPHS_PER_RUN + 1));
        assert_eq!(l.validate(), Err(ListError::RunTooLong));
        l.items = vec![run(0, MAX_GLYPHS_PER_RUN)];
        assert_eq!(l.validate(), Ok(()));

        // push_glyph_run refuses what validate would, and changes nothing.
        let mut l = DisplayList::new();
        l.glyphs = vec![g(0); MAX_GLYPHS - 1];
        let before = l.clone();
        assert!(!l.push_glyph_run(
            FontId(0),
            px_layout::geom::px(1),
            Color::BLACK,
            &[g(1), g(2)]
        ));
        assert_eq!(l, before);
        let too_long = vec![g(0); MAX_GLYPHS_PER_RUN as usize + 1];
        let mut l = DisplayList::new();
        assert!(!l.push_glyph_run(FontId(0), px_layout::geom::px(1), Color::BLACK, &too_long));
        assert!(l.is_empty() && l.glyphs.is_empty());
    }

    #[test]
    fn glyph_runs_do_not_disturb_clip_balance() {
        let mut l = DisplayList::new();
        l.push(Item::PushClip { rect: r() });
        l.push(run(0, 0));
        l.push(Item::PopClip);
        assert!(l.clips_balanced());
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
