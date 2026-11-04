//! Traversal order and stale-handle behaviour of the iterators.

use px_dom::{Arena, NodeData, NodeId};

fn text(arena: &mut Arena, s: &str) -> NodeId {
    match arena.create(NodeData::Text { contents: s.into() }) {
        Ok(id) => id,
        Err(e) => unreachable!("{e:?}"),
    }
}

fn div(arena: &mut Arena) -> NodeId {
    match arena.create(NodeData::Element {
        name: html5ever::QualName::new(
            None,
            html5ever::ns!(html),
            html5ever::LocalName::from("div"),
        ),
        attrs: Vec::new(),
        template_contents: None,
        script_already_started: false,
    }) {
        Ok(id) => id,
        Err(e) => unreachable!("{e:?}"),
    }
}

fn label(arena: &Arena, id: NodeId) -> String {
    arena
        .get(id)
        .and_then(|n| n.text())
        .map(|t| t.to_string())
        .unwrap_or_else(|| "<non-text>".into())
}

fn link(arena: &mut Arena, parent: NodeId, child: NodeId) {
    if let Err(e) = arena.append_child(parent, child) {
        unreachable!("{e:?}");
    }
}

/// doc ─ a ─ (a1, a2) ; b
fn sample() -> (Arena, NodeId, NodeId, NodeId) {
    let mut arena = Arena::new();
    let doc = arena.document();
    let a = div(&mut arena);
    let b = text(&mut arena, "b");
    let a1 = text(&mut arena, "a1");
    let a2 = text(&mut arena, "a2");
    link(&mut arena, doc, a);
    link(&mut arena, doc, b);
    link(&mut arena, a, a1);
    link(&mut arena, a, a2);
    (arena, a, a1, a2)
}

#[test]
fn dom_children_are_in_insertion_order() {
    let (arena, a, _, _) = sample();
    let labels: Vec<_> = arena.child_ids(a).map(|c| label(&arena, c)).collect();
    assert_eq!(labels, ["a1", "a2"]);
}

#[test]
fn dom_children_of_a_leaf_is_empty() {
    let (arena, _, a1, _) = sample();
    assert_eq!(arena.child_ids(a1).count(), 0);
}

#[test]
fn dom_descendants_are_in_document_order() {
    let (arena, _, _, _) = sample();
    let labels: Vec<_> = arena
        .descendants(arena.document())
        .map(|n| label(&arena, n))
        .collect();
    assert_eq!(labels, ["<non-text>", "<non-text>", "a1", "a2", "b"]);
}

#[test]
fn dom_descendants_of_a_subtree_stay_inside_it() {
    let (arena, a, _, _) = sample();
    assert_eq!(arena.descendants(a).count(), 3);
}

#[test]
fn dom_ancestors_are_nearest_first_and_end_at_the_document() {
    let (arena, a, a1, _) = sample();
    let chain: Vec<_> = arena.ancestors(a1).collect();
    assert_eq!(chain, [a, arena.document()]);
    assert_eq!(arena.ancestors(arena.document()).count(), 0);
}

#[test]
fn dom_iterators_over_a_removed_node_yield_nothing() {
    let (mut arena, a, a1, _) = sample();
    arena.remove_subtree(a).expect("remove");
    assert_eq!(arena.descendants(a).count(), 0);
    assert_eq!(arena.child_ids(a).count(), 0);
    assert_eq!(arena.ancestors(a1).count(), 0);
}

#[test]
fn dom_descendants_reflect_detach() {
    let (mut arena, a, a1, _) = sample();
    arena.detach(a1).expect("detach");
    let labels: Vec<_> = arena.descendants(a).map(|n| label(&arena, n)).collect();
    assert_eq!(labels, ["<non-text>", "a2"]);
}

#[test]
fn dom_clone_subtree_produces_a_detached_copy_in_the_same_order() {
    let (mut arena, a, _, _) = sample();
    let copy = arena.clone_subtree(a).expect("clone");
    assert_ne!(copy, a);
    assert!(arena.get(copy).and_then(|n| n.parent()).is_none());
    let labels: Vec<_> = arena.descendants(copy).map(|n| label(&arena, n)).collect();
    assert_eq!(labels, ["<non-text>", "a1", "a2"]);
    arena.validate().expect("tree stays valid");
}

#[test]
fn dom_insert_before_places_the_node_ahead_of_its_sibling() {
    let (mut arena, a, a1, _) = sample();
    let x = text(&mut arena, "x");
    arena.insert_before(a1, x).expect("insert");
    let labels: Vec<_> = arena.child_ids(a).map(|c| label(&arena, c)).collect();
    assert_eq!(labels, ["x", "a1", "a2"]);
    arena.validate().expect("valid");
}
