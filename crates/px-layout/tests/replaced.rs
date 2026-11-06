//! Replaced elements: `<img>` sizing (intrinsic size, `width`/`height`
//! attributes, CSS, ratio, min/max), placement as an atomic inline or a block,
//! shrink-to-fit contribution, and the `alt` fallback when there is no picture.
//!
//! Metrics: `div { font: 10px/10px Ahem }`, so a glyph is 10px wide, a line with
//! only text is 10px tall and its baseline is 8px down.

use std::collections::BTreeMap;

use app_units::Au;
use px_dom::NodeId;
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::px;
use px_layout::replaced::{layout_document_with_images, layout_document_with_pictures};

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
                    div { font: 10px/10px Ahem } head, style, title { display: none }";

struct Laid {
    tree: FragmentTree,
    arena: px_dom::Arena,
}

fn id_of(arena: &px_dom::Arena, id: &str) -> NodeId {
    let document = arena.document();
    core::iter::once(document)
        .chain(arena.descendants(document))
        .find(|n| {
            arena
                .get(*n)
                .and_then(|node| node.attrs())
                .is_some_and(|attrs| {
                    attrs
                        .iter()
                        .any(|a| &*a.name.local == "id" && &*a.value == id)
                })
        })
        .expect("an element with that id")
}

/// `images` are `(id, width, height)` in CSS px.
fn lay(body: &str, css: &str, images: &[(&str, u32, u32)]) -> Laid {
    let dom = px_dom::parse(&format!("<html><body>{body}</body></html>"));
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("a root element");
    let table: BTreeMap<NodeId, (u32, u32)> = images
        .iter()
        .map(|(id, w, h)| (id_of(&arena, id), (*w, *h)))
        .collect();
    let tree = layout_document_with_images(&arena, &root, px(800), None, &table).expect("laid out");
    Laid { tree, arena }
}

/// As `lay`, with the `ratio_only` elements marked (an SVG with a viewBox only).
fn lay_ratio(body: &str, css: &str, images: &[(&str, u32, u32)], ratio_only: &[&str]) -> Laid {
    let dom = px_dom::parse(&format!("<html><body>{body}</body></html>"));
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("a root element");
    let table: BTreeMap<NodeId, (u32, u32)> = images
        .iter()
        .map(|(id, w, h)| (id_of(&arena, id), (*w, *h)))
        .collect();
    let ro = ratio_only.iter().map(|id| id_of(&arena, id)).collect();
    let tree =
        layout_document_with_pictures(&arena, &root, px(800), None, &table, &ro).expect("laid out");
    Laid { tree, arena }
}

impl Laid {
    fn frag(&self, id: &str) -> (Au, Au, Au, Au) {
        let node = id_of(&self.arena, id);
        let f = self
            .tree
            .in_layout_order()
            .into_iter()
            .filter_map(|(_, i)| self.tree.get(i))
            .find(|f| f.node == Some(node))
            .expect("a fragment for that element");
        (f.inline_offset, f.block_offset, f.size.inline, f.size.block)
    }

    /// `(width, height)` in whole px.
    fn size(&self, id: &str) -> (i32, i32) {
        let (_, _, w, h) = self.frag(id);
        (w.0 / 60, h.0 / 60)
    }

    fn lines(&self) -> Vec<FragmentId> {
        self.tree
            .in_layout_order()
            .into_iter()
            .map(|(_, i)| i)
            .filter(|i| {
                self.tree
                    .get(*i)
                    .is_some_and(|f| f.kind == FragmentKind::Line)
            })
            .collect()
    }
}

#[test]
fn a_block_image_takes_its_intrinsic_size() {
    let l = lay("<img id=i>", "img { display: block }", &[("i", 40, 20)]);
    assert_eq!(l.size("i"), (40, 20));
}

#[test]
fn one_stated_side_makes_the_other_follow_the_ratio() {
    let l = lay(
        "<img id=a width=100><img id=b height=30><img id=c style='width:60px'><img id=d style='height:10px'>",
        "img { display: block }",
        &[("a", 40, 20), ("b", 40, 20), ("c", 40, 20), ("d", 40, 20)],
    );
    assert_eq!(l.size("a"), (100, 50));
    assert_eq!(l.size("b"), (60, 30));
    assert_eq!(l.size("c"), (60, 30));
    assert_eq!(l.size("d"), (20, 10));
}

#[test]
fn two_stated_sides_are_used_as_given_and_css_beats_attributes() {
    let l = lay(
        "<img id=a width=100 height=10><img id=b width=100 height=10 style='width:30px'>",
        "img { display: block }",
        &[("a", 40, 20), ("b", 40, 20)],
    );
    assert_eq!(l.size("a"), (100, 10));
    // The CSS width wins; the height attribute still applies.
    assert_eq!(l.size("b"), (30, 10));
}

#[test]
fn percentages_resolve_against_the_containing_block_width() {
    let l = lay(
        "<img id=a style='width:50%'><img id=b width='25%'>",
        "img { display: block }",
        &[("a", 40, 20), ("b", 40, 20)],
    );
    assert_eq!(l.size("a"), (400, 200));
    assert_eq!(l.size("b"), (200, 100));
}

#[test]
fn min_and_max_bounds_keep_the_ratio_on_an_auto_axis() {
    let l = lay(
        "<img id=a style='max-width:30px'><img id=b style='min-width:100px'>\
         <img id=c style='max-height:10px'><img id=d style='width:200px; max-width:50px'>",
        "img { display: block }",
        &[("a", 40, 20), ("b", 40, 20), ("c", 40, 20), ("d", 40, 20)],
    );
    assert_eq!(l.size("a"), (30, 15));
    assert_eq!(l.size("b"), (100, 50));
    assert_eq!(l.size("c"), (20, 10));
    // A stated width is still bounded; the auto height follows.
    assert_eq!(l.size("d"), (50, 25));
}

#[test]
fn border_box_sizing_takes_padding_and_border_out_of_a_stated_width() {
    let l = lay(
        "<img id=a style='width:100px; box-sizing:border-box; padding:10px; border:5px solid'>",
        "img { display: block }",
        &[("a", 40, 20)],
    );
    // Border box 100 wide: content 70, height by ratio 35, plus 30 of surround.
    assert_eq!(l.size("a"), (100, 65));
}

#[test]
fn an_inline_image_sits_on_the_baseline_of_its_line() {
    // Strut: 8 above the baseline, 2 below. The image is 20 tall on the
    // baseline, so the line is 20 above + 2 below = 22 tall.
    let l = lay("<div>ab<img id=i>cd</div>", "", &[("i", 30, 20)]);
    let (x, _, w, h) = l.frag("i");
    assert_eq!((x.0 / 60, w.0 / 60, h.0 / 60), (20, 30, 20));
    let lines = l.lines();
    assert_eq!(lines.len(), 1);
    let line = l.tree.get(lines[0]).expect("line");
    assert_eq!(line.size.block.0 / 60, 22);
    // The image's bottom edge is on the baseline: top = baseline - 20.
    let (_, y, _, _) = l.frag("i");
    assert_eq!(y.0, line.block_offset.0 + line.baseline.0 - 20 * 60);
}

#[test]
fn an_inline_image_wraps_like_a_word() {
    // 800px wide viewport; two 500px images cannot share a line.
    let l = lay(
        "<div><img id=a><img id=b></div>",
        "",
        &[("a", 500, 10), ("b", 500, 10)],
    );
    let (_, ya, _, _) = l.frag("a");
    let (xb, yb, _, _) = l.frag("b");
    assert_eq!(xb, Au(0));
    assert!(yb > ya);
    assert_eq!(l.lines().len(), 2);
}

#[test]
fn vertical_align_applies_to_an_inline_image() {
    let l = lay(
        "<div>a<img id=i style='vertical-align:middle'>b</div>",
        "",
        &[("i", 10, 40)],
    );
    // Middle: the image's midpoint is half an x-height above the baseline;
    // whatever the number, it is not the baseline case (bottom at baseline).
    let lines = l.lines();
    let line = l.tree.get(lines[0]).expect("line");
    let (_, y, _, h) = l.frag("i");
    assert_ne!(y.0 + h.0, line.block_offset.0 + line.baseline.0);
}

#[test]
fn a_floated_image_shrinks_to_its_size() {
    let l = lay(
        "<img id=i style='float:left; margin:5px'>",
        "",
        &[("i", 40, 20)],
    );
    let (x, y, w, h) = l.frag("i");
    assert_eq!((x.0 / 60, y.0 / 60, w.0 / 60, h.0 / 60), (5, 5, 40, 20));
}

#[test]
fn an_image_widens_a_shrink_to_fit_box() {
    let l = lay(
        "<div id=d style='float:left'><img id=i style='display:block'></div>",
        "",
        &[("i", 123, 20)],
    );
    assert_eq!(l.size("d"), (123, 20));
}

#[test]
fn auto_margins_centre_a_block_image() {
    let l = lay(
        "<img id=i style='margin:0 auto'>",
        "img { display: block }",
        &[("i", 100, 20)],
    );
    let (x, _, w, _) = l.frag("i");
    assert_eq!((x.0 / 60, w.0 / 60), (350, 100));
}

#[test]
fn without_a_picture_the_alt_text_is_the_content() {
    // No entry: the img is an inline element holding "abc".
    let l = lay("<div>x<img id=i alt='abc'>y</div>", "", &[]);
    let lines = l.lines();
    assert_eq!(lines.len(), 1);
    let width: i32 = l
        .tree
        .text_runs(lines[0])
        .iter()
        .map(|r| r.width.0 / 60)
        .sum();
    assert_eq!(width, 50, "x + abc + y at 10px per glyph");
}

#[test]
fn alt_is_ignored_when_there_is_a_picture_and_when_it_is_empty() {
    let with = lay("<div>x<img id=i alt='abc'>y</div>", "", &[("i", 10, 10)]);
    let w: i32 = with
        .tree
        .text_runs(with.lines()[0])
        .iter()
        .map(|r| r.width.0 / 60)
        .sum();
    // x, the atomic inline's object character, y: not the three of "abc".
    assert_eq!(w, 30);
    let empty = lay("<div>x<img id=i alt=''>y</div>", "", &[]);
    let w: i32 = empty
        .tree
        .text_runs(empty.lines()[0])
        .iter()
        .map(|r| r.width.0 / 60)
        .sum();
    assert_eq!(w, 20);
}

#[test]
fn layout_is_the_same_on_repeat_runs_and_the_table_is_cleared() {
    let a = lay("<div>a<img id=i>b</div>", "", &[("i", 33, 17)]);
    let b = lay("<div>a<img id=i>b</div>", "", &[("i", 33, 17)]);
    assert_eq!(a.frag("i"), b.frag("i"));
    // A later layout without the table sees no picture.
    let dom = px_dom::parse("<html><body><div><img id=i alt=zz></div>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(BASE, "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("root");
    let tree = px_layout::block::layout_document(&arena, &root, px(800)).expect("tree");
    let img = id_of(&arena, "i");
    assert!(
        tree.in_layout_order()
            .into_iter()
            .filter_map(|(_, i)| tree.get(i))
            .all(|f| f.node != Some(img) || f.kind != FragmentKind::Block),
        "no block fragment for an image that has no picture"
    );
}

#[test]
fn degenerate_pictures_and_huge_stated_sizes_do_not_break_layout() {
    let l = lay(
        "<img id=a style='width:99999999px'><img id=b width=99999999999999999999>",
        "img { display: block }",
        &[("a", 1, 1), ("b", 0, 5)],
    );
    let (w, h) = l.size("a");
    assert!(w <= 1_000_000 && h <= 1_000_000);
}

#[test]
fn a_ratio_only_picture_fills_the_containing_width() {
    // Intrinsic 300x150 (the ratio 2:1), but no size of its own: 800 wide.
    let l = lay_ratio(
        "<img id=i>",
        "img { display: block }",
        &[("i", 300, 150)],
        &["i"],
    );
    assert_eq!(l.size("i"), (800, 400));
    // Inside a 200px box it is 200 wide; a stated width still wins.
    let l = lay_ratio(
        "<div style='width:200px'><img id=a><img id=b width=50></div>",
        "img { display: block }",
        &[("a", 300, 150), ("b", 300, 150)],
        &["a", "b"],
    );
    assert_eq!(l.size("a"), (200, 100));
    assert_eq!(l.size("b"), (50, 25));
    // Without the mark, the intrinsic size.
    let l = lay(
        "<div style='width:200px'><img id=a></div>",
        "img { display: block }",
        &[("a", 300, 150)],
    );
    assert_eq!(l.size("a"), (300, 150));
    // Max-width bounds it, the ratio follows.
    let l = lay_ratio(
        "<img id=i style='max-width:100px'>",
        "img { display: block }",
        &[("i", 300, 150)],
        &["i"],
    );
    assert_eq!(l.size("i"), (100, 50));
}
