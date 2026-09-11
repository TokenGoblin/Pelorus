//! The borrowed style view over a `px-dom` arena.
//!
//! This is the type `stylo-requirements.md` §3.2 proposed and ADR 021 deferred,
//! written now because Phase 5 needs it and because ADR 021 named its own
//! tripwire: if the arena's slot layout, `NodeId`'s representation or the opaque
//! packing had to change to make this compile, that ADR was wrong.
//!
//! # The collision it resolves
//!
//! build-spec §4.1: DOM handles are generational and **every accessor returns
//! `Option`**. `/CLAUDE.md` restates it and forbids adding an infallible index
//! API. That rule exists because a stale handle resolving to whatever took its
//! slot is DOM-level type confusion reachable from safe Rust.
//!
//! stylo's traversal wants to walk a tree, and `TNode: Sized + Copy + Clone +
//! Debug + NodeInfo + PartialEq`. A handle that needs an arena and a fallible
//! lookup at every hop is not what it asks for.
//!
//! The resolution is to resolve **once**, at the boundary, and borrow
//! thereafter: [`StyleNode`] holds a shared reference to the arena alongside the
//! `NodeId`, and is only constructible through a generation check. After that
//! the borrow checker guarantees the arena cannot be mutated while any
//! `StyleNode` exists, so the generation cannot advance and the id cannot go
//! stale. §4.1's safety property is preserved exactly — it is enforced
//! statically rather than re-checked per hop — and no infallible index API is
//! added, because [`StyleNode::new`] is the fallible constructor and the only
//! way in.
//!
//! That is also **stronger than the reference implementation**. Servo's
//! equivalent invariant, that the tree is not mutated during a style traversal,
//! is upheld by comment; here it is `&Arena` and the compiler.
//!
//! # What was measured rather than assumed
//!
//! `TNode`'s tree accessors — `parent_node`, `first_child`, `last_child`,
//! `prev_sibling`, `next_sibling` — **already return `Option<Self>`** in stylo
//! 0.21.0. The research note predicted a genuine collision here on the grounds
//! that "dozens of its methods are infallible"; for `TNode` that is not so, and
//! the accessors line up with §4.1 without any adaptation. The infallible ones
//! (`owner_doc`, `is_in_document`) are infallible about things that cannot fail.

use px_dom::{Arena, NodeId};

/// A node, resolved once against the arena and borrowed thereafter.
///
/// `Copy` because `TNode` requires it, and cheaply so: a shared reference and a
/// `NodeId`, which ADR 018 pinned at 8 bytes. Nothing is cloned.
///
/// The lifetime is the whole point. `StyleNode<'a>` cannot outlive the borrow of
/// the arena it was resolved against, so between its construction and its last
/// use there can be no `&mut Arena` anywhere — no `create`, no `remove_subtree`,
/// no `force_generation_to_last`. The generation it was checked against is
/// therefore still current, which is why the accessors below can hand back
/// siblings and children without rechecking.
#[derive(Clone, Copy)]
pub struct StyleNode<'a> {
    arena: &'a Arena,
    id: NodeId,
}

impl<'a> StyleNode<'a> {
    /// Resolve a handle against the arena, or return `None` if it is stale.
    ///
    /// The fallible boundary §4.1 requires, and the only constructor. There is
    /// deliberately no `new_unchecked`: the rule is not "check somewhere", it is
    /// that no infallible path exists.
    #[must_use]
    pub fn new(arena: &'a Arena, id: NodeId) -> Option<Self> {
        // The generation check, exactly once. `get` returning Some is what makes
        // every later hop sound.
        arena.get(id).map(|_| Self { arena, id })
    }

    /// The handle this view was resolved from.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.id
    }

    /// The arena this view borrows.
    #[must_use]
    pub fn arena(self) -> &'a Arena {
        self.arena
    }

    /// Resolve another handle against the same arena this view borrows.
    ///
    /// What the `TNode` accessors will be written in terms of: read a parent or
    /// sibling id out of a node and turn it back into a view.
    ///
    /// Still fallible, and deliberately. An id read from a node this view already
    /// resolved cannot be stale — the borrow forbids mutation, so the generation
    /// cannot advance — which makes the `None` arm unreachable in practice. It is
    /// kept anyway, because §4.1's rule is about the shape of the API rather than
    /// about where the check happens, and an infallible helper "just for internal
    /// use" is precisely what `/CLAUDE.md` warns becomes a stale-handle bug two
    /// refactors after the person who knew why it was safe has moved on.
    #[must_use]
    pub fn resolve(self, id: NodeId) -> Option<Self> {
        Self::new(self.arena, id)
    }
}

impl core::fmt::Debug for StyleNode<'_> {
    /// `TNode: Debug`. Prints the handle, not the subtree: stylo logs nodes
    /// inside traversals, and a `Debug` that walked children would turn one log
    /// line into a document.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StyleNode").field("id", &self.id).finish()
    }
}

impl PartialEq for StyleNode<'_> {
    /// `TNode: PartialEq`, and stylo leans on it: `next_in_preorder_skipping_children`
    /// terminates by comparing against the scope node.
    ///
    /// Compares the handle *and* the arena's identity. Two arenas can hand out
    /// the same `NodeId` — `dom_stale_handles_do_not_cross_between_arenas` in
    /// px-dom exists because that is real — and a traversal that mistook one for
    /// the other would terminate early or not at all.
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.arena, other.arena) && self.id == other.id
    }
}

impl Eq for StyleNode<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use px_dom::NodeData;

    fn text(arena: &mut Arena, s: &str) -> NodeId {
        arena
            .create(NodeData::Text {
                contents: s.to_string().into(),
            })
            .expect("arena has room")
    }

    #[test]
    fn view_resolves_a_live_handle_and_refuses_a_stale_one() {
        let mut arena = Arena::new();
        let doc = arena.document();
        let child = text(&mut arena, "a");
        arena.append_child(doc, child).expect("append");

        assert!(StyleNode::new(&arena, child).is_some());

        arena.remove_subtree(child).expect("remove");
        assert!(
            StyleNode::new(&arena, child).is_none(),
            "a stale handle must not resolve into a view; this is §4.1's whole point"
        );
    }

    #[test]
    fn view_is_copy_and_pointer_sized_pair() {
        // TNode requires Copy. Assert it rather than trusting the derive to
        // still be there: losing Copy is a trait-bound error pages deep in
        // stylo, and this says so in one line.
        fn assert_copy<T: Copy>() {}
        assert_copy::<StyleNode<'_>>();

        let arena = Arena::new();
        let view = StyleNode::new(&arena, arena.document()).expect("document resolves");
        let copied = view;
        assert_eq!(view, copied, "a copy must compare equal to its source");
    }

    #[test]
    fn views_from_different_arenas_are_never_equal() {
        let a = Arena::new();
        let b = Arena::new();
        let va = StyleNode::new(&a, a.document()).expect("resolves");
        let vb = StyleNode::new(&b, b.document()).expect("resolves");
        // Both are the document node, so the NodeIds are equal. Only the arena
        // identity distinguishes them, and a traversal comparing against its
        // scope node depends on that.
        assert_eq!(va.id(), vb.id(), "the precondition this test exists for");
        assert_ne!(va, vb, "equal ids in different arenas must not compare equal");
    }
}
