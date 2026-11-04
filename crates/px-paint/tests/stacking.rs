//! CSS 2.1 Appendix E painting order, through the product path.
//!
//! Each box has a distinct red channel (`#0N0000`), so an expectation is the list
//! of `N` in the order the fills are emitted. Derived by hand from the CSS.

use px_layout::geom::px;
use px_paint::{DisplayList, Item, build};

const UA: &str = "html, body, div, p { display: block } \
     head, style, script, title, meta, link { display: none } \
     body { margin: 0 } \
     .p { position: absolute; width: 50px; height: 50px; left: 0; top: 0 }";

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

/// The paint sequence: `N` for a fill of `#0N0000`, `T` for a text run, after the
/// canvas fill.
fn order(list: &DisplayList) -> String {
    let mut out = Vec::new();
    for item in list.items.iter().skip(1) {
        match item {
            Item::FillRect { color, .. } => out.push(color.r.to_string()),
            Item::TextRun { .. } => out.push("T".to_owned()),
            _ => {}
        }
    }
    out.join(",")
}

#[test]
fn px_paint_stacking_higher_z_index_paints_later_whatever_the_tree_order() {
    let a_low = list_for(
        "<style>#a { background: #010000; z-index: 1 } #b { background: #020000; z-index: 2 }</style>\
         <div class=p id=a></div><div class=p id=b></div>",
    );
    assert_eq!(order(&a_low), "1,2");
    let a_high = list_for(
        "<style>#a { background: #010000; z-index: 2 } #b { background: #020000; z-index: 1 }</style>\
         <div class=p id=a></div><div class=p id=b></div>",
    );
    assert_eq!(order(&a_high), "2,1");
}

#[test]
fn px_paint_stacking_negative_z_index_sits_behind_in_flow_backgrounds() {
    // #n is inside #f, which has a background; #n's context is the root's, and
    // negative z-index children paint (step 2) before in-flow blocks (step 3).
    let list = list_for(
        "<style>#f { background: #010000; width: 100px; height: 100px } \
         #n { background: #020000; z-index: -1 }</style>\
         <div id=f><div class=p id=n></div></div>",
    );
    assert_eq!(order(&list), "2,1");
}

#[test]
fn px_paint_stacking_float_is_above_block_backgrounds_and_below_inline_text() {
    // The float comes first in tree order and overlaps the block; Appendix E puts
    // block backgrounds (3) under floats (4) under inline content (5).
    let list = list_for(
        "<style>#fl { float: left; width: 50px; height: 50px; background: #020000 } \
         #bl { width: 200px; background: #010000 }</style>\
         <div id=fl></div><div id=bl>hello</div>",
    );
    assert_eq!(order(&list), "1,2,T");
}

#[test]
fn px_paint_stacking_nested_contexts_isolate_their_children() {
    // #c has z-index 100 but lives in #a's context (z 1); #b (z 2) is a sibling
    // context and so paints after everything in #a, #c included.
    let list = list_for(
        "<style>#a { background: #010000; z-index: 1 } #b { background: #020000; z-index: 2 } \
         #c { background: #030000; z-index: 100 }</style>\
         <div class=p id=a><div class=p id=c></div></div><div class=p id=b></div>",
    );
    assert_eq!(order(&list), "1,3,2");
}

#[test]
fn px_paint_stacking_ties_break_in_tree_order() {
    // z auto, z 0 and an equal positive z: positioned auto/0 in tree order (step
    // 6), then positive contexts, ties in tree order (step 7).
    let list = list_for(
        "<style>#a { background: #010000; z-index: 3 } #b { background: #020000 } \
         #c { background: #030000; z-index: 3 } #d { background: #040000; z-index: 0 }</style>\
         <div class=p id=a></div><div class=p id=b></div>\
         <div class=p id=c></div><div class=p id=d></div>",
    );
    assert_eq!(order(&list), "2,4,1,3");
}

#[test]
fn px_paint_stacking_positioned_auto_descendant_joins_the_enclosing_context() {
    // #p is positioned z auto; its positioned child #q is painted in the root's
    // step 6 in tree order, after #p, and before the later sibling #r.
    let list = list_for(
        "<style>#p { background: #010000 } #q { background: #020000 } \
         #r { background: #030000 }</style>\
         <div class=p id=p><div class=p id=q></div></div><div class=p id=r></div>",
    );
    assert_eq!(order(&list), "1,2,3");
}

#[test]
fn px_paint_stacking_clips_stay_balanced_across_reordered_phases() {
    let list = list_for(
        "<style>#o { overflow: hidden; width: 100px; height: 100px; background: #010000 } \
         #n { background: #020000; z-index: -1 } #t { float: left; width: 10px; height: 10px; background: #030000 }</style>\
         <div id=o><div class=p id=n></div><div id=t></div>text</div>",
    );
    assert!(list.clips_balanced());
}
