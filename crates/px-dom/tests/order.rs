//! Tree ordering.
//!
//! §9 Phase 4 names "tree ordering" alongside mutation-safe iteration and
//! ranges. Document order is the primitive underneath all three: ranges are
//! defined by two points in it, selection is a range, and
//! `compareDocumentPosition` is this question exactly.

use px_dom::{Arena, NodeData, NodeId};

// `unreachable!` rather than `expect` in these helpers.
//
// The workspace denies `expect_used`, and clippy's `allow-expect-in-tests`
// exemption only reaches code inside a `#[test]` function -- a helper called
// *by* tests is ordinary code as far as the lint is concerned. That is a
// reasonable place for clippy to draw the line and there is no reason to widen
// clippy.toml for it: these really are unreachable. A freshly created arena
// cannot be exhausted, and a node built two lines above is not stale.
fn node(arena: &mut Arena, label: &str) -> NodeId {
    match arena.create(NodeData::Text {
        contents: label.into(),
    }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

fn link(arena: &mut Arena, parent: NodeId, child: NodeId) {
    if let Err(error) = arena.append_child(parent, child) {
        unreachable!("appending a fresh node cannot fail here: {error:?}");
    }
}

fn label(arena: &Arena, id: NodeId) -> String {
    arena
        .get(id)
        .and_then(|node| node.text())
        .map(|text| text.to_string())
        .unwrap_or_else(|| "<gone>".to_owned())
}

/// Build:
///
/// ```text
/// document
/// ├── a
/// │   ├── a1
/// │   └── a2
/// └── b
///     └── b1
/// ```
fn sample() -> (Arena, Vec<(&'static str, NodeId)>) {
    let mut arena = Arena::new();
    let doc = arena.document();

    let a = node(&mut arena, "a");
    let b = node(&mut arena, "b");
    let a1 = node(&mut arena, "a1");
    let a2 = node(&mut arena, "a2");
    let b1 = node(&mut arena, "b1");

    link(&mut arena, doc, a);
    link(&mut arena, doc, b);
    link(&mut arena, a, a1);
    link(&mut arena, a, a2);
    link(&mut arena, b, b1);

    (
        arena,
        vec![("a", a), ("b", b), ("a1", a1), ("a2", a2), ("b1", b1)],
    )
}

/// Document order is pre-order: a node, then its descendants, then its next
/// sibling.
#[test]
fn dom_order_is_pre_order_depth_first() {
    let (arena, _) = sample();
    let visited: Vec<String> = arena
        .descendants(arena.document())
        .map(|id| label(&arena, id))
        .collect();

    assert_eq!(
        visited,
        vec!["<gone>", "a", "a1", "a2", "b", "b1"],
        "the document node has no text, hence the first entry"
    );
}

/// Children come back in insertion order, and `insert_before` puts a node
/// where it says.
#[test]
fn dom_order_children_follow_insertion_and_insert_before() {
    let (mut arena, ids) = sample();
    let a = ids.iter().find(|(name, _)| *name == "a").expect("a").1;
    let a2 = ids.iter().find(|(name, _)| *name == "a2").expect("a2").1;

    let inserted = node(&mut arena, "a1.5");
    arena.insert_before(a2, inserted).expect("insert");

    let children: Vec<String> = arena.child_ids(a).map(|id| label(&arena, id)).collect();
    assert_eq!(children, vec!["a1", "a1.5", "a2"]);

    // And the first-child edge case, which is the one that gets the parent's
    // `first_child` pointer wrong.
    let a1 = ids.iter().find(|(name, _)| *name == "a1").expect("a1").1;
    let first = node(&mut arena, "a0");
    arena.insert_before(a1, first).expect("insert");
    let children: Vec<String> = arena.child_ids(a).map(|id| label(&arena, id)).collect();
    assert_eq!(children, vec!["a0", "a1", "a1.5", "a2"]);
}

/// `precedes` agrees with the traversal, for every pair.
///
/// Checking every pair rather than a few chosen ones: an ordering that is
/// right about the cases somebody thought of is the normal way this goes
/// wrong.
#[test]
fn dom_order_precedes_agrees_with_document_order_for_every_pair() {
    let (arena, _) = sample();
    let order: Vec<NodeId> = arena.descendants(arena.document()).collect();

    for (i, first) in order.iter().enumerate() {
        for (j, second) in order.iter().enumerate() {
            if i == j {
                assert_eq!(
                    arena.precedes(*first, *second),
                    None,
                    "a node does not precede itself; that question has no answer"
                );
                continue;
            }
            assert_eq!(
                arena.precedes(*first, *second),
                Some(i < j),
                "{} vs {}",
                label(&arena, *first),
                label(&arena, *second)
            );
        }
    }
}

/// An ancestor precedes its descendants. A descendant never precedes its
/// ancestor.
#[test]
fn dom_order_ancestors_precede_descendants() {
    let (arena, ids) = sample();
    let a = ids.iter().find(|(name, _)| *name == "a").expect("a").1;

    for descendant in arena.descendants(a).skip(1) {
        assert_eq!(arena.precedes(a, descendant), Some(true));
        assert_eq!(arena.precedes(descendant, a), Some(false));
    }
}

/// Ordering questions about stale handles have no answer.
#[test]
fn dom_order_refuses_stale_handles() {
    let (mut arena, ids) = sample();
    let a = ids.iter().find(|(name, _)| *name == "a").expect("a").1;
    let b = ids.iter().find(|(name, _)| *name == "b").expect("b").1;

    arena.remove_subtree(a).expect("remove");

    assert_eq!(
        arena.precedes(a, b),
        None,
        "a removed node has no position, and answering `false` would say b \
         comes first, which is a different and untrue claim"
    );
    assert_eq!(arena.precedes(b, a), None);
}

/// Detaching a node removes it from document order without invalidating it.
///
/// This is the distinction `remove_subtree` and `detach` draw, and getting it
/// backwards would either leak every removed node or invalidate handles the
/// tree builder still holds.
#[test]
fn dom_order_detached_nodes_leave_document_order_but_stay_alive() {
    let (mut arena, ids) = sample();
    let b = ids.iter().find(|(name, _)| *name == "b").expect("b").1;
    let b1 = ids.iter().find(|(name, _)| *name == "b1").expect("b1").1;

    arena.detach(b).expect("detach");

    let visited: Vec<String> = arena
        .descendants(arena.document())
        .map(|id| label(&arena, id))
        .collect();
    assert_eq!(visited, vec!["<gone>", "a", "a1", "a2"]);

    assert!(arena.get(b).is_some(), "detached is not removed");
    assert!(arena.get(b1).is_some(), "nor is its subtree");
    assert_eq!(
        arena.descendants(b).count(),
        2,
        "the detached subtree keeps its own shape"
    );
}
