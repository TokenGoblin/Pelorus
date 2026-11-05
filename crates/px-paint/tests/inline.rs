//! Display-list items for inline content: per-element glyph colour, and the
//! backgrounds and borders of inline boxes, through the product path.
//!
//! Ahem at `10px/10px`: a glyph is 10px wide, the content area 10px tall with the
//! baseline 8px down, and a line is 10px tall. Expectations are derived by hand
//! from the CSS in each test.

use px_layout::geom::px;
use px_paint::{
    BorderSide, BorderStyle, Color, DisplayList, FontId, Item, PositionedGlyph, Rect, build,
};

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

fn fill(r: Rect, c: Color) -> Item {
    Item::FillRect { rect: r, color: c }
}

/// `(color, glyph x positions in px)` of every glyph run.
fn runs(list: &DisplayList) -> Vec<(Color, Vec<i32>)> {
    list.items
        .iter()
        .filter_map(|i| match *i {
            Item::GlyphRun {
                color, start, len, ..
            } => Some((
                color,
                list.run_glyphs(start, len)
                    .expect("in bounds")
                    .iter()
                    .map(|g: &PositionedGlyph| g.x.0 / 60)
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

fn is_glyph_run(i: &Item) -> bool {
    matches!(i, Item::GlyphRun { font, .. } if *font == FontId(0))
}

#[test]
fn px_paint_inline_each_span_paints_its_text_in_its_own_colour() {
    let list = list_for(
        "<div style='color: #ff0000'>a <span style='color: #00ff00'>b</span> \
         <em style='color: #0000ff'>c</em></div>",
    );
    assert_eq!(
        runs(&list),
        vec![
            (rgb(255, 0, 0), vec![0, 10]),
            (rgb(0, 255, 0), vec![20]),
            (rgb(255, 0, 0), vec![30]),
            (rgb(0, 0, 255), vec![40]),
        ],
        "{:?}",
        list.items
    );
}

#[test]
fn px_paint_inline_a_colour_is_inherited_by_nested_spans() {
    let list = list_for("<div><span style='color: #00ff00'>a<b>b</b></span></div>");
    let colors: Vec<Color> = runs(&list).iter().map(|r| r.0).collect();
    assert_eq!(colors, vec![rgb(0, 255, 0), rgb(0, 255, 0)]);
}

#[test]
fn px_paint_inline_a_span_background_and_border_are_drawn_under_its_text() {
    // Edges: border 2 + padding 3 on each side. "ab" is at 5 and 15, the box is
    // 5 + 20 + 5 = 30 wide. Vertically the content area is the 10px font box at
    // y = 0 (8 up from the baseline), plus 2px border above and below: y = -2,
    // height 14.
    let list = list_for(
        "<div><span style='background: #ff0000; border: 2px solid #0000ff; padding: 0 3px'>ab</span></div>",
    );
    let side = BorderSide {
        width: px(2),
        style: BorderStyle::Solid,
        color: rgb(0, 0, 255),
    };
    assert_eq!(list.items.len(), 4, "{:?}", list.items);
    assert_eq!(list.items[1], fill(rect(0, -2, 30, 14), rgb(255, 0, 0)));
    assert_eq!(
        list.items[2],
        Item::Border {
            rect: rect(0, -2, 30, 14),
            sides: [side; 4],
        }
    );
    assert!(is_glyph_run(&list.items[3]), "the text is on top");
    assert_eq!(runs(&list)[0].1, vec![5, 15]);
}

#[test]
fn px_paint_inline_a_wrapped_span_owns_its_edges_only_at_its_ends() {
    // Text "aaa bbb ccc ddd" at 60px: lines "aaa", "bbb", "ccc", "ddd". The span
    // is "bbb ccc": its first piece (line 2) has the left border and no right, its
    // second (line 3) the right and no left.
    let list = list_for(
        "<div style='width: 60px'>aaa <span style='background: #ff0000; \
         border: 2px solid #0000ff'>bbb ccc</span> ddd</div>",
    );
    let side = |w: i32, style| BorderSide {
        width: px(w),
        style,
        color: rgb(0, 0, 255),
    };
    let solid = |w| side(w, BorderStyle::Solid);
    let none = side(0, BorderStyle::None);
    let items = &list.items[1..];
    // line 1 text, line 2 box + text, line 3 box + text, line 4 text.
    assert_eq!(items.len(), 8, "{items:?}");
    assert!(is_glyph_run(&items[0]));
    assert_eq!(items[1], fill(rect(0, 8, 32, 14), rgb(255, 0, 0)));
    assert_eq!(
        items[2],
        Item::Border {
            rect: rect(0, 8, 32, 14),
            sides: [solid(2), none, solid(2), solid(2)],
        }
    );
    assert!(is_glyph_run(&items[3]));
    assert_eq!(items[4], fill(rect(0, 18, 32, 14), rgb(255, 0, 0)));
    assert_eq!(
        items[5],
        Item::Border {
            rect: rect(0, 18, 32, 14),
            sides: [solid(2), solid(2), solid(2), none],
        }
    );
    assert!(is_glyph_run(&items[6]));
    assert!(is_glyph_run(&items[7]));
}

#[test]
fn px_paint_inline_hidden_span_paints_nothing_but_visible_children_do() {
    let list = list_for(
        "<div><span style='visibility: hidden; background: #ff0000'>a\
         <span style='visibility: visible'>b</span></span></div>",
    );
    assert!(
        !list
            .items
            .iter()
            .any(|i| matches!(i, Item::FillRect { color, .. } if color.r == 255 && color.g == 0)),
        "the hidden box's background is not drawn: {:?}",
        list.items
    );
    // Only "b" is drawn; its pen is after the hidden "a".
    assert_eq!(runs(&list), vec![(rgb(0, 0, 0), vec![10])]);
}

#[test]
fn px_paint_inline_an_inline_block_is_painted_after_the_text_of_its_line() {
    // The box is empty: its baseline is its bottom margin edge, 10 above the
    // line's baseline, so the line is 12 tall and the box is at y 0, x 10.
    let list = list_for(
        "<div>a<span style='display: inline-block; width: 20px; height: 10px; \
         background: #ff0000'></span>b</div>",
    );
    assert_eq!(list.items.len(), 3, "{:?}", list.items);
    assert!(is_glyph_run(&list.items[1]));
    assert_eq!(list.items[2], fill(rect(10, 0, 20, 10), rgb(255, 0, 0)));
    // "a" at 0 and "b" after the 20px box at 30.
    assert_eq!(runs(&list)[0].1, vec![0, 30]);
}

#[test]
fn px_paint_inline_is_deterministic() {
    let html = "<div style='width: 90px'>aa <span style='background: #ff0000; border: 1px solid; \
                padding: 0 2px'>bb cc dd</span> <b style='color: #00f'>ee</b>\
                <span style='display: inline-block; background: #0f0'>ff<br>gg</span> hh</div>";
    assert_eq!(list_for(html), list_for(html));
}
