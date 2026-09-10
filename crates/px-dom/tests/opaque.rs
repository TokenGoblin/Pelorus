//! The packed node identity stylo will use as a key.
//!
//! `docs/research/stylo-requirements.md` §3.5. Stylo keys its snapshot map and
//! its traversal-root comparison on `OpaqueNode`, which in Servo is a pointer.
//! Two properties matter, and neither is checked by anything else:
//!
//! - **Never zero.** `OpaqueElement` is `NonNull<()>` reached through
//!   `NonNull::new_unchecked`, so a zero value is undefined behaviour with no
//!   diagnostic. The note: *"One line in Phase 4, an afternoon of debugging in
//!   Phase 5."*
//! - **Injective across slot reuse.** Pointer-derived identity is stale-unsafe:
//!   a removed element's snapshot can match a different element later
//!   allocated at the same address. A generational handle cannot do that, and
//!   the packing has to preserve it.

use px_dom::{Arena, NodeData, NodeId, OpaqueNodeId};

fn text(arena: &mut Arena, s: &str) -> NodeId {
    match arena.create(NodeData::Text { contents: s.into() }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

/// Never zero, for every handle an arena can produce.
///
/// The type says so, but the type is only as good as the packing: a
/// `NonZeroUsize` built from a zero would be caught here rather than becoming
/// a null `NonNull` in Phase 5.
#[test]
fn opaque_identity_is_never_zero() {
    let mut arena = Arena::new();
    let doc = arena.document();

    // The document node is index 0. With a zero generation it would pack to
    // zero, which is the exact case §3.5 warns about.
    assert_eq!(doc.index(), 0);
    assert_eq!(
        doc.to_opaque().get() & 0xFFFF_FFFF,
        1,
        "generation starts at 1"
    );

    for i in 0..2_000 {
        let id = text(&mut arena, &format!("n{i}"));
        arena.append_child(doc, id).expect("append");
        assert!(id.to_opaque().get() != 0);
    }
}

/// Packing round-trips.
#[test]
fn opaque_identity_round_trips() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let mut ids = vec![doc];
    for i in 0..500 {
        let id = text(&mut arena, &format!("n{i}"));
        arena.append_child(doc, id).expect("append");
        ids.push(id);
    }

    for id in ids {
        let opaque = id.to_opaque();
        assert_eq!(
            NodeId::from_opaque(opaque),
            Some(id),
            "the packing must be reversible, or an OpaqueNode cannot be turned \
             back into something the arena can resolve"
        );
    }
}

/// Distinct handles pack to distinct values — including handles that share a
/// slot.
///
/// This is the property that makes stylo's snapshot map safe here and unsafe
/// in Servo. A pointer key would be identical for the old and new occupants of
/// a reused slot; a generational key must not be.
#[test]
fn opaque_identity_survives_slot_reuse() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let first = text(&mut arena, "first");
    arena.append_child(doc, first).expect("append");
    let first_opaque = first.to_opaque();

    arena.remove_subtree(first).expect("remove");

    let second = text(&mut arena, "second");
    arena.append_child(doc, second).expect("append");

    assert_eq!(
        first.index(),
        second.index(),
        "the test is only meaningful if the slot was reused"
    );
    assert_ne!(
        first_opaque,
        second.to_opaque(),
        "the old and new occupants of one slot packed to the same key -- a \
         stale snapshot would be matched to the wrong element, which is the \
         bug §3.5 says pointer identity has and this design does not"
    );

    // And the stale key does not resolve to the live node.
    let recovered = NodeId::from_opaque(first_opaque).expect("well-formed");
    assert!(
        arena.get(recovered).is_none(),
        "a key recovered from a stale opaque value must miss, not resolve"
    );
}

/// Injective over a large, deliberately adversarial set of handles.
///
/// Checking every pair rather than a sample: a packing that collides does so
/// for structured reasons — a shift by the wrong amount, a mask that is one
/// bit short — and those collisions cluster exactly where a sample would not
/// look.
#[test]
fn opaque_identity_is_injective() {
    use std::collections::HashSet;

    let mut arena = Arena::new();
    let doc = arena.document();
    let mut seen: HashSet<OpaqueNodeId> = HashSet::new();
    seen.insert(doc.to_opaque());

    // Churn hard, so the same indices come back with many different
    // generations. That is where a packing that drops or overlaps bits shows.
    for round in 0..64 {
        let mut batch = Vec::new();
        for i in 0..64 {
            let id = text(&mut arena, &format!("r{round}n{i}"));
            arena.append_child(doc, id).expect("append");
            assert!(
                seen.insert(id.to_opaque()),
                "two live handles packed to the same key at round {round}, node {i}"
            );
            batch.push(id);
        }
        for id in batch.iter().step_by(2) {
            arena.remove_subtree(*id).expect("remove");
        }
    }

    assert!(
        seen.len() > 4_000,
        "the churn should have produced thousands of keys"
    );
}

/// A value that did not come from `to_opaque` is refused rather than
/// misinterpreted.
///
/// The shape that matters is a raw pointer: stylo's own `OpaqueElement` holds
/// one, so a mix-up between the two representations is possible. A pointer
/// with a zero low word is refused; one without is not detectable, and saying
/// so is more useful than implying a check that is not there.
#[test]
fn opaque_identity_refuses_a_zero_generation() {
    // Low half zero: cannot have come from `to_opaque`.
    let pointer_shaped = OpaqueNodeId::new(0x7FFF_0000_0000_0000).expect("non-zero");
    assert_eq!(
        NodeId::from_opaque(pointer_shaped),
        None,
        "a zero generation is not a handle this arena ever produced"
    );

    // And a well-formed one is accepted.
    let mut arena = Arena::new();
    let id = text(&mut arena, "x");
    assert!(NodeId::from_opaque(id.to_opaque()).is_some());
}
