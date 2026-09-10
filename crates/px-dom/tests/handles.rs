//! Stale handles resolve to `None`. Always, and permanently.
//!
//! §9 Phase 4's gate item is *"stale-handle fuzz target proves every stale
//! lookup returns `None`"*. The fuzz target explores; these tests pin the
//! cases that matter and would otherwise only be hit by luck.
//!
//! The failure being prevented is DOM-level type confusion in safe Rust: a
//! handle to a removed `<script>` resolving to whatever now occupies the slot,
//! with no `unsafe` block anywhere for a reviewer to find.

use px_dom::{Arena, NodeData, TreeError};

// `unreachable!` rather than `expect` in these helpers.
//
// The workspace denies `expect_used`, and clippy's `allow-expect-in-tests`
// exemption only reaches code inside a `#[test]` function -- a helper called
// *by* tests is ordinary code as far as the lint is concerned. That is a
// reasonable place for clippy to draw the line and there is no reason to widen
// clippy.toml for it: these really are unreachable. A freshly created arena
// cannot be exhausted, and a node built two lines above is not stale.
fn text(arena: &mut Arena, s: &str) -> px_dom::NodeId {
    match arena.create(NodeData::Text { contents: s.into() }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

/// A handle to a removed node stops resolving.
#[test]
fn dom_stale_handle_does_not_resolve_after_removal() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let node = text(&mut arena, "gone");
    arena.append_child(doc, node).expect("append");

    assert!(arena.get(node).is_some());
    arena.remove_subtree(node).expect("remove");
    assert!(
        arena.get(node).is_none(),
        "a handle to a removed node must not resolve"
    );
    assert!(arena.get_mut(node).is_none(), "nor for mutation");
}

/// The one that matters: after the slot is reused, the old handle must not
/// resolve to the **new** occupant.
///
/// Returning `None` for a removed node is the easy half. Returning `None` for
/// a *reused* slot is the property the generation counter exists for, and the
/// one a naive index-based arena gets wrong while passing the test above.
#[test]
fn dom_stale_handle_does_not_resolve_to_the_slot_reuser() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let first = text(&mut arena, "first");
    arena.append_child(doc, first).expect("append");
    arena.remove_subtree(first).expect("remove");

    // The next allocation takes the freed slot.
    let second = text(&mut arena, "second");
    arena.append_child(doc, second).expect("append");

    assert_eq!(
        first.index(),
        second.index(),
        "this test is only meaningful if the slot was actually reused; if the \
         allocator stops reusing, the test needs rewriting rather than deleting"
    );
    assert_ne!(first, second, "the generation must differ");

    assert!(
        arena.get(first).is_none(),
        "the stale handle resolved to the node that took its slot -- this is \
         DOM-level type confusion, in safe Rust"
    );
    assert!(arena.get(second).is_some());
}

/// Removing a subtree invalidates every handle in it, not only the root's.
#[test]
fn dom_stale_handles_are_invalidated_throughout_a_removed_subtree() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let root = text(&mut arena, "root");
    arena.append_child(doc, root).expect("append");

    let mut descendants = Vec::new();
    let mut parent = root;
    for depth in 0..100 {
        let child = text(&mut arena, &format!("d{depth}"));
        arena.append_child(parent, child).expect("append");
        descendants.push(child);
        parent = child;
    }

    arena.remove_subtree(root).expect("remove");

    for (depth, id) in descendants.iter().enumerate() {
        assert!(
            arena.get(*id).is_none(),
            "a handle to a node at depth {depth} of a removed subtree still resolves"
        );
    }
    assert_eq!(arena.len(), 1, "only the document should be left");
}

/// Every operation refuses a stale handle rather than doing something with it.
///
/// A handle that resolves to `None` on read but is silently accepted by
/// `append_child` would put a node under a parent that no longer exists.
#[test]
fn dom_stale_handles_are_refused_by_every_tree_operation() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let dead = text(&mut arena, "dead");
    arena.append_child(doc, dead).expect("append");
    arena.remove_subtree(dead).expect("remove");

    let live = text(&mut arena, "live");
    arena.append_child(doc, live).expect("append");

    assert_eq!(arena.append_child(dead, live), Err(TreeError::NoSuchNode));
    assert_eq!(arena.append_child(live, dead), Err(TreeError::NoSuchNode));
    assert_eq!(arena.insert_before(dead, live), Err(TreeError::NoSuchNode));
    assert_eq!(arena.remove_subtree(dead), Err(TreeError::NoSuchNode));
    assert_eq!(arena.depth(dead), Err(TreeError::NoSuchNode));
    assert_eq!(arena.children(dead), None);
    assert!(!arena.contains(dead));

    // Detaching an already-dead node is the one that is not an error, because
    // detach is idempotent by design and the tree builder relies on it.
    // Assert it explicitly so the asymmetry is a decision and not an omission.
    assert_eq!(
        arena.detach(dead),
        Err(TreeError::NoSuchNode),
        "detach is idempotent for a *live* detached node, not for a dead handle"
    );
}

/// A handle from one arena must not resolve in another.
///
/// Two documents in one process is the ordinary case — a page and its
/// `<template>` contents, a fragment being parsed — and handles are plain
/// values that can be moved between them by accident.
#[test]
fn dom_stale_handles_do_not_cross_between_arenas() {
    let mut first = Arena::new();
    let mut second = Arena::new();

    let a = text(&mut first, "a");
    first.append_child(first.document(), a).expect("append");

    // Fill the second arena so the same index exists there too.
    let b = text(&mut second, "b");
    second.append_child(second.document(), b).expect("append");

    assert_eq!(
        a.index(),
        b.index(),
        "the test needs the indices to collide to be worth anything"
    );

    // This is the honest statement of what generational handles give and what
    // they do not. Within an arena, a stale handle is caught by its
    // generation. *Across* arenas, two live nodes can share an index and a
    // generation, and nothing in the handle distinguishes them -- so this
    // resolves, and it resolves to the wrong node.
    //
    // The mitigation is not in the handle, it is in the process model: an
    // arena and its handles never leave the content process that owns them,
    // and IPC carries no NodeId. Recorded as a test so the limit is written
    // down where somebody proposing to send a NodeId over IPC will meet it.
    assert!(
        second.get(a).is_some(),
        "if this ever starts returning None, handles gained arena identity and \
         this test should be rewritten to assert the stronger property"
    );
}
