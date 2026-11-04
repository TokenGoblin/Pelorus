//! Glyph runs through the software rasterizer, with the real Ahem font.
//!
//! Hand-derived numbers. Ahem has 1000 units per em; every letter but `p` is the
//! box x 0..1000, y -200..800. At 16 px (`size64` 1024, scale 4096 per 1000
//! units in 1/256 px):
//! - x: 0 and 1000 map to exactly 0 and 4096, so a box is 16 whole columns.
//! - y top: round_half_up(800 * 4096 / 1000) = 3277 (1/256 px) above the baseline;
//!   bottom: round_half_up(200 * 4096 / 1000) = 819 below.
//!
//! With the baseline at pixel row 20 (5120): top = 1843, which is inside row 7
//! (1792..2048) covering 205/256 of it: coverage `(205*512*255 + 65536) >> 17` =
//! 204. The bottom = 5939, inside row 23 (5888..6144) covering 51/256:
//! `(51*512*255 + 65536) >> 17` = 51. Rows 8..=22 are fully covered.
//! Black at alpha `a` over white is `255 - a` (`(255*(255-a) + 127) / 255`).

use std::cell::Cell;

use px_layout::geom::{Au, px};
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::{GlyphSource, RasterError, rasterize_with};
use px_paint::{Canvas, Color, DisplayList, FontId, Item, ListError, PositionedGlyph, Rect};
use px_text::registry::AHEM_BYTES;
use px_text::{Font, PathCmd as TextCmd};

const AHEM: FontId = FontId(7);

struct Ahem {
    font: Font,
    outline_calls: Cell<u32>,
}

impl Ahem {
    fn new() -> Ahem {
        Ahem {
            font: Font::from_bytes(AHEM_BYTES).expect("ahem"),
            outline_calls: Cell::new(0),
        }
    }
    fn gid(&self, c: char) -> u32 {
        u32::from(self.font.glyph_index(c).expect("glyph"))
    }
}

impl GlyphSource for Ahem {
    fn units_per_em(&self, font: FontId) -> Option<u32> {
        (font == AHEM).then(|| u32::from(self.font.units_per_em()))
    }
    fn outline(&self, font: FontId, glyph: u32) -> Option<Vec<PathCmd>> {
        if font != AHEM {
            return None;
        }
        self.outline_calls.set(self.outline_calls.get() + 1);
        let o = self.font.outline(u16::try_from(glyph).ok()?).ok()?;
        let p = |p: px_text::Pt| Pt { x: p.x, y: p.y };
        Some(
            o.commands
                .iter()
                .map(|c| match *c {
                    TextCmd::MoveTo(a) => PathCmd::MoveTo(p(a)),
                    TextCmd::LineTo(a) => PathCmd::LineTo(p(a)),
                    TextCmd::QuadTo(a, b) => PathCmd::QuadTo(p(a), p(b)),
                    TextCmd::Close => PathCmd::Close,
                })
                .collect(),
        )
    }
}

fn at(src: &Ahem, c: char, x_px: i32, y_px: i32) -> PositionedGlyph {
    PositionedGlyph {
        glyph_id: src.gid(c),
        x: px(x_px),
        y: px(y_px),
    }
}

fn run_list(glyphs: &[PositionedGlyph], size_px: i32, color: Color) -> DisplayList {
    let mut l = DisplayList::new();
    assert!(l.push_glyph_run(AHEM, px(size_px), color, glyphs));
    l
}

fn gray(c: &Canvas, x: u32, y: u32) -> u8 {
    let p = c.pixel(x, y).expect("pixel");
    assert_eq!((p[1], p[2], p[3]), (p[0], p[0], 255));
    p[0]
}

#[test]
fn ahem_x_at_16px_is_a_16_by_16_box_with_exact_edge_rows() {
    let src = Ahem::new();
    let l = run_list(&[at(&src, 'X', 2, 20)], 16, Color::BLACK);
    let c = rasterize_with(&l, 24, 30, &src).unwrap();
    for y in 0..30 {
        for x in 0..24 {
            let inside_x = (2..18).contains(&x);
            let want = match y {
                7 if inside_x => 255 - 204,
                8..=22 if inside_x => 0,
                23 if inside_x => 255 - 51,
                _ => 255,
            };
            assert_eq!(gray(&c, x, y), want, "({x},{y})");
        }
    }
}

#[test]
fn two_glyphs_land_where_their_origins_say() {
    let src = Ahem::new();
    // 'X' then 'p' one advance (16 px) apart. 'p' is box y -200..0: rows 20..=22
    // fully, row 23 at 51 (bottom 5939 as above), nothing above the baseline.
    let l = run_list(
        &[at(&src, 'X', 0, 20), at(&src, 'p', 16, 20)],
        16,
        Color::BLACK,
    );
    let c = rasterize_with(&l, 40, 30, &src).unwrap();
    assert_eq!(gray(&c, 15, 15), 0); // X, last column
    assert_eq!(gray(&c, 16, 15), 255); // p has no ink above the baseline
    assert_eq!(gray(&c, 16, 19), 255);
    assert_eq!(gray(&c, 16, 20), 0);
    assert_eq!(gray(&c, 31, 22), 0);
    assert_eq!(gray(&c, 32, 22), 255); // past the second advance
    assert_eq!(gray(&c, 20, 23), 255 - 51);
    assert_eq!(gray(&c, 20, 24), 255);
    assert_eq!(gray(&c, 15, 23), 255 - 51); // X's bottom row
}

#[test]
fn the_current_clip_restricts_glyph_ink() {
    let src = Ahem::new();
    let mut l = DisplayList::new();
    l.push(Item::PushClip {
        rect: Rect {
            x: px(4),
            y: px(10),
            width: px(6),
            height: px(5),
        },
    });
    assert!(l.push_glyph_run(AHEM, px(16), Color::BLACK, &[at(&src, 'X', 0, 20)]));
    l.push(Item::PopClip);
    // After the pop the clip is gone again.
    assert!(l.push_glyph_run(AHEM, px(16), Color::BLACK, &[at(&src, 'X', 20, 20)]));
    let c = rasterize_with(&l, 40, 30, &src).unwrap();
    for y in 0..30 {
        for x in 0..16 {
            let inside = (4..10).contains(&x) && (10..15).contains(&y);
            assert_eq!(gray(&c, x, y), if inside { 0 } else { 255 }, "({x},{y})");
        }
    }
    assert_eq!(gray(&c, 20, 15), 0);
    assert_eq!(gray(&c, 35, 22), 0);
}

#[test]
fn colour_and_alpha_apply() {
    let src = Ahem::new();
    let red = Color {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    let l = run_list(&[at(&src, 'X', 0, 20)], 16, red);
    let c = rasterize_with(&l, 16, 30, &src).unwrap();
    assert_eq!(c.pixel(3, 15), Some([255, 0, 0, 255]));
    // Row 7 has coverage 204: green/blue = (0*204 + 255*51 + 127)/255 = 51.
    assert_eq!(c.pixel(3, 7), Some([255, 51, 51, 255]));
}

#[test]
fn a_glyph_is_rasterized_once_per_font_size_and_id() {
    let src = Ahem::new();
    let g = [
        at(&src, 'X', 0, 20),
        at(&src, 'X', 16, 20),
        at(&src, 'X', 32, 20),
        at(&src, 'p', 48, 20),
    ];
    let mut l = DisplayList::new();
    assert!(l.push_glyph_run(AHEM, px(16), Color::BLACK, &g));
    assert!(l.push_glyph_run(AHEM, px(16), Color::BLACK, &g));
    // A different size is a different mask.
    assert!(l.push_glyph_run(AHEM, px(10), Color::BLACK, &g[..1]));
    rasterize_with(&l, 80, 30, &src).unwrap();
    assert_eq!(src.outline_calls.get(), 3); // X@16, p@16, X@10
}

#[test]
fn identical_lists_give_identical_bytes() {
    let src = Ahem::new();
    let l = run_list(
        &[at(&src, 'X', 3, 20), at(&src, 'p', 19, 20)],
        13,
        Color::BLACK,
    );
    let a = rasterize_with(&l, 50, 30, &src).unwrap();
    let b = rasterize_with(&l, 50, 30, &src).unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_run_beyond_the_arena_is_an_error_and_draws_nothing() {
    let src = Ahem::new();
    let mut l = run_list(&[at(&src, 'X', 0, 20)], 16, Color::BLACK);
    for (start, len) in [(0, 2), (1, 1), (u32::MAX, 1), (0, u32::MAX)] {
        l.items = vec![Item::GlyphRun {
            font: AHEM,
            size: px(16),
            color: Color::BLACK,
            start,
            len,
        }];
        assert!(
            matches!(rasterize_with(&l, 8, 8, &src), Err(RasterError::BadList(_))),
            "{start}+{len}"
        );
    }
    l.items = vec![Item::GlyphRun {
        font: AHEM,
        size: px(16),
        color: Color::BLACK,
        start: 0,
        len: 1 << 20,
    }];
    assert_eq!(
        rasterize_with(&l, 8, 8, &src),
        Err(RasterError::BadList(ListError::RunTooLong))
    );
}

#[test]
fn unknown_fonts_glyphs_and_silly_sizes_draw_nothing_and_do_not_panic() {
    let src = Ahem::new();
    let white = Canvas::new(16, 16).unwrap();
    for (font, glyph, size) in [
        (FontId(99), src.gid('X'), px(16)),
        (AHEM, 60_000, px(16)),
        (AHEM, src.gid('X'), px(0)),
        (AHEM, src.gid('X'), Au(-5)),
        (AHEM, src.gid('X'), Au(i32::MAX)),
        (AHEM, src.gid('X'), px(100_000)),
    ] {
        let mut l = DisplayList::new();
        assert!(l.push_glyph_run(
            font,
            size,
            Color::BLACK,
            &[PositionedGlyph {
                glyph_id: glyph,
                x: px(0),
                y: px(10)
            }]
        ));
        let c = rasterize_with(&l, 16, 16, &src).unwrap();
        assert_eq!(c, white, "{font:?} {glyph} {size:?}");
    }
    // Extreme positions are skipped, not wrapped.
    let mut l = DisplayList::new();
    let edge = PositionedGlyph {
        glyph_id: src.gid('X'),
        x: Au(i32::MAX),
        y: Au(i32::MIN),
    };
    assert!(l.push_glyph_run(AHEM, px(16), Color::BLACK, &[edge]));
    assert_eq!(rasterize_with(&l, 16, 16, &src).unwrap(), white);
}

#[test]
fn without_a_source_a_run_draws_nothing() {
    let src = Ahem::new();
    let l = run_list(&[at(&src, 'X', 0, 10)], 16, Color::BLACK);
    assert_eq!(
        px_paint::rasterize(&l, 16, 16).unwrap(),
        Canvas::new(16, 16).unwrap()
    );
}
