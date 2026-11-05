//! `border-radius` and `corner-shape` through the product path (style, layout,
//! build) and through the rasterizer. Expectations are worked out by hand from
//! the CSS in each test.

use px_layout::geom::{Au, px};
use px_paint::display_list::{Corner, CornerShape, RoundedRect};
use px_paint::{BorderSide, BorderStyle, Color, DisplayList, Item, Rect, build, rasterize};

const UA: &str = "html, body, div { display: block } \
     head, style, script, title, meta, link { display: none } \
     body { margin: 0 }";

fn list_for(html: &str) -> DisplayList {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    for id in arena.descendants(arena.document()) {
        let is_style = arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_some_and(|q| q.local.as_ref() == "style");
        if is_style {
            engine.add_author_stylesheet(&px_layout::inline::collect_text(&arena, id), "x:test");
        }
    }
    let root = engine.style_root_for(&arena);
    assert!(engine.resolve(&arena, &root).is_some());
    let tree = px_layout::block::layout_document(&arena, &root, px(800)).expect("laid out");
    build(&arena, &root, &tree)
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect {
        x: px(x),
        y: px(y),
        width: px(w),
        height: px(h),
    }
}

fn round(r: i32) -> Corner {
    Corner {
        rx: px(r),
        ry: px(r),
        shape: CornerShape::ROUND,
    }
}

fn only_shape(list: &DisplayList) -> RoundedRect {
    list.items
        .iter()
        .find_map(|i| match i {
            Item::FillRoundedRect { shape, .. } => Some(*shape),
            _ => None,
        })
        .expect("a rounded fill")
}

#[test]
fn a_box_without_radius_emits_no_rounded_item() {
    let l = list_for("<style>div{width:50px;height:50px;background:#f00}</style><div></div>");
    assert!(l.items.iter().all(|i| !matches!(
        i,
        Item::FillRoundedRect { .. } | Item::RoundedBorder { .. } | Item::PushRoundedClip { .. }
    )));
}

#[test]
fn all_four_corners_and_elliptical_radii_reach_the_item() {
    let l = list_for(
        "<style>div{width:100px;height:60px;background:#f00;\
         border-radius: 10px 20px 30px 5px / 8px 16px 24px 4px}</style><div></div>",
    );
    let s = only_shape(&l);
    assert_eq!(s.rect, rect(0, 0, 100, 60));
    let want = [(10, 8), (20, 16), (30, 24), (5, 4)];
    for (c, (rx, ry)) in s.corners.iter().zip(want) {
        assert_eq!((c.rx, c.ry), (px(rx), px(ry)));
        assert_eq!(c.shape, CornerShape::ROUND);
    }
}

#[test]
fn percentages_resolve_against_the_border_box() {
    let l = list_for(
        "<style>div{width:200px;height:100px;background:#f00;border-radius:10% / 20%}</style><div></div>",
    );
    let s = only_shape(&l);
    assert_eq!(s.corners[0].rx, px(20));
    assert_eq!(s.corners[0].ry, px(20));
}

#[test]
fn overlapping_radii_are_scaled_by_the_smallest_ratio() {
    // 100 wide: top 80 + 80 = 160, f = 100/160 = 5/8; left/right 80 + 20 = 100 of
    // 100 is fine on its own. Every radius scales by 5/8.
    let l = list_for(
        "<style>div{width:100px;height:100px;background:#f00;\
         border-radius:80px 80px 20px 20px}</style><div></div>",
    );
    let s = only_shape(&l);
    assert_eq!(s.corners[0].rx, px(50));
    assert_eq!(s.corners[1].rx, px(50));
    assert_eq!(s.corners[2].rx, Au(px(20).0 * 5 / 8));
    // Idempotent.
    assert_eq!(s.clamped(), s);
    // A radius far past the box is the pill: min(L/S) binds.
    let l = list_for(
        "<style>div{width:100px;height:40px;background:#f00;border-radius:9999px}</style><div></div>",
    );
    let s = only_shape(&l);
    assert!(
        s.corners
            .iter()
            .all(|c| c.rx == px(20) || c.rx.0 <= px(50).0)
    );
    assert!(s.corners[0].rx.0 * 2 <= px(100).0 && s.corners[0].ry.0 * 2 <= px(40).0);
}

#[test]
fn clamping_never_lets_adjacent_radii_exceed_a_side() {
    let rr = RoundedRect::new(
        rect(0, 0, 37, 41),
        [round(500), round(300), round(7), round(1000)],
    );
    let c = rr.corners;
    assert!(c[0].rx.0 + c[1].rx.0 <= rr.rect.width.0);
    assert!(c[3].rx.0 + c[2].rx.0 <= rr.rect.width.0);
    assert!(c[0].ry.0 + c[3].ry.0 <= rr.rect.height.0);
    assert!(c[1].ry.0 + c[2].ry.0 <= rr.rect.height.0);
    // Negative radii are zero.
    let n = RoundedRect::new(rect(0, 0, 10, 10), [round(-5); 4]);
    assert!(n.corners.iter().all(|c| c.rx == Au(0) && c.ry == Au(0)));
    assert!(!n.is_rounded());
}

#[test]
fn a_border_makes_a_rounded_border_item_and_padding_box_insets_the_radii() {
    let l = list_for(
        "<style>div{width:80px;height:80px;border:5px solid #00f;background:#f00;\
         border-radius:20px;background-clip:padding-box}</style><div></div>",
    );
    let fill = only_shape(&l);
    // Padding box: 90 - 10, radii 20 - 5.
    assert_eq!(fill.rect, rect(5, 5, 80, 80));
    assert_eq!(fill.corners[0].rx, px(15));
    let border = l
        .items
        .iter()
        .find_map(|i| match i {
            Item::RoundedBorder { shape, sides } => Some((*shape, *sides)),
            _ => None,
        })
        .expect("a rounded border");
    assert_eq!(border.0.rect, rect(0, 0, 90, 90));
    assert_eq!(border.0.corners[2].rx, px(20));
    let side = BorderSide {
        width: px(5),
        style: BorderStyle::Solid,
        color: Color {
            r: 0,
            g: 0,
            b: 255,
            a: 255,
        },
    };
    assert_eq!(border.1, [side; 4]);
}

#[test]
fn overflow_clips_with_the_padding_edge_shape_and_pops_balanced() {
    let l = list_for(
        "<style>#a{width:60px;height:60px;overflow:hidden;border:4px solid #000;border-radius:16px}\
         #b{width:200px;height:200px;background:#0f0}</style><div id=a><div id=b></div></div>",
    );
    let clip = l
        .items
        .iter()
        .find_map(|i| match i {
            Item::PushRoundedClip { shape } => Some(*shape),
            _ => None,
        })
        .expect("a rounded clip");
    assert_eq!(clip.rect, rect(4, 4, 60, 60));
    assert_eq!(clip.corners[0].rx, px(12));
    assert!(l.clips_balanced());
}

#[test]
fn corner_shape_reaches_the_item() {
    let l = list_for(
        "<style>div{width:50px;height:50px;background:#f00;border-radius:10px;\
         corner-shape: squircle scoop notch superellipse(3)}</style><div></div>",
    );
    let s = only_shape(&l);
    assert_eq!(s.corners[0].shape, CornerShape::SQUIRCLE);
    assert_eq!(s.corners[1].shape, CornerShape::SCOOP);
    assert_eq!(s.corners[2].shape, CornerShape::NOTCH);
    // 2^3 = 8, in 1/16.
    assert_eq!(s.corners[3].shape, CornerShape::Convex(128));
}

#[test]
fn the_default_corner_shape_is_round() {
    let l = list_for(
        "<style>div{width:50px;height:50px;background:#f00;border-radius:10px}</style><div></div>",
    );
    assert!(
        only_shape(&l)
            .corners
            .iter()
            .all(|c| c.shape == CornerShape::ROUND)
    );
}

// ------------------------------------------------------------------ raster

fn item_list(items: &[Item]) -> DisplayList {
    DisplayList {
        items: items.to_vec(),
        ..DisplayList::default()
    }
}

const RED: Color = Color {
    r: 255,
    g: 0,
    b: 0,
    a: 255,
};

fn fill_rr(shape: RoundedRect) -> Item {
    Item::FillRoundedRect { shape, color: RED }
}

#[test]
fn a_rounded_fill_cuts_the_corners_and_keeps_the_middle() {
    let shape = RoundedRect::new(rect(2, 2, 20, 20), [round(8); 4]);
    let c = rasterize(&item_list(&[fill_rr(shape)]), 24, 24).unwrap();
    assert_eq!(c.pixel(12, 12), Some([255, 0, 0, 255]));
    assert_eq!(c.pixel(2, 12), Some([255, 0, 0, 255]));
    assert_eq!(c.pixel(12, 2), Some([255, 0, 0, 255]));
    // The very corner pixel is untouched, and its neighbours are anti-aliased.
    assert_eq!(c.pixel(2, 2), Some([255, 255, 255, 255]));
    let edge = c.pixel(4, 3).unwrap();
    assert!(edge[1] > 0 && edge[1] < 255, "{edge:?}");
}

#[test]
fn rasterizing_twice_is_byte_identical() {
    let shape = RoundedRect::new(
        rect(1, 1, 61, 47),
        [
            round(17),
            Corner {
                rx: px(20),
                ry: px(9),
                shape: CornerShape::SQUIRCLE,
            },
            Corner {
                rx: px(11),
                ry: px(11),
                shape: CornerShape::Convex(37),
            },
            Corner {
                rx: px(12),
                ry: px(12),
                shape: CornerShape::SCOOP,
            },
        ],
    );
    let l = item_list(&[fill_rr(shape)]);
    let a = rasterize(&l, 70, 60).unwrap();
    let b = rasterize(&l, 70, 60).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.to_png().unwrap(), b.to_png().unwrap());
}

#[test]
fn a_rounded_clip_confines_a_plain_fill() {
    let clip = RoundedRect::new(rect(0, 0, 20, 20), [round(10); 4]);
    let l = item_list(&[
        Item::PushRoundedClip { shape: clip },
        Item::FillRect {
            rect: rect(0, 0, 20, 20),
            color: RED,
        },
        Item::PopClip,
    ]);
    let c = rasterize(&l, 20, 20).unwrap();
    assert_eq!(c.pixel(10, 10), Some([255, 0, 0, 255]));
    assert_eq!(c.pixel(0, 0), Some([255, 255, 255, 255]));
    assert_eq!(c.pixel(19, 19), Some([255, 255, 255, 255]));
    // The same clip is symmetric.
    assert_eq!(c.pixel(1, 5), c.pixel(18, 5));
    // Popping restores an unrounded clip.
    let l = item_list(&[
        Item::PushRoundedClip { shape: clip },
        Item::PopClip,
        Item::FillRect {
            rect: rect(0, 0, 20, 20),
            color: RED,
        },
    ]);
    assert_eq!(
        rasterize(&l, 20, 20).unwrap().pixel(0, 0),
        Some([255, 0, 0, 255])
    );
}

#[test]
fn an_unrounded_shape_is_pixel_identical_to_the_plain_item() {
    let shape = RoundedRect::new(rect(3, 4, 11, 7), [round(0); 4]);
    let a = rasterize(&item_list(&[fill_rr(shape)]), 20, 20).unwrap();
    let b = rasterize(
        &item_list(&[Item::FillRect {
            rect: rect(3, 4, 11, 7),
            color: RED,
        }]),
        20,
        20,
    )
    .unwrap();
    assert_eq!(a, b);
}

#[test]
fn a_rounded_border_is_a_ring_around_the_unpainted_inside() {
    let shape = RoundedRect::new(rect(0, 0, 40, 40), [round(16); 4]);
    let side = BorderSide {
        width: px(4),
        style: BorderStyle::Solid,
        color: RED,
    };
    let l = item_list(&[Item::RoundedBorder {
        shape,
        sides: [side; 4],
    }]);
    let c = rasterize(&l, 40, 40).unwrap();
    assert_eq!(c.pixel(20, 1), Some([255, 0, 0, 255]));
    assert_eq!(c.pixel(1, 20), Some([255, 0, 0, 255]));
    assert_eq!(c.pixel(20, 20), Some([255, 255, 255, 255]));
    assert_eq!(c.pixel(0, 0), Some([255, 255, 255, 255]));
    // Left-right and top-bottom symmetry, anti-aliased pixels included.
    for (x, y) in [(3, 5), (6, 2), (9, 1), (2, 9)] {
        assert_eq!(c.pixel(x, y), c.pixel(39 - x, y));
        assert_eq!(c.pixel(x, y), c.pixel(x, 39 - y));
    }
}

#[test]
fn a_shape_off_the_canvas_and_a_huge_radius_are_harmless() {
    let huge = RoundedRect::new(rect(-100, -100, 10_000, 10_000), [round(30_000); 4]);
    let l = item_list(&[fill_rr(huge)]);
    assert!(rasterize(&l, 16, 16).is_ok());
    let off = RoundedRect::new(rect(1000, 1000, 10, 10), [round(5); 4]);
    let c = rasterize(&item_list(&[fill_rr(off)]), 16, 16).unwrap();
    assert!(c.pixels.iter().all(|b| *b == 255));
    // An unbalanced rounded clip is refused like a plain one.
    let clip = Item::PushRoundedClip {
        shape: RoundedRect::new(rect(0, 0, 4, 4), [round(1); 4]),
    };
    assert!(rasterize(&item_list(&[clip]), 4, 4).is_err());
}

/// `corner-shape` is pref'd off in stylo's servo build behind
/// `layout.unimplemented`, which `px-css` turns on. The same pref gates other
/// properties (`zoom`, `contain`, counters, `user-select`...). Of them only
/// `zoom` changes a used value: stylo scales lengths by it while computing, so
/// `zoom: 2` doubles a `50px` width, which is what CSS Viewport says it does.
/// This test pins that side effect so that it is a decision, not a surprise.
#[test]
fn the_pref_that_enables_corner_shape_also_enables_zoom() {
    let l =
        list_for("<style>div{width:50px;height:50px;background:#f00;zoom:2}</style><div></div>");
    let r = l
        .items
        .iter()
        .find_map(|i| match i {
            Item::FillRect { rect, color } if color.r == 255 && color.g == 0 => Some(*rect),
            _ => None,
        })
        .expect("a fill");
    assert_eq!(r.width, px(100));
}
