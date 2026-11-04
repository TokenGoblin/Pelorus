//! Hand-derived coverage tests for the integer glyph rasterizer.
//!
//! Units: a `Scale { size64: 64 * s, units_per_em: u }` maps one font unit to
//! `s / u` pixels. Where a test says "1 unit = 1 px" it uses `size64 = 64 * upem`.

use px_paint::Canvas;
use px_paint::Color;
use px_paint::glyph::{GlyphError, Mask, PathCmd, Pt, Scale, composite_mask, rasterize_glyph};

fn p(x: i32, y: i32) -> Pt {
    Pt { x, y }
}

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(p(x0, y1)),
        PathCmd::LineTo(p(x1, y1)),
        PathCmd::LineTo(p(x1, y0)),
        PathCmd::LineTo(p(x0, y0)),
        PathCmd::Close,
    ]
}

fn unit_px(upem: u32) -> Scale {
    Scale {
        size64: 64 * upem,
        units_per_em: upem,
    }
}

fn rows(m: &Mask) -> Vec<Vec<u8>> {
    m.data
        .chunks(m.width as usize)
        .map(|r| r.to_vec())
        .collect()
}

#[test]
fn ahem_box_at_10px_is_all_255() {
    // Ahem em box (0,-200)-(1000,800) at 10 px: x 0..10, y from -8 to +2.
    let m = rasterize_glyph(
        &rect(0, -200, 1000, 800),
        Scale {
            size64: 640,
            units_per_em: 1000,
        },
        (0, 0),
    )
    .unwrap();
    assert_eq!((m.left, m.top, m.width, m.height), (0, -8, 10, 10));
    assert!(m.data.iter().all(|&c| c == 255));
}

#[test]
fn ahem_box_at_16px_has_exact_fractional_edges() {
    // 16 px: top edge 800*16/1000 = 12.8 px above the baseline, which scales to
    // round(3276.8) = 3277 in 1/256 px; bottom edge 3.2 px -> 819.
    // Row -13 spans y in [-3328, -3072]: covered 3328-3277 = 51 of 256... measured
    // from the top: ink starts at -3277, so covered = -3072 - -3277 = 205/256
    // -> (205*255 + 128) / 256 = 204. The bottom row 3 is covered 819-768 = 51/256
    // -> 51 (51*255/256 = 50.8 -> 51).
    let m = rasterize_glyph(
        &rect(0, -200, 1000, 800),
        Scale {
            size64: 1024,
            units_per_em: 1000,
        },
        (0, 0),
    )
    .unwrap();
    assert_eq!((m.left, m.top, m.width, m.height), (0, -13, 16, 17));
    for (j, row) in rows(&m).iter().enumerate() {
        let want = match j {
            0 => 204,
            16 => 51,
            _ => 255,
        };
        assert!(row.iter().all(|&c| c == want), "row {j}: {row:?}");
    }
}

#[test]
fn half_pixel_offset_gives_128_on_both_side_columns() {
    // 4x4 px box shifted right by 0.5 px (offset 32/64): columns 0 and 4 are half.
    let m = rasterize_glyph(&rect(0, 0, 4, 4), unit_px(4), (32, 0)).unwrap();
    assert_eq!((m.left, m.top, m.width, m.height), (0, -4, 5, 4));
    for row in rows(&m) {
        assert_eq!(row, vec![128, 255, 255, 255, 128]);
    }
}

#[test]
fn right_triangle_by_hand() {
    // Vertices (0,0), (4,0), (0,4) in font units (1 unit = 1 px, y up): in device
    // space the hypotenuse x + |y| = 4 crosses the grid at integers, so each
    // diagonal pixel is exactly half covered (127.5 -> 128).
    let tri = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::LineTo(p(4, 0)),
        PathCmd::LineTo(p(0, 4)),
        PathCmd::Close,
    ];
    let m = rasterize_glyph(&tri, unit_px(4), (0, 0)).unwrap();
    assert_eq!((m.left, m.top, m.width, m.height), (0, -4, 4, 4));
    assert_eq!(
        rows(&m),
        vec![
            vec![128, 0, 0, 0],
            vec![255, 128, 0, 0],
            vec![255, 255, 128, 0],
            vec![255, 255, 255, 128],
        ]
    );
}

#[test]
fn winding_direction_does_not_change_the_result() {
    let a = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::LineTo(p(4, 0)),
        PathCmd::LineTo(p(0, 4)),
        PathCmd::Close,
    ];
    let b = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::LineTo(p(0, 4)),
        PathCmd::LineTo(p(4, 0)),
        PathCmd::Close,
    ];
    assert_eq!(
        rasterize_glyph(&a, unit_px(4), (0, 0)).unwrap(),
        rasterize_glyph(&b, unit_px(4), (0, 0)).unwrap()
    );
}

#[test]
fn overlap_fills_and_opposite_overlap_makes_a_hole() {
    // Two same-direction squares overlapping: the overlap stays 255 (clamped).
    let mut two = rect(0, 0, 3, 3);
    two.extend(rect(1, 1, 4, 4));
    let m = rasterize_glyph(&two, unit_px(4), (0, 0)).unwrap();
    assert!(m.data.iter().all(|&c| c == 255 || c == 0));
    assert_eq!(m.coverage(1, 2), Some(255)); // inside the overlap
    // Outer square plus an opposite-direction inner square: a hole.
    let mut ring = rect(0, 0, 6, 6);
    ring.extend([
        PathCmd::MoveTo(p(2, 2)),
        PathCmd::LineTo(p(4, 2)),
        PathCmd::LineTo(p(4, 4)),
        PathCmd::LineTo(p(2, 4)),
        PathCmd::Close,
    ]);
    let m = rasterize_glyph(&ring, unit_px(6), (0, 0)).unwrap();
    assert_eq!(m.coverage(0, 0), Some(255));
    assert_eq!(m.coverage(2, 2), Some(0));
    assert_eq!(m.coverage(3, 3), Some(0));
    assert_eq!(m.coverage(4, 4), Some(255));
}

/// Independent oracle for the parabola `y = x (8 - x) / 4` over `x in 0..=8`
/// (the quadratic `(0,0) (4,8) (8,0)`): per-pixel area by `S x S` integer
/// point sampling, scaled to 0..=255 with round-half-up. Shares no code with the
/// rasterizer (no subdivision of the curve, no edges): it tests the implicit form.
fn parabola_oracle(cx: i64, row_top_font_y: i64) -> u8 {
    // The pixel is x in [cx, cx+1), font y in (row_top_font_y - 1, row_top_font_y].
    const S: i64 = 128;
    let mut inside = 0i64;
    for u in 0..S {
        let big_x = 2 * S * cx + 2 * u + 1; // x = big_x / (2S)
        for v in 0..S {
            // font y of the sample = (row_top_font_y*2S - (2v+1)) / (2S)
            let big_y = row_top_font_y * 2 * S - (2 * v + 1);
            if big_y >= 0
                && (0..=16 * S).contains(&big_x)
                && 4 * big_y * (2 * S) <= big_x * (16 * S - big_x)
            {
                inside += 1;
            }
        }
    }
    ((inside * 255 + S * S / 2) / (S * S)) as u8
}

fn quad_shape() -> Vec<PathCmd> {
    vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::QuadTo(p(4, 8), p(8, 0)),
        PathCmd::Close,
    ]
}

#[test]
fn quadratic_matches_the_independent_oracle_within_10() {
    // Flattening bound is 8/256 px of boundary error, so a pixel's area is off by
    // at most 1/32 -> 8 of 255, plus the oracle's own sampling error (< 2).
    const TOL: i32 = 10;
    let m = rasterize_glyph(&quad_shape(), unit_px(8), (0, 0)).unwrap();
    // Control points reach y = 8, so the mask is 8 x 8 rows high.
    assert_eq!((m.left, m.top, m.width, m.height), (0, -8, 8, 8));
    let mut worst = 0;
    for my in 0..m.height {
        // Mask row my covers device y in [top+my, top+my+1) = font y up to -(top+my).
        let top_font_y = -(i64::from(m.top) + i64::from(my));
        for mx in 0..m.width {
            let got = i32::from(m.coverage(mx, my).unwrap());
            let want = i32::from(parabola_oracle(i64::from(mx), top_font_y));
            worst = worst.max((got - want).abs());
            assert!(
                (got - want).abs() <= TOL,
                "pixel ({mx},{my}): got {got}, oracle {want}"
            );
        }
    }
    // The curve rises to y = 4, so rows above that are empty.
    for my in 0..4 {
        assert!((0..8).all(|mx| m.coverage(mx, my) == Some(0)));
    }
    eprintln!("worst quadratic deviation: {worst}/255");
}

#[test]
fn cubic_equal_to_the_elevated_quadratic_matches_it() {
    // Degree elevation: C1 = Q0 + 2/3 (Q1 - Q0) = (8,16)/3 ... scaled by 3 so the
    // control points are integers: quad (0,0) (6,12) (12,0) -> cubic
    // (0,0) (4,8) (8,8) (12,0).
    let q = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::QuadTo(p(6, 12), p(12, 0)),
        PathCmd::Close,
    ];
    let c = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::CubicTo(p(4, 8), p(8, 8), p(12, 0)),
        PathCmd::Close,
    ];
    let mq = rasterize_glyph(&q, unit_px(12), (0, 0)).unwrap();
    let mc = rasterize_glyph(&c, unit_px(12), (0, 0)).unwrap();
    // The cubic's hull is tighter (y up to 8 vs 12), so compare on the common rows
    // by position rather than by index.
    assert_eq!(mq.width, mc.width);
    for y in mc.top..i32::try_from(mc.height).unwrap() + mc.top {
        for x in 0..mc.width {
            let a = i32::from(mq.coverage(x, u32::try_from(y - mq.top).unwrap()).unwrap());
            let b = i32::from(mc.coverage(x, u32::try_from(y - mc.top).unwrap()).unwrap());
            assert!((a - b).abs() <= 12, "({x},{y}): quad {a} cubic {b}");
        }
    }
}

#[test]
fn same_glyph_twice_gives_identical_bytes() {
    let outline = {
        let mut o = quad_shape();
        o.extend(rect(10, 0, 20, 5));
        o.push(PathCmd::MoveTo(p(0, 20)));
        o.push(PathCmd::CubicTo(p(5, 30), p(15, 30), p(20, 20)));
        o
    };
    let scale = Scale {
        size64: 17 * 64 + 21,
        units_per_em: 24,
    };
    let a = rasterize_glyph(&outline, scale, (13, -7)).unwrap();
    let b = rasterize_glyph(&outline, scale, (13, -7)).unwrap();
    assert_eq!(a, b);
    assert!(a.data.iter().any(|&c| c != 0));
}

#[test]
fn degenerate_and_hostile_inputs_do_not_panic() {
    let s = unit_px(1000);
    // Empty outline, a lone move, a zero-area line, a point.
    assert_eq!(rasterize_glyph(&[], s, (0, 0)).unwrap(), Mask::default());
    assert_eq!(
        rasterize_glyph(&[PathCmd::MoveTo(p(5, 5))], s, (0, 0)).unwrap(),
        Mask::default()
    );
    let line = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::LineTo(p(100, 0)),
        PathCmd::Close,
    ];
    let m = rasterize_glyph(&line, s, (0, 0)).unwrap();
    assert!(m.data.iter().all(|&c| c == 0));
    // A sliver on pixel boundaries.
    let sliver = rect(0, 0, 1000, 0);
    assert!(rasterize_glyph(&sliver, s, (0, 0)).unwrap().data.is_empty());
    // Scale errors.
    assert_eq!(
        rasterize_glyph(
            &line,
            Scale {
                size64: 64,
                units_per_em: 0
            },
            (0, 0)
        ),
        Err(GlyphError::BadScale)
    );
    assert_eq!(
        rasterize_glyph(
            &line,
            Scale {
                size64: u32::MAX,
                units_per_em: 1
            },
            (0, 0)
        ),
        Err(GlyphError::BadScale)
    );
    assert_eq!(
        rasterize_glyph(
            &rect(0, 0, 10, 10),
            Scale {
                size64: 0,
                units_per_em: 1
            },
            (0, 0)
        )
        .unwrap(),
        Mask::default()
    );
    // Huge coordinates: refused as too large, never overflowing.
    let huge = rect(i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    assert_eq!(rasterize_glyph(&huge, s, (0, 0)), Err(GlyphError::TooLarge));
    let max_scale = Scale {
        size64: 1 << 20,
        units_per_em: 1,
    };
    assert!(rasterize_glyph(&huge, max_scale, (i32::MAX, i32::MIN)).is_err());
    // A zero-area point far away: either an empty mask or an error, no panic.
    let far = vec![
        PathCmd::MoveTo(p(i32::MAX, i32::MAX)),
        PathCmd::LineTo(p(i32::MAX, i32::MAX)),
    ];
    let _ = rasterize_glyph(&far, max_scale, (i32::MAX, i32::MAX));
    // A box just over the size limit.
    assert_eq!(
        rasterize_glyph(&rect(0, 0, 5000, 5000), unit_px(1), (0, 0)),
        Err(GlyphError::TooLarge)
    );
    // Curves with enormous control points hit the segment cap, not a loop.
    let wild = vec![
        PathCmd::MoveTo(p(0, 0)),
        PathCmd::CubicTo(p(1000, 100_000), p(-1000, -100_000), p(1000, 0)),
    ];
    let _ = rasterize_glyph(&wild, unit_px(1000), (0, 0));
}

#[test]
fn composite_full_half_and_translucent() {
    let m = rasterize_glyph(&rect(0, 0, 4, 4), unit_px(4), (32, 0)).unwrap();
    let mut c = Canvas::new(8, 8).unwrap();
    composite_mask(&mut c, &m, 1, 6, Color::BLACK);
    // Mask rows span device y 6-4 .. 6; columns x 1..=5.
    assert_eq!(c.pixel(2, 3), Some([0, 0, 0, 255])); // full coverage
    // Half coverage (128) over white: (0*128 + 255*127 + 127)/255 = 127.
    assert_eq!(c.pixel(1, 3), Some([127, 127, 127, 255]));
    assert_eq!(c.pixel(5, 3), Some([127, 127, 127, 255]));
    assert_eq!(c.pixel(0, 3), Some([255, 255, 255, 255]));
    assert_eq!(c.pixel(6, 3), Some([255, 255, 255, 255]));
    assert_eq!(c.pixel(2, 1), Some([255, 255, 255, 255]));
    // Translucent colour: alpha 128 * coverage 255 -> 128; mix(0, 255, 128).
    let mut c = Canvas::new(8, 8).unwrap();
    let half = Color {
        r: 0,
        g: 0,
        b: 0,
        a: 128,
    };
    composite_mask(&mut c, &m, 1, 6, half);
    assert_eq!(c.pixel(2, 3), Some([127, 127, 127, 255]));
    // Off-canvas placement is clipped silently.
    composite_mask(&mut c, &m, -100, -100, Color::BLACK);
    composite_mask(&mut c, &m, i32::MAX, i32::MAX, Color::BLACK);
    composite_mask(&mut c, &m, i32::MIN, i32::MIN, Color::BLACK);
}
