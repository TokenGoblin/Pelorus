//! Generated content through the product path: style, layout and display list.
//! Expectations are worked out by hand from the CSS in each test.

use px_layout::geom::px;
use px_paint::{DisplayList, Item, build};

const UA: &str = "html, body, div, ul, li { display: block } li { display: list-item } \
     head, style, script, title, meta, link { display: none } body { margin: 0 }";

fn list_for(html: &str) -> DisplayList {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
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
    let root = engine.style_document(&mut arena).expect("styled");
    let tree = px_layout::block::layout_document(&arena, &root, px(800)).expect("laid out");
    build(&arena, &root, &tree)
}

fn fills(l: &DisplayList) -> usize {
    l.items
        .iter()
        .filter(|i| matches!(i, Item::FillRect { .. } | Item::FillRoundedRect { .. }))
        .count()
}

#[test]
fn a_pseudo_element_background_is_painted() {
    let plain = list_for("<style>div{width:50px;height:20px}</style><div></div>");
    let l = list_for(
        "<style>div{width:50px;height:20px} \
         div::before{content:'';display:block;width:10px;height:10px;background:#f00}</style><div></div>",
    );
    assert_eq!(fills(&l), fills(&plain) + 1);
}

#[test]
fn a_disc_marker_is_a_rounded_fill_and_a_square_is_a_plain_one() {
    let disc = list_for(
        "<style>ul{padding-left:40px;margin:0;list-style-type:disc}</style><ul><li></li></ul>",
    );
    assert_eq!(
        disc.items
            .iter()
            .filter(|i| matches!(i, Item::FillRoundedRect { .. }))
            .count(),
        1
    );
    let square = list_for(
        "<style>ul{padding-left:40px;margin:0;list-style-type:square}</style><ul><li></li></ul>",
    );
    assert_eq!(
        square
            .items
            .iter()
            .filter(|i| matches!(i, Item::FillRoundedRect { .. }))
            .count(),
        0
    );
    let none = list_for(
        "<style>ul{padding-left:40px;margin:0;list-style-type:none}</style><ul><li></li></ul>",
    );
    assert_eq!(fills(&square), fills(&none) + 1);
}

#[test]
fn list_style_none_paints_no_marker() {
    let l = list_for("<style>ul{list-style:none}</style><ul><li></li></ul>");
    let bare = list_for("<style></style><div></div>");
    assert_eq!(fills(&l), fills(&bare));
}

#[test]
fn a_numbered_marker_paints_text() {
    let numbered = list_for(
        "<style>ol{list-style-type:decimal;padding-left:40px}</style><ol style='display:block'><li>x</li></ol>",
    );
    let none = list_for(
        "<style>ol{list-style-type:none;padding-left:40px}</style><ol style='display:block'><li>x</li></ol>",
    );
    let glyphs = |l: &DisplayList| {
        l.items
            .iter()
            .filter(|i| matches!(i, Item::GlyphRun { .. } | Item::TextRun { .. }))
            .count()
    };
    assert_eq!(glyphs(&numbered), glyphs(&none) + 1);
}
