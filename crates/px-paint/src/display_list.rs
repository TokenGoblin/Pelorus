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

/// The shape of one corner: `superellipse(K)` of CSS Borders 4 (`corner-shape`),
/// carried as an integer so that no float reaches the rasterizer.
///
/// The curve is `u^n + v^n = 1` with `n = 2^K`, where `u` and `v` run from 0 at
/// the corner box's inner edges to 1 at its outer edges. `n16` is `n` in 1/16
/// (so `round`, `K = 1`, `n = 2`, is 32; `squircle`, `K = 2`, `n = 4`, is 64;
/// `bevel`, `K = 0`, `n = 1`, is 16). [`CornerShape::INFINITE`] stands for
/// `K = infinity` (`square`) or `-infinity` (`notch`) and every `n` the
/// rasterizer cannot tell from it. `Concave` is `K < 0`: the same curve mirrored,
/// bulging into the box (`scoop` is `K = -1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerShape {
    /// `K >= 0`.
    Convex(u16),
    /// `K < 0`, with `n = 2^-K`.
    Concave(u16),
}

impl CornerShape {
    /// `n16` standing for an infinite exponent.
    pub const INFINITE: u16 = u16::MAX;
    /// `round`, the initial value of `corner-shape`.
    pub const ROUND: CornerShape = CornerShape::Convex(32);
    /// `squircle`.
    pub const SQUIRCLE: CornerShape = CornerShape::Convex(64);
    /// `bevel`.
    pub const BEVEL: CornerShape = CornerShape::Convex(16);
    /// `scoop`.
    pub const SCOOP: CornerShape = CornerShape::Concave(32);
    /// `square`: the corner is not cut at all.
    pub const SQUARE: CornerShape = CornerShape::Convex(Self::INFINITE);
    /// `notch`: the whole corner box is cut away.
    pub const NOTCH: CornerShape = CornerShape::Concave(Self::INFINITE);

    /// Is this corner a plain right angle whatever its radii?
    pub fn is_square(self) -> bool {
        self == Self::SQUARE
    }
}

/// One corner of a [`RoundedRect`]: an elliptical radius and a shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Corner {
    /// Horizontal radius.
    pub rx: Au,
    /// Vertical radius.
    pub ry: Au,
    pub shape: CornerShape,
}

impl Corner {
    pub const SQUARE_CORNER: Corner = Corner {
        rx: Au(0),
        ry: Au(0),
        shape: CornerShape::ROUND,
    };

    /// Does this corner cut anything?
    pub fn is_rounded(&self) -> bool {
        self.rx.0 > 0 && self.ry.0 > 0 && !self.shape.is_square()
    }
}

/// A rectangle with four independently shaped corners, in the order top-left,
/// top-right, bottom-right, bottom-left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundedRect {
    pub rect: Rect,
    pub corners: [Corner; 4],
}

impl RoundedRect {
    /// `rect` with `corners`, radii made non-negative and scaled down so that
    /// they do not overlap (see [`RoundedRect::clamped`]).
    pub fn new(rect: Rect, corners: [Corner; 4]) -> RoundedRect {
        RoundedRect { rect, corners }.clamped()
    }

    /// Does any corner cut anything? A list item whose shape is not rounded is
    /// better emitted as its plain rectangle form.
    pub fn is_rounded(&self) -> bool {
        self.corners.iter().any(Corner::is_rounded)
    }

    /// CSS Backgrounds 3 section 5.5 (overlapping curves): negative radii become
    /// zero; then, with `f = min(L_i / S_i)` over the four sides (`L` the side's
    /// length, `S` the sum of the two radii along it), if `f < 1` every radius is
    /// multiplied by `f`. Integer arithmetic, each radius `r` becoming
    /// `floor(r * L / S)` for the binding side, so the sums never exceed the
    /// sides. Idempotent.
    #[must_use]
    pub fn clamped(self) -> RoundedRect {
        let mut c = self.corners;
        for k in &mut c {
            k.rx = Au(k.rx.0.max(0));
            k.ry = Au(k.ry.0.max(0));
        }
        let w = i64::from(self.rect.width.0.max(0));
        let h = i64::from(self.rect.height.0.max(0));
        let sums = [
            (w, i64::from(c[0].rx.0) + i64::from(c[1].rx.0)),
            (w, i64::from(c[3].rx.0) + i64::from(c[2].rx.0)),
            (h, i64::from(c[0].ry.0) + i64::from(c[3].ry.0)),
            (h, i64::from(c[1].ry.0) + i64::from(c[2].ry.0)),
        ];
        // The binding side: the smallest L / S, compared by cross-multiplication.
        let mut best: Option<(i64, i64)> = None;
        for (l, s) in sums {
            if s > l && best.is_none_or(|(bl, bs)| l * bs < bl * s) {
                best = Some((l, s));
            }
        }
        if let Some((l, s)) = best {
            for k in &mut c {
                k.rx = Au(scale(k.rx.0, l, s));
                k.ry = Au(scale(k.ry.0, l, s));
            }
        }
        RoundedRect {
            rect: self.rect,
            corners: c,
        }
    }

    /// The same shape shrunk by border widths `[top, right, bottom, left]`: the
    /// padding edge, with each radius reduced by the width of the adjacent
    /// sides (never below zero), as CSS Backgrounds 3 section 5.2 does for the
    /// inner border edge.
    #[must_use]
    pub fn inset(self, widths: [Au; 4]) -> RoundedRect {
        let [t, r, b, l] = widths.map(|w| w.0.max(0));
        let sub = |v: Au, d: i32| Au(v.0.saturating_sub(d).max(0));
        let mut c = self.corners;
        c[0].rx = sub(c[0].rx, l);
        c[0].ry = sub(c[0].ry, t);
        c[1].rx = sub(c[1].rx, r);
        c[1].ry = sub(c[1].ry, t);
        c[2].rx = sub(c[2].rx, r);
        c[2].ry = sub(c[2].ry, b);
        c[3].rx = sub(c[3].rx, l);
        c[3].ry = sub(c[3].ry, b);
        RoundedRect {
            rect: Rect {
                x: Au(self.rect.x.0.saturating_add(l)),
                y: Au(self.rect.y.0.saturating_add(t)),
                width: Au((self.rect.width.0 - l - r).max(0)),
                height: Au((self.rect.height.0 - t - b).max(0)),
            },
            corners: c,
        }
    }
}

/// `floor(v * l / s)` for `0 <= v <= s`, `s > 0`; never above `v`.
fn scale(v: i32, l: i64, s: i64) -> i32 {
    i32::try_from((i64::from(v) * l.max(0)) / s.max(1)).unwrap_or(0)
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
    /// More than [`MAX_IMAGES`] images, or one whose bytes do not match its size.
    BadImage,
    /// An [`Item::Image`] names an image the list does not have.
    ImageOutOfBounds,
}

impl std::fmt::Display for ListError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ListError::TooManyGlyphs => "glyph arena too large",
            ListError::RunTooLong => "glyph run too long",
            ListError::RunOutOfBounds => "glyph run outside the arena",
            ListError::BadImage => "malformed or too many images",
            ListError::ImageOutOfBounds => "image index outside the table",
        })
    }
}

impl std::error::Error for ListError {}

/// A decoded raster image: non-premultiplied sRGB RGBA, row-major, `width *
/// height * 4` bytes. Lives in [`DisplayList::images`] behind an `Arc`, so cloning
/// a list (the shell clones one per frame) never copies pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageData {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl ImageData {
    /// Whether `rgba` is exactly `width * height * 4` bytes and neither side is 0.
    #[must_use]
    pub fn is_well_formed(&self) -> bool {
        u64::from(self.width) * u64::from(self.height) * 4 == self.rgba.len() as u64
            && self.width > 0
            && self.height > 0
    }
}

/// How an image is resampled to its destination. Both are integer-only and
/// deterministic; see [`crate::image`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFilter {
    /// The source pixel under the destination pixel's centre.
    Nearest,
    /// Bilinear when magnifying, a box filter when minifying, per axis.
    Smooth,
}

/// Most distinct images one list may carry.
pub const MAX_IMAGES: usize = 1024;

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
    /// Fill a rounded rectangle (a background with `border-radius`): anti-aliased
    /// coverage on an integer grid, see [`crate::raster`].
    FillRoundedRect {
        shape: RoundedRect,
        color: Color,
    },
    /// A border around a rounded `shape`: the ring between the shape and the
    /// shape inset by the side widths. Sides in CSS order.
    RoundedBorder {
        shape: RoundedRect,
        sides: [BorderSide; 4],
    },
    /// As [`Item::PushClip`], but the clip is a rounded rectangle. Closed by the
    /// same [`Item::PopClip`].
    PushRoundedClip {
        shape: RoundedRect,
    },
    /// Draw image number `image` of [`DisplayList::images`] stretched over
    /// `rect` (snapped like any box), resampled with `filter`, composited
    /// source-over through the current clip, rounded clips included. A tile of a
    /// repeating background is one item. The caller clips what must not show
    /// (`object-fit: cover`, a background's painting area) with a clip.
    Image {
        rect: Rect,
        image: u32,
        filter: ImageFilter,
    },
}

/// Items in paint order, back to front.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DisplayList {
    pub items: Vec<Item>,
    /// The glyphs [`Item::GlyphRun`] windows into. At most [`MAX_GLYPHS`].
    pub glyphs: Vec<PositionedGlyph>,
    /// The images [`Item::Image`] names, by index. At most [`MAX_IMAGES`].
    pub images: Vec<std::sync::Arc<ImageData>>,
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

    /// Add an image to the list's table (the same `Arc` twice is one entry) and
    /// return its index, or `None` when the table is full.
    pub fn add_image(&mut self, image: &std::sync::Arc<ImageData>) -> Option<u32> {
        if let Some(i) = self
            .images
            .iter()
            .position(|have| std::sync::Arc::ptr_eq(have, image))
        {
            return u32::try_from(i).ok();
        }
        if self.images.len() >= MAX_IMAGES {
            return None;
        }
        self.images.push(image.clone());
        u32::try_from(self.images.len() - 1).ok()
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
        if self.images.len() > MAX_IMAGES || self.images.iter().any(|i| !i.is_well_formed()) {
            return Err(ListError::BadImage);
        }
        for item in &self.items {
            match item {
                Item::GlyphRun { start, len, .. } => {
                    if *len > MAX_GLYPHS_PER_RUN {
                        return Err(ListError::RunTooLong);
                    }
                    if self.run_glyphs(*start, *len).is_none() {
                        return Err(ListError::RunOutOfBounds);
                    }
                }
                Item::Image { image, .. } if (*image as usize) >= self.images.len() => {
                    return Err(ListError::ImageOutOfBounds);
                }
                _ => {}
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
                Item::PushClip { .. } | Item::PushRoundedClip { .. } => depth += 1,
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
