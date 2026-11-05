//! Display-list expectations for the inline gaps closed in Phase 9:
//! `text-decoration` lines, through the product path.
//!
//! Ahem at `10px/10px` (ascent 8, descent 2): a glyph is 10px wide, the baseline
//! is 8px below the line's top. A decoration is one pixel thick (a sixteenth of
//! 10px rounds to 1px, the minimum), an underline starts 1px below the baseline,
//! an overline at the top of the ascent and a line-through 3px above the baseline.

use px_layout::geom::px;
use px_paint::{Color, DisplayList, Item, Rect, build};

const UA: &str = "html, body, div, p { display: block } \
     head, style, script, title, meta, link { display: none } \
     body { margin: 0 } \
     div { font: 10px/10px Ahem }";

fn list_for(html: &str) -> DisplayList {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
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

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color { r, g, b, a: 255 }
}

/// Items after the canvas fill.
fn body(list: &DisplayList) -> Vec<Item> {
    list.items.iter().skip(1).copied().collect()
}

fn fills(list: &DisplayList) -> Vec<(Rect, Color)> {
    body(list)
        .into_iter()
        .filter_map(|i| match i {
            Item::FillRect { rect, color } => Some((rect, color)),
            _ => None,
        })
        .collect()
}

#[test]
fn px_paint_inline_gaps_underline_is_a_rect_under_the_baseline_below_the_glyphs() {
    let list = list_for("<div style='text-decoration: underline'>ab</div>");
    let items = body(&list);
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(
        items[0],
        Item::FillRect {
            rect: rect(0, 9, 20, 1),
            color: rgb(0, 0, 0)
        }
    );
    assert!(matches!(items[1], Item::GlyphRun { .. }), "text over it");
}

#[test]
fn px_paint_inline_gaps_overline_and_line_through_positions_and_order() {
    let list = list_for("<div style='text-decoration: overline line-through'>ab</div>");
    let items = body(&list);
    assert_eq!(items.len(), 3, "{items:?}");
    assert_eq!(
        items[0],
        Item::FillRect {
            rect: rect(0, 0, 20, 1),
            color: rgb(0, 0, 0)
        }
    );
    assert!(matches!(items[1], Item::GlyphRun { .. }));
    assert_eq!(
        items[2],
        Item::FillRect {
            rect: rect(0, 5, 20, 1),
            color: rgb(0, 0, 0)
        },
        "line-through is over the text"
    );
}

#[test]
fn px_paint_inline_gaps_decoration_colour_is_the_declaring_elements() {
    let list = list_for(
        "<div style='text-decoration: underline; text-decoration-color: #ff0000; color: #00ff00'>\
         a<span style='color: #0000ff'>b</span></div>",
    );
    // Both pieces are underlined in the div's decoration colour, not their text colour.
    assert_eq!(
        fills(&list),
        vec![
            (rect(0, 9, 10, 1), rgb(255, 0, 0)),
            (rect(10, 9, 10, 1), rgb(255, 0, 0)),
        ]
    );
}

#[test]
fn px_paint_inline_gaps_decoration_propagates_to_block_children_not_inline_blocks() {
    let list = list_for(
        "<div style='text-decoration: underline'><p>a</p>\
         <p style='display: inline-block'>b</p></div>",
    );
    let f = fills(&list);
    assert_eq!(f.len(), 1, "{f:?}");
    assert_eq!(f[0].0, rect(0, 9, 10, 1));
}

#[test]
fn px_paint_inline_gaps_a_span_adds_its_own_decoration_to_an_ancestors() {
    let list = list_for(
        "<div style='text-decoration: underline'>a<span style='text-decoration: line-through; \
         text-decoration-color: #ff0000'>b</span></div>",
    );
    let f = fills(&list);
    assert_eq!(
        f,
        vec![
            (rect(0, 9, 10, 1), rgb(0, 0, 0)),
            (rect(10, 9, 10, 1), rgb(0, 0, 0)),
            (rect(10, 5, 10, 1), rgb(255, 0, 0)),
        ]
    );
}

#[test]
fn px_paint_inline_gaps_decoration_follows_vertical_align() {
    let list = list_for(
        "<div style='text-decoration: underline'>a<span style='vertical-align: 5px'>b</span></div>",
    );
    let f = fills(&list);
    // The line grows to 15px (ascent 13), so the baseline is at 13: "a" underlines at
    // 14, "b" 5px higher.
    assert_eq!(f[0].0, rect(0, 14, 10, 1));
    assert_eq!(f[1].0, rect(10, 9, 10, 1), "raised 5px with its text");
}
