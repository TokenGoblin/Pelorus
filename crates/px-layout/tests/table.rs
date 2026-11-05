//! Tables (CSS 2.1 chapter 17), unit tests.
//!
//! Every expectation is worked out by hand from the spec's algorithm in the
//! comment above the test. Rectangles are `(x, y, width, height)` in whole
//! pixels, absolute, for every block-level fragment at or below the table, in
//! layout order. The font is Ahem at 10px (one em per glyph) with 10px lines, so a
//! character is 10px wide.

use px_layout::block::layout_document;
use px_layout::fragment::FragmentKind;
use px_layout::geom::px;

type R = (i32, i32, i32, i32);

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
     * { font: 10px/10px Ahem } \
     table { border-spacing: 0 } td, th { padding: 0 }";

fn page(html: &str, css: &str) -> Vec<(FragmentKind, R)> {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let style_root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &style_root)
        .expect("the document has a root element");
    let tree = layout_document(&arena, &style_root, px(800)).expect("a root element exists");
    let au = |v: app_units::Au| v.0 / 60;
    tree.in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| tree.get(id))
        .filter(|f| matches!(f.kind, FragmentKind::Block | FragmentKind::AnonymousBlock))
        .map(|f| {
            (
                f.kind,
                (
                    au(f.inline_offset),
                    au(f.block_offset),
                    au(f.size.inline),
                    au(f.size.block),
                ),
            )
        })
        .collect()
}

/// The rectangles below `<body>` (the first three are the ICB, html and body).
fn rects(html: &str, css: &str) -> Vec<R> {
    page(&format!("<html><body>{html}</body></html>"), css)
        .into_iter()
        .skip(3)
        .map(|(_, r)| r)
        .collect()
}

#[test]
fn table_scratch() {
    let cases: &[(&str, &str)] = &[
        ("<table style='border-spacing:5px'><tr><td style='width:50px;height:20px;padding:3px'>a</table>", ""),
        ("<table><tr><td colspan=2 style='width:100px;height:10px'>a<tr><td style='width:30px'>b<td>c</table>", ""),
        ("<table><tr><td rowspan=2 style='height:50px;width:10px'>a<td style='height:10px'>b<tr><td>c</table>", ""),
        ("<table><caption style='height:15px'>cap</caption><tr><td style='width:40px;height:10px'>a</table>", ""),
        ("<table style='width:300px'><tr><td>aaaa<td>bb</table>", ""),
        ("<table style='width:300px;table-layout:fixed'><tr><td style='width:50px'>aaaaaaaaaaaaaaaaaaa<td>bb<td>c</table>", ""),
        ("<table><tr style='height:40px'><td style='vertical-align:bottom'>a<td style='vertical-align:middle'>b<td style='vertical-align:top'>c</table>", ""),
        ("<table style='border-collapse:collapse'><tr><td style='border:4px solid;width:20px;height:10px'>a<td style='border:2px solid;width:20px'>b</table>", ""),
        ("<div style='display:table-cell;width:30px'>x</div>", ""),
        ("<span style='display:inline-table'><span style='display:table-cell;width:30px'>x</span></span>", ""),
        ("<table style='width:200px'><tr><td style='width:25%'>a<td>b</table>", ""),
        ("<table border=1 cellpadding=2 cellspacing=3><tr><td>ab</table>", ""),
    ];
    for (h, c) in cases {
        eprintln!("{h}\n   {:?}", rects(h, c));
    }
}
