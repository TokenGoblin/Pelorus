//! End to end through the product path: parse, style, lay out, then build the
//! accessibility tree from the real `StyleRoot` and `FragmentTree`.
//!
//! This file doubles as the reference adapter. `px-a11y` itself depends on
//! `px-dom` only and reads style and geometry through [`StyleSource`] and
//! [`BoundsSource`]; the two adapters below are everything a crate that
//! depends on `px-css` and `px-layout` needs to write.
//!
//! One thing the adapter cannot supply yet: `px_css::paint::PaintStyle` does
//! not expose `display`, so `display: none` set by a style sheet cannot be
//! reported through [`StyleHints`]. The hidden attribute, `aria-hidden` and
//! the user-agent never-rendered list still apply, and `visibility` does flow
//! through. The gap is recorded in docs/backlog.md.

mod common;

use std::collections::HashMap;

use px_a11y::{AccessibilityTree, Bounds, BuildInputs, DumpOptions, StyleHints};
use px_dom::NodeId;
use px_layout::fragment::FragmentTree;
use px_layout::geom::{Au, px};

const UA: &str = "html, body, div, p, ul, li, h1, button, section { display: block } \
     head, style, script, title, meta, link { display: none } \
     body { margin: 0 }";

fn to_px(a: Au) -> i32 {
    // Round half up from 1/60 px.
    (a.0 + 30).div_euclid(60)
}

/// Border boxes of the fragments that belong to a DOM node.
fn bounds_from(tree: &FragmentTree) -> HashMap<NodeId, Bounds> {
    let mut out: HashMap<NodeId, Bounds> = HashMap::new();
    for (_, id) in tree.in_layout_order() {
        let Some(f) = tree.get(id) else { continue };
        let Some(node) = f.node else { continue };
        // `inline_offset` / `block_offset` are the x / y of a horizontal-tb
        // box; the first fragment of a node wins.
        out.entry(node).or_insert(Bounds {
            x: to_px(f.inline_offset),
            y: to_px(f.block_offset),
            width: to_px(f.size.inline),
            height: to_px(f.size.block),
        });
    }
    out
}

fn build(html: &str) -> (px_dom::Dom, AccessibilityTree) {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    let document = dom.arena.document();
    for id in dom.arena.descendants(document) {
        let is_style = dom
            .arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_some_and(|q| q.local.as_ref() == "style");
        if is_style {
            engine
                .add_author_stylesheet(&px_layout::inline::collect_text(&dom.arena, id), "x:test");
        }
    }
    let root = engine.style_root_for(&dom.arena);
    assert!(engine.resolve(&dom.arena, &root).is_some());
    let Some(fragments) = px_layout::block::layout_document(&dom.arena, &root, px(800)) else {
        unreachable!("layout of a parsed page succeeds");
    };

    let bounds = bounds_from(&fragments);
    let mut styles: HashMap<NodeId, StyleHints> = HashMap::new();
    let styled = px_css::view::with_dom(&dom.arena, &root, |view| {
        for id in dom.arena.descendants(document) {
            if let Some(p) = px_css::paint::paint_style(view, id) {
                styles.insert(
                    id,
                    StyleHints {
                        display_none: false,
                        visibility_hidden: !p.visible,
                    },
                );
            }
        }
    });
    assert!(styled.is_some());

    let tree = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: Some(&styles),
            bounds: Some(&bounds),
        },
    );
    (dom, tree)
}

#[test]
fn a11y_layout_bounds_come_from_fragments() {
    let (_, tree) = build(
        "<style>#a { width: 100px; height: 40px } #b { width: 60px; height: 20px }</style>\
         <button id=a>A</button><button id=b>B</button>",
    );
    let dump = tree.dump_with(DumpOptions { bounds: true });
    // Two stacked blocks: a at the origin, b below it.
    assert!(
        dump.contains("button name=\"A\" [focusable] @0,0 100x40"),
        "{dump}"
    );
    assert!(
        dump.contains("button name=\"B\" [focusable] @0,40 60x20"),
        "{dump}"
    );
}

#[test]
fn a11y_layout_visibility_hidden_flows_through_computed_style() {
    let (_, tree) = build(
        "<style>.v { visibility: hidden } .s { visibility: visible }</style>\
         <div class=v><button>hidden</button><button class=s>shown</button></div>\
         <p>after</p>",
    );
    let dump = common::strip_root(&tree.dump());
    // `visibility` inherits: the first button is hidden with its parent, the
    // second sets visible again and stays.
    assert_eq!(
        dump,
        "button name=\"shown\" [focusable]\nparagraph\n  text \"after\"\n"
    );
}

#[test]
fn a11y_layout_tree_and_dump_are_stable_across_builds() {
    let html = "<style>div { width: 50px; height: 10px }</style><h1>T</h1><ul><li>a</li></ul>";
    let (dom, first) = build(html);
    let again = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    // Same structure; bounds differ only because the second build had none.
    assert_eq!(first.dump(), again.dump());
}
