//! Coverage of rounded rectangles, integer only.
//!
//! A [`Shape`] is a rounded rectangle on the device pixel grid: its rectangle is
//! snapped exactly as a [`crate::display_list::Item::FillRect`]'s is, so a shape
//! with no rounded corner covers precisely the pixels the plain rectangle does.
//! Its radii are kept in [`SUB`] units per pixel (`Au * 4`, 1/240 px).
//!
//! **Coverage.** A pixel is sampled `8 x 8` times on a fixed grid, the sample
//! `(i, j)` sitting at `((2i + 1) / 16, (2j + 1) / 16)` of the pixel (integers in
//! 1/240 px: `15 * (2i + 1)`). The hit count `h` in `0..=64` becomes the coverage
//! `(255 h + 32) / 64`. A pixel the corner curves cannot touch is 255 or 0 without
//! sampling, and a pixel whose nearest and farthest points (from the corner) are
//! both on one side of the curve is decided from those two points: inside-ness is
//! monotone in the distance from the shape's outer corner, so this agrees with
//! sampling exactly.
//!
//! **The curve.** In the corner box (`rx` by `ry`, at the shape's outer corner),
//! with `X`, `Y` the distance from the outer corner, a convex corner contains the
//! point when `u^n + v^n <= 1`, `u = (rx - X) / rx`, `v = (ry - Y) / ry`; a
//! concave corner (`K < 0`: `scoop`, `notch`) contains it when `u^n + v^n >= 1`
//! with `u = X / rx`. `n = n16 / 16`. Powers are fixed point with 20 fractional
//! bits: the integer part of `n` by square-and-multiply, the fraction by up to
//! four square roots (`u^(1/2)`, `u^(1/4)`, ...) with an integer square root. No
//! float, no table, no libm, so the same bytes on every platform.

use crate::display_list::{CornerShape, Rect, RoundedRect};

/// Sub-pixel units per pixel: `Au` (1/60 px) times 4.
pub const SUB: i64 = 240;
/// Samples per axis.
const N: i64 = 8;
/// Distance between neighbouring samples' offsets: sample `i` is at `STEP * (2i + 1)`.
const STEP: i64 = SUB / (2 * N);
/// Fractional bits of the fixed-point power.
const FIX: u32 = 20;
const ONE: u64 = 1 << FIX;

/// Round half up, as the rasterizer's `snap` does.
fn snap(au: i64) -> i64 {
    (au + 30).div_euclid(60)
}

fn isqrt(v: u64) -> u64 {
    if v < 2 {
        return v;
    }
    let mut x = 1u64 << (v.ilog2() / 2 + 1);
    loop {
        let y = (x + v / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

fn mul(a: u64, b: u64) -> u64 {
    (a * b + (ONE >> 1)) >> FIX
}

/// `u^(n16 / 16)` for `u` in `0..=ONE`, fixed point.
fn pow16(u: u64, n16: u32) -> u64 {
    if u >= ONE {
        return ONE;
    }
    if u == 0 {
        return 0;
    }
    let mut r = ONE;
    let mut base = u;
    let mut e = n16 >> 4;
    while e > 0 {
        if e & 1 == 1 {
            r = mul(r, base);
        }
        e >>= 1;
        if e > 0 {
            base = mul(base, base);
        }
        if r == 0 {
            return 0;
        }
    }
    let mut root = u;
    for bit in (0..4).rev() {
        root = isqrt(root << FIX);
        if (n16 >> bit) & 1 == 1 {
            r = mul(r, root);
        }
    }
    r
}

/// One corner in shape-local sub-units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Corner {
    rx: i64,
    ry: i64,
    shape: CornerShape,
}

impl Corner {
    fn active(&self) -> bool {
        self.rx > 0 && self.ry > 0 && !self.shape.is_square()
    }

    /// Is the point at distance `(x, y)` from the outer corner inside the shape?
    fn inside(&self, x: i64, y: i64) -> bool {
        if x >= self.rx || y >= self.ry {
            return true;
        }
        if x < 0 || y < 0 {
            return false;
        }
        match self.shape {
            CornerShape::Convex(n) => {
                if n == CornerShape::INFINITE {
                    return true;
                }
                let u = ((self.rx - x) << FIX) as u64 / self.rx as u64;
                let v = ((self.ry - y) << FIX) as u64 / self.ry as u64;
                pow16(u, u32::from(n)) + pow16(v, u32::from(n)) <= ONE
            }
            CornerShape::Concave(n) => {
                if n == CornerShape::INFINITE {
                    return false;
                }
                let u = (x << FIX) as u64 / self.rx as u64;
                let v = (y << FIX) as u64 / self.ry as u64;
                pow16(u, u32::from(n)) + pow16(v, u32::from(n)) >= ONE
            }
        }
    }
}

/// A rounded rectangle on the pixel grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    /// Half-open pixel rectangle.
    pub x0: i64,
    pub y0: i64,
    pub x1: i64,
    pub y1: i64,
    /// Top-left, top-right, bottom-right, bottom-left.
    c: [Corner; 4],
}

/// What is known about a pixel without sampling it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hint {
    Full,
    Empty,
    Partial,
}

impl Shape {
    /// The shape of an item: rectangle snapped, radii in sub-units, clamped
    /// against the snapped size.
    pub fn from_item(rr: &RoundedRect) -> Shape {
        let r: &Rect = &rr.rect;
        let (x, y) = (i64::from(r.x.0), i64::from(r.y.0));
        let x0 = snap(x);
        let y0 = snap(y);
        let x1 = snap(x + i64::from(r.width.0));
        let y1 = snap(y + i64::from(r.height.0));
        let c = rr.corners.map(|k| Corner {
            rx: i64::from(k.rx.0.max(0)) * (SUB / 60),
            ry: i64::from(k.ry.0.max(0)) * (SUB / 60),
            shape: k.shape,
        });
        Shape::from_parts(x0, y0, x1, y1, c)
    }

    fn from_parts(x0: i64, y0: i64, x1: i64, y1: i64, mut c: [Corner; 4]) -> Shape {
        let w = (x1 - x0).max(0) * SUB;
        let h = (y1 - y0).max(0) * SUB;
        let sums = [
            (w, c[0].rx + c[1].rx),
            (w, c[3].rx + c[2].rx),
            (h, c[0].ry + c[3].ry),
            (h, c[1].ry + c[2].ry),
        ];
        let mut best: Option<(i64, i64)> = None;
        for (l, s) in sums {
            if s > l && best.is_none_or(|(bl, bs)| l * bs < bl * s) {
                best = Some((l, s));
            }
        }
        if let Some((l, s)) = best {
            for k in &mut c {
                k.rx = k.rx * l / s;
                k.ry = k.ry * l / s;
            }
        }
        Shape { x0, y0, x1, y1, c }
    }

    /// The border's inner edge: the pixel rectangle `(x0, y0, x1, y1)` (inside
    /// this one) with every radius reduced by the distance between the two edges
    /// on the sides it touches. `None` if the inner rectangle is empty.
    pub fn inner(&self, x0: i64, y0: i64, x1: i64, y1: i64) -> Option<Shape> {
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let (l, t) = ((x0 - self.x0) * SUB, (y0 - self.y0) * SUB);
        let (r, b) = ((self.x1 - x1) * SUB, (self.y1 - y1) * SUB);
        let sub = |c: Corner, dx: i64, dy: i64| Corner {
            rx: (c.rx - dx).max(0),
            ry: (c.ry - dy).max(0),
            shape: c.shape,
        };
        let c = [
            sub(self.c[0], l, t),
            sub(self.c[1], r, t),
            sub(self.c[2], r, b),
            sub(self.c[3], l, b),
        ];
        Some(Shape::from_parts(x0, y0, x1, y1, c))
    }

    /// Does any corner cut anything?
    pub fn is_rounded(&self) -> bool {
        self.c.iter().any(Corner::active)
    }

    /// Distance of sub-unit point `(sx, sy)` (relative to the origin) from the
    /// outer corner `i`.
    fn local(&self, i: usize, sx: i64, sy: i64) -> (i64, i64) {
        let w = (self.x1 - self.x0) * SUB;
        let h = (self.y1 - self.y0) * SUB;
        match i {
            0 => (sx, sy),
            1 => (w - sx, sy),
            2 => (w - sx, h - sy),
            _ => (sx, h - sy),
        }
    }

    fn inside_sample(&self, sx: i64, sy: i64) -> bool {
        let w = (self.x1 - self.x0) * SUB;
        let h = (self.y1 - self.y0) * SUB;
        if sx < 0 || sy < 0 || sx >= w || sy >= h {
            return false;
        }
        for (i, k) in self.c.iter().enumerate() {
            if k.active() {
                let (x, y) = self.local(i, sx, sy);
                if x < k.rx && y < k.ry {
                    return k.inside(x, y);
                }
            }
        }
        true
    }

    /// Pixel extent of corner `i`'s box, rounded up.
    fn box_px(&self, i: usize) -> (i64, i64) {
        let k = &self.c[i];
        if k.active() {
            ((k.rx + SUB - 1) / SUB, (k.ry + SUB - 1) / SUB)
        } else {
            (0, 0)
        }
    }

    /// What can be said of pixel `(px, py)` without sampling.
    pub fn hint(&self, px: i64, py: i64) -> Hint {
        if px < self.x0 || px >= self.x1 || py < self.y0 || py >= self.y1 {
            return Hint::Empty;
        }
        let (rx, ry) = (px - self.x0, py - self.y0);
        let (w, h) = (self.x1 - self.x0, self.y1 - self.y0);
        let mut full = true;
        for i in 0..4 {
            let (bw, bh) = self.box_px(i);
            if bw == 0 {
                continue;
            }
            // Pixel position within this corner's frame, in pixels.
            let (cx, cy) = match i {
                0 => (rx, ry),
                1 => (w - 1 - rx, ry),
                2 => (w - 1 - rx, h - 1 - ry),
                _ => (rx, h - 1 - ry),
            };
            if cx >= bw || cy >= bh {
                continue;
            }
            let k = &self.c[i];
            // The pixel's near and far distance from the outer corner.
            let (nx, ny) = (cx * SUB, cy * SUB);
            let (fx, fy) = (nx + SUB, ny + SUB);
            if k.inside(nx, ny) {
                continue;
            }
            if !k.inside(fx, fy) {
                return Hint::Empty;
            }
            full = false;
        }
        if full { Hint::Full } else { Hint::Partial }
    }

    /// Coverage of pixel `(px, py)`, `0..=255`.
    pub fn coverage(&self, px: i64, py: i64) -> u32 {
        self.ring_coverage(None, px, py)
    }

    /// Coverage of this shape and not `inner` (a border ring); `inner = None`
    /// is the shape itself.
    pub fn ring_coverage(&self, inner: Option<&Shape>, px: i64, py: i64) -> u32 {
        let outer_hint = self.hint(px, py);
        if outer_hint == Hint::Empty {
            return 0;
        }
        let inner_hint = inner.map_or(Hint::Empty, |s| s.hint(px, py));
        match (outer_hint, inner_hint) {
            (_, Hint::Full) => 0,
            (Hint::Full, Hint::Empty) => 255,
            _ => {
                let (ox, oy) = ((px - self.x0) * SUB, (py - self.y0) * SUB);
                let mut hits = 0u32;
                for j in 0..N {
                    let sy = oy + STEP * (2 * j + 1);
                    for i in 0..N {
                        let sx = ox + STEP * (2 * i + 1);
                        if self.inside_sample(sx, sy) {
                            let in_inner = inner.is_some_and(|s| {
                                s.inside_sample(
                                    sx - (s.x0 - self.x0) * SUB,
                                    sy - (s.y0 - self.y0) * SUB,
                                )
                            });
                            hits += u32::from(!in_inner);
                        }
                    }
                }
                to_cov(hits)
            }
        }
    }

    /// Pixel columns `[a, b)` of row `py` that no corner box touches, within the
    /// rectangle. Everything in the span is fully covered.
    pub fn solid_span(&self, py: i64) -> (i64, i64) {
        let ry = py - self.y0;
        let h = self.y1 - self.y0;
        let mut a = self.x0;
        let mut b = self.x1;
        for i in 0..4 {
            let (bw, bh) = self.box_px(i);
            let row = if i < 2 { ry } else { h - 1 - ry };
            if bw > 0 && row < bh {
                if i == 0 || i == 3 {
                    a = a.max(self.x0 + bw);
                } else {
                    b = b.min(self.x1 - bw);
                }
            }
        }
        (a, b.max(a))
    }
}

fn to_cov(hits: u32) -> u32 {
    (hits * 255 + (N * N) as u32 / 2) / (N * N) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display_list::{Corner as C, Rect};
    use px_layout::geom::px;

    fn rr(w: i32, h: i32, r: i32, shape: CornerShape) -> RoundedRect {
        let c = C {
            rx: px(r),
            ry: px(r),
            shape,
        };
        RoundedRect::new(
            Rect {
                x: px(0),
                y: px(0),
                width: px(w),
                height: px(h),
            },
            [c; 4],
        )
    }

    fn total(s: &Shape) -> u32 {
        let mut t = 0;
        for y in s.y0..s.y1 {
            for x in s.x0..s.x1 {
                t += s.coverage(x, y);
            }
        }
        t
    }

    #[test]
    fn pow_is_exact_on_the_easy_cases() {
        assert_eq!(pow16(ONE / 2, 16), ONE / 2);
        assert_eq!(pow16(ONE / 2, 32), ONE / 4);
        assert_eq!(pow16(ONE / 2, 64), ONE / 16);
        assert_eq!(pow16(ONE, 999), ONE);
        assert_eq!(pow16(0, 16), 0);
        // 0.25^(1/2) = 0.5, 0.25^(3/2) = 0.125
        assert_eq!(pow16(ONE / 4, 8), ONE / 2);
        assert_eq!(pow16(ONE / 4, 24), ONE / 8);
    }

    #[test]
    fn isqrt_is_the_floor_root() {
        for v in [0u64, 1, 2, 3, 4, 8, 9, 15, 16, 17, 1 << 40, (1 << 40) - 1] {
            let r = isqrt(v);
            assert!(r * r <= v && (r + 1) * (r + 1) > v, "{v}");
        }
    }

    #[test]
    fn an_unrounded_shape_covers_exactly_its_rectangle() {
        let s = Shape::from_item(&rr(5, 3, 0, CornerShape::ROUND));
        assert_eq!(total(&s), 5 * 3 * 255);
        let s = Shape::from_item(&rr(5, 3, 9, CornerShape::SQUARE));
        assert!(!s.is_rounded());
        assert_eq!(total(&s), 5 * 3 * 255);
    }

    #[test]
    fn a_circle_has_about_pi_r_squared_coverage() {
        let s = Shape::from_item(&rr(20, 20, 10, CornerShape::ROUND));
        let area = total(&s) * 100 / 255;
        // pi * 100 = 314.159 pixels, in hundredths.
        assert!(area.abs_diff(31416) < 150, "{area}");
    }

    #[test]
    fn a_squircle_covers_more_than_a_circle_and_less_than_a_square() {
        let c = total(&Shape::from_item(&rr(20, 20, 10, CornerShape::ROUND)));
        let q = total(&Shape::from_item(&rr(20, 20, 10, CornerShape::SQUIRCLE)));
        let b = total(&Shape::from_item(&rr(20, 20, 10, CornerShape::BEVEL)));
        let sq = total(&Shape::from_item(&rr(20, 20, 10, CornerShape::SQUARE)));
        assert!(b < c && c < q && q < sq, "{b} {c} {q} {sq}");
        // A bevel at full radius is a diamond: half the square.
        assert!((b / 255).abs_diff(200) <= 3, "{}", b / 255);
    }

    #[test]
    fn concave_corners_cut_away_what_convex_ones_keep() {
        let scoop = total(&Shape::from_item(&rr(20, 20, 5, CornerShape::SCOOP)));
        let round = total(&Shape::from_item(&rr(20, 20, 5, CornerShape::ROUND)));
        let notch = total(&Shape::from_item(&rr(20, 20, 5, CornerShape::NOTCH)));
        assert!(notch < scoop && scoop < round);
        assert_eq!(notch, (400 - 4 * 25) * 255);
        // Scoop and round cut complementary parts of each corner box.
        let both = scoop + round;
        let want = (2 * 400 - 4 * 25) * 255;
        assert!(both.abs_diff(want) < 4 * 255, "{both} {want}");
    }

    #[test]
    fn symmetry_in_both_axes() {
        let s = Shape::from_item(&rr(17, 13, 6, CornerShape::SQUIRCLE));
        for y in 0..13 {
            for x in 0..17 {
                let c = s.coverage(x, y);
                assert_eq!(c, s.coverage(16 - x, y), "{x},{y}");
                assert_eq!(c, s.coverage(x, 12 - y), "{x},{y}");
            }
        }
    }

    #[test]
    fn hints_agree_with_sampling() {
        for shape in [
            CornerShape::ROUND,
            CornerShape::SQUIRCLE,
            CornerShape::SCOOP,
            CornerShape::Convex(23),
        ] {
            let s = Shape::from_item(&rr(30, 24, 11, shape));
            for y in -1..25 {
                for x in -1..31 {
                    let h = s.hint(x, y);
                    let (ox, oy) = ((x - s.x0) * SUB, (y - s.y0) * SUB);
                    let mut hits = 0;
                    for j in 0..N {
                        for i in 0..N {
                            hits += u32::from(
                                s.inside_sample(ox + STEP * (2 * i + 1), oy + STEP * (2 * j + 1)),
                            );
                        }
                    }
                    match h {
                        Hint::Full => assert_eq!(hits, 64, "{x},{y}"),
                        Hint::Empty => assert_eq!(hits, 0, "{x},{y}"),
                        Hint::Partial => {}
                    }
                }
            }
        }
    }

    #[test]
    fn solid_span_is_solid() {
        let s = Shape::from_item(&rr(30, 24, 11, CornerShape::SQUIRCLE));
        for y in 0..24 {
            let (a, b) = s.solid_span(y);
            for x in a..b {
                assert_eq!(s.coverage(x, y), 255, "{x},{y}");
            }
        }
    }

    #[test]
    fn overlapping_radii_scale_down() {
        // 10 wide, radii 8 + 8: f = 10/16.
        let s = Shape::from_item(&rr(10, 40, 8, CornerShape::ROUND));
        assert_eq!(s.c[0].rx, 5 * SUB);
        assert_eq!(s.c[0].ry, 5 * SUB);
    }
}
