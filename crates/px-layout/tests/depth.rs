//! Phase 6 gate item: layout walks are iterative.
//!
//! 100,000 nested boxes overflow the stack of any recursive walk. Building,
//! laying out and *dropping* the tree all have to survive it; Drop is the one
//! that gets forgotten, because the compiler writes it.

use px_layout::{Au, BoxStyle, LayoutTree, Length, layout};

#[test]
fn a_hundred_thousand_levels_of_nesting_lay_out_and_drop() {
    const DEPTH: usize = 100_000;
    let style = BoxStyle {
        padding: px_layout::Edges {
            top: Length::px(1),
            ..Default::default()
        },
        ..Default::default()
    };
    let mut tree = LayoutTree::new();
    let mut parent = None;
    for _ in 0..DEPTH {
        parent = tree.push(parent, None, style);
        assert!(parent.is_some());
    }
    layout(&mut tree, Au::from_px(800));
    let root = tree.root().unwrap();
    // Each level contributes 1px of top padding, and the innermost is empty.
    assert_eq!(
        tree.get(root).unwrap().geometry.height,
        Au::from_px(DEPTH as i32)
    );
    drop(tree);
}

#[test]
fn a_wide_flat_tree_lays_out_in_linear_order() {
    let mut tree = LayoutTree::new();
    let root = tree.push(None, None, BoxStyle::default()).unwrap();
    let style = BoxStyle {
        height: Length::px(1),
        ..Default::default()
    };
    let kids: Vec<_> = (0..50_000)
        .map(|_| tree.push(Some(root), None, style).unwrap())
        .collect();
    layout(&mut tree, Au::from_px(800));
    let last = tree.get(*kids.last().unwrap()).unwrap().geometry;
    assert_eq!(last.y, Au::from_px(49_999));
}
