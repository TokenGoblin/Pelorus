//! Phase 7: CSS Grid Layout, as geometry.
//!
//! Every expected rectangle below was worked out by hand from css-grid-1, and the
//! arithmetic is written next to it, so that a failure can be argued about from
//! the spec rather than from what the engine happens to print. `/CLAUDE.md`: do
//! not infer behaviour from another browser.
//!
//! # Conventions
//!
//! The fixture has no user-agent stylesheet, so every box says what it is. The
//! viewport is 800px wide, `body` has no margin, and every element has
//! `font-size: 10px; line-height: 20px`, which with the fixed-advance test font
//! (`src/text.rs`: half the font size per character) makes one character 5px wide
//! and one line 20px tall.
//!
//! A pixel is 60 app units. Most expected values are whole pixels and are written
//! with [`r`]; the ones that are not (`1fr` of 800px split three ways) are written
//! in app units with [`ra`], because the arithmetic is exact there and a rounded
//! pixel figure would be a different claim.

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::{au, px};

type Rect = (Au, Au, Au, Au);

/// A rectangle in whole pixels: x, y, width, height.
fn r(x: i32, y: i32, w: i32, h: i32) -> Rect {
    (px(x), px(y), px(w), px(h))
}

/// A rectangle in app units, for the cases whose figures are not whole pixels.
fn ra(x: i32, y: i32, w: i32, h: i32) -> Rect {
    (au(x), au(y), au(w), au(h))
}

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
     * { font-size: 10px; line-height: 20px } \
     #g { display: grid }";

/// Lay `<html><body>{body}</body></html>` out with `css` after [`BASE`].
fn lay(body: &str, css: &str) -> FragmentTree {
    let html = format!("<html><body>{body}</body></html>");
    let dom = px_dom::parse(&html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let style_root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &style_root)
        .expect("the document has a root element");
    layout_document(&arena, &style_root, px(800)).expect("a root element exists")
}

fn rect_of(tree: &FragmentTree, id: FragmentId) -> Rect {
    let f = tree.get(id).expect("a fragment the tree just named");
    (f.inline_offset, f.block_offset, f.size.inline, f.size.block)
}

/// Every fragment at `depth`, in layout order.
fn at_depth(tree: &FragmentTree, depth: usize) -> Vec<FragmentId> {
    tree.in_layout_order()
        .into_iter()
        .filter(|(d, _)| *d == depth)
        .map(|(_, id)| id)
        .collect()
}

/// The grid container (the only fragment at depth 3: ICB, html, body, container)
/// and its children's rectangles, in the order the tree lists them.
fn grid(tree: &FragmentTree) -> (Rect, Vec<Rect>) {
    let container = *at_depth(tree, 3)
        .first()
        .expect("a grid container at depth 3");
    let kids = tree
        .children(container)
        .iter()
        .map(|k| rect_of(tree, *k))
        .collect();
    (rect_of(tree, container), kids)
}

/// `n` empty boxes of the given inline style.
fn boxes(n: usize, style: &str) -> String {
    (0..n)
        .map(|_| format!("<div class=i style=\"{style}\"></div>"))
        .collect()
}

fn wrap(items: &str) -> String {
    format!("<div id=g>{items}</div>")
}

// --- Track sizes: fixed, flexible, percentage -------------------------------

/// Two fixed columns, rows sized by their items.
///
/// Columns 100 and 200. Four items 30px tall: each row is 30px (the tallest item,
/// §11.5 step 2), so the container is 60. A fixed track is its own base size and
/// growth limit (§11.4), and there is nothing `auto` for §11.8 to stretch.
#[test]
fn layout_grid_fixed_columns_and_auto_rows() {
    let tree = lay(
        &wrap(&boxes(4, "height:30px")),
        "#g { grid-template-columns: 100px 200px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 60));
    assert_eq!(
        items,
        vec![
            r(0, 0, 100, 30),
            r(100, 0, 200, 30),
            r(0, 30, 100, 30),
            r(100, 30, 200, 30)
        ]
    );
}

/// `1fr 2fr` in 800px.
///
/// Leftover space is 800px = 48000au; flex factor sum 3; one fr is 16000au, so the
/// columns are 16000au and 32000au (§7.2.3, §11.7.1).
#[test]
fn layout_grid_fr_tracks_share_the_free_space() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { grid-template-columns: 1fr 2fr }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![ra(0, 0, 16000, 600), ra(16000, 0, 32000, 600)]);
}

/// Fixed, flexible and gutters together.
///
/// `100px 1fr 2fr` with a 10px column gap in 800px. Two gutters are fixed tracks
/// (§11.1): leftover = 800 - 100 - 20 = 680px = 40800au, divided by 3 gives 13600au
/// per fr. Column starts: 0; 100px + 10px = 6600au; 6600 + 13600 + 600 = 20800au.
/// Widths: 6000, 13600, 27200 (sum with gutters: 6000+13600+27200+1200 = 48000).
#[test]
fn layout_grid_fr_with_fixed_tracks_and_gaps() {
    let tree = lay(
        &wrap(&boxes(3, "height:10px")),
        "#g { grid-template-columns: 100px 1fr 2fr; column-gap: 10px }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![
            ra(0, 0, 6000, 600),
            ra(6600, 0, 13600, 600),
            ra(20800, 0, 27200, 600)
        ]
    );
}

/// §11.7.1: the flex factor sum is floored at 1, so fractions below 1fr in total
/// do not fill the space.
///
/// `0.25fr 0.25fr` in 800px: sum 0.5 is treated as 1, so each track is
/// 0.25 x 800 = 200px, and the remaining 400px is left unfilled.
#[test]
fn layout_grid_fr_below_one_does_not_fill() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { grid-template-columns: 0.25fr 0.25fr }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 200, 10), r(200, 0, 200, 10)]);
}

/// Percentages resolve against the grid container's content box (§11.3 step 3).
#[test]
fn layout_grid_percentage_columns() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { grid-template-columns: 25% 75% }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 200, 10), r(200, 0, 600, 10)]);
}

/// `minmax(100px, 1fr) minmax(300px, 500px)` in 800px.
///
/// Initial: base 100 / limit infinity (flexible), base 300 / limit 500. After
/// resolving intrinsic sizes the flexible track's infinite limit becomes its base
/// (100). Maximise tracks (§11.6): free space 800 - 400 = 400 goes to bases up to
/// their limits; the first is frozen at 100, the second rises to 500. Expand
/// flexible tracks (§11.7): leftover = 800 - 500 = 300, one flex factor, so the
/// first track becomes 300. Result 300 and 500.
#[test]
fn layout_grid_minmax_flexible_and_clamped() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { grid-template-columns: minmax(100px, 1fr) minmax(300px, 500px) }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 300, 10), r(300, 0, 500, 10)]);
}

/// `auto` tracks are stretched (§11.8) when `justify-content` is `normal`.
///
/// Items "abcdefgh" (8 chars = 40px) and "abcd" (20px), each a single word, so
/// min-content = max-content. Bases 40 and 20 rise to their growth limits (§11.6);
/// the remaining 800 - 60 = 740px is shared equally between the two auto tracks
/// (§11.8): 370 each, giving 410 and 390.
#[test]
fn layout_grid_auto_tracks_stretch_to_fill() {
    let tree = lay(
        &wrap("<div class=i>abcdefgh</div><div class=i>abcd</div>"),
        "#g { grid-template-columns: auto auto }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 410, 20), r(410, 0, 390, 20)]);
}

/// With `justify-content: start` nothing stretches (§11.8 applies to `normal` and
/// `stretch` only): the tracks stay at their content sizes, 40 and 20.
#[test]
fn layout_grid_justify_content_start_disables_stretch() {
    let tree = lay(
        &wrap("<div class=i>abcdefgh</div><div class=i>abcd</div>"),
        "#g { grid-template-columns: auto auto; justify-content: start }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 40, 20), r(40, 0, 20, 20)]);
}

/// `min-content` and `max-content` tracks.
///
/// Item one "aa bb cc": widest word 2 chars = 10px, whole run 40px. Item two
/// "aa bb c" is 35px on one line. Column one is min-content = 10px, so item one
/// wraps onto three lines (60px); column two is max-content = 35px. The row is as
/// tall as the tallest item (60) and both items stretch to it (§6.2 `normal`).
#[test]
fn layout_grid_min_content_and_max_content_tracks() {
    let tree = lay(
        &wrap("<div class=i>aa bb cc</div><div class=i>aa bb c</div>"),
        "#g { grid-template-columns: min-content max-content }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 10, 60), r(10, 0, 35, 60)]);
}

/// Row sizes are found after column sizes (§11.1 step 2), because an item's height
/// depends on the width it is given.
///
/// A 20px column holding "aaa bbb ccc" (each word 15px): "aaa bbb" would be 35px,
/// so every word breaks onto its own line: three lines, 60px.
#[test]
fn layout_grid_rows_depend_on_column_width() {
    let tree = lay(
        &wrap("<div class=i>aaa bbb ccc</div>"),
        "#g { grid-template-columns: 20px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 20, 60)]);
    assert_eq!(container, r(0, 0, 800, 60));
}

/// `fit-content(arg)` is `minmax(auto, max-content)` with the growth limit clamped
/// to the argument (§7.2.3.1, §11.5).
///
/// "aa bb cc dd ee" is 70px wide on one line and 10px at its narrowest. fit-content(50px)
/// ends at 50 (70 clamped); fit-content(500px) ends at 70. Neither is `auto`, so
/// §11.8 leaves them alone. At 50px the text wraps ("aa bb cc" is 40, adding " dd" is
/// 55) onto two lines: the row is 40.
#[test]
fn layout_grid_fit_content_tracks() {
    let tree = lay(
        &wrap("<div class=i>aa bb cc dd ee</div><div class=i>aa bb cc dd ee</div>"),
        "#g { grid-template-columns: fit-content(50px) fit-content(500px) }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 50, 40), r(50, 0, 70, 40)]);
}

/// `min-width: auto` on a grid item keeps a `1fr` track from collapsing below the
/// item's min-content size (§6.6, and §11.5 "minimum contribution").
///
/// Item one is a single 60-character word = 300px in a 400px container. Its track
/// is `minmax(auto, 1fr)`, so its base size is 300. §11.7.1: hypothetical fr size
/// is 400 / 2 = 200 < 300, so that track is treated as inflexible; leftover
/// 400 - 300 = 100 goes to the other. Result 300 and 100.
#[test]
fn layout_grid_automatic_minimum_size_floors_flexible_tracks() {
    let word = "x".repeat(60);
    let tree = lay(
        &wrap(&format!("<div class=i>{word}</div><div class=i></div>")),
        "#g { width: 400px; grid-template-columns: 1fr 1fr }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 300, 20), r(300, 0, 100, 20)]);
}

/// The same grid with `min-width: 0` on the item: the minimum contribution is 0,
/// so the tracks are 200 and 200 and the word overflows (§6.6 applies only to
/// `min-width: auto`).
#[test]
fn layout_grid_explicit_min_width_removes_the_floor() {
    let word = "x".repeat(60);
    let tree = lay(
        &wrap(&format!(
            "<div class=i style=\"min-width:0\">{word}</div><div class=i></div>"
        )),
        "#g { width: 400px; grid-template-columns: 1fr 1fr }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 200, 20), r(200, 0, 200, 20)]);
}

/// A spanning item grows the tracks it crosses (§11.5 step 3).
///
/// `auto auto auto`, `justify-content: start`. A (col 1) is 20px, C (col 3) is
/// 10px, B (cols 1-2, row 2) is a 100px word. After single-span items: col1 base
/// 20 / limit 20, col2 base 0 / limit infinity, col3 10 / 10. B's minimum
/// contribution is 100: space = 100 - (20 + 0) = 80, distributed up to limits
/// (col1 is full) so col2 takes all 80. Columns 20, 80, 10; B is 100 wide.
#[test]
fn layout_grid_spanning_item_grows_unfrozen_tracks_first() {
    let word = "x".repeat(20);
    let tree = lay(
        &wrap(&format!(
            "<div class=i style=\"grid-area:1/1\">abcd</div>\
             <div class=i style=\"grid-area:2/1/3/3\">{word}</div>\
             <div class=i style=\"grid-area:1/3\">ab</div>"
        )),
        "#g { grid-template-columns: auto auto auto; justify-content: start }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![r(0, 0, 20, 20), r(0, 20, 100, 20), r(100, 0, 10, 20)]
    );
}

/// A tall spanning item with auto rows: space beyond the growth limits is split
/// equally among the spanned tracks (§11.5.1, "distribute space beyond limits").
///
/// Rows start at 20 each (limits 20). The item spanning both needs 100: space 60,
/// every track is at its limit, so the extra is shared 30/30: rows 50 and 50.
#[test]
fn layout_grid_spanning_item_splits_beyond_limits_equally() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-area:1/1/3/2;height:100px\"></div>\
             <div class=i style=\"grid-area:1/2;height:20px\"></div>\
             <div class=i style=\"grid-area:2/2;height:20px\"></div>",
        ),
        "#g { grid-template-columns: 100px 100px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(
        items,
        vec![r(0, 0, 100, 100), r(100, 0, 100, 20), r(100, 50, 100, 20)]
    );
    assert_eq!(container, r(0, 0, 800, 100));
}

/// Flexible rows in an auto-height container (§11.7.1, indefinite free space).
///
/// `1fr 2fr` rows, items 30px and 90px tall. The flex fraction is the maximum of
/// base / factor (30/1, 90/2 = 45) and each item's own max-content / factors
/// (30, 45): 45. Rows 45 and 90; container 135.
#[test]
fn layout_grid_flexible_rows_in_an_auto_height_container() {
    let tree = lay(
        &wrap("<div class=i style=\"height:30px\"></div><div class=i style=\"height:90px\"></div>"),
        "#g { grid-template-rows: 1fr 2fr }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 135));
    assert_eq!(items, vec![r(0, 0, 800, 30), r(0, 45, 800, 90)]);
}

/// With a definite height, flexible rows share it: 300px as `1fr 2fr` is 100 and 200,
/// and unsized items stretch to fill them.
#[test]
fn layout_grid_flexible_rows_in_a_definite_height_container() {
    let tree = lay(
        &wrap(&boxes(2, "")),
        "#g { height: 300px; grid-template-rows: 1fr 2fr }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 300));
    assert_eq!(items, vec![r(0, 0, 800, 100), r(0, 100, 800, 200)]);
}

/// Percentage rows resolve against a definite container height: 25% and 75% of 200.
#[test]
fn layout_grid_percentage_rows_against_a_definite_height() {
    let tree = lay(
        &wrap(&boxes(2, "")),
        "#g { height: 200px; grid-template-rows: 25% 75% }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 800, 50), r(0, 50, 800, 150)]);
}

// --- repeat() ----------------------------------------------------------------

/// `repeat(auto-fill, 100px)` in 800px: eight columns (§7.2.3.2). Ten items fill
/// one row of eight and two of the next.
#[test]
fn layout_grid_repeat_auto_fill_counts_columns() {
    let tree = lay(
        &wrap(&boxes(10, "height:20px")),
        "#g { grid-template-columns: repeat(auto-fill, 100px) }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 40));
    assert_eq!(items.first().copied(), Some(r(0, 0, 100, 20)));
    assert_eq!(items.get(7).copied(), Some(r(700, 0, 100, 20)));
    assert_eq!(items.get(8).copied(), Some(r(0, 20, 100, 20)));
    assert_eq!(items.get(9).copied(), Some(r(100, 20, 100, 20)));
}

/// `auto-fit` collapses empty repeated tracks (§7.2.3.2), so three items share the
/// whole 800px: 8 tracks are generated, 5 collapse, and `minmax(100px, 1fr)` over
/// three tracks is 800 / 3 = 16000au each. `auto-fill` keeps the empty tracks, so
/// the same items are 100px wide.
#[test]
fn layout_grid_repeat_auto_fit_collapses_empty_tracks() {
    let fit = lay(
        &wrap(&boxes(3, "height:10px")),
        "#g { grid-template-columns: repeat(auto-fit, minmax(100px, 1fr)) }",
    );
    assert_eq!(
        grid(&fit).1,
        vec![
            ra(0, 0, 16000, 600),
            ra(16000, 0, 16000, 600),
            ra(32000, 0, 16000, 600)
        ]
    );
    let fill = lay(
        &wrap(&boxes(3, "height:10px")),
        "#g { grid-template-columns: repeat(auto-fill, minmax(100px, 1fr)) }",
    );
    assert_eq!(
        grid(&fill).1,
        vec![r(0, 0, 100, 10), r(100, 0, 100, 10), r(200, 0, 100, 10)]
    );
}

/// Gaps count when working out how many tracks fit: 100px tracks with a 50px gap
/// in 800px. n tracks need 100n + 50(n-1) <= 800, so n = 5 (750). Item six wraps.
#[test]
fn layout_grid_repeat_auto_fill_accounts_for_gaps() {
    let tree = lay(
        &wrap(&boxes(6, "height:20px")),
        "#g { grid-template-columns: repeat(auto-fill, 100px); column-gap: 50px }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items.get(4).copied(), Some(r(600, 0, 100, 20)));
    assert_eq!(items.get(5).copied(), Some(r(0, 20, 100, 20)));
}

// --- Placement: lines, names, areas, spans ----------------------------------

/// `grid-template-areas` with gaps (§7.3).
///
/// Columns `100px 1fr`, rows `40px 60px`, `gap: 10px 20px` (row gap 10, column gap
/// 20). Area h spans both columns of row 1: width 100 + 20 + 680 = 800. Row 2
/// starts at 40 + 10 = 50. s is 100 wide; m starts at 100 + 20 = 120 and is
/// 800 - 120 = 680 wide. Container: 40 + 10 + 60 = 110.
#[test]
fn layout_grid_template_areas_and_gaps() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-area:h\"></div>\
             <div class=i style=\"grid-area:s\"></div>\
             <div class=i style=\"grid-area:m\"></div>",
        ),
        "#g { grid-template-areas: \"h h\" \"s m\"; \
              grid-template-columns: 100px 1fr; grid-template-rows: 40px 60px; \
              gap: 10px 20px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 110));
    assert_eq!(
        items,
        vec![r(0, 0, 800, 40), r(0, 50, 100, 60), r(120, 50, 680, 60)]
    );
}

/// Line numbers, negative line numbers and spans (§8.3).
///
/// Four 100px columns (lines 1..5), three 30px rows. A: `2 / span 2` is lines 2-4.
/// B: `1 / -1` is lines 1-5 (-1 is the last line). C: `-3 / -2` is lines 3-4.
#[test]
fn layout_grid_line_numbers_negative_lines_and_spans() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:2 / span 2;grid-row:1\"></div>\
             <div class=i style=\"grid-column:1 / -1;grid-row:2\"></div>\
             <div class=i style=\"grid-column:-3 / -2;grid-row:3\"></div>",
        ),
        "#g { grid-template-columns: repeat(4, 100px); grid-template-rows: repeat(3, 30px) }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 90));
    assert_eq!(
        items,
        vec![r(100, 0, 200, 30), r(0, 30, 400, 30), r(200, 60, 100, 30)]
    );
}

/// Named lines, including those written inside `repeat()` (§7.2.3, §8.3).
///
/// `repeat(2, [c] 50px) [end]` has lines 1:[c] 2:[c] 3:[end]. `c 2` is the second
/// line named c (line 2), so A is the second track: x = 50, width 50. B runs from
/// the first c (line 1) to `end` (line 3): x = 0, width 100.
#[test]
fn layout_grid_named_lines_inside_repeat() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:c 2;grid-row:1;height:20px\"></div>\
             <div class=i style=\"grid-column:c / end;grid-row:2;height:20px\"></div>",
        ),
        "#g { grid-template-columns: repeat(2, [c] 50px) [end] }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(50, 0, 50, 20), r(0, 20, 100, 20)]);
}

/// A bare identifier names an area's edges (§8.3): `grid-column: main` is
/// `main-start / main-end`, and the implicit lines exist without being written in
/// the track list (§7.3.1).
#[test]
fn layout_grid_area_names_create_implicit_lines() {
    let tree = lay(
        &wrap("<div class=i style=\"grid-column:main;grid-row:main\"></div>"),
        "#g { grid-template-areas: \". main\" \". main\"; \
              grid-template-columns: 100px 300px; grid-template-rows: 20px 30px }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(100, 0, 300, 50)]);
}

/// Auto-placement, sparse (§8.5): the cursor never goes back.
///
/// Three 100px columns, two 30px rows. A spans 2 columns and lands at (1,1). B also
/// spans 2: column 3 is too narrow, so it moves to row 2. C: the cursor is at
/// (row 2, col 1); columns 1 and 2 are taken by B, so C goes to (row 2, col 3).
#[test]
fn layout_grid_auto_placement_sparse() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:span 2\"></div>\
             <div class=i style=\"grid-column:span 2\"></div>\
             <div class=i></div>",
        ),
        "#g { grid-template-columns: repeat(3, 100px); grid-template-rows: repeat(2, 30px) }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![r(0, 0, 200, 30), r(0, 30, 200, 30), r(200, 30, 100, 30)]
    );
}

/// Auto-placement, dense (§8.5): each item restarts from the start of the grid, so
/// C fills the hole left in row 1.
#[test]
fn layout_grid_auto_placement_dense() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:span 2\"></div>\
             <div class=i style=\"grid-column:span 2\"></div>\
             <div class=i></div>",
        ),
        "#g { grid-template-columns: repeat(3, 100px); grid-template-rows: repeat(2, 30px); \
              grid-auto-flow: row dense }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![r(0, 0, 200, 30), r(0, 30, 200, 30), r(200, 0, 100, 30)]
    );
}

/// `grid-auto-flow: column` fills columns first and creates implicit columns
/// sized by `grid-auto-columns`.
///
/// Two 30px rows, auto-columns 50px, five items: column 1 holds items 0 and 1,
/// column 2 items 2 and 3, column 3 item 4.
#[test]
fn layout_grid_auto_flow_column() {
    let tree = lay(
        &wrap(&boxes(5, "")),
        "#g { grid-template-rows: 30px 30px; grid-auto-flow: column; grid-auto-columns: 50px; \
              justify-content: start }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![
            r(0, 0, 50, 30),
            r(0, 30, 50, 30),
            r(50, 0, 50, 30),
            r(50, 30, 50, 30),
            r(100, 0, 50, 30),
        ]
    );
}

/// The §7.6 example: explicit 20px column and row, implicit 40px ones. A is 20x20,
/// B 40 wide, C 40 tall, D 40x40.
#[test]
fn layout_grid_implicit_tracks_use_the_auto_sizes() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:1;grid-row:1\"></div>\
             <div class=i style=\"grid-column:2;grid-row:1\"></div>\
             <div class=i style=\"grid-column:1;grid-row:2\"></div>\
             <div class=i style=\"grid-column:2;grid-row:2\"></div>",
        ),
        "#g { grid-template-columns: 20px; grid-auto-columns: 40px; \
              grid-template-rows: 20px; grid-auto-rows: 40px; justify-content: start }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(
        items,
        vec![
            r(0, 0, 20, 20),
            r(20, 0, 40, 20),
            r(0, 20, 20, 40),
            r(20, 20, 40, 40)
        ]
    );
}

/// `grid-auto-rows` with several sizes repeats as a pattern: 20, 40, 20.
#[test]
fn layout_grid_auto_rows_repeat_as_a_pattern() {
    let tree = lay(&wrap(&boxes(3, "")), "#g { grid-auto-rows: 20px 40px }");
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 80));
    assert_eq!(
        items,
        vec![r(0, 0, 800, 20), r(0, 20, 800, 40), r(0, 60, 800, 20)]
    );
}

/// Items can be placed before the explicit grid (§7.6): `span 2 / 2` ends at line 2
/// and starts one line before line 1, creating an implicit column on the start side,
/// sized by `grid-auto-columns` (the last implicit track before the explicit grid
/// takes the last size). The second item is pushed to row 2 because column 1
/// is taken in row 1.
#[test]
fn layout_grid_implicit_tracks_before_the_explicit_grid() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"grid-column:span 2 / 2;height:20px\"></div>\
             <div class=i style=\"grid-column:1;height:20px\"></div>",
        ),
        "#g { grid-template-columns: 100px; grid-auto-columns: 40px; justify-content: start }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 140, 20), r(40, 20, 100, 20)]);
}

/// `order` changes both auto-placement and the order the tree lists the items
/// (§8.5, "order-modified document order").
#[test]
fn layout_grid_order_reorders_placement() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"height:10px\"></div>\
             <div class=i style=\"height:20px;order:-1\"></div>",
        ),
        "#g { grid-template-columns: 100px 100px }",
    );
    let (_, items) = grid(&tree);
    assert_eq!(items, vec![r(0, 0, 100, 20), r(100, 0, 100, 10)]);
}

/// An absolutely positioned child is not a grid item (§9.1): it takes no cell.
#[test]
fn layout_grid_absolute_child_takes_no_cell() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"height:20px\"></div>\
             <div class=i style=\"position:absolute;width:5px;height:5px\"></div>\
             <div class=i style=\"height:20px\"></div>",
        ),
        "#g { grid-template-columns: 100px 100px }",
    );
    let (_, items) = grid(&tree);
    // Items come first, in order; the out-of-flow child is listed after them, at
    // the start of the content box (its static position).
    assert_eq!(items.len(), 3);
    assert_eq!(items.first().copied(), Some(r(0, 0, 100, 20)));
    assert_eq!(items.get(1).copied(), Some(r(100, 0, 100, 20)));
    assert_eq!(items.get(2).copied(), Some(r(0, 0, 5, 5)));
}

// --- Items: sizing, margins, alignment --------------------------------------

/// Unsized items stretch to their grid area (§6.2, `normal` behaves as `stretch`).
#[test]
fn layout_grid_items_stretch_by_default() {
    let tree = lay(
        &wrap(&boxes(1, "")),
        "#g { grid-template-columns: 200px; grid-template-rows: 100px }",
    );
    assert_eq!(grid(&tree).1, vec![r(0, 0, 200, 100)]);
}

/// `justify-self` and `align-self` place a sized item inside a 200x100 area.
///
/// 50x20 item: center gives x = (200-50)/2 = 75, y = (100-20)/2 = 40; end gives
/// x = 150, y = 80 (offset by the area's own origin at 200 for the second column);
/// start is the area origin.
#[test]
fn layout_grid_self_alignment() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"justify-self:center;align-self:center\"></div>\
             <div class=i style=\"justify-self:end;align-self:end\"></div>\
             <div class=i style=\"justify-self:start;align-self:start\"></div>",
        ),
        "#g { grid-template-columns: repeat(3, 200px); grid-template-rows: 100px } \
         .i { width: 50px; height: 20px }",
    );
    assert_eq!(
        grid(&tree).1,
        vec![r(75, 40, 50, 20), r(350, 80, 50, 20), r(400, 0, 50, 20)]
    );
}

/// `justify-items` and `align-items` on the container set the default; `justify-self`
/// on the item overrides it.
#[test]
fn layout_grid_items_alignment_defaults_and_overrides() {
    let tree = lay(
        &wrap(
            "<div class=i></div>\
             <div class=i style=\"justify-self:start\"></div>",
        ),
        "#g { grid-template-columns: repeat(2, 200px); grid-template-rows: 100px; \
              justify-items: end; align-items: center } \
         .i { width: 50px; height: 20px }",
    );
    assert_eq!(grid(&tree).1, vec![r(150, 40, 50, 20), r(200, 40, 50, 20)]);
}

/// Auto margins absorb free space and disable stretching (§6.? alignment, "auto
/// margins"): `margin: auto` centres; `margin-left: auto` pushes right.
#[test]
fn layout_grid_auto_margins_absorb_free_space() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"margin:auto\"></div>\
             <div class=i style=\"margin-left:auto\"></div>",
        ),
        "#g { grid-template-columns: repeat(2, 200px); grid-template-rows: 100px } \
         .i { width: 50px; height: 20px }",
    );
    assert_eq!(grid(&tree).1, vec![r(75, 40, 50, 20), r(350, 0, 50, 20)]);
}

/// An item with `width: auto` and auto margins is sized to fit its content rather
/// than stretched: "abcd" is 20px wide and 20px tall, centred in 200x100: (90, 40).
#[test]
fn layout_grid_auto_margins_size_to_content() {
    let tree = lay(
        &wrap("<div class=i style=\"margin:auto\">abcd</div>"),
        "#g { grid-template-columns: 200px; grid-template-rows: 100px }",
    );
    assert_eq!(grid(&tree).1, vec![r(90, 40, 20, 20)]);
}

/// Margins are inside the grid area and do not collapse (§6.4): a 30px item with
/// 5px margins in a 100px column. Row = 30 + 10 = 40, item at (5, 5), 90 wide.
#[test]
fn layout_grid_item_margins_are_inside_the_area() {
    let tree = lay(
        &wrap("<div class=i style=\"margin:5px;height:30px\"></div>"),
        "#g { grid-template-columns: 100px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 40));
    assert_eq!(items, vec![r(5, 5, 90, 30)]);
}

/// Adjacent items' margins add rather than collapse (§6.4): two rows of 30px items
/// with 5px vertical margins are 40px each, 80 in total.
#[test]
fn layout_grid_item_margins_do_not_collapse() {
    let tree = lay(
        &wrap(&boxes(2, "margin:5px 0;height:30px")),
        "#g { grid-template-columns: 100px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 80));
    assert_eq!(items, vec![r(0, 5, 100, 30), r(0, 45, 100, 30)]);
}

/// `box-sizing` applies to items: a `border-box` item with width 100 and padding
/// and border stays 100 wide; a `content-box` one is 100 + 2*10 + 2*5 = 130.
#[test]
fn layout_grid_item_box_sizing() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"box-sizing:border-box\"></div>\
             <div class=i style=\"box-sizing:content-box\"></div>",
        ),
        "#g { grid-template-columns: repeat(2, 200px); justify-content: start } \
         .i { width: 100px; height: 40px; padding: 10px; border: 5px solid black }",
    );
    assert_eq!(grid(&tree).1, vec![r(0, 0, 100, 40), r(200, 0, 130, 70)]);
}

/// `max-width` limits a stretched item; it is then aligned to the start.
#[test]
fn layout_grid_stretch_is_limited_by_max_width() {
    let tree = lay(
        &wrap("<div class=i style=\"max-width:50px\"></div>"),
        "#g { grid-template-columns: 200px; grid-template-rows: 30px }",
    );
    assert_eq!(grid(&tree).1, vec![r(0, 0, 50, 30)]);
}

/// Percentage padding on a grid item resolves against the grid area's width
/// (§6.4): 10% of 200 is 20, so the child starts 20px into the item.
#[test]
fn layout_grid_item_percentage_padding_uses_the_area() {
    let tree = lay(
        &wrap("<div class=i style=\"padding-left:10%\"><div style=\"height:10px\"></div></div>"),
        "#g { grid-template-columns: 200px }",
    );
    let item = *at_depth(&tree, 4).first().expect("an item");
    let child = *tree.children(item).first().expect("the item's child");
    assert_eq!(rect_of(&tree, item), r(0, 0, 200, 10));
    assert_eq!(rect_of(&tree, child), r(20, 0, 180, 10));
}

/// Text directly inside a grid container becomes an anonymous grid item (§6), and
/// a following element is the next item.
#[test]
fn layout_grid_text_runs_become_anonymous_items() {
    let tree = lay(
        "<div id=g>hello<div class=i>x</div></div>",
        "#g { grid-template-columns: 100px 100px }",
    );
    let container = *at_depth(&tree, 3).first().expect("the container");
    let kids = tree.children(container);
    assert_eq!(kids.len(), 2);
    let first = tree.get(kids[0]).expect("anonymous item");
    assert_eq!(first.kind, FragmentKind::AnonymousBlock);
    assert_eq!(rect_of(&tree, kids[0]), r(0, 0, 100, 20));
    assert_eq!(rect_of(&tree, kids[1]), r(100, 0, 100, 20));
}

// --- Aligning the tracks ----------------------------------------------------

const FOUR: &str = "#g { width: 400px; height: 300px; \
     grid-template-columns: 100px 100px; grid-template-rows: 50px 50px }";

/// `justify-content: center` and `align-content: end` (§10.5).
///
/// Free space is 400-200 = 200 across and 300-100 = 200 down. Centre: the columns
/// start at 100. End: the rows start at 200.
#[test]
fn layout_grid_content_alignment_center_and_end() {
    let tree = lay(
        &wrap(&boxes(4, "")),
        &format!("{FOUR} #g {{ justify-content: center; align-content: end }}"),
    );
    assert_eq!(
        grid(&tree).1,
        vec![
            r(100, 200, 100, 50),
            r(200, 200, 100, 50),
            r(100, 250, 100, 50),
            r(200, 250, 100, 50)
        ]
    );
}

/// `space-between`: all 200px of free space goes between the two tracks.
#[test]
fn layout_grid_content_alignment_space_between() {
    let tree = lay(
        &wrap(&boxes(4, "")),
        &format!("{FOUR} #g {{ justify-content: space-between; align-content: space-between }}"),
    );
    assert_eq!(
        grid(&tree).1,
        vec![
            r(0, 0, 100, 50),
            r(300, 0, 100, 50),
            r(0, 250, 100, 50),
            r(300, 250, 100, 50)
        ]
    );
}

/// `space-around`: 200 / 2 tracks = 100 per track, half (50) on each side. The
/// first track starts at 50, the second at 50 + 100 + 100 = 250.
#[test]
fn layout_grid_content_alignment_space_around() {
    let tree = lay(
        &wrap(&boxes(4, "")),
        &format!("{FOUR} #g {{ justify-content: space-around; align-content: space-around }}"),
    );
    assert_eq!(
        grid(&tree).1,
        vec![
            r(50, 50, 100, 50),
            r(250, 50, 100, 50),
            r(50, 200, 100, 50),
            r(250, 200, 100, 50)
        ]
    );
}

/// `space-evenly`: 200 / 3 = 4000au each. Columns start at 4000 and
/// 4000 + 6000 + 4000 = 14000; rows at 4000 and 4000 + 3000 + 4000 = 11000.
#[test]
fn layout_grid_content_alignment_space_evenly() {
    let tree = lay(
        &wrap(&boxes(4, "")),
        &format!("{FOUR} #g {{ justify-content: space-evenly; align-content: space-evenly }}"),
    );
    assert_eq!(
        grid(&tree).1,
        vec![
            ra(4000, 4000, 6000, 3000),
            ra(14000, 4000, 6000, 3000),
            ra(4000, 11000, 6000, 3000),
            ra(14000, 11000, 6000, 3000),
        ]
    );
}

/// A spanning item absorbs the distributed space in the gutter it crosses (§10.5):
/// with `space-between` the item spanning both columns is 400 wide, not 200.
#[test]
fn layout_grid_spanning_item_grows_with_content_distribution() {
    let tree = lay(
        &wrap("<div class=i style=\"grid-column:1 / 3\"></div>"),
        "#g { width: 400px; grid-template-columns: 100px 100px; justify-content: space-between }",
    );
    assert_eq!(grid(&tree).1, vec![r(0, 0, 400, 0)]);
}

/// `align-content` only matters once the container is taller than its rows:
/// `min-height: 300px` with one 50px row centred puts it at 125.
#[test]
fn layout_grid_align_content_uses_min_height() {
    let tree = lay(
        &wrap(&boxes(1, "")),
        "#g { min-height: 300px; grid-template-rows: 50px; align-content: center }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 300));
    assert_eq!(items, vec![r(0, 125, 800, 50)]);
}

/// A percentage `column-gap` resolves against the content box width: 10% of 800.
#[test]
fn layout_grid_percentage_gap() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { grid-template-columns: 100px 100px; column-gap: 10% }",
    );
    assert_eq!(grid(&tree).1, vec![r(0, 0, 100, 10), r(180, 0, 100, 10)]);
}

// --- The grid container ------------------------------------------------------

/// Padding and border shrink the grid's content box: 800 - 2*10 = 780, so `1fr 1fr`
/// is 390 each, starting 10px in.
#[test]
fn layout_grid_container_padding() {
    let tree = lay(
        &wrap(&boxes(2, "height:30px")),
        "#g { padding: 10px; grid-template-columns: 1fr 1fr }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 50));
    assert_eq!(items, vec![r(10, 10, 390, 30), r(400, 10, 390, 30)]);
}

/// A grid container establishes a formatting context: it contains its children's
/// margins instead of collapsing with them, so a 10px child margin stays inside.
#[test]
fn layout_grid_container_does_not_collapse_margins_with_items() {
    let tree = lay(&wrap(&boxes(1, "margin-top:10px;height:30px")), "");
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 800, 40));
    assert_eq!(items, vec![r(0, 10, 800, 30)]);
}

/// `inline-grid` is sized shrink-to-fit (§5.1): two 100px columns make a 200px box.
#[test]
fn layout_grid_inline_grid_shrinks_to_fit() {
    let tree = lay(
        &wrap(&boxes(2, "height:10px")),
        "#g { display: inline-grid; grid-template-columns: 100px 100px }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container, r(0, 0, 200, 10));
    assert_eq!(items, vec![r(0, 0, 100, 10), r(100, 0, 100, 10)]);
}

/// A grid inside a grid: the inner container is stretched to its 600px area and
/// lays out its own two `1fr` columns at 300px each.
#[test]
fn layout_grid_nested_grids() {
    let tree = lay(
        "<div id=g><div class=i></div>\
         <div id=h><div class=i style=\"height:10px\"></div><div class=i style=\"height:10px\"></div></div></div>",
        "#g { grid-template-columns: 200px 1fr } \
         #h { display: grid; grid-template-columns: 1fr 1fr }",
    );
    let outer = at_depth(&tree, 4);
    assert_eq!(rect_of(&tree, outer[0]), r(0, 0, 200, 10));
    assert_eq!(rect_of(&tree, outer[1]), r(200, 0, 600, 10));
    let inner = at_depth(&tree, 5);
    assert_eq!(rect_of(&tree, inner[0]), r(200, 0, 300, 10));
    assert_eq!(rect_of(&tree, inner[1]), r(500, 0, 300, 10));
}

/// A grid item that is itself a block container lays its own children out at the
/// item's width: the child is as wide as the 100px column.
#[test]
fn layout_grid_item_contents_use_the_item_width() {
    let tree = lay(
        &wrap("<div class=i><div style=\"height:10px\"></div></div>"),
        "#g { grid-template-columns: 100px }",
    );
    let item = *at_depth(&tree, 4).first().expect("an item");
    let child = *tree.children(item).first().expect("the child");
    assert_eq!(rect_of(&tree, child), r(0, 0, 100, 10));
}

// --- Hostile input, determinism, depth --------------------------------------

/// `repeat(100000, 1px)` and a span of 99999 are clamped rather than allocated.
/// The first item is still the first 1px column, and layout returns.
#[test]
fn layout_grid_huge_track_counts_are_bounded() {
    let tree = lay(
        &wrap(
            "<div class=i style=\"height:10px;grid-column:1 / span 99999\"></div>\
             <div class=i style=\"height:10px;grid-row:99999999\"></div>\
             <div class=i style=\"height:10px;grid-column:-99999999\"></div>",
        ),
        "#g { grid-template-columns: repeat(100000, 1px); grid-template-rows: repeat(100000, 1px) }",
    );
    let (container, items) = grid(&tree);
    assert_eq!(container.0, px(0));
    assert_eq!(items.len(), 3);
}

/// 20,000 auto-placed items: placement must not be quadratic in a way that hangs.
#[test]
fn layout_grid_many_items_lay_out_in_bounded_time() {
    let items = boxes(20_000, "height:1px");
    let tree = lay(
        &wrap(&items),
        "#g { grid-template-columns: repeat(10, 1fr) }",
    );
    let (container, kids) = grid(&tree);
    assert_eq!(kids.len(), 20_000);
    assert_eq!(container.3, px(2000));
}

/// Repeat runs agree to the app unit.
#[test]
fn layout_grid_is_deterministic() {
    let body = wrap(&format!(
        "{}<div class=i>some text that wraps around</div>",
        boxes(7, "height:13px")
    ));
    let css = "#g { grid-template-columns: repeat(auto-fill, minmax(70px, 1fr)); gap: 3px 7px }";
    let first = lay(&body, css);
    let second = lay(&body, css);
    assert_eq!(first, second);
}

/// Grids nested as deep as the parser allows (480 levels) on a small stack: the walks are iterative.
#[test]
fn layout_grid_deep_nesting_does_not_overflow_the_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            // px-dom abandons a parse nested past MAX_DEPTH (512); stay just under it.
            let depth = 480;
            let mut body = String::new();
            for _ in 0..depth {
                body.push_str("<div class=n>");
            }
            body.push_str("<div class=i></div>");
            for _ in 0..depth {
                body.push_str("</div>");
            }
            let tree = lay(&body, ".n { display: grid }");
            tree.len()
        })
        .expect("spawn");
    let count = handle.join().expect("no stack overflow, no panic");
    assert!(count >= 480);
}
