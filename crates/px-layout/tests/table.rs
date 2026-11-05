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

fn laid_out(html: &str, css: &str) -> Vec<(FragmentKind, R)> {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(css, "https://example.invalid/a.css");
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
fn t(html: &str, css: &str) -> Vec<R> {
    laid_out(
        &format!("<html><body>{html}</body></html>"),
        &format!("{BASE} {css}"),
    )
    .into_iter()
    .skip(3)
    .map(|(_, r)| r)
    .collect()
}

// Two cells side by side. Widths 50 and 70 (content-box, padding 0), no spacing.
// The table is 120 wide; the first row is as tall as its tallest cell, 20.
#[test]
fn layout_table_cells_sit_side_by_side() {
    let r = t(
        "<table><tr><td style='width:50px;height:20px'>a<td style='width:70px'>b</table>",
        "",
    );
    assert_eq!(
        r,
        vec![
            (0, 0, 120, 20),
            (0, 0, 120, 20),
            (0, 0, 120, 20),
            (0, 0, 50, 20),
            (50, 0, 70, 20)
        ]
    );
}

// border-spacing 5 on every side; the cell is 50 + 2*3 padding = 56 wide and
// 20 + 6 = 26 tall. One column, so spacing on both sides: 56 + 10 = 66 wide.
#[test]
fn layout_table_border_spacing_surrounds_the_cells() {
    let r = t(
        "<table style='border-spacing:5px'><tr><td style='width:50px;height:20px;padding:3px'>a</table>",
        "",
    );
    assert_eq!(r[0], (0, 0, 66, 36));
    assert_eq!(r[3], (5, 5, 56, 26));
}

// A cell with colspan=2 and width 100 over two columns whose second-row cells ask
// for 30 and (auto) 10: the spanning cell's 100 is shared out so the cell with a
// stated width keeps its 30 and the other takes the rest, 70.
#[test]
fn layout_table_colspan_gives_the_shortfall_to_unstated_columns() {
    let r = t(
        "<table><tr><td colspan=2 style='width:100px;height:10px'>a\
         <tr><td style='width:30px'>b<td>c</table>",
        "",
    );
    assert_eq!(r[0].2, 100);
    assert_eq!(r[5], (0, 10, 30, 10));
    assert_eq!(r[6], (30, 10, 70, 10));
}

// rowspan=2 with height 50 over two 10px-content rows: the shortfall (50 - 20) is
// shared equally, so each row is 25.
#[test]
fn layout_table_rowspan_shortfall_is_shared_between_rows() {
    let r = t(
        "<table><tr><td rowspan=2 style='height:50px;width:10px'>a<td style='height:10px'>b\
         <tr><td>c</table>",
        "",
    );
    assert_eq!(r[0], (0, 0, 20, 50));
    assert_eq!(r[3], (0, 0, 10, 50));
    assert_eq!(r[4], (10, 0, 10, 25));
    assert_eq!(r[6], (10, 25, 10, 25));
}

// A top caption sits above the grid box and the wrapper contains both.
#[test]
fn layout_table_caption_goes_above_the_grid() {
    let r = t(
        "<table><caption style='height:15px'>cap</caption><tr><td style='width:40px;height:10px'>a</table>",
        "",
    );
    assert_eq!(r[0], (0, 0, 40, 25));
    assert_eq!(r[1], (0, 0, 40, 15));
    assert_eq!(r[2], (0, 15, 40, 10));
}

#[test]
fn layout_table_caption_side_bottom_goes_below_the_grid() {
    let r = t(
        "<table><caption style='height:15px;caption-side:bottom'>cap</caption><tr><td style='width:40px;height:10px'>a</table>",
        "",
    );
    assert_eq!(r[0], (0, 0, 40, 25));
    assert_eq!(r[1], (0, 0, 40, 10));
    assert_eq!(r[5], (0, 10, 40, 15));
}

// width:300 with auto columns whose max-content widths are 40 ("aaaa") and 20
// ("bb"): the 300 is shared in proportion to max-content, 200 and 100.
#[test]
fn layout_table_auto_layout_shares_the_width_by_max_content() {
    let r = t("<table style='width:300px'><tr><td>aaaa<td>bb</table>", "");
    assert_eq!(r[3], (0, 0, 200, 10));
    assert_eq!(r[4], (200, 0, 100, 10));
}

// Fixed layout takes widths from the first row only: 50 for the first cell, the
// other 250 split equally between the two unstated columns.
#[test]
fn layout_table_fixed_layout_ignores_content() {
    let r = t(
        "<table style='width:300px;table-layout:fixed'><tr><td style='width:50px'>aaaaaaaaaaaaaaaaaaa<td>bb<td>c</table>",
        "",
    );
    assert_eq!(r[3], (0, 0, 50, 10));
    assert_eq!(r[4], (50, 0, 125, 10));
    assert_eq!(r[5], (175, 0, 125, 10));
}

// A 25% column of a 200px table is 50px.
#[test]
fn layout_table_percentage_columns_take_their_share() {
    let r = t(
        "<table style='width:200px'><tr><td style='width:25%'>a<td>b</table>",
        "",
    );
    assert_eq!(r[3], (0, 0, 50, 10));
    assert_eq!(r[4], (50, 0, 150, 10));
}

// Collapsed borders: the 4px border at the table's left edge is half outside the
// table (2) and half inside the cell (2); the shared 4px line between A and B is
// 2 + 2; B's own 2px right border is 1 + 1. A: 2 + 20 + 2 = 24, B: 2 + 20 + 1 = 23,
// table: 2 + 24 + 23 + 1 = 50; height 14 + 2 + 2.
#[test]
fn layout_table_collapsed_borders_are_shared_and_halved() {
    let r = t(
        "<table style='border-collapse:collapse'><tr>\
         <td style='border:4px solid;width:20px;height:10px'>a\
         <td style='border:2px solid;width:20px'>b</table>",
        "",
    );
    assert_eq!(r[0], (0, 0, 50, 18));
    assert_eq!(r[3], (2, 2, 24, 14));
    assert_eq!(r[4], (26, 2, 23, 14));
}

// A `display: table-cell` box with no table around it gets an anonymous table,
// row group and row (17.2.1): table, group, row and the cell.
#[test]
fn layout_table_anonymous_boxes_wrap_stray_cells() {
    let r = t("<div style='display:table-cell;width:30px'>x</div>", "");
    assert_eq!(r.len(), 4);
    assert_eq!(r[0], (0, 0, 30, 10));
}

// Text directly in a table row is wrapped in an anonymous cell.
#[test]
fn layout_table_text_in_a_row_becomes_a_cell() {
    let r = t(
        "<div style='display:table'><div style='display:table-row'>abc</div></div>",
        "",
    );
    assert_eq!(r[3], (0, 0, 30, 10));
}

#[test]
fn layout_table_inline_table_sits_on_the_line() {
    let r = t(
        "<span style='display:inline-table'><span style='display:table-cell;width:30px'>y</span></span>",
        "",
    );
    assert_eq!(r[0].2, 30);
}

// Margins `auto` on a table centre it, whatever its width: (800 - 120) / 2.
#[test]
fn layout_table_auto_margins_centre_it() {
    let r = t(
        "<table style='margin:0 auto'><tr><td style='width:120px'>a</table>",
        "",
    );
    assert_eq!(r[0].0, 340);
}

// A deeply nested table must not overflow the stack: the walk is iterative and
// the intrinsic measure of nested tables is depth-limited.
#[test]
fn layout_table_deep_nesting_does_not_overflow() {
    let depth = 150;
    let mut html = String::new();
    for _ in 0..depth {
        html.push_str("<table><tr><td>");
    }
    html.push('x');
    let child = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || t(&html, "").len())
        .expect("spawn");
    assert!(child.join().expect("no overflow") > depth);
}

// HTML presentational attributes: border=1 gives the table a 1px border and its
// cells a 1px inset border; cellpadding and cellspacing take effect. 'ab' is 20
// wide and 10 tall: cell 20 + 2*2 + 2*1 = 26 by 16; table adds spacing 3 and
// border 1 on each side: 34 by 24.
#[test]
fn layout_table_html_attributes_map_to_css() {
    let r = laid_out(
        "<html><body style='margin:0'><table border=1 cellpadding=2 cellspacing=3>\
         <tr><td>ab</table>",
        "html, body { display: block } * { font: 10px/10px Ahem }",
    );
    let table = r
        .into_iter()
        .map(|(_, r)| r)
        .nth(3)
        .expect("a table fragment");
    assert_eq!(table, (0, 0, 34, 24));
}
