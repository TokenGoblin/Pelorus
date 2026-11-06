//! Anti-aliased polygon filling, integer only.
//!
//! Edges are kept in device space at 1/256 pixel. Each pixel row is sampled at
//! [`SUBS`] evenly spaced scanlines; on each, the exact horizontal extent of
//! every filled span is accumulated (to 1/256 pixel), so coverage has 16
//! vertical and 256 horizontal levels. Overlapping and self-intersecting
//! shapes are handled by a real winding count (non-zero or even-odd), not by
//! signed-area cancellation.

use super::geom::{Mat, P};
use crate::DecodeError;

/// Sample scanlines per pixel row.
pub const SUBS: i64 = 16;
const SUB_STEP: i64 = 256 / SUBS;
/// Device coordinates are clamped to +-2^20 pixels.
const DEV_MAX: i64 = 1 << 28;

#[derive(Clone, Copy, Debug)]
struct Edge {
    x0: i64,
    y0: i64,
    x1: i64,
    y1: i64,
    dir: i32,
}

/// Edges of the polygons added so far (each polygon implicitly closed).
#[derive(Debug)]
pub struct Edges {
    list: Vec<Edge>,
    max: usize,
}

fn dev(v: super::fixed::Q) -> i64 {
    (v.0 >> 8).clamp(-DEV_MAX, DEV_MAX)
}

impl Edges {
    pub fn new(max: usize) -> Edges {
        Edges {
            list: Vec::new(),
            max,
        }
    }

    /// Add a polygon given in user space, transformed by `m`. When `orient` is
    /// set the polygon is wound positively in user space first, so that a set
    /// of pieces filled non-zero is their union whatever the transform.
    pub fn add_poly(&mut self, pts: &[P], m: &Mat, orient: bool) -> Result<(), DecodeError> {
        if pts.len() < 3 {
            return Ok(());
        }
        let reversed = orient && {
            let mut area: i128 = 0;
            for (i, a) in pts.iter().enumerate() {
                let b = pts.get((i + 1) % pts.len()).unwrap_or(a);
                area += (i128::from(a.x.0) * i128::from(b.y.0))
                    - (i128::from(b.x.0) * i128::from(a.y.0));
            }
            area < 0
        };
        let n = pts.len();
        let at = |i: usize| -> (i64, i64) {
            let k = if reversed { n - 1 - i } else { i };
            let q = m.apply(pts.get(k).copied().unwrap_or_default());
            (dev(q.x), dev(q.y))
        };
        let mut prev = at(0);
        for i in 1..=n {
            let next = at(i % n);
            self.push(prev, next)?;
            prev = next;
        }
        Ok(())
    }

    fn push(&mut self, a: (i64, i64), b: (i64, i64)) -> Result<(), DecodeError> {
        if a.1 == b.1 {
            return Ok(());
        }
        if self.list.len() >= self.max {
            return Err(DecodeError::LimitExceeded("svg edges"));
        }
        let (dir, lo, hi) = if a.1 < b.1 { (1, a, b) } else { (-1, b, a) };
        self.list.push(Edge {
            x0: lo.0,
            y0: lo.1,
            x1: hi.0,
            y1: hi.1,
            dir,
        });
        Ok(())
    }
}

/// Fill `edges` into a `w` by `h` area. `row(y, x0, alpha)` is called for each
/// pixel row that has any coverage, with the 8-bit coverage of pixels
/// `x0..x0 + alpha.len()`. `work` counts edge-sample evaluations and the call
/// fails once it passes `max_work`.
pub fn fill(
    edges: &mut Edges,
    even_odd: bool,
    w: usize,
    h: usize,
    work: &mut u64,
    max_work: u64,
    mut row: impl FnMut(usize, usize, &[u8]),
) -> Result<(), DecodeError> {
    if edges.list.is_empty() || w == 0 || h == 0 {
        return Ok(());
    }
    edges.list.sort_by_key(|e| e.y0);
    let wq = (w as i64) * 256;
    let first_row = (edges.list.first().map_or(0, |e| e.y0) >> 8).max(0);
    let last_row = (edges.list.iter().map(|e| e.y1).max().unwrap_or(0) >> 8).min(h as i64 - 1);
    let mut cov = vec![0i32; w + 2];
    let mut diff = vec![0i32; w + 2];
    let mut alpha: Vec<u8> = Vec::new();
    let mut active: Vec<Edge> = Vec::new();
    let mut crossings: Vec<(i64, i32)> = Vec::new();
    let mut next = 0usize;
    for y in first_row..=last_row {
        let top = y * 256;
        let bottom = top + 256;
        active.retain(|e| e.y1 > top);
        while let Some(e) = edges.list.get(next).filter(|e| e.y0 < bottom) {
            if e.y1 > top {
                active.push(*e);
            }
            next += 1;
        }
        if active.is_empty() {
            continue;
        }
        let (mut minx, mut maxx) = (w + 1, 0usize);
        for k in 0..SUBS {
            let ys = top + (SUB_STEP / 2) + (k * SUB_STEP);
            crossings.clear();
            for e in &active {
                if e.y0 <= ys && ys < e.y1 {
                    let x = e.x0 + (((ys - e.y0) * (e.x1 - e.x0)) / (e.y1 - e.y0));
                    crossings.push((x.clamp(0, wq), e.dir));
                }
            }
            *work += active.len() as u64 + 1;
            if *work > max_work {
                return Err(DecodeError::LimitExceeded("svg work"));
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_unstable_by_key(|c| c.0);
            let mut wind = 0i32;
            let mut xa = 0i64;
            for &(x, dir) in &crossings {
                let was = if even_odd { wind & 1 != 0 } else { wind != 0 };
                wind += dir;
                let now = if even_odd { wind & 1 != 0 } else { wind != 0 };
                if !was && now {
                    xa = x;
                } else if was && !now && x > xa {
                    let (ia, ib) = ((xa >> 8) as usize, (x >> 8) as usize);
                    if ia == ib {
                        add(&mut cov, ia, (x - xa) as i32);
                    } else {
                        add(&mut cov, ia, (256 - (xa & 255)) as i32);
                        add(&mut diff, ia + 1, 256);
                        add(&mut diff, ib, -256);
                        add(&mut cov, ib, (x & 255) as i32);
                    }
                    minx = minx.min(ia);
                    maxx = maxx.max(ib);
                }
            }
        }
        if minx <= maxx {
            let hi = maxx.min(w - 1);
            alpha.clear();
            let mut acc = 0i32;
            for x in 0..=hi {
                acc += diff.get(x).copied().unwrap_or(0);
                if x >= minx {
                    let c = cov.get(x).copied().unwrap_or(0) + acc;
                    let a = ((c.clamp(0, 4096) * 255) + 2048) >> 12;
                    alpha.push(a as u8);
                }
            }
            *work += alpha.len() as u64;
            if let Some(s) = alpha.get(..) {
                row(y as usize, minx, s);
            }
            // Clear exactly what was touched.
            let clear_to = (maxx + 2).min(w + 2);
            if let Some(s) = cov.get_mut(minx..clear_to) {
                s.fill(0);
            }
            if let Some(s) = diff.get_mut(minx..clear_to) {
                s.fill(0);
            }
        }
    }
    Ok(())
}

fn add(v: &mut [i32], i: usize, d: i32) {
    if let Some(x) = v.get_mut(i) {
        *x += d;
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixed::Q;
    use super::super::geom::p;
    use super::*;

    fn square(x0: i64, y0: i64, x1: i64, y1: i64) -> Vec<P> {
        let q = |a: i64, b: i64| p(Q(a * 65536 / 4), Q(b * 65536 / 4));
        vec![q(x0, y0), q(x1, y0), q(x1, y1), q(x0, y1)]
    }

    fn render(edges: &mut Edges, eo: bool, w: usize, h: usize) -> Vec<u8> {
        let mut img = vec![0u8; w * h];
        let mut work = 0;
        fill(edges, eo, w, h, &mut work, u64::MAX, |y, x0, a| {
            for (i, v) in a.iter().enumerate() {
                if let Some(px) = img.get_mut(y * w + x0 + i) {
                    *px = *v;
                }
            }
        })
        .unwrap();
        img
    }

    #[test]
    fn pixel_aligned_square_is_exact() {
        let mut e = Edges::new(100);
        e.add_poly(&square(4, 4, 12, 12), &Mat::IDENTITY, true)
            .unwrap();
        let img = render(&mut e, false, 5, 5);
        // Covers pixels 1..3 on both axes.
        for y in 0..5 {
            for x in 0..5 {
                let want = if (1..3).contains(&x) && (1..3).contains(&y) {
                    255
                } else {
                    0
                };
                assert_eq!(img[y * 5 + x], want, "{x},{y}");
            }
        }
    }

    #[test]
    fn half_pixel_edges_are_half_covered() {
        let mut e = Edges::new(100);
        e.add_poly(&square(2, 0, 6, 8), &Mat::IDENTITY, true)
            .unwrap();
        let img = render(&mut e, false, 3, 2);
        // x in 0.5..1.5: pixel 0 half, pixel 1 half.
        assert_eq!(img[0], 128);
        assert_eq!(img[1], 128);
        assert_eq!(img[2], 0);
    }

    #[test]
    fn winding_rules_differ_on_nested_same_direction_squares() {
        let build = || {
            let mut e = Edges::new(100);
            e.add_poly(&square(0, 0, 16, 16), &Mat::IDENTITY, false)
                .unwrap();
            e.add_poly(&square(4, 4, 12, 12), &Mat::IDENTITY, false)
                .unwrap();
            e
        };
        let nz = render(&mut build(), false, 4, 4);
        let eo = render(&mut build(), true, 4, 4);
        assert_eq!(nz[5], 255);
        assert_eq!(eo[5], 0);
        assert_eq!(eo[0], 255);
    }

    #[test]
    fn huge_and_offscreen_geometry_is_clamped_not_fatal() {
        let mut e = Edges::new(100);
        let big = |v: i64| p(Q(v << 16), Q(v << 16));
        e.add_poly(
            &[
                big(-1_000_000),
                p(Q(1_000_000 << 16), Q(-1_000_000 << 16)),
                big(1_000_000),
            ],
            &Mat::IDENTITY,
            true,
        )
        .unwrap();
        let _ = render(&mut e, false, 8, 8);
        let mut e = Edges::new(1);
        assert!(
            e.add_poly(&square(0, 0, 4, 4), &Mat::IDENTITY, true)
                .is_err()
        );
    }

    #[test]
    fn work_limit_stops_the_fill() {
        let mut e = Edges::new(100);
        e.add_poly(&square(0, 0, 400, 400), &Mat::IDENTITY, true)
            .unwrap();
        let mut work = 0;
        assert!(fill(&mut e, false, 100, 100, &mut work, 50, |_, _, _| {}).is_err());
    }
}
