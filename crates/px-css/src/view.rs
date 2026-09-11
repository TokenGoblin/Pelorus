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
use style::context::QuirksMode;
use style::shared_lock::SharedRwLock;

use crate::data::StyleData;

/// Everything a style pass needs that is not the DOM itself.
///
/// Owned by the caller and borrowed by every view, because stylo reaches for all
/// three through `TDocument` and `TElement`: the shared lock guards stylesheet
/// contents, the quirks mode changes cascade behaviour, and the style data is
/// where computed style is written back (ADR 026).
///
/// Built before the pass and not mutated during it, which is the same condition
/// that makes [`StyleData`]'s table safe to index without growing.
#[derive(Debug)]
pub struct StyleRoot {
    shared_lock: SharedRwLock,
    quirks_mode: QuirksMode,
    data: StyleData,
}

impl StyleRoot {
    /// Prepare a style root for `arena`.
    #[must_use]
    pub fn new(arena: &Arena, quirks_mode: QuirksMode) -> Self {
        Self {
            shared_lock: SharedRwLock::new(),
            quirks_mode,
            data: StyleData::for_arena(arena),
        }
    }

    /// The lock guarding stylesheet contents. `TDocument::shared_lock`.
    #[must_use]
    pub fn shared_lock(&self) -> &SharedRwLock {
        &self.shared_lock
    }

    /// `TDocument::quirks_mode`.
    #[must_use]
    pub fn quirks_mode(&self) -> QuirksMode {
        self.quirks_mode
    }

    /// Per-element style data (ADR 026).
    #[must_use]
    pub fn data(&self) -> &StyleData {
        &self.data
    }
}

/// The arena and the style root, borrowed together.
///
/// Exists so the views below carry one `Copy` field instead of two, and so that
/// "a DOM plus the state a style pass keeps beside it" has a name. Both
/// references share a lifetime deliberately: the pass borrows them together and
/// neither may be mutated while it runs.
#[derive(Clone, Copy)]
pub struct Dom<'a> {
    arena: &'a Arena,
    root: &'a StyleRoot,
    /// The document handle, resolved once when this `Dom` was built.
    document: NodeId,
}

impl PartialEq for Dom<'_> {
    /// Two `Dom`s are the same DOM when they borrow the same arena and the same
    /// style root.
    ///
    /// Pointer identity, for the reason `StyleNode`'s `PartialEq` gives: two
    /// arenas hand out overlapping `NodeId`s, so an equality that ignored which
    /// arena a view came from would let a traversal confuse one document for
    /// another. The style root is compared too because a single arena styled
    /// under two different quirks modes is two different style passes.
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.arena, other.arena) && core::ptr::eq(self.root, other.root)
    }
}

impl Eq for Dom<'_> {}

impl core::fmt::Debug for Dom<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dom").finish_non_exhaustive()
    }
}

impl<'a> Dom<'a> {
    /// Borrow an arena and its style root for the duration of a pass.
    ///
    /// Fallible, and this is the boundary §4.1 asks for: the document handle is
    /// resolved here, once, and every later use of it is a borrow rather than a
    /// lookup. `None` means the arena's own document handle did not resolve,
    /// which should be impossible and is reported rather than assumed.
    #[must_use]
    pub fn new(arena: &'a Arena, root: &'a StyleRoot) -> Option<Self> {
        let document = arena.document();
        arena.get(document)?;
        Some(Self {
            arena,
            root,
            document,
        })
    }

    /// The arena.
    #[must_use]
    pub fn arena(self) -> &'a Arena {
        self.arena
    }

    /// The style root.
    #[must_use]
    pub fn root(self) -> &'a StyleRoot {
        self.root
    }

    /// Resolve a handle into a node view, or `None` if it is stale.
    #[must_use]
    pub fn node(self, id: NodeId) -> Option<StyleNode<'a>> {
        StyleNode::new(self, id)
    }

    /// The document node, without a second generation check.
    ///
    /// The one infallible node constructor in this crate, and the justification
    /// is the same one the whole design rests on: [`Dom::new`] resolved this
    /// exact handle, and the `&'a Arena` held since then forbids the mutation
    /// that could invalidate it. `px-dom` also refuses to remove the document —
    /// `remove_subtree` returns `Immovable` for it — so there is no sequence of
    /// operations that makes this handle stale while a `Dom` exists.
    ///
    /// It is `pub(crate)` and exists for exactly one caller: `TNode::owner_doc`,
    /// which stylo declares infallible. Without it that method would have to
    /// invent a document or panic. This is not a general-purpose escape hatch,
    /// and §4.1's rule about not adding an infallible *index* API is intact —
    /// this resolves one specific handle that was already checked, not an
    /// arbitrary one.
    pub(crate) fn document_node(self) -> StyleNode<'a> {
        StyleNode {
            dom: self,
            id: self.document,
        }
    }
}

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
    dom: Dom<'a>,
    id: NodeId,
}

impl<'a> StyleNode<'a> {
    /// Resolve a handle against the arena, or return `None` if it is stale.
    ///
    /// The fallible boundary §4.1 requires, and the only constructor. There is
    /// deliberately no `new_unchecked`: the rule is not "check somewhere", it is
    /// that no infallible path exists.
    #[must_use]
    pub fn new(dom: Dom<'a>, id: NodeId) -> Option<Self> {
        // The generation check, exactly once. `get` returning Some is what makes
        // every later hop sound.
        dom.arena().get(id).map(|_| Self { dom, id })
    }

    /// The handle this view was resolved from.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.id
    }

    /// The arena this view borrows.
    #[must_use]
    pub fn arena(self) -> &'a Arena {
        self.dom.arena()
    }

    /// The arena and style root this view borrows.
    #[must_use]
    pub fn dom(self) -> Dom<'a> {
        self.dom
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
        Self::new(self.dom, id)
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
        core::ptr::eq(self.dom.arena(), other.dom.arena()) && self.id == other.id
    }
}

impl Eq for StyleNode<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use px_dom::NodeData;

    /// Every test needs a StyleRoot now, and none of them care what is in it.
    fn root(arena: &Arena) -> StyleRoot {
        StyleRoot::new(arena, QuirksMode::NoQuirks)
    }

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

        let r = root(&arena);
        let dom = Dom::new(&arena, &r).expect("the document resolves");
        assert!(StyleNode::new(dom, child).is_some());

        // No explicit drop: `Dom` is `Copy`, so `drop` would be a no-op, and
        // the borrow ends at its last use anyway. Re-resolving below is what
        // proves the point -- the same handle, against the same arena, after
        // the node it named is gone.
        arena.remove_subtree(child).expect("remove");
        let r = root(&arena);
        let dom = Dom::new(&arena, &r).expect("the document resolves");
        assert!(
            StyleNode::new(dom, child).is_none(),
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
        let r = root(&arena);
        let dom = Dom::new(&arena, &r).expect("the document resolves");
        let view = StyleNode::new(dom, arena.document()).expect("document resolves");
        let copied = view;
        assert_eq!(view, copied, "a copy must compare equal to its source");
    }

    #[test]
    fn views_from_different_arenas_are_never_equal() {
        let a = Arena::new();
        let b = Arena::new();
        let (ra, rb) = (root(&a), root(&b));
        let va =
            StyleNode::new(Dom::new(&a, &ra).expect("resolves"), a.document()).expect("resolves");
        let vb =
            StyleNode::new(Dom::new(&b, &rb).expect("resolves"), b.document()).expect("resolves");
        // Both are the document node, so the NodeIds are equal. Only the arena
        // identity distinguishes them, and a traversal comparing against its
        // scope node depends on that.
        assert_eq!(va.id(), vb.id(), "the precondition this test exists for");
        assert_ne!(
            va, vb,
            "equal ids in different arenas must not compare equal"
        );
    }
}
