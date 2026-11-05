//! Anti-aliased shapes drawn with nothing but `FillRect`.
//!
//! The software rasterizer has no curves and no anti-aliasing (ADR 033 (f)), and
//! it must stay integer-only so one baseline serves both OSes. The chrome's
//! squircles, circles and strokes therefore reduce to coverage: each pixel is
//! sampled 4 x 4 times on a fixed grid, the hit count becomes an alpha, and equal
//! runs of pixels are merged (across rows too) into translucent `FillRect`s. The
//! display list that comes out is plain, hashable and rasterizes identically
//! everywhere.
//!
//! Determinism: sample positions and all shape geometry are integers in 1/8 px.
//! The one non-integer test, the superellipse, uses `+ - * /` and `sqrt` on `f64`
//! only. Those are correctly rounded IEEE 754 operations, so the result is the
//! same on every platform (this is why the exponent is 37/8 = 4.625, "about 4.6"
//! per `ui-direction.md`, and not 4.6: `u^4.625 = u^4 * (u^5)^(1/8)` and the
//! eighth root is three square roots, where `powf` would be libm-dependent).

use px_brand::palette::Rgb;
use px_layout::geom::px;
use px_paint::{Color, DisplayList, Item, Rect as PaintRect};

use crate::geom::Rect;

/// Fixed-point scale: sample and shape coordinates are in 1/8 pixel.
pub const SUB: i32 = 8;

/// Corner exponent as a fraction of eight: 37/8 = 4.625.
const EXP_EIGHTHS: u32 = 37;

/// A region of the plane that can be asked "is this sample inside".
pub trait Shape {
    /// Pixel bounds; everything outside has zero coverage.
    fn bounds(&self) -> Rect;
    /// A pixel's coverage when it can be known without sampling.
    fn hint(&self, px: i32, py: i32) -> Option<u8>;
    /// Is the sample point (1/8 px units) inside?
    fn inside(&self, sx: i32, sy: i32) -> bool;
}

/// `u^(37/8)` for `u` in `0..=1`, using only correctly rounded operations.
fn pow_squircle(u: f64) -> f64 {
    debug_assert_eq!(EXP_EIGHTHS, 37);
    let u2 = u * u;
    let u4 = u2 * u2;
    // 37/8 = 4 + 5/8, and u^(5/8) = ((u^5)^(1/2))^(1/2))^(1/2).
    let u5 = u4 * u;
    u4 * u5.sqrt().sqrt().sqrt()
}

/// Is `(dx, dy)`, a distance from the corner's centre, inside the superellipse
/// of "radius" `r`? All in 1/8 px.
fn corner_inside(dx: i32, dy: i32, r: i32) -> bool {
    if dx <= 0 || dy <= 0 {
        return true;
    }
    if dx >= r || dy >= r {
        // On or past the straight edge of the corner box: only the axis itself
        // is inside, and a sample is never exactly on it.
        return false;
    }
    let u = f64::from(dx) / f64::from(r);
    let v = f64::from(dy) / f64::from(r);
    pow_squircle(u) + pow_squircle(v) <= 1.0
}

/// A rectangle with squircle corners, radii `[top-left, top-right,
/// bottom-right, bottom-left]` in whole pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Squircle {
    pub rect: Rect,
    pub radii: [i32; 4],
}

impl Squircle {
    /// All four corners the same. The radius is capped at half the shorter side.
    pub fn new(rect: Rect, radius: i32) -> Squircle {
        Squircle::with_radii(rect, [radius; 4])
    }

    pub fn with_radii(rect: Rect, radii: [i32; 4]) -> Squircle {
        let cap = (rect.w.min(rect.h) / 2).max(0);
        Squircle {
            rect,
            radii: radii.map(|r| r.clamp(0, cap)),
        }
    }

    /// The inside edge of a `width` px border: the rect inset, radii reduced.
    fn inset(&self, width: i32) -> Squircle {
        Squircle::with_radii(
            self.rect.inset(width),
            self.radii.map(|r| (r - width).max(0)),
        )
    }

    /// A ring `width` px thick along the inside of this shape's edge.
    pub fn stroke(self, width: i32) -> Stroke {
        Stroke {
            outer: self,
            inner: self.inset(width),
        }
    }

    /// Corner boxes in 1/8 px: `(x0, y0, x1, y1, centre_x, centre_y, r, corner)`.
    fn zones(&self) -> [(i32, i32, i32, i32, i32, i32, i32); 4] {
        let (x0, y0) = (self.rect.x * SUB, self.rect.y * SUB);
        let (x1, y1) = (self.rect.right() * SUB, self.rect.bottom() * SUB);
        let r = self.radii.map(|r| r * SUB);
        [
            (x0, y0, x0 + r[0], y0 + r[0], x0 + r[0], y0 + r[0], r[0]),
            (x1 - r[1], y0, x1, y0 + r[1], x1 - r[1], y0 + r[1], r[1]),
            (x1 - r[2], y1 - r[2], x1, y1, x1 - r[2], y1 - r[2], r[2]),
            (x0, y1 - r[3], x0 + r[3], y1, x0 + r[3], y1 - r[3], r[3]),
        ]
    }
}

impl Shape for Squircle {
    fn bounds(&self) -> Rect {
        self.rect
    }

    fn hint(&self, px: i32, py: i32) -> Option<u8> {
        if !self.rect.contains(crate::geom::Point { x: px, y: py }) {
            return Some(0);
        }
        let (bx0, by0) = (px * SUB, py * SUB);
        let (bx1, by1) = (bx0 + SUB, by0 + SUB);
        for (zx0, zy0, zx1, zy1, ..) in self.zones() {
            if zx1 > zx0 && bx0 < zx1 && bx1 > zx0 && by0 < zy1 && by1 > zy0 {
                return None;
            }
        }
        Some(255)
    }

    fn inside(&self, sx: i32, sy: i32) -> bool {
        let (x0, y0) = (self.rect.x * SUB, self.rect.y * SUB);
        let (x1, y1) = (self.rect.right() * SUB, self.rect.bottom() * SUB);
        if sx < x0 || sx >= x1 || sy < y0 || sy >= y1 {
            return false;
        }
        for (zx0, zy0, zx1, zy1, cx, cy, r) in self.zones() {
            if r > 0 && sx >= zx0 && sx < zx1 && sy >= zy0 && sy < zy1 {
                return corner_inside((sx - cx).abs(), (sy - cy).abs(), r);
            }
        }
        true
    }
}

/// A squircle border: outer shape minus inner shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stroke {
    outer: Squircle,
    inner: Squircle,
}

impl Shape for Stroke {
    fn bounds(&self) -> Rect {
        self.outer.rect
    }

    fn hint(&self, px: i32, py: i32) -> Option<u8> {
        if self.outer.hint(px, py) == Some(0) || self.inner.hint(px, py) == Some(255) {
            return Some(0);
        }
        None
    }

    fn inside(&self, sx: i32, sy: i32) -> bool {
        self.outer.inside(sx, sy) && !self.inner.inside(sx, sy)
    }
}

/// A disc, or a ring when `hole > 0`. Centre and radii in 1/8 px.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Circle {
    pub cx: i32,
    pub cy: i32,
    pub r: i32,
    pub hole: i32,
}

impl Circle {
    /// A disc centred at pixel coordinates (may be fractional: `x8` is x * 8).
    pub fn disc(cx8: i32, cy8: i32, r8: i32) -> Circle {
        Circle {
            cx: cx8,
            cy: cy8,
            r: r8,
            hole: 0,
        }
    }

    pub fn ring(cx8: i32, cy8: i32, r8: i32, thickness8: i32) -> Circle {
        Circle {
            cx: cx8,
            cy: cy8,
            r: r8,
            hole: (r8 - thickness8).max(0),
        }
    }
}

impl Shape for Circle {
    fn bounds(&self) -> Rect {
        let x0 = (self.cx - self.r).div_euclid(SUB);
        let y0 = (self.cy - self.r).div_euclid(SUB);
        let x1 = (self.cx + self.r + SUB - 1).div_euclid(SUB);
        let y1 = (self.cy + self.r + SUB - 1).div_euclid(SUB);
        Rect::new(x0, y0, x1 - x0, y1 - y0)
    }

    fn hint(&self, _px: i32, _py: i32) -> Option<u8> {
        None
    }

    fn inside(&self, sx: i32, sy: i32) -> bool {
        let dx = i64::from(sx - self.cx);
        let dy = i64::from(sy - self.cy);
        let d2 = dx * dx + dy * dy;
        d2 <= i64::from(self.r) * i64::from(self.r)
            && d2 >= i64::from(self.hole) * i64::from(self.hole)
    }
}

/// A thick polyline with round caps and joins: the union of capsules. Points in
/// 1/8 px, `half_width` likewise.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Path {
    segments: Vec<((i32, i32), (i32, i32))>,
    half_width: i32,
}

impl Path {
    pub fn polyline(points: &[(i32, i32)], half_width8: i32) -> Path {
        Path {
            segments: points.windows(2).map(|w| (w[0], w[1])).collect(),
            half_width: half_width8,
        }
    }
}

/// Squared distance from `p` to the segment `a..b` is at most `hw2`.
fn near_segment(p: (i32, i32), a: (i32, i32), b: (i32, i32), hw2: i64) -> bool {
    let (px, py) = (i64::from(p.0), i64::from(p.1));
    let (ax, ay) = (i64::from(a.0), i64::from(a.1));
    let (bx, by) = (i64::from(b.0), i64::from(b.1));
    let (vx, vy) = (bx - ax, by - ay);
    let (wx, wy) = (px - ax, py - ay);
    let len2 = vx * vx + vy * vy;
    let dot = wx * vx + wy * vy;
    if len2 == 0 || dot <= 0 {
        return wx * wx + wy * wy <= hw2;
    }
    if dot >= len2 {
        let (ux, uy) = (px - bx, py - by);
        return ux * ux + uy * uy <= hw2;
    }
    let cross = wx * vy - wy * vx;
    // |cross| / len <= hw  <=>  cross^2 <= hw^2 * len^2
    i128::from(cross) * i128::from(cross) <= i128::from(hw2) * i128::from(len2)
}

impl Shape for Path {
    fn bounds(&self) -> Rect {
        let mut it = self.segments.iter().flat_map(|(a, b)| [*a, *b]);
        let Some(first) = it.next() else {
            return Rect::default();
        };
        let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
        for (x, y) in it {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let hw = self.half_width;
        let px0 = (x0 - hw).div_euclid(SUB);
        let py0 = (y0 - hw).div_euclid(SUB);
        let px1 = (x1 + hw + SUB - 1).div_euclid(SUB);
        let py1 = (y1 + hw + SUB - 1).div_euclid(SUB);
        Rect::new(px0, py0, px1 - px0, py1 - py0)
    }

    fn hint(&self, _px: i32, _py: i32) -> Option<u8> {
        None
    }

    fn inside(&self, sx: i32, sy: i32) -> bool {
        let hw2 = i64::from(self.half_width) * i64::from(self.half_width);
        self.segments
            .iter()
            .any(|(a, b)| near_segment((sx, sy), *a, *b, hw2))
    }
}

/// 4 x 4 coverage of one pixel, 0..=255.
fn coverage(shape: &dyn Shape, px: i32, py: i32) -> u8 {
    if let Some(a) = shape.hint(px, py) {
        return a;
    }
    let mut hits = 0u32;
    for j in 0..4 {
        for i in 0..4 {
            if shape.inside(px * SUB + 2 * i + 1, py * SUB + 2 * j + 1) {
                hits += 1;
            }
        }
    }
    ((hits * 255 + 8) / 16) as u8
}

/// A merged run of equal-alpha pixels, still open for vertical growth.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Run {
    x0: i32,
    x1: i32,
    alpha: u8,
    y0: i32,
}

fn emit(list: &mut DisplayList, run: Run, y1: i32, color: Color) {
    let a = (u32::from(run.alpha) * u32::from(color.a) + 127) / 255;
    if a == 0 {
        return;
    }
    list.push(Item::FillRect {
        rect: PaintRect {
            x: px(run.x0),
            y: px(run.y0),
            width: px(run.x1 - run.x0),
            height: px(y1 - run.y0),
        },
        color: Color {
            a: a as u8,
            ..color
        },
    });
}

/// Fill `shape` with `color`, anti-aliased, appended to `list`.
pub fn fill(list: &mut DisplayList, shape: &dyn Shape, color: Color) {
    let b = shape.bounds();
    if b.is_empty() || color.a == 0 {
        return;
    }
    let mut prev: Vec<Run> = Vec::new();
    for y in b.y..b.bottom() {
        let mut cur: Vec<Run> = Vec::new();
        let mut x = b.x;
        while x < b.right() {
            let a = coverage(shape, x, y);
            let start = x;
            x += 1;
            while x < b.right() && coverage(shape, x, y) == a {
                x += 1;
            }
            if a > 0 {
                let carried = prev
                    .iter()
                    .find(|r| r.x0 == start && r.x1 == x && r.alpha == a)
                    .map_or(y, |r| r.y0);
                cur.push(Run {
                    x0: start,
                    x1: x,
                    alpha: a,
                    y0: carried,
                });
            }
        }
        for r in &prev {
            if !cur
                .iter()
                .any(|c| c.x0 == r.x0 && c.x1 == r.x1 && c.alpha == r.alpha && c.y0 == r.y0)
            {
                emit(list, *r, y, color);
            }
        }
        prev = cur;
    }
    for r in prev {
        emit(list, r, b.bottom(), color);
    }
}

/// Convert a palette colour and an alpha to a paint colour.
pub fn color(c: Rgb, alpha: u8) -> Color {
    Color {
        r: c.0,
        g: c.1,
        b: c.2,
        a: alpha,
    }
}

/// A soft shadow under `rect`: `layers` squircles growing outward, each faint,
/// so alpha builds up toward the rect and falls off quadratically.
pub fn shadow(
    list: &mut DisplayList,
    rect: Rect,
    radius: i32,
    dy: i32,
    layers: i32,
    peak_alpha: u8,
) {
    let total: i32 = (1..=layers).map(|k| k * k).sum();
    if total == 0 {
        return;
    }
    for i in (0..layers).rev() {
        // Outermost layer first; layer weight grows inward.
        let weight = layers - i;
        let alpha = (i32::from(peak_alpha) * weight * weight / total).clamp(0, 255);
        // A layer's alpha is *added* by the layers drawn after it, so use the
        // per-layer share, not the running total.
        let grown = Rect::new(rect.x, rect.y + dy, rect.w, rect.h).inset(-i);
        fill(
            list,
            &Squircle::new(grown, radius + i),
            Color {
                r: 0,
                g: 0,
                b: 0,
                a: alpha.min(255) as u8,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_paint::raster::rasterize;

    fn render(shape: &dyn Shape, w: u32, h: u32, c: Color) -> px_paint::Canvas {
        let mut l = DisplayList::new();
        fill(&mut l, shape, c);
        rasterize(&l, w, h).unwrap()
    }

    const BLACK: Color = Color::BLACK;

    fn lum(c: &px_paint::Canvas, x: u32, y: u32) -> u8 {
        c.pixel(x, y).unwrap()[0]
    }

    #[test]
    fn a_squircle_is_solid_inside_empty_at_the_corner_and_symmetric() {
        let s = Squircle::new(Rect::new(1, 1, 30, 30), 11);
        let c = render(&s, 32, 32, BLACK);
        assert_eq!(lum(&c, 16, 16), 0, "centre is ink");
        assert_eq!(lum(&c, 1, 1), 255, "extreme corner pixel is untouched");
        assert_eq!(lum(&c, 16, 1), 0, "the straight top edge is ink");
        // 4-fold mirror symmetry of the whole bitmap.
        for y in 0..32 {
            for x in 0..32 {
                let m = lum(&c, 31 - x, y);
                assert_eq!(lum(&c, x, y), m, "mirror x at ({x},{y})");
                let m = lum(&c, x, 31 - y);
                assert_eq!(lum(&c, x, y), m, "mirror y at ({x},{y})");
            }
        }
    }

    #[test]
    fn a_squircle_corner_is_squarer_than_a_circle_of_the_same_radius() {
        // On the diagonal, the circle of radius r is r*(1-1/sqrt2) = 0.29 r in
        // from the box corner; the superellipse is 0.18 r. At r = 20 the pixel 4
        // in is inside the squircle and outside the circle.
        let r = 20;
        let sq = render(&Squircle::new(Rect::new(0, 0, 60, 60), r), 60, 60, BLACK);
        assert!(lum(&sq, 4, 4) < 128, "squircle covers (4,4)");
        let circle_centre = r * SUB;
        let disc = Circle::disc(circle_centre, circle_centre, r * SUB);
        let ci = render(&disc, 60, 60, BLACK);
        assert!(lum(&ci, 4, 4) > 128, "circle does not cover (4,4)");
    }

    #[test]
    fn edges_are_anti_aliased() {
        let s = Squircle::new(Rect::new(0, 0, 40, 40), 14);
        let c = render(&s, 40, 40, BLACK);
        let partial = (0..40u32)
            .flat_map(|y| (0..40u32).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let v = lum(&c, x, y);
                v != 0 && v != 255
            })
            .count();
        assert!(partial > 10, "only {partial} partial pixels");
    }

    #[test]
    fn output_is_compact_and_deterministic() {
        let s = Squircle::new(Rect::new(0, 0, 920, 568), 22);
        let mut a = DisplayList::new();
        fill(&mut a, &s, BLACK);
        let mut b = DisplayList::new();
        fill(&mut b, &s, BLACK);
        assert_eq!(a, b);
        // Four corners of ~22 rows each, plus the merged middle band.
        assert!(a.len() < 400, "{} items", a.len());
        assert!(a.clips_balanced());
    }

    #[test]
    fn a_stroke_is_a_hairline_that_follows_the_edge() {
        let s = Squircle::new(Rect::new(0, 0, 40, 40), 12).stroke(1);
        let c = render(&s, 40, 40, BLACK);
        assert_eq!(lum(&c, 20, 0), 0, "top edge");
        assert_eq!(lum(&c, 0, 20), 0, "left edge");
        assert_eq!(lum(&c, 20, 1), 255, "one pixel in is empty");
        assert_eq!(lum(&c, 20, 20), 255, "centre is empty");
    }

    #[test]
    fn a_ring_has_a_hole_and_a_path_has_round_ends() {
        let ring = Circle::ring(10 * SUB, 10 * SUB, 8 * SUB, 2 * SUB);
        let c = render(&ring, 20, 20, BLACK);
        assert_eq!(lum(&c, 10, 10), 255, "hole");
        assert!(lum(&c, 10, 3) < 40, "ring body");
        let p = Path::polyline(&[(4 * SUB, 10 * SUB), (16 * SUB, 10 * SUB)], SUB);
        let c = render(&p, 20, 20, BLACK);
        assert_eq!(lum(&c, 10, 9), 0);
        assert_eq!(lum(&c, 10, 10), 0);
        assert_eq!(lum(&c, 10, 12), 255);
        assert!(lum(&c, 3, 10) > 0, "cap extends past the endpoint");
    }

    #[test]
    fn alpha_composes_with_the_colour_alpha() {
        let s = Squircle::new(Rect::new(0, 0, 8, 8), 0);
        let half = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 128,
        };
        let c = render(&s, 8, 8, half);
        let v = lum(&c, 4, 4);
        assert!((120..=127).contains(&v), "{v}");
    }

    #[test]
    fn radius_is_capped_so_a_tiny_rect_does_not_invert() {
        let s = Squircle::new(Rect::new(0, 0, 6, 6), 50);
        assert_eq!(s.radii, [3; 4]);
        let c = render(&s, 6, 6, BLACK);
        assert!(lum(&c, 3, 3) < 20);
    }
}
