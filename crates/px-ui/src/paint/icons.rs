//! The chrome's icons: vector strokes, no font and no bitmap.
//!
//! Each icon is a few points in 1/8 logical pixels around the centre of its
//! button, stroked 2 px wide with round caps. Axis-aligned strokes sit on whole
//! pixel boundaries (button centres are whole pixels), so they land on exactly
//! two device pixels at 1x and stay crisp at 2x and 3x; diagonals and arcs are
//! anti-aliased by the shape module's 4 x 4 coverage. Nothing here calls trig:
//! the arc comes from an exact table, so the bytes match on every platform.

use px_paint::Color;

use super::Painter;
use crate::geom::Rect;
use crate::shape::SUB;

const U: i32 = SUB;
/// Stroke half-width, 1/8 px: 2 px lines.
const HALF: i32 = 8;

/// `(cos, sin) * 1000` for 0, 15, ..., 75 degrees.
const FIRST_QUADRANT: [(i32, i32); 6] = [
    (1000, 0),
    (966, 259),
    (866, 500),
    (707, 707),
    (500, 866),
    (259, 966),
];

/// `(cos, sin) * 1000` of `k * 15` degrees, `k` in `0..24`, y down.
fn unit(k: usize) -> (i32, i32) {
    let (mut c, mut s) = FIRST_QUADRANT[k % 6];
    for _ in 0..(k / 6) % 4 {
        (c, s) = (-s, c);
    }
    (c, s)
}

fn centre(r: Rect) -> (i32, i32) {
    let m = r.center();
    (m.x * SUB, m.y * SUB)
}

pub(super) fn back(p: &mut Painter, r: Rect, col: Color) {
    let (cx, cy) = centre(r);
    p.path(
        cx,
        cy,
        &[(3 * U, -6 * U), (-3 * U, 0), (3 * U, 6 * U)],
        HALF,
        col,
    );
}

pub(super) fn forward(p: &mut Painter, r: Rect, col: Color) {
    let (cx, cy) = centre(r);
    p.path(
        cx,
        cy,
        &[(-3 * U, -6 * U), (3 * U, 0), (-3 * U, 6 * U)],
        HALF,
        col,
    );
}

/// An X: the same shape as close, larger, because stop is cancel.
pub(super) fn stop(p: &mut Painter, r: Rect, col: Color) {
    cross(p, r, 5 * U, HALF, col);
}

pub(super) fn close(p: &mut Painter, r: Rect, col: Color) {
    cross(p, r, 4 * U, 7, col);
}

fn cross(p: &mut Painter, r: Rect, d: i32, half: i32, col: Color) {
    let (cx, cy) = centre(r);
    p.path(cx, cy, &[(-d, -d), (d, d)], half, col);
    p.path(cx, cy, &[(-d, d), (d, -d)], half, col);
}

pub(super) fn new_tab(p: &mut Painter, r: Rect, col: Color) {
    let (cx, cy) = centre(r);
    p.path(cx, cy, &[(-5 * U, 0), (5 * U, 0)], HALF, col);
    p.path(cx, cy, &[(0, -5 * U), (0, 5 * U)], HALF, col);
}

/// Three dots, vertical.
pub(super) fn menu(p: &mut Painter, r: Rect, col: Color) {
    let (cx, cy) = centre(r);
    for dy in [-7, 0, 7] {
        p.circle(cx, cy + dy * U, 14, col);
    }
}

/// Three quarters of a 6.5 px circle in 15 degree steps, then an arrowhead at
/// its start, upper right.
pub(super) fn reload(p: &mut Painter, r: Rect, col: Color) {
    let (cx, cy) = centre(r);
    let rad = 52; // 6.5 px
    let pts: Vec<(i32, i32)> = (2..=22)
        .rev()
        .map(|k| {
            let (c, s) = unit(k);
            (rad * c / 1000, rad * s / 1000)
        })
        .collect();
    p.path(cx, cy, &pts, HALF, col);
    if let Some(&(hx, hy)) = pts.first() {
        p.path(
            cx,
            cy,
            &[(hx + U, hy - 5 * U), (hx, hy), (hx - 5 * U, hy - U)],
            HALF,
            col,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::unit;

    #[test]
    fn the_table_is_a_unit_circle_with_the_right_quadrants() {
        assert_eq!(unit(0), (1000, 0));
        assert_eq!(unit(6), (0, 1000));
        assert_eq!(unit(12), (-1000, 0));
        assert_eq!(unit(18), (0, -1000));
        assert_eq!(unit(3), (707, 707));
        assert_eq!(unit(22), (866, -500));
        for k in 0..24 {
            let (c, s) = unit(k);
            let r2 = c * c + s * s;
            assert!((990_000..=1_010_000).contains(&r2), "k={k} {r2}");
        }
    }
}
