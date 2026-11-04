//! The integer glyph rasterizer (Phase 9 unit 5).
//!
//! An outline in font units goes in; an 8-bit coverage [`Mask`] comes out. There
//! is **no floating point anywhere in this module**: every value is `i64`/`i32`
//! arithmetic, every division has a stated rounding, and the same input produces
//! the same bytes on every run and every OS. No hash-ordered container is used.
//!
//! Placement note: ADR 036 / the phase-9 plan place this in the software
//! rasterizer's path. The software rasterizer lives in this crate (`px-gpu` wraps
//! [`crate::raster::rasterize`]), so the module is here; `px-gpu` reaches it
//! through `px-paint`.
//!
//! # Rules
//!
//! **Scale (the one rounding of the input).** A [`Scale`] is a pixel size in
//! 1/64 px (`size64`) and `units_per_em`. A font-unit coordinate `u` becomes
//! device space in 1/256 px (`SUB = 256`) as
//! `round_half_up(u * size64 * 4 / units_per_em)`, that is
//! `floor((2*u*size64*4 + upem) / (2*upem))`. The y axis is negated *first* (font y
//! is up, device y is down) and rounded in the device direction. The optional
//! placement offset is in 1/64 px, added after scaling, exactly (times 4).
//!
//! **Curves.** Quadratic and cubic segments are flattened by uniform subdivision
//! into `n` lines, where `n` is the smallest integer whose conservative error
//! bound is at most [`FLATNESS`] (8/256 = 1/32 px), capped at
//! [`MAX_CURVE_SEGMENTS`]. Subdivision points are evaluated independently from the
//! control points as exact rationals and rounded half up to 1/256 px, so no error
//! accumulates along the curve.
//!
//! **Coverage.** Exact-area scanline coverage by signed-area accumulation. Each
//! line is cut at the pixel-row boundaries and then at the pixel-column
//! boundaries; the crossing coordinate is `floor` of the exact rational, so the
//! only inexactness is that crossings are floored to 1/256 px (zero for axis-aligned
//! edges and for any crossing that already lies on the 1/256 grid). Within a piece
//! the covered area is the exact trapezoid. Areas are integers in 1/131072 px^2.
//!
//! **Fill rule.** Nonzero winding, evaluated as `min(|sum of signed areas|, 1)`
//! per pixel. A pixel is exact when its partial coverage comes from edges of one
//! winding sense; where edges of different contours cross *inside* one pixel the
//! value is the clamped signed sum (the same approximation every accumulation
//! rasterizer makes). Fully-inside and fully-outside pixels are always exact, so
//! same-direction overlap fills (255) and opposite-direction overlap (a hole)
//! cancels.
//!
//! **Coverage byte.** `S` in 1/131072 px^2, clamped to `0..=131072`:
//! `(S * 255 + 65536) >> 17`, round half up. Full coverage is exactly 255 and
//! half a pixel is 128.
//!
//! **Unclosed contours** are closed with a straight line, as in every fill.
//!
//! # Bounds (fail closed)
//!
//! The mask is the pixel bounding box of every point of the scaled outline
//! (control points included, which over-covers a curve's box but never clips it).
//! Width and height are each at most [`MAX_DIMENSION`] and the pixel count at most
//! [`MAX_PIXELS`]; larger inputs return [`GlyphError::TooLarge`] before anything is
//! allocated. All point arithmetic happens in `i64` after that check, relative to
//! the mask origin, so nothing can overflow. A zero-area outline yields an empty
//! (0 by 0) mask.

use crate::display_list::Color;
use crate::raster::Canvas;
use std::fmt;

/// Device sub-pixel units per pixel (1/256 px).
pub const SUB: i64 = 256;
/// Largest mask width or height.
pub const MAX_DIMENSION: i64 = 4096;
/// Largest mask pixel count.
pub const MAX_PIXELS: i64 = 1 << 22;
/// Largest accepted `Scale::size64` (16384 px).
pub const MAX_SIZE64: u32 = 1 << 20;
/// Most lines one curve is flattened into.
pub const MAX_CURVE_SEGMENTS: i64 = 128;
/// Flattening error bound in 1/256 px.
pub const FLATNESS: i64 = 8;

/// A point in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pt {
    pub x: i32,
    pub y: i32,
}

/// One path command, in font units, y up. The shape matches `px_text::PathCmd`
/// plus the cubic segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCmd {
    MoveTo(Pt),
    LineTo(Pt),
    /// Control point, end point.
    QuadTo(Pt, Pt),
    /// Two control points, end point.
    CubicTo(Pt, Pt, Pt),
    Close,
}

/// Font units to pixels: `size64` is the pixel size in 1/64 px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scale {
    pub size64: u32,
    pub units_per_em: u32,
}

/// Why no mask was produced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlyphError {
    /// `units_per_em` is zero or `size64` exceeds [`MAX_SIZE64`].
    BadScale,
    /// The mask would exceed [`MAX_DIMENSION`] or [`MAX_PIXELS`].
    TooLarge,
    /// The mask's position does not fit an `i32`.
    OutOfRange,
}

impl fmt::Display for GlyphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            GlyphError::BadScale => "invalid glyph scale",
            GlyphError::TooLarge => "glyph mask exceeds raster limits",
            GlyphError::OutOfRange => "glyph position out of range",
        })
    }
}

impl std::error::Error for GlyphError {}

/// An 8-bit coverage mask, row-major, `data.len() == width * height`.
///
/// `(left, top)` is the device pixel of `data[0]` relative to the glyph origin
/// (so `top` is negative for ink above the baseline).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Mask {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

impl Mask {
    /// Coverage at mask-local `(x, y)`, or `None` outside the mask.
    pub fn coverage(&self, x: u32, y: u32) -> Option<u8> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let i = (y as usize)
            .checked_mul(self.width as usize)?
            .checked_add(x as usize)?;
        self.data.get(i).copied()
    }
}

type P = (i64, i64);

/// `round_half_up(num / den)` for `den > 0`.
fn rdiv(num: i64, den: i64) -> i64 {
    (2 * num + den).div_euclid(2 * den)
}

/// Smallest `n >= 1` with `n * n >= v`.
fn ceil_sqrt(v: i64) -> i64 {
    let mut n = 1i64;
    while n * n < v {
        n += 1;
        if n >= MAX_CURVE_SEGMENTS {
            break;
        }
    }
    n
}

#[derive(Clone, Copy)]
enum Seg {
    Move(P),
    Line(P),
    Quad(P, P),
    Cubic(P, P, P),
    Close,
}

/// Rasterize `outline` at `scale`, with the origin moved by `offset64` (1/64 px).
pub fn rasterize_glyph(
    outline: &[PathCmd],
    scale: Scale,
    offset64: (i32, i32),
) -> Result<Mask, GlyphError> {
    if scale.units_per_em == 0 || scale.size64 > MAX_SIZE64 {
        return Err(GlyphError::BadScale);
    }
    if scale.size64 == 0 {
        return Ok(Mask::default());
    }
    let upem = i64::from(scale.units_per_em);
    let k = i64::from(scale.size64) * 4;
    let map = |p: Pt| -> P {
        (
            rdiv(i64::from(p.x) * k, upem) + i64::from(offset64.0) * 4,
            rdiv(-i64::from(p.y) * k, upem) + i64::from(offset64.1) * 4,
        )
    };
    let mut segs: Vec<Seg> = Vec::with_capacity(outline.len());
    let mut bbox: Option<(i64, i64, i64, i64)> = None;
    let mut grow = |p: P| {
        bbox = Some(match bbox {
            None => (p.0, p.1, p.0, p.1),
            Some((a, b, c, d)) => (a.min(p.0), b.min(p.1), c.max(p.0), d.max(p.1)),
        });
    };
    for cmd in outline {
        segs.push(match *cmd {
            PathCmd::MoveTo(a) => {
                let a = map(a);
                grow(a);
                Seg::Move(a)
            }
            PathCmd::LineTo(a) => {
                let a = map(a);
                grow(a);
                Seg::Line(a)
            }
            PathCmd::QuadTo(a, b) => {
                let (a, b) = (map(a), map(b));
                grow(a);
                grow(b);
                Seg::Quad(a, b)
            }
            PathCmd::CubicTo(a, b, c) => {
                let (a, b, c) = (map(a), map(b), map(c));
                grow(a);
                grow(b);
                grow(c);
                Seg::Cubic(a, b, c)
            }
            PathCmd::Close => Seg::Close,
        });
    }
    let Some((min_x, min_y, max_x, max_y)) = bbox else {
        return Ok(Mask::default());
    };
    let x0 = min_x.div_euclid(SUB);
    let y0 = min_y.div_euclid(SUB);
    let x1 = -(-max_x).div_euclid(SUB); // ceil
    let y1 = -(-max_y).div_euclid(SUB);
    let (w, h) = (x1 - x0, y1 - y0);
    if w <= 0 || h <= 0 {
        return Ok(Mask::default());
    }
    if w > MAX_DIMENSION || h > MAX_DIMENSION || w * h > MAX_PIXELS {
        return Err(GlyphError::TooLarge);
    }
    let left = i32::try_from(x0).map_err(|_| GlyphError::OutOfRange)?;
    let top = i32::try_from(y0).map_err(|_| GlyphError::OutOfRange)?;
    let (wu, hu) = (w as usize, h as usize);
    let mut acc = vec![0i32; wu * hu];
    let origin = (x0 * SUB, y0 * SUB);
    let local = |p: P| -> P { (p.0 - origin.0, p.1 - origin.1) };

    let mut cur: Option<P> = None;
    let mut start: Option<P> = None;
    for seg in segs {
        match seg {
            Seg::Move(p) => {
                close(&mut acc, wu, hu, cur, start, &local);
                cur = Some(p);
                start = Some(p);
            }
            Seg::Line(p) => {
                let from = *cur.get_or_insert(p);
                start.get_or_insert(from);
                add_line(&mut acc, wu, hu, local(from), local(p));
                cur = Some(p);
            }
            Seg::Quad(c, p) => {
                let from = *cur.get_or_insert(p);
                start.get_or_insert(from);
                flatten_quad(&mut acc, wu, hu, local(from), local(c), local(p));
                cur = Some(p);
            }
            Seg::Cubic(c1, c2, p) => {
                let from = *cur.get_or_insert(p);
                start.get_or_insert(from);
                flatten_cubic(
                    &mut acc,
                    wu,
                    hu,
                    local(from),
                    local(c1),
                    local(c2),
                    local(p),
                );
                cur = Some(p);
            }
            Seg::Close => {
                close(&mut acc, wu, hu, cur, start, &local);
                cur = start;
            }
        }
    }
    close(&mut acc, wu, hu, cur, start, &local);

    let mut data = vec![0u8; wu * hu];
    for (row, out) in acc.chunks(wu).zip(data.chunks_mut(wu)) {
        let mut sum: i64 = 0;
        for (a, o) in row.iter().zip(out.iter_mut()) {
            sum += i64::from(*a);
            let s = sum.abs().min(131_072);
            *o = ((s * 255 + 65_536) >> 17) as u8;
        }
    }
    Ok(Mask {
        left,
        top,
        width: w as u32,
        height: h as u32,
        data,
    })
}

/// Draw the closing line of the current contour, if it is open.
fn close(
    acc: &mut [i32],
    w: usize,
    h: usize,
    cur: Option<P>,
    start: Option<P>,
    local: &impl Fn(P) -> P,
) {
    if let (Some(c), Some(s)) = (cur, start)
        && c != s
    {
        add_line(acc, w, h, local(c), local(s));
    }
}

fn flatten_quad(acc: &mut [i32], w: usize, h: usize, p0: P, p1: P, p2: P) {
    let d = (p0.0 - 2 * p1.0 + p2.0).abs() + (p0.1 - 2 * p1.1 + p2.1).abs();
    // error <= d / (4 n^2) <= FLATNESS
    let n = ceil_sqrt((d + 4 * FLATNESS - 1) / (4 * FLATNESS));
    let mut prev = p0;
    for i in 1..=n {
        let next = if i == n {
            p2
        } else {
            let (a, b) = (n - i, i);
            let den = n * n;
            (
                rdiv(a * a * p0.0 + 2 * a * b * p1.0 + b * b * p2.0, den),
                rdiv(a * a * p0.1 + 2 * a * b * p1.1 + b * b * p2.1, den),
            )
        };
        add_line(acc, w, h, prev, next);
        prev = next;
    }
}

fn flatten_cubic(acc: &mut [i32], w: usize, h: usize, p0: P, p1: P, p2: P, p3: P) {
    let d1 = (p0.0 - 2 * p1.0 + p2.0).abs() + (p0.1 - 2 * p1.1 + p2.1).abs();
    let d2 = (p1.0 - 2 * p2.0 + p3.0).abs() + (p1.1 - 2 * p2.1 + p3.1).abs();
    let d = d1.max(d2);
    // error <= 3 d / (4 n^2) <= FLATNESS
    let n = ceil_sqrt((3 * d + 4 * FLATNESS - 1) / (4 * FLATNESS));
    let mut prev = p0;
    for i in 1..=n {
        let next = if i == n {
            p3
        } else {
            let (a, b) = (n - i, i);
            let den = n * n * n;
            let (c0, c1, c2, c3) = (a * a * a, 3 * a * a * b, 3 * a * b * b, b * b * b);
            (
                rdiv(c0 * p0.0 + c1 * p1.0 + c2 * p2.0 + c3 * p3.0, den),
                rdiv(c0 * p0.1 + c1 * p1.1 + c2 * p2.1 + c3 * p3.1, den),
            )
        };
        add_line(acc, w, h, prev, next);
        prev = next;
    }
}

/// Accumulate one directed line (mask-local 1/256 px coordinates).
fn add_line(acc: &mut [i32], w: usize, h: usize, p0: P, p1: P) {
    if p0.1 == p1.1 {
        return;
    }
    let (dir, a, b) = if p0.1 < p1.1 {
        (1i32, p0, p1)
    } else {
        (-1i32, p1, p0)
    };
    let dxl = b.0 - a.0;
    let dyl = b.1 - a.1;
    let first = a.1.div_euclid(SUB);
    let last = (b.1 - 1).div_euclid(SUB);
    for r in first..=last {
        let ya = a.1.max(r * SUB);
        let yb = b.1.min((r + 1) * SUB);
        let xa = a.0 + ((ya - a.1) * dxl).div_euclid(dyl);
        let xb = a.0 + ((yb - a.1) * dxl).div_euclid(dyl);
        row_piece(acc, w, h, r, dir, (xa, ya), (xb, yb));
    }
}

/// One line piece inside pixel row `r`, split at pixel-column boundaries.
fn row_piece(acc: &mut [i32], w: usize, h: usize, r: i64, dir: i32, from: P, to: P) {
    if r < 0 || r >= h as i64 {
        return;
    }
    let dx = to.0 - from.0;
    let dy = to.1 - from.1;
    let mut prev = from;
    if dx != 0 {
        let step = if dx > 0 { SUB } else { -SUB };
        let mut gx = if dx > 0 {
            (from.0.div_euclid(SUB) + 1) * SUB
        } else {
            (from.0 - 1).div_euclid(SUB) * SUB
        };
        while (dx > 0 && gx < to.0) || (dx < 0 && gx > to.0) {
            let y = from.1 + ((gx - from.0).abs() * dy) / dx.abs();
            emit(acc, w, r, dir, prev, (gx, y));
            prev = (gx, y);
            gx += step;
        }
    }
    emit(acc, w, r, dir, prev, to);
}

/// A piece lying in one pixel cell column: add its area and cover.
fn emit(acc: &mut [i32], w: usize, r: i64, dir: i32, p0: P, p1: P) {
    let dy = p1.1 - p0.1;
    if dy <= 0 {
        return;
    }
    let col = p0.0.min(p1.0).div_euclid(SUB);
    if col < 0 || col >= w as i64 {
        return;
    }
    let fa = (p0.0 - col * SUB).clamp(0, SUB);
    let fb = (p1.0 - col * SUB).clamp(0, SUB);
    let base = (r as usize) * w + col as usize;
    let left = (dy * (2 * SUB - fa - fb)) as i32;
    let right = (dy * (fa + fb)) as i32;
    let d = dir;
    if let Some(c) = acc.get_mut(base) {
        *c = c.saturating_add(d * left);
    }
    if (col as usize) + 1 < w
        && let Some(c) = acc.get_mut(base + 1)
    {
        *c = c.saturating_add(d * right);
    }
}

/// Composite `mask` onto `canvas` in `color`, with the mask's glyph origin at
/// device pixel `(x, y)`. The effective alpha of a pixel is
/// `(color.a * coverage + 127) / 255` (round half up), then the canvas's ordinary
/// source-over rule applies. Pixels outside the canvas are skipped. For a
/// rectangular clip use [`composite_mask_clipped`].
pub fn composite_mask(canvas: &mut Canvas, mask: &Mask, x: i32, y: i32, color: Color) {
    let full = (0, 0, i64::from(canvas.width), i64::from(canvas.height));
    composite_mask_clipped(canvas, mask, x, y, color, full);
}

/// As [`composite_mask`], also restricted to the half-open device rectangle
/// `clip = (x0, y0, x1, y1)` (the rasterizer's current clip).
pub fn composite_mask_clipped(
    canvas: &mut Canvas,
    mask: &Mask,
    x: i32,
    y: i32,
    color: Color,
    clip: (i64, i64, i64, i64),
) {
    if color.a == 0 || mask.width == 0 || mask.height == 0 {
        return;
    }
    let (cw, ch) = (i64::from(canvas.width), i64::from(canvas.height));
    let (cx0, cy0) = (clip.0.max(0), clip.1.max(0));
    let (cx1, cy1) = (clip.2.min(cw), clip.3.min(ch));
    for my in 0..mask.height {
        let py = i64::from(y) + i64::from(mask.top) + i64::from(my);
        if py < cy0 || py >= cy1 {
            continue;
        }
        for mx in 0..mask.width {
            let px = i64::from(x) + i64::from(mask.left) + i64::from(mx);
            if px < cx0 || px >= cx1 {
                continue;
            }
            let Some(cov) = mask.coverage(mx, my) else {
                continue;
            };
            if cov == 0 {
                continue;
            }
            let a = (u32::from(color.a) * u32::from(cov) + 127) / 255;
            let i = ((py * cw + px) * 4) as usize;
            if let Some(px4) = canvas.pixels.get_mut(i..i + 4) {
                crate::raster::blend_px(
                    px4,
                    Color {
                        a: a as u8,
                        ..color
                    },
                );
            }
        }
    }
}
