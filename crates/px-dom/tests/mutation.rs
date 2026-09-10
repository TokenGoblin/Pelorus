//! Mutation-safe iteration.
//!
//! §9 Phase 4 names it, and it is worth being precise about what it means
//! here, because the phrase usually describes something this DOM gets for
//! free and does not describe the thing that actually bites.
//!
//! The borrow checker already refuses to let a caller hold a borrowing
//! iterator across a mutation — that half needs no tests, and a test asserting
//! it would be asserting that Rust works.
//!
//! What needs testing is the pattern that compiles: **collect handles, mutate,
//! then use what was collected.** Every real caller does this, because every
//! real caller mutates the tree it is walking. In a DOM whose handles are
//! indices, the collected handles silently come to mean different nodes. Here
//! they must stop resolving instead.

use px_dom::{Arena, NodeData, NodeId, TreeError};

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

fn label(arena: &Arena, id: NodeId) -> Option<String> {
    arena
        .get(id)
        .and_then(|node| node.text())
        .map(|text| text.to_string())
}

/// Collect, remove, then use the collected handles.
///
/// The survivors must still be themselves, and the removed ones must be gone —
/// not silently reassigned.
#[test]
fn dom_mutation_collected_handles_do_not_drift_onto_other_nodes() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let children: Vec<NodeId> = (0..64)
        .map(|i| {
            let child = node(&mut arena, &format!("c{i}"));
            arena.append_child(doc, child).expect("append");
            child
        })
        .collect();

    // Remove every other child, freeing 32 slots.
    for (i, id) in children.iter().enumerate() {
        if i % 2 == 0 {
            arena.remove_subtree(*id).expect("remove");
        }
    }

    // Allocate into the freed slots. This is the step that makes the test
    // meaningful: without it, "the handle does not resolve" is true simply
    // because nothing occupies the slot.
    let mut reused = Vec::new();
    for i in 0..32 {
        let fresh = node(&mut arena, &format!("new{i}"));
        arena.append_child(doc, fresh).expect("append");
        reused.push(fresh);
    }

    let mut slots_actually_reused = 0;
    for (i, id) in children.iter().enumerate() {
        if i % 2 == 0 {
            assert_eq!(
                label(&arena, *id),
                None,
                "handle to removed c{i} resolved after its slot was reused"
            );
            if reused.iter().any(|fresh| fresh.index() == id.index()) {
                slots_actually_reused += 1;
            }
        } else {
            assert_eq!(
                label(&arena, *id),
                Some(format!("c{i}")),
                "a surviving node changed identity"
            );
        }
    }
    assert_eq!(
        slots_actually_reused, 32,
        "every freed slot should have been taken; if the allocator stops \
         reusing them this test is no longer testing anything"
    );
}

/// Mutating while holding a snapshot of children.
///
/// `Arena::children` hands back a `Vec` precisely so a caller can do this. The
/// handles in it must behave the same way: survivors resolve, removed ones do
/// not.
#[test]
fn dom_mutation_snapshot_of_children_survives_reordering() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let parent = node(&mut arena, "parent");
    arena.append_child(doc, parent).expect("append");
    for i in 0..8 {
        let child = node(&mut arena, &format!("c{i}"));
        arena.append_child(parent, child).expect("append");
    }

    let snapshot = arena.children(parent).expect("children");
    assert_eq!(snapshot.len(), 8);

    // Reverse the child order by re-appending each in reverse. Every one of
    // these moves rewrites the sibling links the snapshot was taken from.
    for id in snapshot.iter().rev() {
        arena.append_child(parent, *id).expect("re-append");
    }

    let after: Vec<String> = arena
        .child_ids(parent)
        .filter_map(|id| label(&arena, id))
        .collect();
    assert_eq!(
        after,
        vec!["c7", "c6", "c5", "c4", "c3", "c2", "c1", "c0"],
        "re-appending in reverse should reverse the child list"
    );

    for id in &snapshot {
        assert!(
            arena.get(*id).is_some(),
            "moving a node must not invalidate its handle"
        );
    }
}

/// `reparent_children` moves every child and leaves the source empty.
///
/// The adoption agency algorithm depends on this, and it is a walk that
/// rewrites the links it is walking — the case `Arena::children`'s snapshot
/// exists for.
#[test]
fn dom_mutation_reparent_children_moves_all_of_them() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let from = node(&mut arena, "from");
    let to = node(&mut arena, "to");
    arena.append_child(doc, from).expect("append");
    arena.append_child(doc, to).expect("append");

    for i in 0..16 {
        let child = node(&mut arena, &format!("c{i}"));
        arena.append_child(from, child).expect("append");
    }
    let moved = arena.children(from).expect("children");

    arena.reparent_children(from, to).expect("reparent");

    assert_eq!(arena.child_ids(from).count(), 0, "source left empty");
    assert_eq!(arena.child_ids(to).count(), 16, "all sixteen arrived");

    let order: Vec<String> = arena
        .child_ids(to)
        .filter_map(|id| label(&arena, id))
        .collect();
    let expected: Vec<String> = (0..16).map(|i| format!("c{i}")).collect();
    assert_eq!(order, expected, "order must be preserved");

    for id in &moved {
        assert_eq!(
            arena.get(*id).and_then(|node| node.parent()),
            Some(to),
            "every moved child should now report the new parent"
        );
    }
}

/// A mutation that would build a cycle is refused.
///
/// Not a hypothetical: the tree builder moves subtrees around, and a cycle
/// turns every subsequent walk into a hang. The bound on each traversal means
/// it would not hang forever — it would silently truncate, which is worse.
#[test]
fn dom_mutation_cycles_are_refused() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let grandparent = node(&mut arena, "gp");
    let parent = node(&mut arena, "p");
    let child = node(&mut arena, "c");
    arena.append_child(doc, grandparent).expect("append");
    arena.append_child(grandparent, parent).expect("append");
    arena.append_child(parent, child).expect("append");

    assert_eq!(
        arena.append_child(child, grandparent),
        Err(TreeError::WouldCycle),
        "putting an ancestor under its own descendant"
    );
    assert_eq!(
        arena.append_child(parent, parent),
        Err(TreeError::WouldCycle),
        "a node under itself"
    );
    assert_eq!(
        arena.append_child(child, parent),
        Err(TreeError::WouldCycle),
        "a parent under its own child"
    );

    // The tree is unchanged by the refusals.
    assert_eq!(arena.descendants(grandparent).count(), 3);
    assert_eq!(arena.depth(child), Ok(3));
}

/// Removing a node twice does not double-free its slot.
///
/// The second removal has a stale handle, so it must be refused. If it were
/// not, the slot would be pushed onto the free list twice and two live nodes
/// would eventually share it — the same confusion generations exist to
/// prevent, arrived at from the other direction.
#[test]
fn dom_mutation_double_removal_does_not_double_free() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let victim = node(&mut arena, "victim");
    arena.append_child(doc, victim).expect("append");

    assert_eq!(arena.remove_subtree(victim), Ok(1));
    assert_eq!(arena.remove_subtree(victim), Err(TreeError::NoSuchNode));

    let a = node(&mut arena, "a");
    let b = node(&mut arena, "b");
    assert_ne!(
        a.index(),
        b.index(),
        "two allocations landed in the same slot; the free list was corrupted"
    );
    assert_eq!(label(&arena, a).as_deref(), Some("a"));
    assert_eq!(label(&arena, b).as_deref(), Some("b"));
}

/// Nothing here retires a slot, and the arena says so.
///
/// ADR 018 chose the 32/32 layout on the argument that retirement is
/// unreachable in practice. This asserts the premise on a workload that churns
/// hard, so that if retirement ever starts happening during ordinary testing
/// the ADR's reasoning gets revisited rather than quietly outlived.
#[test]
fn dom_mutation_churn_retires_nothing() {
    let mut arena = Arena::new();
    let doc = arena.document();

    for _ in 0..10_000 {
        let id = node(&mut arena, "churn");
        arena.append_child(doc, id).expect("append");
        arena.remove_subtree(id).expect("remove");
    }

    assert_eq!(
        arena.retired(),
        0,
        "ten thousand reuses retired a slot; with 32 generation bits that \
         should take four billion, so either the layout changed or the \
         generation counter is not doing what ADR 018 assumed"
    );
    assert_eq!(arena.len(), 1, "only the document survives");
}

/// A parsed document validates, and so does one that has been mutated.
///
/// This does not prove `Arena::validate` can *detect* anything — the public
/// API cannot build a corrupt tree, which is the whole point of it. What it
/// proves is that the validator agrees with the arena about what a well-formed
/// tree is, which is the half that rots: a validator whose idea of the
/// invariants drifts from the code's starts rejecting healthy trees, and the
/// tempting fix is to loosen the validator.
///
/// The detection half is checked by breaking the arena on purpose; see
/// `Arena::validate`'s documentation for the procedure and what it produced.
#[test]
fn dom_mutation_validate_accepts_real_trees() {
    let dom = px_dom::parse(
        r#"<!DOCTYPE html><html><head><title>t</title></head>
           <body><p>a<b>b</b></p><table><tr><td>d</td></tr></table>
           <ul><li>1<li>2</ul><template><span>x</span></template></body></html>"#,
    );
    assert_eq!(dom.arena.validate(), Ok(()), "a parsed document");

    let mut arena = Arena::new();
    let doc = arena.document();
    let mut nodes = Vec::new();
    for i in 0..64 {
        let child = node(&mut arena, &format!("c{i}"));
        let parent = if i % 3 == 0 {
            doc
        } else {
            nodes.last().copied().unwrap_or(doc)
        };
        arena.append_child(parent, child).expect("append");
        nodes.push(child);
    }
    assert_eq!(arena.validate(), Ok(()), "after building");

    for id in nodes.iter().step_by(5) {
        let _ = arena.remove_subtree(*id);
        assert_eq!(arena.validate(), Ok(()), "after removing {id:?}");
    }
    for id in nodes.iter().rev().take(8) {
        let _ = arena.append_child(doc, *id);
        assert_eq!(arena.validate(), Ok(()), "after moving {id:?}");
    }
}

/// A slot's address does not move when the arena grows.
///
/// This is the property `docs/research/stylo-requirements.md` §3.3 calls the
/// Phase 4 decision, and ADR 020 records. Phase 5's `StyleNode<'dom>` is meant
/// to hold `&'dom Slot` directly; with a flat `Vec` that is sound only because
/// the borrow of the *whole tree* is the unit of safety, which forbids ever
/// handing out a node reference that outlives a mutation window.
///
/// Comparing addresses as integers rather than holding references across the
/// growth, because holding them is exactly what the borrow checker refuses —
/// and that refusal is why the flat layout looks fine right up until Phase 5
/// needs it not to be.
#[test]
fn dom_mutation_slot_addresses_are_stable_across_growth() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let watched: Vec<NodeId> = (0..8)
        .map(|i| {
            let id = node(&mut arena, &format!("w{i}"));
            arena.append_child(doc, id).expect("append");
            id
        })
        .collect();

    let address =
        |arena: &Arena, id: NodeId| arena.get(id).map(|node| std::ptr::from_ref(node) as usize);
    let before: Vec<Option<usize>> = watched.iter().map(|id| address(&arena, *id)).collect();
    assert!(before.iter().all(Option::is_some));

    // Grow well past any plausible initial capacity, and past several chunk
    // boundaries.
    for i in 0..20_000 {
        let id = node(&mut arena, &format!("f{i}"));
        arena.append_child(doc, id).expect("append");
    }

    let after: Vec<Option<usize>> = watched.iter().map(|id| address(&arena, *id)).collect();
    assert_eq!(
        before, after,
        "a slot moved when the arena grew; Phase 5 wants to hold &'dom Slot \
         across a traversal, and a flat Vec cannot promise that"
    );
}
