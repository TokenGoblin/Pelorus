//! Display-list construction through the product path: parse HTML, style it,
//! lay it out, build. Expectations are derived by hand from the CSS in each test.

use px_layout::geom::{Au, px};
use px_paint::{
    BorderSide, BorderStyle, Color, DisplayList, FontId, Item, PositionedGlyph, Rect, build,
};

const VIEWPORT: i32 = 800;

const UA: &str = "html, body, div, p { display: block } \
     head, style, script, title, meta, link { display: none } \
     body { margin: 0 }";

fn list_for(html: &str) -> DisplayList {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(VIEWPORT as f32, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    let document = arena.document();
    for id in arena.descendants(document) {
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
    let tree = px_layout::block::layout_document(&arena, &root, px(VIEWPORT)).expect("laid out");
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

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

fn fill(r: Rect, c: Color) -> Item {
    Item::FillRect { rect: r, color: c }
}

/// Items after the canvas white fill (and a propagated canvas colour, if any).
fn after_canvas(list: &DisplayList, canvas_items: usize) -> &[Item] {
    &list.items[canvas_items..]
}

#[test]
fn px_paint_background_then_border_with_exact_rects() {
    let list = list_for(
        "<style>div { width: 100px; height: 50px; background: #ff0000; \
         border: 2px solid #0000ff }</style><div></div>",
    );
    // Content 100x50 + 2px border each side = 104x54 border box at the origin.
    let side = BorderSide {
        width: px(2),
        style: BorderStyle::Solid,
        color: rgb(0, 0, 255),
    };
    assert_eq!(
        list.items,
        vec![
            fill(rect(0, 0, VIEWPORT, 54), Color::WHITE),
            fill(rect(0, 0, 104, 54), rgb(255, 0, 0)),
            Item::Border {
                rect: rect(0, 0, 104, 54),
                sides: [side; 4],
            },
        ]
    );
}

#[test]
fn px_paint_nested_boxes_paint_parent_before_child_and_siblings_in_order() {
    let list = list_for(
        "<style>#a { width: 100px; height: 40px; background: #010101 } \
         #b { width: 50px; height: 10px; background: #020202 } \
         #c { width: 60px; height: 20px; background: #030303 }</style>\
         <div id=a><div id=b></div></div><div id=c></div>",
    );
    // a at y=0 (100x40); b inside it at the origin (50x10); c below a at y=40.
    assert_eq!(
        after_canvas(&list, 1),
        &[
            fill(rect(0, 0, 100, 40), rgb(1, 1, 1)),
            fill(rect(0, 0, 50, 10), rgb(2, 2, 2)),
            fill(rect(0, 40, 60, 20), rgb(3, 3, 3)),
        ]
    );
}

#[test]
fn px_paint_overflow_hidden_clips_descendants_at_the_padding_box_and_balances() {
    let list = list_for(
        "<style>#a { width: 100px; height: 40px; overflow: hidden; border: 3px solid #000 } \
         #b { width: 10px; height: 10px; background: #0a0a0a }</style>\
         <div id=a><div id=b></div></div>",
    );
    assert!(list.clips_balanced());
    let items = after_canvas(&list, 1);
    // Border box 106x46; padding box inset by 3px: (3,3) 100x40.
    assert!(matches!(items[0], Item::Border { .. }), "{items:?}");
    assert_eq!(
        items[1],
        Item::PushClip {
            rect: rect(3, 3, 100, 40)
        }
    );
    assert_eq!(items[2], fill(rect(3, 3, 10, 10), rgb(10, 10, 10)));
    assert_eq!(items[3], Item::PopClip);
    assert_eq!(items.len(), 4);
}

#[test]
fn px_paint_transparent_background_and_no_border_emit_nothing() {
    let list = list_for("<style>div { width: 10px; height: 10px }</style><div></div>");
    assert_eq!(
        list.items,
        vec![fill(rect(0, 0, VIEWPORT, 10), Color::WHITE)]
    );
}

#[test]
fn px_paint_alpha_background_is_passed_through() {
    let list = list_for(
        "<style>div { width: 10px; height: 10px; background: rgba(255, 0, 0, 0.5) }</style><div></div>",
    );
    // 0.5 * 255 = 127.5, rounded to 128.
    assert_eq!(
        after_canvas(&list, 1),
        &[fill(
            rect(0, 0, 10, 10),
            Color {
                r: 255,
                g: 0,
                b: 0,
                a: 128
            }
        )]
    );
}

#[test]
fn px_paint_border_none_and_hidden_emit_no_item_and_mixed_sides_keep_order() {
    let list = list_for(
        "<style>div { width: 10px; height: 10px; border-style: none; border-width: 5px } \
         p { width: 10px; height: 10px; border-style: hidden; border-width: 5px }</style>\
         <div></div><p></p>",
    );
    assert_eq!(list.items.len(), 1, "{:?}", list.items);

    let list = list_for(
        "<style>div { width: 10px; height: 10px; border-style: solid none dashed none; \
         border-width: 1px 2px 3px 4px; border-color: #111 #222 #333 #444 }</style><div></div>",
    );
    // Left and right are style none, so their widths are 0: 10 wide. Top 1 and
    // bottom 3 are drawn: 10 + 1 + 3 = 14 tall.
    let items = after_canvas(&list, 1);
    assert_eq!(
        items,
        &[Item::Border {
            rect: rect(0, 0, 10, 14),
            sides: [
                BorderSide {
                    width: px(1),
                    style: BorderStyle::Solid,
                    color: rgb(0x11, 0x11, 0x11)
                },
                BorderSide {
                    width: Au(0),
                    style: BorderStyle::None,
                    color: rgb(0x22, 0x22, 0x22)
                },
                BorderSide {
                    width: px(3),
                    style: BorderStyle::Solid,
                    color: rgb(0x33, 0x33, 0x33)
                },
                BorderSide {
                    width: Au(0),
                    style: BorderStyle::None,
                    color: rgb(0x44, 0x44, 0x44)
                },
            ],
        }]
    );
}

#[test]
fn px_paint_visibility_hidden_skips_the_box_but_not_visible_children() {
    let list = list_for(
        "<style>#a { width: 100px; height: 40px; background: #010101; visibility: hidden } \
         #b { width: 10px; height: 10px; background: #020202; visibility: visible } \
         #c { width: 10px; height: 10px; background: #030303 }</style>\
         <div id=a><div id=b></div><div id=c></div></div>",
    );
    // a is hidden; b opts back in; c inherits hidden.
    assert_eq!(
        after_canvas(&list, 1),
        &[fill(rect(0, 0, 10, 10), rgb(2, 2, 2))]
    );
}

#[test]
fn px_paint_root_background_propagates_to_the_canvas_once() {
    let list = list_for(
        "<style>html { background: #102030 } body { background: #ff0000; height: 20px }</style>\
         <body></body>",
    );
    // Canvas white, then the html colour over the whole canvas. Body keeps its own
    // red, since only html's propagated. html itself paints nothing again.
    assert_eq!(
        list.items[1],
        fill(list_canvas(&list), rgb(0x10, 0x20, 0x30))
    );
    assert_eq!(
        list.items[2],
        fill(rect(0, 0, VIEWPORT, 20), rgb(255, 0, 0))
    );
    assert_eq!(list.items.len(), 3);
}

#[test]
fn px_paint_body_background_propagates_when_html_has_none() {
    let list = list_for("<style>body { background: #00ff00; height: 20px }</style><body></body>");
    // Painted exactly once, as the canvas, not again for the body box.
    assert_eq!(list.items[1], fill(list_canvas(&list), rgb(0, 255, 0)));
    assert_eq!(list.items.len(), 2, "{:?}", list.items);
}

fn list_canvas(list: &DisplayList) -> Rect {
    match list.items[0] {
        Item::FillRect { rect, color } => {
            assert_eq!(color, Color::WHITE);
            rect
        }
        other => panic!("first item is not the canvas: {other:?}"),
    }
}

/// The glyph runs of a list: `(font, size, colour, glyphs)`.
type Run = (FontId, Au, Color, Vec<PositionedGlyph>);

fn runs(list: &DisplayList) -> Vec<Run> {
    list.items
        .iter()
        .filter_map(|i| match *i {
            Item::GlyphRun {
                font,
                size,
                color,
                start,
                len,
            } => Some((
                font,
                size,
                color,
                list.run_glyphs(start, len).expect("in bounds").to_vec(),
            )),
            _ => None,
        })
        .collect()
}

fn gid(c: char) -> u32 {
    let font = px_layout::text::font_of(px_text::FontId(0)).expect("ahem");
    u32::from(font.glyph_index(c).expect("glyph"))
}

/// Ahem: ascent 800/1000 em, descent 200/1000, no line gap. At 16 px the
/// content area is 16 px tall and the baseline is 12.8 px = 768 Au below the
/// line top.
const BASELINE_16: Au = Au(768);

#[test]
fn px_paint_text_hi_at_16px_is_two_glyphs_on_the_baseline() {
    let list = list_for(
        "<style>div { margin-left: 8px; font-size: 16px; color: #0000ff }</style><div>Hi</div>",
    );
    assert!(
        !list.items.iter().any(|i| matches!(i, Item::TextRun { .. })),
        "the placeholder is gone"
    );
    let r = runs(&list);
    assert_eq!(r.len(), 1, "{:?}", list.items);
    let (font, size, color, glyphs) = &r[0];
    assert_eq!(*font, FontId(0));
    assert_eq!(*size, px(16));
    assert_eq!(*color, rgb(0, 0, 255));
    let expect = |c, x| PositionedGlyph {
        glyph_id: gid(c),
        x: px(x),
        y: BASELINE_16,
    };
    assert_eq!(glyphs, &[expect('H', 8), expect('i', 24)]);
}

#[test]
fn px_paint_text_colour_comes_from_the_run_element() {
    let list = list_for(
        "<style>div { color: #ff0000 } .g { color: #00ff00 }</style>\
         <div>a<div class=g>b</div></div>",
    );
    // Layout records one element per run (inline boxes are not split yet), so
    // the colour is tested per block.
    let colors: Vec<Color> = runs(&list).iter().map(|r| r.2).collect();
    assert_eq!(colors, [rgb(255, 0, 0), rgb(0, 255, 0)], "{:?}", list.items);
}

#[test]
fn px_paint_text_rtl_line_is_start_aligned_at_the_right_edge() {
    // 'ab' is 32 px wide in a 100 px box with direction rtl: it starts at 68.
    let list = list_for(
        "<style>div { width: 100px; font-size: 16px; direction: rtl }</style><div>ab</div>",
    );
    let r = runs(&list);
    let xs: Vec<Au> = r.iter().flat_map(|r| r.3.iter().map(|g| g.x)).collect();
    assert_eq!(xs, [px(68), px(84)], "{:?}", list.items);
}

#[test]
fn px_paint_text_rtl_script_runs_are_left_to_right_in_visual_order() {
    // Two Hebrew letters in a 100 px rtl box: the run is right-aligned and its
    // glyphs ascend in x whatever the logical order.
    let list = list_for(
        "<style>div { width: 100px; font-size: 16px; direction: rtl }</style>\
         <div>\u{5d0}\u{5d1}</div>",
    );
    let xs: Vec<Au> = runs(&list)
        .iter()
        .flat_map(|r| r.3.iter().map(|g| g.x))
        .collect();
    assert_eq!(xs, [px(68), px(84)], "{:?}", list.items);
}

#[test]
fn px_paint_text_mixed_direction_keeps_every_glyph_inside_the_line() {
    let list = list_for(
        "<style>div { width: 200px; font-size: 16px; direction: ltr }</style>\
         <div>ab \u{5d0}\u{5d1} cd</div>",
    );
    let mut xs: Vec<Au> = runs(&list)
        .iter()
        .flat_map(|r| r.3.iter().map(|g| g.x))
        .collect();
    xs.sort();
    // "ab", space, two Hebrew letters, space, "cd": eight cells of 16 px from
    // x = 0, each glyph origin on a distinct cell.
    let cells: Vec<Au> = (0..8).map(|i| px(16 * i)).collect();
    assert_eq!(xs, cells, "{:?}", list.items);
}

#[test]
fn px_paint_text_hidden_is_skipped_and_visible_descendants_paint() {
    let list = list_for(
        "<style>div { visibility: hidden; font-size: 16px } .v { visibility: visible }</style>\
         <div>aa<div class=v>b</div></div>",
    );
    let r = runs(&list);
    assert_eq!(r.len(), 1, "{:?}", list.items);
    assert_eq!(r[0].3.len(), 1);
    assert_eq!(r[0].3[0].glyph_id, gid('b'));
    assert_eq!(r[0].3[0].x, px(0));
}

#[test]
fn px_paint_text_is_wrapped_in_the_overflow_clip() {
    let list = list_for(
        "<style>div { width: 20px; height: 16px; overflow: hidden; font-size: 16px }</style>\
         <div>Hello</div>",
    );
    let push = list
        .items
        .iter()
        .position(|i| matches!(i, Item::PushClip { .. }))
        .expect("clip");
    let glyph = list
        .items
        .iter()
        .position(|i| matches!(i, Item::GlyphRun { .. }))
        .expect("run");
    let pop = list
        .items
        .iter()
        .position(|i| matches!(i, Item::PopClip))
        .expect("pop");
    assert!(push < glyph && glyph < pop, "{:?}", list.items);
    assert!(list.clips_balanced());
}

#[test]
fn px_paint_text_paints_after_blocks_and_before_positioned() {
    // Appendix E: inline content (5) is above in-flow backgrounds (3) and
    // below positioned boxes (6).
    let list = list_for(
        "<style>.a { background: #010000; font-size: 16px } \
         .p { position: absolute; z-index: 0; background: #020000; width: 5px; height: 5px }</style>\
         <div class=a>x</div><div class=p></div>",
    );
    let seq: Vec<&str> = list
        .items
        .iter()
        .skip(1)
        .map(|i| match i {
            Item::FillRect { color, .. } if color.r == 1 => "bg",
            Item::FillRect { .. } => "pos",
            Item::GlyphRun { .. } => "text",
            _ => "?",
        })
        .collect();
    assert_eq!(seq, ["bg", "text", "pos"]);
}

#[test]
fn px_paint_text_glyph_list_is_deterministic() {
    let html = "<style>div { width: 60px; font-size: 16px }</style><div>one two three four</div>";
    assert_eq!(list_for(html), list_for(html));
}

#[test]
fn px_paint_text_in_a_100000_deep_document_builds_on_a_small_stack() {
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut html = String::new();
            for _ in 0..100_000 {
                // The DOM caps nesting at 512, so only the first levels keep
                // their text; the point is that the rest cannot overflow.
                html.push_str("<div>t");
            }
            runs(&list_for(&html)).len()
        })
        .expect("spawn");
    assert!(worker.join().expect("no overflow") > 0);
}

#[test]
fn px_paint_is_deterministic() {
    let html = "<style>div { width: 30px; height: 30px; background: #123456; \
                border: 1px solid #fff; overflow: hidden }</style>\
                <div>text <div>more</div></div><div></div>";
    assert_eq!(list_for(html), list_for(html));
}

/// 100,000 nested divs, on a deliberately small stack.
#[test]
fn px_paint_deep_nesting_builds_without_overflowing_the_stack() {
    const DEPTH: usize = 100_000;
    let worker = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut html = String::from("<style>div { background: #010203 }</style>");
            for _ in 0..DEPTH {
                html.push_str("<div>");
            }
            let list = list_for(&html);
            assert!(list.clips_balanced());
            list.len()
        })
        .expect("spawn");
    let len = worker.join().expect("no panic or overflow");
    assert!(len > 1);
}
