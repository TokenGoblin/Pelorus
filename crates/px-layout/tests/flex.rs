//! Phase 7, unit "flex": CSS Flexible Box Layout Level 1, section 9.
//!
//! Every expected number below was worked out by hand from the algorithm in
//! section 9 before `flex.rs` existed. The working is in the comment above each
//! test, because an expectation that cannot be re-derived is a number somebody
//! copied from the implementation.
//!
//! # Conventions
//!
//! - The page is `html > body > #f`, `#f` being the flex container and its
//!   children the items, all with `margin: 0; padding: 0` unless a test says
//!   otherwise. Fonts are 10px and lines 20px, so the stub text metric
//!   (`crate::text`) makes a character 5px wide.
//! - [`run`] returns the rectangle of every block-level fragment from `#f`
//!   downwards, in layout order, as `(x, y, width, height)` in **app units**
//!   (60 to the pixel). [`p`] builds one from pixels and [`q`] from quarter
//!   pixels, because several distributions below land on half pixels.
//! - Layout order is *order-modified document order* (section 5.4), so a test
//!   about `order` reads its rectangles in visual order.

use px_layout::block::layout_document;
use px_layout::fragment::FragmentKind;
use px_layout::geom::px;

/// `(x, y, width, height)` in app units.
type R = (i32, i32, i32, i32);

/// A rectangle from whole pixels.
fn p(x: i32, y: i32, w: i32, h: i32) -> R {
    (x * 60, y * 60, w * 60, h * 60)
}

/// A rectangle from quarter pixels (15 app units each).
fn q(x: i32, y: i32, w: i32, h: i32) -> R {
    (x * 15, y * 15, w * 15, h * 15)
}

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
     * { font-size: 10px; line-height: 20px } \
     #f { display: flex }";

/// Lay out `<div id=f>{items}</div>` under `css` and return every block-level
/// rectangle at or below `#f`.
fn run(items: &str, css: &str) -> Vec<R> {
    let html = format!("<html><body><div id=f>{items}</div></body></html>");
    run_page(&html, css)
}

fn run_page(html: &str, css: &str) -> Vec<R> {
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
    tree.in_layout_order()
        .into_iter()
        .filter(|(depth, _)| *depth >= 3)
        .filter_map(|(_, id)| tree.get(id))
        .filter(|f| matches!(f.kind, FragmentKind::Block | FragmentKind::AnonymousBlock))
        .map(|f| {
            (
                f.inline_offset.0,
                f.block_offset.0,
                f.size.inline.0,
                f.size.block.0,
            )
        })
        .collect()
}

const THREE: &str = "<div id=a></div><div id=b></div><div id=c></div>";
const TWO: &str = "<div id=a></div><div id=b></div>";

// ---------------------------------------------------------------------------
// Basics: single-line row, section 9.2 and 9.3
// ---------------------------------------------------------------------------

/// Widths 50, 100, 150, no growth: items sit end to end from the main-start edge,
/// each as tall as the tallest (20), which is the container's auto height.
#[test]
fn layout_flex_row_items_sit_side_by_side() {
    let got = run(
        THREE,
        "#f { width: 400px } #a { width: 50px; height: 20px } \
         #b { width: 100px; height: 20px } #c { width: 150px; height: 20px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 400, 20),
            p(0, 0, 50, 20),
            p(50, 0, 100, 20),
            p(150, 0, 150, 20)
        ]
    );
}

/// 9.7: free space 400 - (50 + 100) = 250 shared 1:3. a gets 62.5 -> 112.5,
/// b gets 187.5 -> 287.5.
#[test]
fn layout_flex_grow_shares_free_space_by_factor() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { width: 50px; height: 10px; flex-grow: 1 } \
         #b { width: 100px; height: 10px; flex-grow: 3 }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), q(0, 0, 450, 40), q(450, 0, 1150, 40)]
    );
}

/// 9.7 step 4c: shrinking is weighted by flex-shrink *times base size*. Bases 100
/// and 300 in a 200 container overflow by 200; scaled factors 100 : 300 so a loses
/// 50 and b loses 150.
#[test]
fn layout_flex_shrink_is_weighted_by_base_size() {
    let got = run(
        TWO,
        "#f { width: 200px } #a { width: 100px; height: 10px } \
         #b { width: 300px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 200, 10), p(0, 0, 50, 10), p(50, 0, 150, 10)]
    );
}

/// A flex-shrink of 0 keeps the item at its base size, which then overflows.
#[test]
fn layout_flex_shrink_zero_item_overflows() {
    let got = run(
        TWO,
        "#f { width: 200px } #a { width: 150px; height: 10px; flex-shrink: 0 } \
         #b { width: 150px; height: 10px }",
    );
    // Total 300, overflow 100; only b shrinks: 150 - 100 = 50.
    assert_eq!(
        got,
        vec![p(0, 0, 200, 10), p(0, 0, 150, 10), p(150, 0, 50, 10)]
    );
}

/// 9.2.3: a definite `flex-basis` beats `width` as the base size.
#[test]
fn layout_flex_basis_overrides_width() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { width: 300px; flex-basis: 100px; height: 10px } \
         #b { width: 50px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 100, 10), p(100, 0, 50, 10)]
    );
}

/// `flex: 1` is `1 1 0%`: four items split 400 into four 100s.
#[test]
fn layout_flex_one_shorthand_divides_equally() {
    let got = run(
        "<div id=a></div><div id=a></div><div id=a></div><div id=a></div>",
        "#f { width: 400px } #a { flex: 1; height: 10px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 400, 10),
            p(0, 0, 100, 10),
            p(100, 0, 100, 10),
            p(200, 0, 100, 10),
            p(300, 0, 100, 10)
        ]
    );
}

/// 9.7 step 4d, max violation. Basis 0, both grow 1, free space 400. First pass
/// 200 each; a is capped at 50 (violation 50 - 200 = -150, negative so freeze the
/// max violators). Second pass: b takes 400 - 50 = 350.
#[test]
fn layout_flex_max_width_freezes_and_redistributes() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { flex: 1 1 0; max-width: 50px; height: 10px } \
         #b { flex: 1 1 0; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 50, 10), p(50, 0, 350, 10)]
    );
}

/// 9.7 step 4d, min violation. First pass 200 each; a has min-width 250
/// (violation +50, positive so freeze min violators). b takes 400 - 250 = 150.
#[test]
fn layout_flex_min_width_freezes_and_redistributes() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { flex: 1 1 0; min-width: 250px; height: 10px } \
         #b { flex: 1 1 0; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 250, 10), p(250, 0, 150, 10)]
    );
}

/// 4.5: `min-width: auto` is the content-based minimum. a is 80 wide but holds ten
/// characters (50px min-content), so its minimum is min(80, 50) = 50. b is 120
/// with `min-width: 0`.
///
/// Container 100, bases 80 + 120, free space -100. Scaled shrink 80 : 120 gives
/// a 40 and b 60. a violates its minimum (+10 total violation) so a freezes at
/// 50; b then has 100 - 50 = 50. Both stretch to the 20px line (a holds one line
/// of text).
#[test]
fn layout_flex_content_based_minimum_stops_shrinking() {
    let got = run(
        "<div id=a>xxxxxxxxxx</div><div id=b></div>",
        "#f { width: 100px } #a { width: 80px } #b { width: 120px; min-width: 0 }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 100, 20), p(0, 0, 50, 20), p(50, 0, 50, 20)]
    );
}

// ---------------------------------------------------------------------------
// justify-content, gap, direction: section 8.2, 9.5
// ---------------------------------------------------------------------------

const JC: &str = "#f { width: 400px } #a, #b, #c { width: 50px; height: 10px }";

/// Free space 400 - 150 = 250 goes at the end.
#[test]
fn layout_flex_justify_flex_end() {
    let got = run(THREE, &format!("{JC} #f {{ justify-content: flex-end }}"));
    assert_eq!(
        got[1..],
        [p(250, 0, 50, 10), p(300, 0, 50, 10), p(350, 0, 50, 10)]
    );
}

/// Half of 250 at each end.
#[test]
fn layout_flex_justify_center() {
    let got = run(THREE, &format!("{JC} #f {{ justify-content: center }}"));
    assert_eq!(
        got[1..],
        [p(125, 0, 50, 10), p(175, 0, 50, 10), p(225, 0, 50, 10)]
    );
}

/// Two gaps of 125 between three items.
#[test]
fn layout_flex_justify_space_between() {
    let got = run(
        THREE,
        &format!("{JC} #f {{ justify-content: space-between }}"),
    );
    assert_eq!(
        got[1..],
        [p(0, 0, 50, 10), p(175, 0, 50, 10), p(350, 0, 50, 10)]
    );
}

/// 250 / 3 = 83.33 per item, half either side: 41.67 = 2500 app units. The
/// second item starts at 2500 + 3000 (item) + 5000 (two half gaps) = 10500.
#[test]
fn layout_flex_justify_space_around() {
    let got = run(
        THREE,
        &format!("{JC} #f {{ justify-content: space-around }}"),
    );
    assert_eq!(
        got[1..],
        [
            (2500, 0, 3000, 600),
            (10500, 0, 3000, 600),
            (18500, 0, 3000, 600)
        ]
    );
}

/// Four equal spaces of 250 / 4 = 62.5 = 3750 app units.
#[test]
fn layout_flex_justify_space_evenly() {
    let got = run(
        THREE,
        &format!("{JC} #f {{ justify-content: space-evenly }}"),
    );
    assert_eq!(
        got[1..],
        [
            (3750, 0, 3000, 600),
            (10500, 0, 3000, 600),
            (17250, 0, 3000, 600)
        ]
    );
}

/// Negative free space: `space-between` falls back to `flex-start` (8.2).
#[test]
fn layout_flex_justify_space_between_falls_back_when_overflowing() {
    let got = run(
        TWO,
        "#f { width: 100px; justify-content: space-between } \
         #a, #b { width: 80px; height: 10px; flex-shrink: 0 }",
    );
    assert_eq!(got[1..], [p(0, 0, 80, 10), p(80, 0, 80, 10)]);
}

/// Negative free space with `center` overflows equally both sides.
#[test]
fn layout_flex_justify_center_overflows_both_ways() {
    let got = run(
        TWO,
        "#f { width: 100px; justify-content: center } \
         #a, #b { width: 80px; height: 10px; flex-shrink: 0 }",
    );
    // Free space 100 - 160 = -60, so start at -30.
    assert_eq!(got[1..], [p(-30, 0, 80, 10), p(50, 0, 80, 10)]);
}

/// `column-gap` goes between items, not at the ends.
#[test]
fn layout_flex_column_gap_separates_row_items() {
    let got = run(THREE, &format!("{JC} #f {{ column-gap: 10px }}"));
    assert_eq!(
        got[1..],
        [p(0, 0, 50, 10), p(60, 0, 50, 10), p(120, 0, 50, 10)]
    );
}

/// Gaps come out of the free space before `space-between` splits it:
/// 400 - 150 - 2 * 10 = 230, so 115 extra between each pair.
#[test]
fn layout_flex_gap_is_subtracted_before_distributing() {
    let got = run(
        THREE,
        &format!("{JC} #f {{ column-gap: 10px; justify-content: space-between }}"),
    );
    assert_eq!(
        got[1..],
        [p(0, 0, 50, 10), p(175, 0, 50, 10), p(350, 0, 50, 10)]
    );
}

/// Gaps also reduce what grow can hand out: 400 - 2 * 20 = 360, three equals of 120.
#[test]
fn layout_flex_gap_reduces_growable_space() {
    let got = run(
        THREE,
        "#f { width: 400px; column-gap: 20px } #a, #b, #c { flex: 1; height: 10px }",
    );
    assert_eq!(
        got[1..],
        [p(0, 0, 120, 10), p(140, 0, 120, 10), p(280, 0, 120, 10)]
    );
}

/// `row-reverse` starts at the right edge and lays items leftwards, in order.
#[test]
fn layout_flex_row_reverse_starts_at_the_end_edge() {
    let got = run(
        TWO,
        "#f { width: 400px; flex-direction: row-reverse } \
         #a { width: 50px; height: 10px } #b { width: 100px; height: 10px }",
    );
    assert_eq!(got[1..], [p(350, 0, 50, 10), p(250, 0, 100, 10)]);
}

/// 5.4: `order` reorders the items; layout order follows. b (order -1) goes first.
#[test]
fn layout_flex_order_reorders_items() {
    let got = run(
        THREE,
        "#f { width: 400px } #a { width: 10px; height: 10px } \
         #b { width: 20px; height: 10px; order: -1 } #c { width: 30px; height: 10px }",
    );
    assert_eq!(
        got[1..],
        [p(0, 0, 20, 10), p(20, 0, 10, 10), p(30, 0, 30, 10)]
    );
}

/// Items with equal `order` keep document order; a positive order goes last.
#[test]
fn layout_flex_order_ties_keep_document_order() {
    let got = run(
        THREE,
        "#f { width: 400px } #a { width: 10px; height: 10px; order: 1 } \
         #b { width: 20px; height: 10px } #c { width: 30px; height: 10px }",
    );
    assert_eq!(
        got[1..],
        [p(0, 0, 20, 10), p(20, 0, 30, 10), p(50, 0, 10, 10)]
    );
}

// ---------------------------------------------------------------------------
// Wrapping: 9.3
// ---------------------------------------------------------------------------

const FIVE: &str =
    "<div id=a></div><div id=a></div><div id=a></div><div id=a></div><div id=a></div>";

/// Five 80-wide items in 200: two fit (160), three would not (240). Lines of
/// 2, 2, 1; line cross size 10 each; container height 30.
#[test]
fn layout_flex_wrap_breaks_into_lines() {
    let got = run(
        FIVE,
        "#f { width: 200px; flex-wrap: wrap } #a { width: 80px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 200, 30),
            p(0, 0, 80, 10),
            p(80, 0, 80, 10),
            p(0, 10, 80, 10),
            p(80, 10, 80, 10),
            p(0, 20, 80, 10)
        ]
    );
}

/// `row-gap` goes between lines: lines at 0, 15, 30; container 40.
#[test]
fn layout_flex_row_gap_separates_lines() {
    let got = run(
        FIVE,
        "#f { width: 200px; flex-wrap: wrap; row-gap: 5px } #a { width: 80px; height: 10px }",
    );
    assert_eq!(got[0], p(0, 0, 200, 40));
    assert_eq!(got[5], p(0, 30, 80, 10));
    assert_eq!(got[3], p(0, 15, 80, 10));
}

/// Each wrapped line distributes its own free space: line 1 holds a, b (60 + 60
/// of 200, grow 1: 100 each) and line 2 holds c alone (grows to 200).
#[test]
fn layout_flex_each_line_flexes_independently() {
    let got = run(
        THREE,
        "#f { width: 200px; flex-wrap: wrap } #a, #b, #c { flex: 1 0 120px; height: 10px }",
    );
    // 120 + 120 > 200 so every item is on its own line and grows to 200.
    assert_eq!(
        got,
        vec![
            p(0, 0, 200, 30),
            p(0, 0, 200, 10),
            p(0, 10, 200, 10),
            p(0, 20, 200, 10)
        ]
    );
}

/// `flex-flow: row wrap` is the two longhands together.
#[test]
fn layout_flex_flow_shorthand_sets_direction_and_wrap() {
    let got = run(
        FIVE,
        "#f { width: 200px; flex-flow: row wrap } #a { width: 80px; height: 10px }",
    );
    assert_eq!(got[0], p(0, 0, 200, 30));
    assert_eq!(got[5], p(0, 20, 80, 10));
}

/// 9.4 and 5.2: `wrap-reverse` swaps cross-start and cross-end, so the first line
/// is at the bottom and `flex-start` alignment hugs the bottom of each line.
///
/// Container 200 x 100. Items 80 x 10, so lines [a, b] and [c], each 10 high.
/// `align-content: stretch` shares the 80 of free space: each line 50 high. Line
/// one is nearest cross-start, which is the bottom: y 50..100, item at 90. Line
/// two is y 0..50, item at 40.
#[test]
fn layout_flex_wrap_reverse_stacks_lines_from_the_bottom() {
    let got = run(
        THREE,
        "#f { width: 200px; height: 100px; flex-wrap: wrap-reverse } \
         #a, #b, #c { width: 80px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 200, 100),
            p(0, 90, 80, 10),
            p(80, 90, 80, 10),
            p(0, 40, 80, 10)
        ]
    );
}

// ---------------------------------------------------------------------------
// Cross axis: align-items, align-self, align-content: 9.4, 9.6
// ---------------------------------------------------------------------------

const AL: &str = "#f { width: 200px; height: 100px } \
     #a { width: 50px; height: 20px } #b { width: 50px; height: 40px }";

/// `normal` behaves as `stretch` on a flex item; a definite height is not
/// stretched, an auto one is.
#[test]
fn layout_flex_align_items_stretch_fills_the_line() {
    let got = run(
        TWO,
        "#f { width: 200px; height: 100px } #a { width: 50px } #b { width: 50px; height: 30px }",
    );
    assert_eq!(got[1..], [p(0, 0, 50, 100), p(50, 0, 50, 30)]);
}

#[test]
fn layout_flex_align_items_flex_end() {
    let got = run(TWO, &format!("{AL} #f {{ align-items: flex-end }}"));
    assert_eq!(got[1..], [p(0, 80, 50, 20), p(50, 60, 50, 40)]);
}

#[test]
fn layout_flex_align_items_center() {
    let got = run(TWO, &format!("{AL} #f {{ align-items: center }}"));
    // (100 - 20) / 2 = 40 and (100 - 40) / 2 = 30.
    assert_eq!(got[1..], [p(0, 40, 50, 20), p(50, 30, 50, 40)]);
}

#[test]
fn layout_flex_align_items_flex_start() {
    let got = run(TWO, &format!("{AL} #f {{ align-items: flex-start }}"));
    assert_eq!(got[1..], [p(0, 0, 50, 20), p(50, 0, 50, 40)]);
}

/// `align-self` beats `align-items` for one item.
#[test]
fn layout_flex_align_self_overrides_align_items() {
    let got = run(
        TWO,
        &format!("{AL} #f {{ align-items: flex-start }} #b {{ align-self: flex-end }}"),
    );
    assert_eq!(got[1..], [p(0, 0, 50, 20), p(50, 60, 50, 40)]);
}

/// An auto-height container is as tall as its tallest line item: 40. Centred
/// items sit at (40 - 20) / 2 = 10 and 0.
#[test]
fn layout_flex_auto_height_is_the_tallest_item() {
    let got = run(
        TWO,
        "#f { width: 200px; align-items: center } #a { width: 50px; height: 20px } \
         #b { width: 50px; height: 40px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 200, 40), p(0, 10, 50, 20), p(50, 0, 50, 40)]
    );
}

/// Item content reflows at the flexed width, and the item's height follows it.
/// a is 200 wide holding an inner block 30 high, so a is 30 high and so is the
/// container.
#[test]
fn layout_flex_item_content_is_laid_out_at_the_used_width() {
    let got = run(
        "<div id=a><div id=n></div></div>",
        "#f { width: 400px } #a { flex: 0 0 200px } #n { height: 30px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 30), p(0, 0, 200, 30), p(0, 0, 200, 30)]
    );
}

const ML: &str = "#f { width: 200px; height: 100px; flex-wrap: wrap } \
     #a, #b, #c { width: 80px; height: 10px }";

/// Three items wrap to lines [a, b] and [c], each 10 high, free space 80.
#[test]
fn layout_flex_align_content_flex_start() {
    let got = run(THREE, &format!("{ML} #f {{ align-content: flex-start }}"));
    assert_eq!(got[3..], [p(0, 10, 80, 10)]);
}

#[test]
fn layout_flex_align_content_flex_end() {
    let got = run(THREE, &format!("{ML} #f {{ align-content: flex-end }}"));
    // Lines at 80 and 90.
    assert_eq!(got[1], p(0, 80, 80, 10));
    assert_eq!(got[3], p(0, 90, 80, 10));
}

#[test]
fn layout_flex_align_content_center() {
    let got = run(THREE, &format!("{ML} #f {{ align-content: center }}"));
    // 80 free, 40 above.
    assert_eq!(got[1], p(0, 40, 80, 10));
    assert_eq!(got[3], p(0, 50, 80, 10));
}

#[test]
fn layout_flex_align_content_space_between() {
    let got = run(
        THREE,
        &format!("{ML} #f {{ align-content: space-between }}"),
    );
    assert_eq!(got[1], p(0, 0, 80, 10));
    assert_eq!(got[3], p(0, 90, 80, 10));
}

/// 80 / 2 lines = 40 per line, 20 either side: lines at 20 and 20 + 10 + 40 = 70.
#[test]
fn layout_flex_align_content_space_around() {
    let got = run(THREE, &format!("{ML} #f {{ align-content: space-around }}"));
    assert_eq!(got[1], p(0, 20, 80, 10));
    assert_eq!(got[3], p(0, 70, 80, 10));
}

/// Three equal spaces of 80 / 3 = 26.67 = 1600 app units: lines at 1600 and
/// 1600 + 600 + 1600 = 3800.
#[test]
fn layout_flex_align_content_space_evenly() {
    let got = run(THREE, &format!("{ML} #f {{ align-content: space-evenly }}"));
    assert_eq!(got[1].1, 1600);
    assert_eq!(got[3].1, 3800);
}

/// The default `normal` is `stretch`: each line grows to 50, items stay at the
/// top of their line because their heights are definite.
#[test]
fn layout_flex_align_content_stretch_grows_the_lines() {
    let got = run(THREE, ML);
    assert_eq!(got[1], p(0, 0, 80, 10));
    assert_eq!(got[3], p(0, 50, 80, 10));
}

/// Auto-height items on a stretched line take its whole height.
#[test]
fn layout_flex_align_content_stretch_stretches_auto_height_items() {
    let got = run(
        THREE,
        "#f { width: 200px; height: 100px; flex-wrap: wrap } #a, #b, #c { width: 80px }",
    );
    // Each line is 100 / 2 = 50 high and the items stretch to it.
    assert_eq!(got[1], p(0, 0, 80, 50));
    assert_eq!(got[3], p(0, 50, 80, 50));
}

// ---------------------------------------------------------------------------
// Auto margins: 8.1
// ---------------------------------------------------------------------------

/// An auto margin takes the free space before `justify-content` sees it.
#[test]
fn layout_flex_auto_margin_pushes_the_item_to_the_end() {
    let got = run(
        TWO,
        "#f { width: 400px } #a, #b { width: 100px; height: 10px } #b { margin-left: auto }",
    );
    assert_eq!(got[1..], [p(0, 0, 100, 10), p(300, 0, 100, 10)]);
}

/// Auto margins win over `justify-content`: with one it is ignored.
#[test]
fn layout_flex_auto_margin_beats_justify_content() {
    let got = run(
        TWO,
        "#f { width: 400px; justify-content: center } \
         #a, #b { width: 100px; height: 10px } #b { margin-left: auto }",
    );
    assert_eq!(got[1..], [p(0, 0, 100, 10), p(300, 0, 100, 10)]);
}

/// Auto margins on all four sides centre the item in both axes:
/// x = (400 - 100) / 2, y = (100 - 10) / 2.
#[test]
fn layout_flex_auto_margins_centre_in_both_axes() {
    let got = run(
        "<div id=a></div>",
        "#f { width: 400px; height: 100px } #a { width: 100px; height: 10px; margin: auto }",
    );
    assert_eq!(got[1], p(150, 45, 100, 10));
}

/// A cross-axis auto margin pushes the item to the far edge.
#[test]
fn layout_flex_auto_margin_top_pushes_the_item_down() {
    let got = run(
        "<div id=a></div>",
        "#f { width: 400px; height: 100px } #a { width: 100px; height: 20px; margin-top: auto }",
    );
    assert_eq!(got[1], p(0, 80, 100, 20));
}

/// Fixed margins count in the outer sizes: margin 10 either side of a 100 item
/// puts it at 10, and the second item starts at 10 + 100 + 10 + 10.
#[test]
fn layout_flex_margins_are_part_of_the_outer_size() {
    let got = run(
        TWO,
        "#f { width: 400px } #a, #b { width: 100px; height: 10px; margin: 0 10px }",
    );
    assert_eq!(got[1..], [p(10, 0, 100, 10), p(130, 0, 100, 10)]);
}

/// Padding and border are inside the item's border box. A 100 wide content box
/// with 10px padding and a 5px border is 130 wide.
#[test]
fn layout_flex_padding_and_border_widen_the_border_box() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { width: 100px; height: 10px; padding: 10px; \
         border: 5px solid } #b { width: 50px; height: 10px }",
    );
    assert_eq!(got[1], p(0, 0, 130, 40));
    // b has a definite height, so it is not stretched to the 40px line.
    assert_eq!(got[2], p(130, 0, 50, 10));
}

/// `box-sizing: border-box` makes `width` the border box.
#[test]
fn layout_flex_border_box_sizing_applies_to_flex_basis() {
    let got = run(
        TWO,
        "#f { width: 400px } #a { width: 100px; height: 10px; padding: 10px; \
         box-sizing: border-box } #b { width: 50px; height: 10px }",
    );
    // Height 10 border-box cannot be smaller than the 20px of padding, so the
    // content box is 0 and the border box 20.
    assert_eq!(got[1], p(0, 0, 100, 20));
    assert_eq!(got[2], p(100, 0, 50, 10));
}

// ---------------------------------------------------------------------------
// Column direction
// ---------------------------------------------------------------------------

const COL: &str = "#f { width: 200px; flex-direction: column }";

/// Items stack, and `align-items: normal` stretches them across the width.
#[test]
fn layout_flex_column_stacks_and_stretches() {
    let got = run(
        TWO,
        &format!("{COL} #f {{ height: 300px }} #a {{ height: 50px }} #b {{ height: 100px }}"),
    );
    assert_eq!(
        got,
        vec![p(0, 0, 200, 300), p(0, 0, 200, 50), p(0, 50, 200, 100)]
    );
}

/// Free space 300 - 150 = 150 shared 1:2 -> +50, +100.
#[test]
fn layout_flex_column_grow_uses_the_container_height() {
    let got = run(
        TWO,
        &format!(
            "{COL} #f {{ height: 300px }} #a {{ height: 50px; flex-grow: 1 }} \
             #b {{ height: 100px; flex-grow: 2 }}"
        ),
    );
    assert_eq!(got[1..], [p(0, 0, 200, 100), p(0, 100, 200, 200)]);
}

/// With `height: auto` the container is as tall as its items.
#[test]
fn layout_flex_column_auto_height_is_the_sum_of_items() {
    let got = run(
        TWO,
        &format!("{COL} #a {{ height: 50px }} #b {{ height: 100px }}"),
    );
    assert_eq!(
        got,
        vec![p(0, 0, 200, 150), p(0, 0, 200, 50), p(0, 50, 200, 100)]
    );
}

/// 9.2 step 3 and 9.7: an auto-height column raised by `min-height` has free space
/// to distribute. 300 - 150 = 150, centred: offset 75.
#[test]
fn layout_flex_column_min_height_leaves_space_for_justify_content() {
    let got = run(
        TWO,
        &format!(
            "{COL} #f {{ min-height: 300px; justify-content: center }} \
             #a {{ height: 50px }} #b {{ height: 100px }}"
        ),
    );
    assert_eq!(got[0], p(0, 0, 200, 300));
    assert_eq!(got[1..], [p(0, 75, 200, 50), p(0, 125, 200, 100)]);
}

/// Bases 100 and 300 in 100: overflow 300, scaled 100 : 300 -> -75, -225.
/// Empty items have no content minimum, so they shrink to 25 and 75.
#[test]
fn layout_flex_column_shrink_is_weighted_by_base_size() {
    let got = run(
        TWO,
        &format!("{COL} #f {{ height: 100px }} #a {{ height: 100px }} #b {{ height: 300px }}"),
    );
    assert_eq!(got[1..], [p(0, 0, 200, 25), p(0, 25, 200, 75)]);
}

/// Centred in the cross axis: x = (200 - 50) / 2 and (200 - 100) / 2.
#[test]
fn layout_flex_column_align_items_center() {
    let got = run(
        TWO,
        &format!(
            "{COL} #f {{ align-items: center }} #a {{ width: 50px; height: 20px }} \
             #b {{ width: 100px; height: 20px }}"
        ),
    );
    assert_eq!(
        got,
        vec![p(0, 0, 200, 40), p(75, 0, 50, 20), p(50, 20, 100, 20)]
    );
}

/// `column-reverse` starts at the bottom: a occupies 80..100, b 50..80.
#[test]
fn layout_flex_column_reverse_starts_at_the_bottom() {
    let got = run(
        TWO,
        "#f { width: 200px; height: 100px; flex-direction: column-reverse } \
         #a { height: 20px } #b { height: 30px }",
    );
    assert_eq!(got[1..], [p(0, 80, 200, 20), p(0, 50, 200, 30)]);
}

/// A wrapping column breaks on height. Items 60 high in a 100 container: one per
/// line. Lines are 50 wide (the items'), 150 free in a 300 wide container, shared
/// by `align-content: stretch` -> 100 each. Items hug each line's start.
#[test]
fn layout_flex_column_wrap_makes_lines_of_columns() {
    let got = run(
        THREE,
        "#f { width: 300px; height: 100px; flex-flow: column wrap } \
         #a, #b, #c { width: 50px; height: 60px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 300, 100),
            p(0, 0, 50, 60),
            p(100, 0, 50, 60),
            p(200, 0, 50, 60)
        ]
    );
}

/// Column items with auto width that are not stretched are as wide as their
/// content: four characters at 5px.
#[test]
fn layout_flex_column_fit_content_width_when_not_stretched() {
    let got = run(
        "<div id=a>abcd</div>",
        &format!("{COL} #f {{ align-items: flex-start }}"),
    );
    assert_eq!(got, vec![p(0, 0, 200, 20), p(0, 0, 20, 20)]);
}

// ---------------------------------------------------------------------------
// Items and containers of other kinds
// ---------------------------------------------------------------------------

/// 4: text directly in a flex container becomes one anonymous flex item, 9
/// characters at 5px = 45 wide, one 20px line.
#[test]
fn layout_flex_text_becomes_an_anonymous_item() {
    let got = run("abcd efgh", "#f { width: 400px }");
    assert_eq!(got, vec![p(0, 0, 400, 20), p(0, 0, 45, 20)]);
}

/// Whitespace between items generates no item.
#[test]
fn layout_flex_whitespace_between_items_generates_nothing() {
    let got = run(
        "\n  <div id=a></div>\n  <div id=b></div>\n",
        "#f { width: 400px } #a, #b { width: 50px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 50, 10), p(50, 0, 50, 10)]
    );
}

/// A `display: none` child is not an item.
#[test]
fn layout_flex_display_none_child_generates_no_item() {
    let got = run(
        THREE,
        "#f { width: 400px } #a, #b, #c { width: 50px; height: 10px } #b { display: none }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 50, 10), p(50, 0, 50, 10)]
    );
}

/// An inline child of a flex container is blockified: it is an item with its own
/// box, 4 characters wide.
#[test]
fn layout_flex_inline_child_is_blockified() {
    let got = run(
        "<span id=a>abcd</span><span id=b>xy</span>",
        "#f { width: 400px } span { display: inline }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 20), p(0, 0, 20, 20), p(20, 0, 10, 20)]
    );
}

/// 4.1: an absolutely positioned child is out of flow. It takes no part in the
/// line and does not size the container.
#[test]
fn layout_flex_absolute_child_is_not_an_item() {
    let got = run(
        TWO,
        "#f { width: 400px; position: relative } #a { width: 50px; height: 10px } \
         #b { position: absolute; left: 10px; top: 5px; width: 30px; height: 30px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 50, 10), p(10, 5, 30, 30)]
    );
}

/// A flex container inside a flex item. The inner one fills its item (`flex: 1`
/// takes all 400) and lays its own children out in a row.
#[test]
fn layout_flex_nested_containers() {
    let got = run_page(
        "<html><body><div id=f><div id=g><div id=a></div><div id=b></div></div></div></body></html>",
        "#f { width: 400px } #g { display: flex; flex: 1 } \
         #a, #b { width: 50px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 400, 10),
            p(0, 0, 400, 10),
            p(0, 0, 50, 10),
            p(50, 0, 50, 10)
        ]
    );
}

/// A nested container's intrinsic width is the *sum* of its items, not the widest.
/// Outer items are `auto` so they size to content: g is 50 + 100 wide, h follows.
#[test]
fn layout_flex_intrinsic_width_of_a_row_container_sums_its_items() {
    let got = run_page(
        "<html><body><div id=f><div id=g><div id=a></div><div id=b></div></div>\
         <div id=h></div></div></body></html>",
        "#f { width: 400px } #g { display: flex } #a { width: 50px; height: 10px } \
         #b { width: 100px; height: 10px } #h { width: 20px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![
            p(0, 0, 400, 10),
            p(0, 0, 150, 10),
            p(0, 0, 50, 10),
            p(50, 0, 100, 10),
            p(150, 0, 20, 10)
        ]
    );
}

/// An `inline-flex` container shrinks to fit its items: 50 + 100.
#[test]
fn layout_flex_inline_flex_shrinks_to_fit() {
    let got = run(
        TWO,
        "#f { display: inline-flex } #a { width: 50px; height: 20px } \
         #b { width: 100px; height: 20px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 150, 20), p(0, 0, 50, 20), p(50, 0, 100, 20)]
    );
}

/// The container's own padding and border offset the items and add to its size.
#[test]
fn layout_flex_container_padding_and_border() {
    let got = run(
        TWO,
        "#f { width: 400px; padding: 10px; border: 5px solid } \
         #a, #b { width: 50px; height: 10px }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 430, 40), p(15, 15, 50, 10), p(65, 15, 50, 10)]
    );
}

/// A flex container's margins do not collapse with its first item's.
#[test]
fn layout_flex_container_margins_do_not_collapse_with_items() {
    let got = run_page(
        "<html><body><div id=f><div id=a></div></div></body></html>",
        "#f { width: 400px; margin-top: 10px } #a { height: 10px; margin-top: 20px }",
    );
    // The item's margin stays inside the container: the container sits at 10 and
    // its item at 10 + 20.
    // The item is a row item with no width, so it is zero wide.
    assert_eq!(got, vec![p(0, 10, 400, 30), p(0, 30, 0, 10)]);
}

/// Floats do not apply to flex items (3): the item is laid out in the line.
#[test]
fn layout_flex_float_has_no_effect_on_an_item() {
    let got = run(
        TWO,
        "#f { width: 400px } #a, #b { width: 50px; height: 10px } #a { float: left }",
    );
    assert_eq!(
        got,
        vec![p(0, 0, 400, 10), p(0, 0, 50, 10), p(50, 0, 50, 10)]
    );
}

/// Margins on the container are not auto-resolved to centring the way a block's are
/// once it has an explicit width: a flex container is a block-level box.
#[test]
fn layout_flex_container_auto_margins_centre_like_a_block() {
    let got = run_page(
        "<html><body><div id=f><div id=a></div></div></body></html>",
        "#f { width: 400px; margin: 0 auto } #a { width: 50px; height: 10px }",
    );
    assert_eq!(got, vec![p(200, 0, 400, 10), p(200, 0, 50, 10)]);
}

// ---------------------------------------------------------------------------
// Robustness
// ---------------------------------------------------------------------------

/// Two runs over the same input give the same geometry.
#[test]
fn layout_flex_is_deterministic() {
    let css = "#f { width: 300px; flex-wrap: wrap; justify-content: space-around } \
               #a, #b, #c { flex: 1 1 100px; height: 10px }";
    assert_eq!(run(THREE, css), run(THREE, css));
}

/// Flex containers nested past the DOM's depth limit lay out without overflowing a small stack.
#[test]
fn layout_flex_deep_nesting_does_not_overflow_the_stack() {
    // px-dom stops nesting at 512, so ask for a little more and expect that many.
    let depth = 600;
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(move || {
            let mut html = String::from("<html><body>");
            for _ in 0..depth {
                html.push_str("<div class=n>");
            }
            for _ in 0..depth {
                html.push_str("</div>");
            }
            html.push_str("</body></html>");
            let got = run_page(&html, ".n { display: flex }");
            assert!(got.len() >= 500, "only {} fragments", got.len());
        })
        .expect("the thread spawns");
    handle.join().expect("no stack overflow, no panic");
}
