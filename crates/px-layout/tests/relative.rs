//! `position: relative` offsets (CSS 2.1 section 9.4.3).
//!
//! A relatively positioned box is laid out in normal flow and then shifted, with no
//! effect on its siblings or its parent's size. `left` wins over `right`, `top`
//! over `bottom`, `auto` is zero, and nested shifts accumulate. Every expected
//! number below is derived by hand from the CSS in the test.

use px_layout::block::layout_document;
use px_layout::geom::px;

/// `(x, y, width, height)` in px of every fragment with a node, in layout order.
/// Index 0 is `<html>`, 1 is `<body>`, then the document's own boxes.
fn boxes(html: &str, css: &str) -> Vec<(i32, i32, i32, i32)> {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(
        &format!("html, body, div {{ display: block; margin: 0; padding: 0 }} {css}"),
        "https://example.invalid/a.css",
    );
    let style_root = engine.style_root_for(&arena);
    engine.resolve(&arena, &style_root).expect("a root element");
    let tree = layout_document(&arena, &style_root, px(800)).expect("a root element");
    tree.in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| {
            let f = tree.get(id)?;
            f.node?;
            let n = |a: app_units::Au| a.0 / 60;
            Some((
                n(f.inline_offset),
                n(f.block_offset),
                n(f.size.inline),
                n(f.size.block),
            ))
        })
        .collect()
}

#[test]
fn layout_relative_left_top_shift_the_box_only() {
    let b = boxes(
        "<div id=a></div><div id=b></div>",
        "#a { position: relative; left: 10px; top: 20px; width: 100px; height: 50px }
         #b { width: 100px; height: 30px }",
    );
    assert_eq!(b[2], (10, 20, 100, 50));
    // The sibling is exactly where it would have been, and body did not grow.
    assert_eq!(b[3], (0, 50, 100, 30));
    assert_eq!(b[1], (0, 0, 800, 80));
}

#[test]
fn layout_relative_right_bottom_shift_the_other_way() {
    let b = boxes(
        "<div id=a></div>",
        "#a { position: relative; right: 10px; bottom: 5px; width: 100px; height: 50px }",
    );
    assert_eq!(b[2], (-10, -5, 100, 50));
}

#[test]
fn layout_relative_left_beats_right_and_top_beats_bottom() {
    let b = boxes(
        "<div id=a></div>",
        "#a { position: relative; left: 10px; right: 99px; top: 7px; bottom: 50px;
             width: 100px; height: 50px }",
    );
    assert_eq!(b[2], (10, 7, 100, 50));
}

#[test]
fn layout_relative_auto_is_zero() {
    let b = boxes(
        "<div id=a></div>",
        "#a { position: relative; width: 100px; height: 50px }",
    );
    assert_eq!(b[2], (0, 0, 100, 50));
}

#[test]
fn layout_relative_percentages_resolve_against_the_containing_block() {
    let b = boxes(
        "<div id=p><div id=a></div></div>",
        "#p { width: 400px; height: 200px }
         #a { position: relative; left: 25%; top: 10%; width: 50px; height: 50px }",
    );
    // 25% of 400, 10% of 200.
    assert_eq!(b[3], (100, 20, 50, 50));
}

#[test]
fn layout_relative_percent_top_against_auto_height_is_zero() {
    let b = boxes(
        "<div id=p><div id=a></div></div>",
        "#p { width: 400px }
         #a { position: relative; left: 50%; top: 10%; width: 50px; height: 50px }",
    );
    assert_eq!(b[3], (200, 0, 50, 50));
}

#[test]
fn layout_relative_nested_shifts_accumulate_and_carry_descendants() {
    let b = boxes(
        "<div id=a><div id=c><div id=d></div></div></div>",
        "#a { position: relative; left: 10px; top: 10px; width: 300px; height: 200px }
         #c { position: relative; left: 5px; top: -3px; width: 100px; height: 100px }
         #d { width: 20px; height: 20px }",
    );
    assert_eq!(b[2], (10, 10, 300, 200));
    assert_eq!(b[3], (15, 7, 100, 100));
    // A static descendant rides along with both ancestors' shifts.
    assert_eq!(b[4], (15, 7, 20, 20));
}

#[test]
fn layout_relative_negative_offsets_move_up_and_left() {
    let b = boxes(
        "<div id=s></div><div id=a></div>",
        "#s { width: 10px; height: 40px }
         #a { position: relative; left: -30px; top: -15px; width: 100px; height: 50px }",
    );
    assert_eq!(b[3], (-30, 25, 100, 50));
}

#[test]
fn layout_relative_absolute_child_follows_its_shifted_containing_block() {
    let b = boxes(
        "<div id=a><div id=c></div></div>",
        "#a { position: relative; left: 10px; top: 20px; width: 300px; height: 200px }
         #c { position: absolute; left: 5px; top: 6px; width: 10px; height: 10px }",
    );
    assert_eq!(b[3], (15, 26, 10, 10));
}

#[test]
fn layout_relative_deep_chain_on_a_small_stack() {
    let handle = std::thread::Builder::new()
        .stack_size(256 * 1024)
        .spawn(|| {
            // Far past px-dom's depth limit, as in depth.rs: layout must add no
            // lower limit of its own.
            let n = 100_000;
            let html = format!("{}{}", "<div>".repeat(n), "</div>".repeat(n));
            let b = boxes(&html, "div { position: relative; left: 1px; top: 1px }");
            let last = b.last().copied().expect("boxes");
            assert!(last.0 >= 100, "shifts accumulate down the chain: {last:?}");
            assert_eq!(last.0, last.1);
        })
        .expect("spawn");
    handle.join().expect("no overflow");
}

#[test]
fn layout_relative_inline_ancestor_shifts_the_blocks_inside_it() {
    // CSS 2.1 9.2.1.1: the blocks are the span's continuation, so they move with it.
    let b = boxes(
        "<span id=s><div id=a></div></span><div id=b></div>",
        "#s { position: relative; left: 12px; top: 3px }
         #a { width: 50px; height: 20px } #b { width: 50px; height: 20px }",
    );
    assert_eq!(b[2], (12, 3, 50, 20));
    assert_eq!(b[3], (0, 20, 50, 20));
}
