//! The deterministic software rasterizer.
//!
//! This is the `--disable-gpu` fallback and the reason CI rendering is
//! reproducible: the same [`DisplayList`] produces the same bytes on every run and
//! every OS. There is **no floating point anywhere in this module**; geometry is
//! `i64`/`i32` integer arithmetic and blending is integer arithmetic with one
//! documented rounding rule. No hash-ordered container is used.
//!
//! # Rules
//!
//! **Au to device pixels (edge snapping).** `Au` is 1/60 px. Every box *edge* is
//! converted independently with `snap(v) = floor((v + 30) / 60)`, that is round to
//! nearest with halves rounding toward +infinity (so `0.5px -> 1`, `-0.5px -> 0`,
//! `-0.51px -> -1`). A rectangle covers pixels `snap(x) .. snap(x + width)` and
//! `snap(y) .. snap(y + height)`, half-open. Because the far edge is snapped from
//! `x + width` rather than being `snap(x) + snap(width)`, two boxes that touch in
//! layout share exactly one snapped edge: no gap and no overlap, ever. A box that
//! snaps to zero width or height paints nothing.
//!
//! **Blending.** The canvas starts opaque white and stays opaque, so the only
//! operation is source-over onto an opaque destination. With source alpha `a`
//! (0..=255): `a == 255` copies, `a == 0` is skipped, otherwise each channel is
//! `(s*a + d*(255-a) + 127) / 255` in integer arithmetic (round to nearest, halves
//! up). Canvas alpha is always 255. Colours are non-premultiplied sRGB; no gamma
//! handling, by design (that is what browsers do for plain 8-bit blending).
//!
//! **Borders.** The outer box is the snapped rectangle. Each side's inner edge is
//! snapped from the layout position of the inner edge (`x + left_width`, etc.). A
//! side with style `None` has width zero. A side with width > 0 and style `Solid`
//! always covers at least one device pixel (inner edge is pushed out by one if
//! rounding would erase it), and if inner edges cross they meet at their midpoint,
//! so borders can never paint outside the outer box. Where two adjacent sides meet,
//! the corner is *mitred*: the pixels in the corner square are split along the
//! diagonal from the outer corner to the inner corner. A pixel belongs to the
//! horizontal (top/bottom) side if its centre is nearer the horizontal edge in
//! proportion to the band widths, otherwise to the vertical side; a pixel whose
//! centre lies exactly on the diagonal goes to the horizontal side. This is a
//! per-pixel decision with no anti-aliasing, so edges are crisp and reproducible.
//! Sides do not overlap, so translucent borders blend once.
//!
//! **Clips.** `PushClip` intersects the snapped clip rectangle with the current
//! clip; `PopClip` restores the previous one. The initial clip is the canvas.
//!
//! # Bounds (fail closed)
//!
//! Viewport width and height are each in `1..=`[`MAX_DIMENSION`] and the pixel count
//! is at most [`MAX_PIXELS`] (2^25, the PNG codec's limit, so every canvas is
//! encodable); beyond that [`rasterize`] returns [`RasterError::TooLarge`] *before*
//! allocating. Clip nesting deeper than [`MAX_CLIP_DEPTH`] is an error. A list whose
//! clips are unbalanced is rejected with [`RasterError::UnbalancedClips`]
//! before anything is drawn.

use crate::display_list::{BorderSide, BorderStyle, Color, DisplayList, Item, Rect};
use crate::png;
use std::fmt;

/// Largest canvas width or height.
pub const MAX_DIMENSION: u32 = 16384;
/// Largest canvas pixel count.
pub const MAX_PIXELS: u64 = 1 << 25;
/// Deepest permitted `PushClip` nesting.
pub const MAX_CLIP_DEPTH: usize = 256;

/// Why rasterization was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RasterError {
    /// Zero width or height.
    EmptyViewport,
    /// Over [`MAX_DIMENSION`] or [`MAX_PIXELS`].
    TooLarge,
    /// A `PopClip` without a `PushClip`, or a `PushClip` never popped.
    UnbalancedClips,
    /// Clip nesting beyond [`MAX_CLIP_DEPTH`].
    ClipTooDeep,
}

impl fmt::Display for RasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RasterError::EmptyViewport => "empty viewport",
            RasterError::TooLarge => "viewport exceeds raster limits",
            RasterError::UnbalancedClips => "unbalanced clip stack",
            RasterError::ClipTooDeep => "clip nesting too deep",
        })
    }
}

impl std::error::Error for RasterError {}

/// An RGBA8 raster, row-major, `pixels.len() == width * height * 4`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Canvas {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Canvas {
    /// An opaque white canvas, or an error if the size is out of bounds.
    pub fn new(width: u32, height: u32) -> Result<Canvas, RasterError> {
        if width == 0 || height == 0 {
            return Err(RasterError::EmptyViewport);
        }
        if width > MAX_DIMENSION
            || height > MAX_DIMENSION
            || width as u64 * height as u64 > MAX_PIXELS
        {
            return Err(RasterError::TooLarge);
        }
        Ok(Canvas {
            width,
            height,
            pixels: vec![255; width as usize * height as usize * 4],
        })
    }

    /// The RGBA bytes at `(x, y)`, or `None` outside the canvas.
    pub fn pixel(&self, x: u32, y: u32) -> Option<[u8; 4]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        let p = self.pixels.get(i..i + 4)?;
        Some([p[0], p[1], p[2], p[3]])
    }

    /// Encode as a PNG (see [`png::encode`]).
    pub fn to_png(&self) -> Result<Vec<u8>, png::PngError> {
        png::encode(self.width, self.height, &self.pixels)
    }
}

/// A half-open pixel rectangle `[x0, x1) x [y0, y1)`; empty when `x0 >= x1` or
/// `y0 >= y1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PxRect {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
}

impl PxRect {
    fn intersect(self, o: PxRect) -> PxRect {
        PxRect {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        }
    }
    fn is_empty(self) -> bool {
        self.x0 >= self.x1 || self.y0 >= self.y1
    }
}

/// Snap one edge, in Au (1/60 px), to a device pixel boundary. See the module docs.
fn snap(au: i64) -> i64 {
    (au + 30).div_euclid(60)
}

fn snap_rect(r: &Rect) -> PxRect {
    let (x, y) = (r.x.0 as i64, r.y.0 as i64);
    PxRect {
        x0: snap(x),
        y0: snap(y),
        x1: snap(x + r.width.0 as i64),
        y1: snap(y + r.height.0 as i64),
    }
}

/// `(s*a + d*(255-a) + 127) / 255`
fn mix(s: u8, d: u8, a: u32) -> u8 {
    ((s as u32 * a + d as u32 * (255 - a) + 127) / 255) as u8
}

fn blend_px(px: &mut [u8], c: Color) {
    match c.a {
        0 => {}
        255 => {
            px[0] = c.r;
            px[1] = c.g;
            px[2] = c.b;
            px[3] = 255;
        }
        a => {
            let a = a as u32;
            px[0] = mix(c.r, px[0], a);
            px[1] = mix(c.g, px[1], a);
            px[2] = mix(c.b, px[2], a);
            px[3] = 255;
        }
    }
}

impl Canvas {
    /// Blend `c` over the pixels of `r` (already clipped to the canvas by caller).
    fn fill(&mut self, r: PxRect, c: Color) {
        if r.is_empty() || c.a == 0 {
            return;
        }
        let w = self.width as usize;
        for y in r.y0..r.y1 {
            let start = (y as usize * w + r.x0 as usize) * 4;
            let end = (y as usize * w + r.x1 as usize) * 4;
            if let Some(row) = self.pixels.get_mut(start..end) {
                for px in row.as_chunks_mut::<4>().0.iter_mut() {
                    blend_px(px, c);
                }
            }
        }
    }

    fn put(&mut self, x: i64, y: i64, c: Color) {
        let i = (y as usize * self.width as usize + x as usize) * 4;
        if let Some(px) = self.pixels.get_mut(i..i + 4) {
            blend_px(px, c);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

fn draw_border(cv: &mut Canvas, clip: PxRect, rect: &Rect, sides: &[BorderSide; 4]) {
    let outer = snap_rect(rect);
    if outer.is_empty() {
        return;
    }
    let eff = |s: &BorderSide| -> i64 {
        if s.style == BorderStyle::Solid {
            (s.width.0 as i64).max(0)
        } else {
            0
        }
    };
    let (wt, wr, wb, wl) = (
        eff(&sides[0]),
        eff(&sides[1]),
        eff(&sides[2]),
        eff(&sides[3]),
    );
    let (x, y) = (rect.x.0 as i64, rect.y.0 as i64);
    let (w, h) = (rect.width.0 as i64, rect.height.0 as i64);

    let mut ix0 = snap(x + wl);
    let mut ix1 = snap(x + w - wr);
    let mut iy0 = snap(y + wt);
    let mut iy1 = snap(y + h - wb);
    // Positive widths cover at least one device pixel.
    if wl > 0 {
        ix0 = ix0.max(outer.x0 + 1);
    } else {
        ix0 = outer.x0;
    }
    if wr > 0 {
        ix1 = ix1.min(outer.x1 - 1);
    } else {
        ix1 = outer.x1;
    }
    if wt > 0 {
        iy0 = iy0.max(outer.y0 + 1);
    } else {
        iy0 = outer.y0;
    }
    if wb > 0 {
        iy1 = iy1.min(outer.y1 - 1);
    } else {
        iy1 = outer.y1;
    }
    // Keep the inner box inside the outer one; crossing edges meet at the midpoint.
    ix0 = ix0.min(outer.x1);
    ix1 = ix1.max(outer.x0);
    iy0 = iy0.min(outer.y1);
    iy1 = iy1.max(outer.y0);
    if ix0 > ix1 {
        let m = (ix0 + ix1).div_euclid(2);
        ix0 = m;
        ix1 = m;
    }
    if iy0 > iy1 {
        let m = (iy0 + iy1).div_euclid(2);
        iy0 = m;
        iy1 = m;
    }

    let owner = |px: i64, py: i64| -> Side {
        let l = px < ix0;
        let r = px >= ix1;
        let t = py < iy0;
        let b = py >= iy1;
        match (l || r, t || b) {
            (true, false) => {
                if l {
                    Side::Left
                } else {
                    Side::Right
                }
            }
            (false, true) => {
                if t {
                    Side::Top
                } else {
                    Side::Bottom
                }
            }
            _ => {
                // Corner square: split along the diagonal (doubled coordinates so
                // pixel centres are integers).
                let (dx, vband) = if l {
                    (2 * (px - outer.x0) + 1, ix0 - outer.x0)
                } else {
                    (2 * (outer.x1 - px) - 1, outer.x1 - ix1)
                };
                let (dy, hband) = if t {
                    (2 * (py - outer.y0) + 1, iy0 - outer.y0)
                } else {
                    (2 * (outer.y1 - py) - 1, outer.y1 - iy1)
                };
                if dy * vband <= dx * hband {
                    if t { Side::Top } else { Side::Bottom }
                } else if l {
                    Side::Left
                } else {
                    Side::Right
                }
            }
        }
    };
    let color = |s: Side| -> Color {
        match s {
            Side::Top => sides[0].color,
            Side::Right => sides[1].color,
            Side::Bottom => sides[2].color,
            Side::Left => sides[3].color,
        }
    };

    let vis = outer.intersect(clip);
    if vis.is_empty() {
        return;
    }
    for py in vis.y0..vis.y1 {
        let in_band_row = py < iy0 || py >= iy1;
        if in_band_row {
            for px in vis.x0..vis.x1 {
                cv.put(px, py, color(owner(px, py)));
            }
        } else {
            for px in vis.x0..vis.x1.min(ix0) {
                cv.put(px, py, color(owner(px, py)));
            }
            for px in vis.x0.max(ix1)..vis.x1 {
                cv.put(px, py, color(owner(px, py)));
            }
        }
    }
}

/// Rasterize `list` onto a fresh white `viewport_width_px` by `viewport_height_px`
/// canvas. Page-space origin (0, 0) maps to the canvas's top-left pixel.
pub fn rasterize(
    list: &DisplayList,
    viewport_width_px: u32,
    viewport_height_px: u32,
) -> Result<Canvas, RasterError> {
    let mut cv = Canvas::new(viewport_width_px, viewport_height_px)?;
    if !list.clips_balanced() {
        return Err(RasterError::UnbalancedClips);
    }
    let full = PxRect {
        x0: 0,
        y0: 0,
        x1: viewport_width_px as i64,
        y1: viewport_height_px as i64,
    };
    let mut stack: Vec<PxRect> = Vec::new();
    let mut clip = full;
    for item in &list.items {
        match item {
            Item::FillRect { rect, color } | Item::TextRun { rect, color } => {
                let r = snap_rect(rect).intersect(clip);
                cv.fill(r, *color);
            }
            Item::Border { rect, sides } => draw_border(&mut cv, clip, rect, sides),
            Item::PushClip { rect } => {
                if stack.len() >= MAX_CLIP_DEPTH {
                    return Err(RasterError::ClipTooDeep);
                }
                stack.push(clip);
                clip = clip.intersect(snap_rect(rect));
            }
            Item::PopClip => {
                clip = stack.pop().ok_or(RasterError::UnbalancedClips)?;
            }
        }
    }
    Ok(cv)
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_layout::geom::{au, px};

    const RED: Color = Color {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    const BLUE: Color = Color {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    };
    const GREEN: Color = Color {
        r: 0,
        g: 255,
        b: 0,
        a: 255,
    };
    const BLACK: Color = Color::BLACK;
    const WHITE_PX: [u8; 4] = [255, 255, 255, 255];

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect {
            x: px(x),
            y: px(y),
            width: px(w),
            height: px(h),
        }
    }

    fn list(items: &[Item]) -> DisplayList {
        DisplayList {
            items: items.to_vec(),
        }
    }

    fn fill(x: i32, y: i32, w: i32, h: i32, color: Color) -> Item {
        Item::FillRect {
            rect: rect(x, y, w, h),
            color,
        }
    }

    #[test]
    fn empty_list_is_white() {
        let c = rasterize(&DisplayList::new(), 4, 4).unwrap();
        assert_eq!(c.pixels, vec![255; 64]);
    }

    #[test]
    fn rect_on_4x4() {
        let c = rasterize(&list(&[fill(1, 1, 2, 2, RED)]), 4, 4).unwrap();
        for y in 0..4 {
            for x in 0..4 {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                let want = if inside { [255, 0, 0, 255] } else { WHITE_PX };
                assert_eq!(c.pixel(x, y), Some(want), "({x},{y})");
            }
        }
        assert_eq!(c.pixel(4, 0), None);
    }

    #[test]
    fn half_red_over_white_is_exact() {
        let half = Color {
            r: 255,
            g: 0,
            b: 0,
            a: 128,
        };
        let c = rasterize(&list(&[fill(0, 0, 1, 1, half)]), 1, 1).unwrap();
        // r: (255*128 + 255*127 + 127)/255 = 255; g,b: (0 + 255*127 + 127)/255 = 127
        assert_eq!(c.pixel(0, 0), Some([255, 127, 127, 255]));
        let clear = Color { a: 0, ..RED };
        let c = rasterize(&list(&[fill(0, 0, 1, 1, clear)]), 1, 1).unwrap();
        assert_eq!(c.pixel(0, 0), Some(WHITE_PX));
    }

    #[test]
    fn stacked_translucency_blends_in_order() {
        let a = Color { a: 128, ..BLACK };
        let c = rasterize(&list(&[fill(0, 0, 1, 1, a), fill(0, 0, 1, 1, a)]), 1, 1).unwrap();
        // 255 -> (0*128 + 255*127 + 127)/255 = 127 -> (0 + 127*127 + 127)/255 = 63
        assert_eq!(c.pixel(0, 0), Some([63, 63, 63, 255]));
    }

    #[test]
    fn snapping_rounds_each_edge_half_up() {
        assert_eq!(snap(0), 0);
        assert_eq!(snap(29), 0);
        assert_eq!(snap(30), 1);
        assert_eq!(snap(89), 1);
        assert_eq!(snap(90), 2);
        assert_eq!(snap(-30), 0);
        assert_eq!(snap(-31), -1);
    }

    #[test]
    fn adjacent_boxes_neither_gap_nor_overlap() {
        // 1.5px-wide boxes laid end to end, fractional x: A = [0,1.5), B = [1.5,3.0).
        let a = Item::FillRect {
            rect: Rect {
                x: au(0),
                y: px(0),
                width: au(90),
                height: px(1),
            },
            color: RED,
        };
        let b = Item::FillRect {
            rect: Rect {
                x: au(90),
                y: px(0),
                width: au(90),
                height: px(1),
            },
            color: BLUE,
        };
        let c = rasterize(&list(&[a, b]), 4, 1).unwrap();
        assert_eq!(c.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(1, 0), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 0), Some([0, 0, 255, 255]));
        assert_eq!(c.pixel(3, 0), Some(WHITE_PX));
    }

    #[test]
    fn offscreen_and_negative_rects_are_clipped() {
        let c = rasterize(
            &list(&[fill(-5, -5, 7, 7, RED), fill(3, 3, 100, 100, BLUE)]),
            4,
            4,
        )
        .unwrap();
        assert_eq!(c.pixel(0, 0), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 2), Some(WHITE_PX));
        assert_eq!(c.pixel(3, 3), Some([0, 0, 255, 255]));
        // Huge values must not overflow or panic.
        let big = Item::FillRect {
            rect: Rect {
                x: au(i32::MIN),
                y: au(i32::MIN),
                width: au(i32::MAX),
                height: au(i32::MAX),
            },
            color: RED,
        };
        assert!(rasterize(&list(&[big]), 2, 2).is_ok());
    }

    #[test]
    fn text_run_is_a_solid_block() {
        let t = Item::TextRun {
            rect: rect(0, 0, 2, 1),
            color: BLACK,
        };
        let c = rasterize(&list(&[t]), 3, 1).unwrap();
        assert_eq!(c.pixel(1, 0), Some([0, 0, 0, 255]));
        assert_eq!(c.pixel(2, 0), Some(WHITE_PX));
    }

    #[test]
    fn clip_restricts_and_restores() {
        let items = [
            Item::PushClip {
                rect: rect(1, 1, 2, 2),
            },
            fill(0, 0, 4, 4, RED),
            Item::PopClip,
            fill(0, 3, 4, 1, BLUE),
        ];
        let c = rasterize(&list(&items), 4, 4).unwrap();
        assert_eq!(c.pixel(0, 0), Some(WHITE_PX));
        assert_eq!(c.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 2), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(3, 3), Some([0, 0, 255, 255]), "restored after pop");
        assert_eq!(c.pixel(0, 3), Some([0, 0, 255, 255]));
    }

    #[test]
    fn nested_clips_intersect_and_inner_pop_restores_outer() {
        let items = [
            Item::PushClip {
                rect: rect(0, 0, 3, 3),
            },
            Item::PushClip {
                rect: rect(2, 2, 5, 5),
            },
            fill(0, 0, 8, 8, RED), // only (2,2)
            Item::PopClip,
            fill(0, 0, 8, 8, GREEN), // x,y in 0..3, over the red pixel
            Item::PopClip,
        ];
        let c = rasterize(&list(&items), 5, 5).unwrap();
        assert_eq!(c.pixel(2, 2), Some([0, 255, 0, 255]));
        assert_eq!(c.pixel(0, 0), Some([0, 255, 0, 255]));
        assert_eq!(c.pixel(3, 3), Some(WHITE_PX));
        // Disjoint clips leave nothing visible.
        let items = [
            Item::PushClip {
                rect: rect(0, 0, 1, 1),
            },
            Item::PushClip {
                rect: rect(3, 3, 1, 1),
            },
            fill(0, 0, 5, 5, RED),
            Item::PopClip,
            Item::PopClip,
        ];
        let c = rasterize(&list(&items), 5, 5).unwrap();
        assert_eq!(c.pixels, vec![255; 100]);
    }

    #[test]
    fn unbalanced_and_deep_clips_are_errors() {
        assert_eq!(
            rasterize(&list(&[Item::PopClip]), 2, 2),
            Err(RasterError::UnbalancedClips)
        );
        assert_eq!(
            rasterize(
                &list(&[Item::PushClip {
                    rect: rect(0, 0, 1, 1)
                }]),
                2,
                2
            ),
            Err(RasterError::UnbalancedClips)
        );
        let mut l = DisplayList::new();
        for _ in 0..=MAX_CLIP_DEPTH {
            l.push(Item::PushClip {
                rect: rect(0, 0, 1, 1),
            });
        }
        for _ in 0..=MAX_CLIP_DEPTH {
            l.push(Item::PopClip);
        }
        assert_eq!(rasterize(&l, 2, 2), Err(RasterError::ClipTooDeep));
    }

    #[test]
    fn size_bounds_fail_before_allocating() {
        let l = DisplayList::new();
        assert_eq!(rasterize(&l, 0, 5), Err(RasterError::EmptyViewport));
        assert_eq!(rasterize(&l, 5, 0), Err(RasterError::EmptyViewport));
        assert_eq!(rasterize(&l, 16385, 1), Err(RasterError::TooLarge));
        assert_eq!(rasterize(&l, 16384, 16384), Err(RasterError::TooLarge));
        assert_eq!(
            rasterize(&l, u32::MAX, u32::MAX),
            Err(RasterError::TooLarge)
        );
        assert!(Canvas::new(16384, 2048).is_ok());
    }

    fn side(w: i32, color: Color) -> BorderSide {
        BorderSide {
            width: px(w),
            style: BorderStyle::Solid,
            color,
        }
    }

    #[test]
    fn border_corners_are_mitred() {
        // 8x8 box, top red 2, right green 2, bottom black 2, left blue 2.
        let sides = [side(2, RED), side(2, GREEN), side(2, BLACK), side(2, BLUE)];
        let l = list(&[Item::Border {
            rect: rect(0, 0, 8, 8),
            sides,
        }]);
        let c = rasterize(&l, 8, 8).unwrap();
        let r = [255, 0, 0, 255];
        let g = [0, 255, 0, 255];
        let k = [0, 0, 0, 255];
        let b = [0, 0, 255, 255];
        // Top-left corner: diagonal pixels go to the horizontal side (top).
        assert_eq!(c.pixel(0, 0), Some(r));
        assert_eq!(c.pixel(1, 1), Some(r));
        assert_eq!(c.pixel(1, 0), Some(r));
        assert_eq!(c.pixel(0, 1), Some(b));
        // Top-right corner.
        assert_eq!(c.pixel(7, 0), Some(r));
        assert_eq!(c.pixel(6, 1), Some(r));
        assert_eq!(c.pixel(7, 1), Some(g));
        // Bottom-right and bottom-left.
        assert_eq!(c.pixel(7, 7), Some(k));
        assert_eq!(c.pixel(6, 6), Some(k));
        assert_eq!(c.pixel(7, 6), Some(g));
        assert_eq!(c.pixel(0, 7), Some(k));
        assert_eq!(c.pixel(0, 6), Some(b));
        // Straight runs and the interior.
        assert_eq!(c.pixel(4, 0), Some(r));
        assert_eq!(c.pixel(7, 4), Some(g));
        assert_eq!(c.pixel(4, 7), Some(k));
        assert_eq!(c.pixel(0, 4), Some(b));
        assert_eq!(c.pixel(3, 3), Some(WHITE_PX));
        assert_eq!(c.pixel(6, 4), Some(g));
        assert_eq!(c.pixel(5, 4), Some(WHITE_PX));
    }

    #[test]
    fn border_corner_with_unequal_widths_follows_the_diagonal() {
        // Left 4px blue, top 2px red, no right/bottom. Corner square is 4 wide, 2 tall.
        let none = BorderSide {
            width: px(0),
            style: BorderStyle::None,
            color: BLACK,
        };
        let l = list(&[Item::Border {
            rect: rect(0, 0, 10, 6),
            sides: [side(2, RED), none, none, side(4, BLUE)],
        }]);
        let c = rasterize(&l, 10, 6).unwrap();
        let r = [255, 0, 0, 255];
        let b = [0, 0, 255, 255];
        // Row 0 (dy=1): top owns while dy*4 <= dx*2, i.e. dx >= 2: px 0 (dx=1) blue.
        assert_eq!(c.pixel(0, 0), Some(b));
        assert_eq!(c.pixel(1, 0), Some(r));
        assert_eq!(c.pixel(3, 0), Some(r));
        // Row 1 (dy=3): top owns while 12 <= 2*dx, i.e. dx >= 6: x >= 3 (dx=7).
        for x in 0..3 {
            assert_eq!(c.pixel(x, 1), Some(b), "x={x}");
        }
        assert_eq!(c.pixel(3, 1), Some(r));
        assert_eq!(c.pixel(4, 0), Some(r));
        assert_eq!(
            c.pixel(4, 1),
            Some(r),
            "straight top band beyond the corner"
        );
        assert_eq!(
            c.pixel(0, 2),
            Some(b),
            "straight left band below the corner"
        );
        assert_eq!(c.pixel(4, 2), Some(WHITE_PX));
        // No bottom/right border painted.
        assert_eq!(c.pixel(9, 5), Some(WHITE_PX));
        assert_eq!(c.pixel(5, 5), Some(WHITE_PX));
    }

    #[test]
    fn thin_borders_cover_at_least_one_pixel_and_never_escape() {
        let thin = BorderSide {
            width: au(10),
            style: BorderStyle::Solid,
            color: RED,
        };
        let l = list(&[Item::Border {
            rect: rect(1, 1, 3, 3),
            sides: [thin; 4],
        }]);
        let c = rasterize(&l, 5, 5).unwrap();
        assert_eq!(c.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(3, 3), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 2), Some(WHITE_PX));
        assert_eq!(c.pixel(0, 0), Some(WHITE_PX));
        assert_eq!(c.pixel(4, 4), Some(WHITE_PX));
        // Borders thicker than the box fill it and stay inside it.
        let l = list(&[Item::Border {
            rect: rect(1, 1, 2, 2),
            sides: [side(5, RED); 4],
        }]);
        let c = rasterize(&l, 4, 4).unwrap();
        assert_eq!(c.pixel(1, 1), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 2), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(0, 0), Some(WHITE_PX));
        assert_eq!(c.pixel(3, 3), Some(WHITE_PX));
    }

    #[test]
    fn translucent_border_blends_once_per_pixel() {
        let half = Color { a: 128, ..BLACK };
        let l = list(&[Item::Border {
            rect: rect(0, 0, 4, 4),
            sides: [side(1, half); 4],
        }]);
        let c = rasterize(&l, 4, 4).unwrap();
        for (x, y) in [(0, 0), (3, 0), (0, 3), (3, 3), (1, 0), (0, 1)] {
            assert_eq!(c.pixel(x, y), Some([127, 127, 127, 255]), "({x},{y})");
        }
    }

    #[test]
    fn border_respects_clip() {
        let l = list(&[
            Item::PushClip {
                rect: rect(0, 0, 2, 8),
            },
            Item::Border {
                rect: rect(0, 0, 8, 8),
                sides: [side(1, RED); 4],
            },
            Item::PopClip,
        ]);
        let c = rasterize(&l, 8, 8).unwrap();
        assert_eq!(c.pixel(1, 0), Some([255, 0, 0, 255]));
        assert_eq!(c.pixel(2, 0), Some(WHITE_PX));
        assert_eq!(c.pixel(7, 0), Some(WHITE_PX));
        assert_eq!(c.pixel(0, 4), Some([255, 0, 0, 255]));
    }

    fn busy_list(n: usize) -> DisplayList {
        let mut l = DisplayList::new();
        let mut s = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for i in 0..n {
            let r = next();
            let rc = Rect {
                x: au((r % 76_800) as i32),
                y: au(((r >> 16) % 48_000) as i32),
                width: au(((r >> 32) % 12_000) as i32),
                height: au(((r >> 40) % 6_000) as i32),
            };
            let color = Color {
                r: (r >> 8) as u8,
                g: (r >> 24) as u8,
                b: (r >> 48) as u8,
                a: if i % 3 == 0 { 128 } else { 255 },
            };
            l.push(match i % 4 {
                0 => Item::TextRun { rect: rc, color },
                1 => Item::Border {
                    rect: rc,
                    sides: [BorderSide {
                        width: au(100),
                        style: BorderStyle::Solid,
                        color,
                    }; 4],
                },
                _ => Item::FillRect { rect: rc, color },
            });
        }
        l
    }

    #[test]
    fn deterministic_and_fast_enough() {
        let l = busy_list(2000);
        let a = rasterize(&l, 1280, 800).unwrap();
        let b = rasterize(&l, 1280, 800).unwrap();
        assert_eq!(a, b);
        // And the PNG of it is byte-identical and decodes to the same pixels.
        let p1 = a.to_png().unwrap();
        assert_eq!(p1, b.to_png().unwrap());
        let img = png::decode(&p1).unwrap();
        assert_eq!(img.rgba, a.pixels);
    }
}
