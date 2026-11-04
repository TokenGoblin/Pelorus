//! Phase 6 gate item: block layout geometry.
//!
//! ADR 029: WPT CSS2 reftests cannot run before paint (Phase 8), so the
//! behaviour they check is asserted here as border-box geometry, with the spec
//! section each case comes from. Expected values are worked by hand from
//! CSS2, not read back from the implementation.

use std::collections::HashMap;

use px_layout::{Au, BoxId, BoxStyle, Display, Edges, LayoutTree, Length, layout};

fn px(n: i32) -> Au {
    Au::from_px(n)
}

/// Build and lay out `html` in an 800px viewport. Elements are styled by id
/// from `styles`; `<head>` is `display: none`; anything else gets defaults.
fn lay(html: &str, styles: &[(&str, BoxStyle)]) -> (LayoutTree, HashMap<String, BoxId>) {
    let dom = px_dom::parse(html);
    let arena = dom.arena;
    let table: HashMap<&str, BoxStyle> = styles.iter().copied().collect();
    let mut tree = LayoutTree::build(&arena, |node| {
        let n = arena.get(node)?;
        let name = n.element_name()?.local.to_string();
        if name == "head" {
            return None;
        }
        let id = n
            .attrs()?
            .iter()
            .find(|a| &*a.name.local == "id")
            .map(|a| a.value.to_string());
        Some(
            id.and_then(|i| table.get(i.as_str()).copied())
                .unwrap_or_default(),
        )
    });
    layout(&mut tree, px(800));
    let mut ids = HashMap::new();
    let mut stack: Vec<BoxId> = tree.root().into_iter().collect();
    while let Some(b) = stack.pop() {
        if let Some(node) = tree.get(b).and_then(|x| x.node)
            && let Some(attrs) = arena.get(node).and_then(|n| n.attrs())
            && let Some(a) = attrs.iter().find(|a| &*a.name.local == "id")
        {
            ids.insert(a.value.to_string(), b);
        }
        stack.extend_from_slice(tree.children(b));
    }
    (tree, ids)
}

fn geo(tree: &LayoutTree, ids: &HashMap<String, BoxId>, id: &str) -> px_layout::Geometry {
    let b = ids.get(id).unwrap_or_else(|| panic!("no box for #{id}"));
    tree.get(*b).unwrap().geometry
}

fn block() -> BoxStyle {
    BoxStyle::default()
}

fn h(n: i32) -> BoxStyle {
    BoxStyle {
        height: Length::px(n),
        ..block()
    }
}

fn with_margin(mut s: BoxStyle, top: i32, bottom: i32) -> BoxStyle {
    s.margin.top = Length::px(top);
    s.margin.bottom = Length::px(bottom);
    s
}

// ---- CSS2 §10.3.3: widths and horizontal margins ---------------------------

#[test]
fn auto_width_fills_the_containing_block_minus_margins() {
    let mut a = h(10);
    a.margin.left = Length::px(10);
    a.margin.right = Length::px(20);
    let (t, ids) = lay("<div id=a></div>", &[("a", a)]);
    let g = geo(&t, &ids, "a");
    assert_eq!((g.x, g.width), (px(10), px(770)));
}

#[test]
fn auto_margins_centre_a_fixed_width_box() {
    let mut a = h(10);
    a.width = Length::px(200);
    a.margin.left = Length::Auto;
    a.margin.right = Length::Auto;
    let (t, ids) = lay("<div id=a></div>", &[("a", a)]);
    let g = geo(&t, &ids, "a");
    assert_eq!((g.x, g.width), (px(300), px(200)));
}

#[test]
fn over_constrained_widths_give_way_on_the_right_margin() {
    let mut a = h(10);
    a.width = Length::px(200);
    a.margin.left = Length::px(10);
    a.margin.right = Length::px(10);
    let (t, ids) = lay("<div id=a></div>", &[("a", a)]);
    let g = geo(&t, &ids, "a");
    assert_eq!((g.x, g.width, g.margin.right), (px(10), px(200), px(590)));
}

#[test]
fn padding_and_border_are_inside_an_auto_width() {
    let mut a = h(10);
    a.padding = Edges::all(Length::px(5));
    a.border = Edges::all(px(2));
    let (t, ids) = lay("<div id=a></div>", &[("a", a)]);
    let g = geo(&t, &ids, "a");
    assert_eq!(g.width, px(800), "border box fills the containing block");
    assert_eq!(
        g.height,
        px(10 + 14),
        "content height plus padding and border"
    );
    let (cx, cy, cw, ch) = g.content_box();
    assert_eq!((cx, cy, cw, ch), (px(7), px(7), px(786), px(10)));
}

#[test]
fn percentages_resolve_against_the_containing_block_width() {
    let mut a = h(10);
    a.padding.left = Length::Percent(0.1);
    a.width = Length::Percent(0.5);
    let (t, ids) = lay("<div id=a></div>", &[("a", a)]);
    let g = geo(&t, &ids, "a");
    assert_eq!(g.width, px(400 + 80));
}

#[test]
fn nested_blocks_resolve_against_their_parents_content_box() {
    let mut p = block();
    p.padding.left = Length::px(50);
    p.padding.right = Length::px(50);
    let (t, ids) = lay(
        "<div id=p><div id=c style=x></div></div>",
        &[("p", p), ("c", h(5))],
    );
    let c = geo(&t, &ids, "c");
    assert_eq!((c.x, c.width), (px(50), px(700)));
}

// ---- CSS2 §8.3.1: collapsing margins ---------------------------------------

#[test]
fn adjacent_sibling_margins_collapse_to_the_larger() {
    let (t, ids) = lay(
        "<div id=a></div><div id=b></div>",
        &[
            ("a", with_margin(h(50), 0, 20)),
            ("b", with_margin(h(50), 30, 0)),
        ],
    );
    assert_eq!(geo(&t, &ids, "b").y, px(80));
}

#[test]
fn a_positive_and_a_negative_margin_are_summed() {
    let (t, ids) = lay(
        "<div id=a></div><div id=b></div>",
        &[
            ("a", with_margin(h(50), 0, 20)),
            ("b", with_margin(h(50), -5, 0)),
        ],
    );
    assert_eq!(geo(&t, &ids, "b").y, px(65));
}

#[test]
fn two_negative_margins_collapse_to_the_most_negative() {
    let (t, ids) = lay(
        "<div id=a></div><div id=b></div>",
        &[
            ("a", with_margin(h(50), 0, -10)),
            ("b", with_margin(h(50), -20, 0)),
        ],
    );
    assert_eq!(geo(&t, &ids, "b").y, px(30));
}

#[test]
fn a_first_childs_top_margin_collapses_with_its_parents() {
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div>",
        &[
            ("p", with_margin(block(), 10, 0)),
            ("c", with_margin(h(10), 30, 0)),
        ],
    );
    assert_eq!(geo(&t, &ids, "p").y, px(30));
    assert_eq!(geo(&t, &ids, "c").y, px(30));
}

#[test]
fn top_padding_stops_the_parent_child_collapse() {
    let mut p = with_margin(block(), 10, 0);
    p.padding.top = Length::px(1);
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div>",
        &[("p", p), ("c", with_margin(h(10), 30, 0))],
    );
    assert_eq!(geo(&t, &ids, "p").y, px(10));
    assert_eq!(geo(&t, &ids, "c").y, px(10 + 1 + 30));
}

#[test]
fn a_bfc_root_does_not_collapse_with_its_children() {
    let mut p = with_margin(block(), 10, 0);
    p.establishes_bfc = true;
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div>",
        &[("p", p), ("c", with_margin(h(10), 30, 0))],
    );
    assert_eq!(geo(&t, &ids, "p").y, px(10));
    assert_eq!(geo(&t, &ids, "c").y, px(40));
}

#[test]
fn a_last_childs_bottom_margin_escapes_an_auto_height_parent() {
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div><div id=n></div>",
        &[
            ("p", with_margin(block(), 0, 10)),
            ("c", with_margin(h(10), 0, 30)),
            ("n", h(5)),
        ],
    );
    assert_eq!(
        geo(&t, &ids, "p").height,
        px(10),
        "the margin is not inside p"
    );
    assert_eq!(geo(&t, &ids, "n").y, px(40), "max(10, 30) below p");
}

#[test]
fn an_explicit_height_keeps_the_last_childs_margin_inside() {
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div><div id=n></div>",
        &[
            ("p", with_margin(h(50), 0, 10)),
            ("c", with_margin(h(10), 0, 30)),
            ("n", h(5)),
        ],
    );
    assert_eq!(geo(&t, &ids, "n").y, px(60));
}

#[test]
fn an_empty_blocks_own_margins_collapse_through_it() {
    let (t, ids) = lay(
        "<div id=a></div><div id=e></div><div id=b></div>",
        &[
            ("a", with_margin(h(50), 0, 10)),
            ("e", with_margin(block(), 20, 30)),
            ("b", h(5)),
        ],
    );
    assert_eq!(geo(&t, &ids, "e").height, px(0));
    assert_eq!(geo(&t, &ids, "b").y, px(80), "max(10, 20, 30) below a");
}

#[test]
fn bottom_padding_stops_the_last_child_collapse() {
    let mut p = block();
    p.padding.bottom = Length::px(2);
    let (t, ids) = lay(
        "<div id=p><div id=c></div></div><div id=n></div>",
        &[("p", p), ("c", with_margin(h(10), 0, 30)), ("n", h(5))],
    );
    assert_eq!(geo(&t, &ids, "p").height, px(10 + 30 + 2));
    assert_eq!(geo(&t, &ids, "n").y, px(42));
}

// ---- CSS2 §10.6.3: heights --------------------------------------------------

#[test]
fn auto_height_is_the_bottom_of_the_last_in_flow_child() {
    let (t, ids) = lay(
        "<div id=p><div id=a></div><div id=b></div></div>",
        &[("a", h(10)), ("b", h(20))],
    );
    assert_eq!(geo(&t, &ids, "p").height, px(30));
    assert_eq!(geo(&t, &ids, "b").y, px(10));
}

#[test]
fn explicit_height_wins_over_content() {
    let (t, ids) = lay(
        "<div id=p><div id=a></div></div>",
        &[("p", h(5)), ("a", h(100))],
    );
    assert_eq!(geo(&t, &ids, "p").height, px(5));
}

// ---- Tree construction ------------------------------------------------------

#[test]
fn display_none_generates_no_box_for_the_element_or_its_descendants() {
    let none = BoxStyle {
        display: Display::None,
        ..block()
    };
    let (t, ids) = lay(
        "<div id=a></div><div id=gone><div id=inner></div></div><div id=b></div>",
        &[
            ("a", h(10)),
            ("gone", none),
            ("inner", h(999)),
            ("b", h(10)),
        ],
    );
    assert!(!ids.contains_key("gone") && !ids.contains_key("inner"));
    assert_eq!(geo(&t, &ids, "b").y, px(10));
}

#[test]
fn layout_is_idempotent() {
    let (mut t, ids) = lay(
        "<div id=p><div id=c></div></div><div id=n></div>",
        &[
            ("p", with_margin(block(), 3, 7)),
            ("c", with_margin(h(11), 5, 13)),
            ("n", h(2)),
        ],
    );
    let snapshot = |t: &LayoutTree| -> Vec<_> {
        ids.values()
            .map(|b| (*b, t.get(*b).unwrap().geometry))
            .collect::<Vec<_>>()
    };
    let mut first = snapshot(&t);
    first.sort_by_key(|(b, _)| *b);
    layout(&mut t, px(800));
    let mut second = snapshot(&t);
    second.sort_by_key(|(b, _)| *b);
    assert_eq!(first, second);
}

#[test]
fn identical_input_gives_an_identical_box_tree() {
    let html = "<div id=a><div id=b></div><div id=c></div></div><div id=d></div>";
    let styles = [
        ("a", with_margin(block(), 4, 9)),
        ("b", h(3)),
        ("c", with_margin(h(8), 2, 6)),
        ("d", h(1)),
    ];
    let run = || {
        let (t, _) = lay(html, &styles);
        (0..t.len() as u32).filter_map(|_| None::<()>).count();
        let mut out = Vec::new();
        let mut stack: Vec<BoxId> = t.root().into_iter().collect();
        while let Some(b) = stack.pop() {
            out.push((b, t.get(b).unwrap().geometry, t.children(b).to_vec()));
            stack.extend(t.children(b).iter().rev());
        }
        out
    };
    assert_eq!(run(), run());
}
