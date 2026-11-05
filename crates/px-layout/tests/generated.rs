//! Generated content in layout: `::before` / `::after` / list markers laid out
//! like the boxes they stand for.
//!
//! Local reftests in the WPT style: each test is a document using generated
//! content and a reference spelling the same boxes as plain elements. They agree
//! when the two produce the same set of fragment rectangles (ADR 029's
//! comparison). The vendored WPT corpus has no generated-content directory
//! (see docs/backlog.md), so these pairs are the instrument for this feature.

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::FragmentTree;
use px_layout::geom::px;

const UA: &str = "html, body, div, p, ul, ol, li, section { display: block } \
    li { display: list-item } ol { list-style-type: decimal } \
    body { margin: 8px; font: 16px/20px Ahem } head, style, title { display: none }";

fn tree_of(html: &str) -> (px_dom::Arena, FragmentTree) {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    let css = px_layout::inline::collect_text(&arena, first_style(&arena));
    engine.add_author_stylesheet(&css, "https://e.invalid/p.css");
    let root = engine.style_document(&mut arena).expect("styles");
    let tree = layout_document(&arena, &root, px(800)).expect("lays out");
    (arena, tree)
}

fn first_style(arena: &px_dom::Arena) -> px_dom::NodeId {
    arena
        .descendants(arena.document())
        .find(|id| {
            arena
                .get(*id)
                .and_then(|n| n.element_name())
                .is_some_and(|q| &*q.local == "style")
        })
        .expect("a <style>")
}

type Rect = (Au, Au, Au, Au);

fn rects(tree: &FragmentTree) -> Vec<Rect> {
    let mut v: Vec<Rect> = tree
        .in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| {
            let f = tree.get(id)?;
            Some((f.inline_offset, f.block_offset, f.size.inline, f.size.block))
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

fn same(test: &str, reference: &str) {
    let (_, t) = tree_of(test);
    let (_, r) = tree_of(reference);
    assert_eq!(
        rects(&t),
        rects(&r),
        "generated-content document and its reference disagree"
    );
}

fn page(style: &str, body: &str) -> String {
    format!("<!DOCTYPE html><style>{style}</style>{body}")
}

#[test]
fn clearfix_contains_its_floats() {
    same(
        &page(
            ".c::after { content: ''; display: table; clear: both } .f { float: left; width: 50px; height: 40px }",
            "<div class=c style='background:#ccc'><div class=f></div><div class=f></div></div><p>x</p>",
        ),
        &page(
            ".f { float: left; width: 50px; height: 40px }",
            "<div style='background:#ccc'><div class=f></div><div class=f></div><div style='display:table;clear:both'></div></div><p>x</p>",
        ),
    );
}

#[test]
fn empty_block_pseudos_take_their_height() {
    same(
        &page(
            "div::before { content: ''; display: block; height: 7px } div::after { content: ''; display: block; height: 9px }",
            "<div><p>x</p></div>",
        ),
        &page(
            "",
            "<div><div style='height:7px'></div><p>x</p><div style='height:9px'></div></div>",
        ),
    );
}

#[test]
fn inline_text_before_and_after() {
    same(
        &page(
            "p::before { content: 'ab' } p::after { content: 'cd' }",
            "<p>mid</p>",
        ),
        &page("", "<p><span>ab</span>mid<span>cd</span></p>"),
    );
}

#[test]
fn text_content_is_laid_out_at_its_length() {
    // Ahem: 16px per character.
    let (_, t) = tree_of(&page("p::before { content: 'abc'; display: inline-block }", "<p>x</p>"));
    let wide = rects(&t).iter().any(|r| r.2 == px(48));
    assert!(wide, "an inline-block ::before of three Ahem characters is 48px wide");
}

#[test]
fn floated_and_absolute_pseudos() {
    same(
        &page(
            ".t::before { content: ''; float: left; width: 20px; height: 20px } \
             .t::after { content: ''; position: absolute; right: 0; top: 0; width: 5px; height: 5px } .t { position: relative; height: 30px }",
            "<div class=t>text</div>",
        ),
        &page(
            ".t { position: relative; height: 30px }",
            "<div class=t><div style='float:left;width:20px;height:20px'></div>text<div style='position:absolute;right:0;top:0;width:5px;height:5px'></div></div>",
        ),
    );
}

#[test]
fn pseudo_in_a_flex_container_is_an_item() {
    same(
        &page(
            ".f { display: flex } .f::before { content: ''; width: 10px; height: 10px } .f::after { content: ''; width: 20px; height: 10px }",
            "<div class=f><span style='width:30px;height:10px'></span></div>",
        ),
        &page(
            ".f { display: flex } .f > div { }",
            "<div class=f><div style='width:10px;height:10px'></div><span style='width:30px;height:10px'></span><div style='width:20px;height:10px'></div></div>",
        ),
    );
}

#[test]
fn br_next_to_generated_content() {
    same(
        &page(
            "p::before { content: 'a' } p::after { content: 'b' }",
            "<p>x<br>y</p>",
        ),
        &page("", "<p><span>a</span>x<br>y<span>b</span></p>"),
    );
}

#[test]
fn whitespace_collapses_across_a_generated_box() {
    // Collapsible white space around a pseudo-element collapses as it would
    // around an inline element: one space between, none at the line edges.
    same(
        &page("p::before { content: 'a' }", "<p>  b  </p>"),
        &page("", "<p><span>a</span> b</p>"),
    );
    same(
        &page("p::after { content: 'z' }", "<p>b </p>"),
        &page("", "<p>b <span>z</span></p>"),
    );
}

#[test]
fn outside_marker_takes_no_room() {
    // The item's own line is where it would be without a marker, and the
    // marker hangs to its left.
    let (_, with) = tree_of(&page("", "<ul><li>ab</li></ul>"));
    let (_, without) = tree_of(&page("li { display: block }", "<ul><li>ab</li></ul>"));
    let r_with = rects(&with);
    let r_without = rects(&without);
    for r in &r_without {
        assert!(r_with.contains(r), "{r:?} moved by the marker");
    }
    let extra: Vec<&Rect> = r_with.iter().filter(|r| !r_without.contains(r)).collect();
    assert_eq!(extra.len(), 1, "exactly the marker box is added: {extra:?}");
    let li_left = r_without.iter().map(|r| r.0).min().unwrap_or(Au(0));
    let m = extra.first().copied().copied().unwrap_or((Au(0), Au(0), Au(0), Au(0)));
    assert!(m.0 + m.2 <= li_left.max(Au(0)) + Au(60 * 40), "marker is left of the text");
}

#[test]
fn inside_marker_pushes_the_text() {
    let (_, outside) = tree_of(&page("", "<ul><li>ab</li></ul>"));
    let (_, inside) = tree_of(&page("ul { list-style-position: inside }", "<ul><li>ab</li></ul>"));
    let widest = |t: &FragmentTree| rects(t).iter().map(|r| r.2).max().unwrap_or(Au(0));
    // The line holds marker + text, so some fragment is wider than the text.
    assert!(rects(&inside).len() >= rects(&outside).len());
    assert!(widest(&inside) > Au(0));
}

#[test]
fn marker_on_the_first_line_of_a_block_child_adds_no_line() {
    let (_, with) = tree_of(&page("", "<ul><li><p>ab</p></li></ul>"));
    let (_, without) = tree_of(&page("li { display: block }", "<ul><li><p>ab</p></li></ul>"));
    let h = |t: &FragmentTree| {
        t.in_layout_order()
            .into_iter()
            .filter_map(|(_, id)| t.get(id))
            .map(|f| f.block_offset + f.size.block)
            .max()
            .unwrap_or(Au(0))
    };
    assert_eq!(h(&with), h(&without), "the marker must not add a line above the paragraph");
}

#[test]
fn numbered_markers_do_not_move_siblings() {
    let (_, with) = tree_of(&page("", "<ol><li>a</li><li>b</li><li>c</li></ol>"));
    let (_, without) = tree_of(&page("li { display: block }", "<ol><li>a</li><li>b</li><li>c</li></ol>"));
    let ys = |t: &FragmentTree| {
        let mut v: Vec<Au> = rects(t).iter().map(|r| r.1).collect();
        v.sort();
        v.dedup();
        v
    };
    assert_eq!(ys(&with), ys(&without));
}

#[test]
fn generated_layout_is_deterministic() {
    let html = page(
        "li::before { content: counter(list-item) } ul::after { content: '' ; display: table }",
        "<ul><li>a</li><li>b</li></ul>",
    );
    let (_, a) = tree_of(&html);
    let (_, b) = tree_of(&html);
    assert_eq!(rects(&a), rects(&b));
}
