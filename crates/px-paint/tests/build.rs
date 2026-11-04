//! Display-list construction through the product path: parse HTML, style it,
//! lay it out, build. Expectations are derived by hand from the CSS in each test.

use px_layout::geom::{Au, px};
use px_paint::{BorderSide, BorderStyle, Color, DisplayList, Item, Rect, build};

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

#[test]
fn px_paint_text_becomes_a_run_in_the_computed_color() {
    let list = list_for("<style>div { color: #0000ff }</style><div>hi</div>");
    let runs: Vec<_> = list
        .items
        .iter()
        .filter(|i| matches!(i, Item::TextRun { .. }))
        .collect();
    assert_eq!(runs.len(), 1, "{:?}", list.items);
    match runs[0] {
        Item::TextRun { rect, color } => {
            assert_eq!(*color, rgb(0, 0, 255));
            assert_eq!((rect.x, rect.y), (px(0), px(0)));
            assert!(rect.width > Au(0) && rect.height > Au(0));
        }
        _ => unreachable!(),
    }
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
