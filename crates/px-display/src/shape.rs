//! Window shape: every top-level OS window is a squircle (`docs/brand/ui-direction.md`,
//! user directive), with anti-aliased edges where the platform allows.
//!
//! This module is platform-neutral and safe. It turns a [`WindowShape`] and a
//! size into an 8-bit alpha [`Mask`] using `px_ui::shape::coverage`, the very
//! arithmetic the chrome uses for its own squircles (4 x 4 samples per pixel,
//! integer geometry, exponent 37/8 = 4.625 from correctly rounded operations
//! only). The same mask serves every backend:
//!
//! - X11 with a compositor and Win32: per-pixel alpha (premultiplied), so the
//!   corners are anti-aliased.
//! - X11 without a compositor: the mask thresholded to rectangles for the SHAPE
//!   extension (aliased: the documented fallback).
//! - Both: hit testing. A pixel whose alpha is below [`HIT_THRESHOLD`] is
//!   outside the window; pointer input there is dropped.

use px_ui::geom::Rect;
use px_ui::shape::{Squircle, coverage};

/// Alpha at or above which a pixel counts as inside the window (for the SHAPE
/// bitmask and for hit testing). Half coverage.
pub const HIT_THRESHOLD: u8 = 128;

/// What a top-level window is for. Decides how the window manager and the OS are
/// asked to treat it, and the default corner radius.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WindowKind {
    /// The browser window.
    #[default]
    Main,
    /// A modal or auxiliary dialog (framed by the chrome, not the OS).
    Dialog,
    /// A popup (a dropdown, a bubble): unmanaged, does not take focus.
    Popup,
    /// A menu: unmanaged, does not take focus.
    Menu,
    /// A tooltip: unmanaged, does not take focus, ignores the pointer.
    Tooltip,
}

impl WindowKind {
    /// Popups, menus and tooltips are positioned by the application and bypass
    /// the window manager.
    pub fn is_transient(self) -> bool {
        matches!(
            self,
            WindowKind::Popup | WindowKind::Menu | WindowKind::Tooltip
        )
    }
}

/// A squircle outline for a window: a rectangle with superellipse corners
/// (exponent 4.625, as `px-ui` draws them) of this radius in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WindowShape {
    pub radius: u32,
}

impl WindowShape {
    pub const fn squircle(radius: u32) -> WindowShape {
        WindowShape { radius }
    }

    /// The radius for each kind of window: the chrome's window radius for the
    /// main window and dialogs, smaller for the transient kinds.
    pub fn for_kind(kind: WindowKind) -> WindowShape {
        let main = u32::try_from(px_ui::layout::WINDOW_RADIUS).unwrap_or(22);
        WindowShape::squircle(match kind {
            WindowKind::Main | WindowKind::Dialog => main,
            WindowKind::Popup => 16,
            WindowKind::Menu => 14,
            WindowKind::Tooltip => 9,
        })
    }

    /// The shape at a device scale of `dpi / 96`.
    pub fn scaled(self, dpi: u32) -> WindowShape {
        let r = (u64::from(self.radius) * u64::from(dpi.max(1)) + 48) / 96;
        WindowShape::squircle(u32::try_from(r).unwrap_or(u32::MAX))
    }

    /// The alpha mask of a `w` x `h` window of this shape. The radius is capped at
    /// half the shorter side (as `Squircle::new` does).
    pub fn mask(self, w: u32, h: u32) -> Mask {
        let (wi, hi) = (
            i32::try_from(w).unwrap_or(i32::MAX),
            i32::try_from(h).unwrap_or(i32::MAX),
        );
        let sq = Squircle::new(Rect::new(0, 0, wi, hi), self.radius.try_into().unwrap_or(0));
        let mut alpha = Vec::with_capacity(w as usize * h as usize);
        for y in 0..hi {
            for x in 0..wi {
                alpha.push(coverage(&sq, x, y));
            }
        }
        Mask { w, h, alpha }
    }
}

/// Per-pixel window coverage, row-major, 0 (outside) to 255 (inside).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    pub w: u32,
    pub h: u32,
    alpha: Vec<u8>,
}

impl Mask {
    /// A mask that covers everything (no shape).
    pub fn full(w: u32, h: u32) -> Mask {
        Mask {
            w,
            h,
            alpha: vec![255; w as usize * h as usize],
        }
    }

    pub fn alpha(&self) -> &[u8] {
        &self.alpha
    }

    /// Coverage of pixel `(x, y)`; 0 outside the window rectangle.
    pub fn alpha_at(&self, x: i32, y: i32) -> u8 {
        let (Ok(x), Ok(y)) = (u32::try_from(x), u32::try_from(y)) else {
            return 0;
        };
        if x >= self.w || y >= self.h {
            return 0;
        }
        self.alpha
            .get(y as usize * self.w as usize + x as usize)
            .copied()
            .unwrap_or(0)
    }

    /// Is `(x, y)` inside the window? Transparent corners are outside.
    pub fn hit(&self, x: i32, y: i32) -> bool {
        self.alpha_at(x, y) >= HIT_THRESHOLD
    }

    /// The inside pixels as non-overlapping rectangles in YX-banded order, for
    /// the X SHAPE extension. Identical single-run rows are merged vertically,
    /// so a window costs about one rectangle per corner row.
    pub fn rects(&self) -> Vec<Rect> {
        let mut out: Vec<Rect> = Vec::new();
        // Index in `out` of the last band when it is one rectangle that ends on
        // the previous row (so it may grow).
        let mut open: Option<usize> = None;
        for y in 0..self.h as usize {
            let row = self
                .alpha
                .get(y * self.w as usize..(y + 1) * self.w as usize)
                .unwrap_or(&[]);
            let mut runs: Vec<(i32, i32)> = Vec::new();
            let mut start: Option<usize> = None;
            for (x, &a) in row.iter().enumerate() {
                match (a >= HIT_THRESHOLD, start) {
                    (true, None) => start = Some(x),
                    (false, Some(s)) => {
                        runs.push((s as i32, (x - s) as i32));
                        start = None;
                    }
                    _ => {}
                }
            }
            if let Some(s) = start {
                runs.push((s as i32, (row.len() - s) as i32));
            }
            let y = y as i32;
            if let [(x, w)] = runs.as_slice()
                && let (x, w) = (*x, *w)
                && let Some(i) = open
                && let Some(last) = out.get_mut(i)
                && last.x == x
                && last.w == w
                && last.bottom() == y
            {
                last.h += 1;
                continue;
            }
            let single = runs.len() == 1;
            let first = out.len();
            for (x, w) in runs {
                out.push(Rect::new(x, y, w, 1));
            }
            open = if single { Some(first) } else { None };
        }
        out
    }

    /// The whole window bitmap for `UpdateLayeredWindow`: a `fw` x `fh` frame of
    /// top-down B,G,R,A pixels, resampled (nearest) to this mask's size and
    /// premultiplied under it. Returns `w * h * 4` bytes; a frame whose length
    /// does not match, or an empty one, gives a fully transparent bitmap.
    pub fn compose_bgra(&self, frame: &[u8], fw: u32, fh: u32) -> Vec<u8> {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut out = vec![0u8; w * h * 4];
        if fw == 0 || fh == 0 || frame.len() != fw as usize * fh as usize * 4 {
            return out;
        }
        for y in 0..h {
            let sy = y * fh as usize / h.max(1);
            for x in 0..w {
                let sx = x * fw as usize / w.max(1);
                let s = (sy * fw as usize + sx) * 4;
                let (Some(&[b, g, r, sa]), Some(dst)) = (
                    frame
                        .get(s..s + 4)
                        .and_then(|p| <&[u8; 4]>::try_from(p).ok()),
                    out.get_mut((y * w + x) * 4..(y * w + x) * 4 + 4),
                ) else {
                    continue;
                };
                let m = u32::from(self.alpha_at(x as i32, y as i32));
                let a = (m * u32::from(sa) + 127) / 255;
                let p = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
                dst.copy_from_slice(&[p(b), p(g), p(r), a as u8]);
            }
        }
        out
    }

    /// Convert one RGBA row of a frame into premultiplied B,G,R,A under this
    /// mask (row `y`, starting at column `x0`), the layout compositors and
    /// `UpdateLayeredWindow` expect. Rounding is `(v * a + 127) / 255`.
    pub fn premultiply_row(&self, y: i32, x0: i32, rgba: &[[u8; 4]], out: &mut Vec<u8>) {
        for (i, &[r, g, b, sa]) in rgba.iter().enumerate() {
            let m = u32::from(self.alpha_at(x0 + i as i32, y));
            let a = (m * u32::from(sa) + 127) / 255;
            let p = |c: u8| ((u32::from(c) * a + 127) / 255) as u8;
            out.extend_from_slice(&[p(b), p(g), p(r), a as u8]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape() -> WindowShape {
        WindowShape::squircle(22)
    }

    #[test]
    fn mask_is_deterministic_and_matches_the_chrome_arithmetic() {
        let a = shape().mask(120, 80);
        let b = shape().mask(120, 80);
        assert_eq!(a, b);
        let sq = Squircle::new(Rect::new(0, 0, 120, 80), 22);
        for y in 0..80 {
            for x in 0..120 {
                assert_eq!(a.alpha_at(x, y), coverage(&sq, x, y), "({x},{y})");
            }
        }
    }

    #[test]
    fn mask_has_pinned_corner_values() {
        let m = shape().mask(120, 80);
        // Pinned: any change to the arithmetic or the sampling is a visible
        // change to every window corner and must be deliberate.
        assert_eq!(m.alpha_at(0, 0), 0);
        assert_eq!(m.alpha_at(60, 40), 255);
        assert_eq!(m.alpha_at(60, 0), 255);
        assert_eq!(m.alpha_at(0, 40), 255);
        let sum: u32 = m.alpha().iter().map(|&a| u32::from(a)).sum();
        assert_eq!(sum, PINNED_SUM, "mask sum changed");
    }

    #[test]
    fn mask_is_symmetric_in_both_axes() {
        let m = shape().mask(97, 61);
        for y in 0..61 {
            for x in 0..97 {
                assert_eq!(m.alpha_at(x, y), m.alpha_at(96 - x, y));
                assert_eq!(m.alpha_at(x, y), m.alpha_at(x, 60 - y));
            }
        }
    }

    #[test]
    fn corners_are_transparent_and_edges_are_anti_aliased() {
        let m = shape().mask(200, 100);
        assert_eq!(m.alpha_at(0, 0), 0);
        assert!(!m.hit(0, 0));
        assert!(!m.hit(199, 99));
        assert!(m.hit(100, 50));
        assert!(m.hit(22, 0) && m.hit(0, 22));
        // Some pixel on the curve is neither 0 nor 255.
        assert!(m.alpha().iter().any(|&a| a != 0 && a != 255));
        // Outside the rectangle is never hit.
        assert!(!m.hit(-1, 5) && !m.hit(5, 100) && !m.hit(200, 5));
    }

    #[test]
    fn alpha_grows_monotonically_along_the_diagonal() {
        let m = shape().mask(100, 100);
        let mut last = 0;
        for i in 0..22 {
            let a = m.alpha_at(i, i);
            assert!(a >= last);
            last = a;
        }
        assert_eq!(last, 255);
    }

    #[test]
    fn radius_is_capped_and_zero_is_a_rectangle() {
        let m = WindowShape::squircle(1000).mask(40, 20);
        assert!(m.alpha_at(0, 0) == 0 && m.alpha_at(20, 10) == 255);
        let r = WindowShape::squircle(0).mask(10, 10);
        assert!(r.alpha().iter().all(|&a| a == 255));
        assert_eq!(WindowShape::squircle(0).mask(0, 0).alpha().len(), 0);
    }

    #[test]
    fn rects_cover_exactly_the_inside_pixels() {
        let m = shape().mask(150, 90);
        let rects = m.rects();
        assert!(rects.len() < 120, "{} rects", rects.len());
        let mut covered = vec![false; 150 * 90];
        for r in &rects {
            for y in r.y..r.bottom() {
                for x in r.x..r.right() {
                    let i = (y * 150 + x) as usize;
                    assert!(!covered[i], "overlap at ({x},{y})");
                    covered[i] = true;
                }
            }
        }
        for y in 0..90 {
            for x in 0..150 {
                assert_eq!(covered[(y * 150 + x) as usize], m.hit(x, y), "({x},{y})");
            }
        }
        // YX-banded: y never decreases.
        assert!(rects.windows(2).all(|w| w[0].y <= w[1].y));
    }

    #[test]
    fn premultiply_is_exact() {
        let m = Mask::full(2, 1);
        let mut out = Vec::new();
        m.premultiply_row(0, 0, &[[200, 100, 50, 255], [255, 255, 255, 128]], &mut out);
        assert_eq!(out, [50, 100, 200, 255, 128, 128, 128, 128]);
        // Outside the mask: fully transparent, colour zero.
        let mut out = Vec::new();
        shape()
            .mask(100, 100)
            .premultiply_row(0, 0, &[[9, 9, 9, 255]], &mut out);
        assert_eq!(out, [0, 0, 0, 0]);
    }

    #[test]
    fn compose_matches_row_premultiply_and_resamples() {
        let m = shape().mask(60, 40);
        // A 60x40 frame composes to the same bytes as the per-row path.
        let mut bgra = Vec::new();
        let mut rgba_rows: Vec<Vec<[u8; 4]>> = Vec::new();
        for y in 0..40u32 {
            let mut row = Vec::new();
            for x in 0..60u32 {
                let (r, g, b) = ((x * 4) as u8, (y * 6) as u8, 200u8);
                bgra.extend_from_slice(&[b, g, r, 255]);
                row.push([r, g, b, 255]);
            }
            rgba_rows.push(row);
        }
        let mut want = Vec::new();
        for (y, row) in rgba_rows.iter().enumerate() {
            m.premultiply_row(y as i32, 0, row, &mut want);
        }
        assert_eq!(m.compose_bgra(&bgra, 60, 40), want);
        // A half-size frame is stretched; corners stay transparent.
        let small = vec![255u8; 30 * 20 * 4];
        let big = m.compose_bgra(&small, 30, 20);
        assert_eq!(big.len(), 60 * 40 * 4);
        assert_eq!(&big[..4], &[0, 0, 0, 0]);
        let mid = (20 * 60 + 30) * 4;
        assert_eq!(&big[mid..mid + 4], &[255, 255, 255, 255]);
        // A mismatched frame is blank, not a panic.
        assert!(m.compose_bgra(&[1, 2, 3], 60, 40).iter().all(|&b| b == 0));
    }

    #[test]
    fn kinds_have_radii_and_scale() {
        assert_eq!(WindowShape::for_kind(WindowKind::Main).radius, 22);
        assert!(WindowShape::for_kind(WindowKind::Tooltip).radius < 22);
        assert_eq!(shape().scaled(192).radius, 44);
        assert!(WindowKind::Menu.is_transient() && !WindowKind::Dialog.is_transient());
    }

    const PINNED_SUM: u32 = 2_420_204;
}
