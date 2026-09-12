//! The borrowed style view over a `px-dom` arena.
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
//! thereafter: a [`StyleNode`] is only constructible through a generation check,
//! and once it exists the borrow checker guarantees the arena cannot be mutated
//! while it lives — so the generation cannot advance and the handle cannot go
//! stale. §4.1's safety property is preserved exactly, enforced statically rather
//! than re-checked per hop, and no infallible index API is added because
//! [`StyleNode::new`] is the fallible constructor and the only way in.
//!
//! That is **stronger than the reference implementation**. Servo's equivalent
//! invariant, that the tree is not mutated during a style traversal, is upheld by
//! comment; here it is `&Arena` and the compiler.
//!
//! # Why a `StyleNode` is one pointer and not three fields
//!
//! ADR 027. stylo's style-sharing cache keeps its LRU in a thread-local with the
//! element type erased to `usize` and `transmute`s it back, guarded by
//! `assert_eq!(size_of::<SharingCache<E>>(), size_of::<TypelessSharingCache>())`.
//! **`TElement` must therefore be exactly pointer-sized.** Nothing in the trait
//! says so; it is a runtime assertion in a constructor, and the first style pass
//! panics with `left: 10256, right: 9488` if you get it wrong.
//!
//! The obvious view — `{ &Arena, &StyleRoot, NodeId }` — is 32 bytes. So the
//! arena and the style root move into a [`DomCtx`] built once per pass, per-node
//! records point at it, and a `StyleNode` is a reference to one record: **eight
//! bytes, with the lifetime still doing all the work.**
//!
//! The construction is mutually referential — records point at the context, and
//! the context holds the slice of records so a node can reach its siblings — and
//! it is expressible without `unsafe` because `OnceCell::set` takes `&self` and
//! neither type implements `Drop`. ADR 027 first rejected this design as
//! impossible without `unsafe`; that was wrong, and the ADR says so.
//!
//! # What was measured rather than assumed
//!
//! `TNode`'s tree accessors — `parent_node`, `first_child`, `last_child`,
//! `prev_sibling`, `next_sibling` — **already return `Option<Self>`** in stylo
//! 0.21.0. The research note predicted a genuine collision here on the grounds
//! that "dozens of its methods are infallible"; for `TNode` that is not so, and
//! the accessors line up with §4.1 without any adaptation.

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
#[derive(Debug)]
pub struct StyleRoot {
    shared_lock: SharedRwLock,
    quirks_mode: QuirksMode,
    data: StyleData,
}

impl StyleRoot {
    /// Prepare a style root for `arena`, using `shared_lock`.
    ///
    /// The lock must be the one the stylesheets were wrapped with — in practice
    /// `StyleEngine::shared_lock()`. stylo reads declarations through a guard
    /// derived from `TDocument::shared_lock`, and a guard from a *different* lock
    /// does not grant access to those sheets. The failure is not a panic: the
    /// cascade finds no declarations and every element computes to its initial
    /// values, which looks like a stylesheet that did not load.
    ///
    /// Taking the lock as an argument rather than creating one is the point of
    /// this signature. An earlier version called `SharedRwLock::new()` here,
    /// which compiles, runs, and silently styles nothing.
    #[must_use]
    pub fn new(arena: &Arena, shared_lock: SharedRwLock, quirks_mode: QuirksMode) -> Self {
        Self {
            shared_lock,
            quirks_mode,
            data: StyleData::for_arena(arena),
        }
    }

    /// Assemble a style root from a table built elsewhere.
    ///
    /// Used by `StyleEngine::style_root_for`, which is how callers should get one:
    /// it wires the lock, the quirks mode and the inline-style parser to the same
    /// engine, and each of those is a silent failure when they disagree.
    #[must_use]
    pub fn with_data(shared_lock: SharedRwLock, quirks_mode: QuirksMode, data: StyleData) -> Self {
        Self {
            shared_lock,
            quirks_mode,
            data,
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

/// The arena, the style root, and the node records, borrowed together for one
/// style pass.
///
/// Built once per pass rather than carried in every view, because a view has to
/// fit in one machine word — see the module documentation and ADR 027.
pub struct DomCtx<'a> {
    arena: &'a Arena,
    root: &'a StyleRoot,
    /// The node records, filled in after this context exists.
    ///
    /// `OnceCell` because the reference cycle has to be closed in two steps: the
    /// records need `&DomCtx` to be built, and the context needs `&[NodeEntry]`
    /// so a node can find its siblings. `set` takes `&self`, so the second step
    /// needs no mutable borrow, and neither type implements `Drop`, so the borrow
    /// checker accepts a cycle between two locals.
    entries: core::cell::OnceCell<&'a [NodeEntry<'a>]>,
    document: NodeId,
}

/// One node's record: the context, and which node it is.
///
/// One per arena slot, built once per pass. The *view* is a reference to one of
/// these, which is the eight bytes stylo requires.
pub struct NodeEntry<'a> {
    ctx: &'a DomCtx<'a>,
    /// The live handle for this slot, generation included.
    ///
    /// Filled from a walk of the document tree, so it is the handle the arena
    /// currently recognises rather than a fabricated one. Slots not reachable
    /// from the document keep the document's own handle as a placeholder, which
    /// can never equal a request for a different node — so [`StyleNode::new`]
    /// declines them. See its documentation for what that means and why it is the
    /// right trade here.
    id: NodeId,
}

/// A borrow of an arena and its style root, for the duration of one pass.
#[derive(Clone, Copy)]
pub struct Dom<'a>(&'a DomCtx<'a>);

impl PartialEq for Dom<'_> {
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.0, other.0)
    }
}

impl Eq for Dom<'_> {}

impl core::fmt::Debug for Dom<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Dom").finish_non_exhaustive()
    }
}

/// Run `f` with a [`Dom`] borrowing `arena` and `root`.
///
/// A closure rather than a constructor, and that is forced rather than chosen:
/// the context and the node records are two locals that reference each other, and
/// both must outlive every view derived from them. Returning a `Dom` would mean
/// returning a reference to a local about to be dropped. Scoping it this way is
/// what makes the lifetimes work, and it is the only way in.
///
/// Returns `None` without calling `f` if the arena's own document handle does not
/// resolve, which should be impossible and is reported rather than assumed.
pub fn with_dom<R>(arena: &Arena, root: &StyleRoot, f: impl FnOnce(Dom<'_>) -> R) -> Option<R> {
    let document = arena.document();
    arena.get(document)?;

    let ctx = DomCtx {
        arena,
        root,
        entries: core::cell::OnceCell::new(),
        document,
    };

    // One record per slot, indexed by slot index, so a handle resolves to its
    // record by indexing rather than searching. Sized by `slot_count()` and not
    // `len()`, for the reason ADR 026 gives: `len()` counts live nodes while the
    // index runs to the high-water mark, and the two diverge as soon as anything
    // is removed.
    let mut entries: Vec<NodeEntry<'_>> = (0..arena.slot_count())
        .map(|_| NodeEntry {
            ctx: &ctx,
            id: document,
        })
        .collect();

    // Fill in the live handle for every node reachable from the document. The
    // generation matters: it is half of `NodeId`, it is what `to_opaque` packs
    // into stylo's `OpaqueNode`, and a record holding a fabricated generation
    // would hand out an identity that no arena lookup agrees with.
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        if let Some(entry) = entries.get_mut(id.index() as usize) {
            entry.id = id;
        }
    }

    ctx.entries.set(&entries).ok()?;
    Some(f(Dom(&ctx)))
}

impl<'a> Dom<'a> {
    /// The arena.
    #[must_use]
    pub fn arena(self) -> &'a Arena {
        self.0.arena
    }

    /// The style root.
    #[must_use]
    pub fn root(self) -> &'a StyleRoot {
        self.0.root
    }

    /// Resolve a handle into a node view, or `None` if it is stale.
    #[must_use]
    pub fn node(self, id: NodeId) -> Option<StyleNode<'a>> {
        StyleNode::new(self, id)
    }

    /// The document node, without a second generation check.
    ///
    /// The one infallible node constructor in this crate, and the justification
    /// is the one the whole design rests on: [`with_dom`] resolved this exact
    /// handle, and the `&'a Arena` held since then forbids the mutation that
    /// could invalidate it. `px-dom` also refuses to remove the document —
    /// `remove_subtree` returns `Immovable` for it — so no sequence of operations
    /// makes this handle stale while a `Dom` exists.
    ///
    /// `pub(crate)`, for exactly one caller: `TNode::owner_doc`, which stylo
    /// declares infallible. §4.1's rule about not adding an infallible *index*
    /// API is intact — this resolves one specific handle that was already checked.
    pub(crate) fn document_node(self) -> StyleNode<'a> {
        let entry = self
            .0
            .entries
            .get()
            .and_then(|entries| entries.get(self.0.document.index() as usize))
            .expect("with_dom builds a record for every slot and checked this handle");
        StyleNode(entry)
    }
}

/// A node, resolved once against the arena and borrowed thereafter.
///
/// One pointer. See the module documentation for why that is not negotiable.
#[derive(Clone, Copy)]
pub struct StyleNode<'a>(&'a NodeEntry<'a>);

impl<'a> StyleNode<'a> {
    /// Resolve a handle against the arena, or return `None` if it is stale.
    ///
    /// The fallible boundary §4.1 requires, and the only public constructor.
    /// There is deliberately no `new_unchecked`: the rule is not "check
    /// somewhere", it is that no infallible path exists.
    ///
    /// Two checks, not one. The arena must recognise the handle — that is the
    /// generational check §4.1 is about — and the slot's record must be for
    /// *this* handle. The second declines a live node that is not reachable from
    /// the document, because [`with_dom`] fills records by walking the tree.
    /// That is the right trade for a style pass: stylo traverses the document,
    /// and a detached subtree has no computed style to speak of. It would be the
    /// wrong trade for a general-purpose DOM API, which is why this is not one.
    #[must_use]
    pub fn new(dom: Dom<'a>, id: NodeId) -> Option<Self> {
        dom.0.arena.get(id)?;
        let entry = dom.0.entries.get()?.get(id.index() as usize)?;
        (entry.id == id).then_some(StyleNode(entry))
    }

    /// The handle this view was resolved from, generation included.
    #[must_use]
    pub fn id(self) -> NodeId {
        self.0.id
    }

    /// The arena this view borrows.
    #[must_use]
    pub fn arena(self) -> &'a Arena {
        self.0.ctx.arena
    }

    /// The arena and style root this view borrows.
    #[must_use]
    pub fn dom(self) -> Dom<'a> {
        Dom(self.0.ctx)
    }

    /// Resolve another handle against the same arena this view borrows.
    ///
    /// What the `TNode` accessors are written in terms of: read a parent or
    /// sibling id out of a node and turn it back into a view.
    ///
    /// Still fallible, and deliberately. An id read from a node this view already
    /// resolved cannot be stale — the borrow forbids mutation, so the generation
    /// cannot advance — which makes the `None` arm unreachable in practice. It is
    /// kept anyway, because §4.1 is a rule about the shape of the API rather than
    /// about where the check happens, and an infallible helper "just for internal
    /// use" is precisely what `/CLAUDE.md` warns becomes a stale-handle bug two
    /// refactors after the person who knew why it was safe has moved on.
    #[must_use]
    pub fn resolve(self, id: NodeId) -> Option<Self> {
        Self::new(self.dom(), id)
    }
}

impl core::fmt::Debug for StyleNode<'_> {
    /// `TNode: Debug`. Prints the handle, not the subtree: stylo logs nodes
    /// inside traversals, and a `Debug` that walked children would turn one log
    /// line into a document.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("StyleNode").field("id", &self.id()).finish()
    }
}

impl PartialEq for StyleNode<'_> {
    /// `TNode: PartialEq`, and stylo leans on it:
    /// `next_in_preorder_skipping_children` terminates by comparing against the
    /// scope node.
    ///
    /// Pointer equality on the record, which is cheaper than comparing handles
    /// and says more. Two views are the same node exactly when they point at the
    /// same record, and records are per-pass — so a view from one arena can never
    /// compare equal to a view from another, which is a confusion
    /// `dom_stale_handles_do_not_cross_between_arenas` in px-dom exists because
    /// it is real.
    fn eq(&self, other: &Self) -> bool {
        core::ptr::eq(self.0, other.0)
    }
}

impl Eq for StyleNode<'_> {}

#[cfg(test)]
mod tests {
    use super::*;
    use px_dom::NodeData;

    /// Every test needs a `StyleRoot`, and none of them care what is in it.
    fn root(arena: &Arena) -> StyleRoot {
        StyleRoot::new(arena, SharedRwLock::new(), QuirksMode::NoQuirks)
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
        let resolved =
            with_dom(&arena, &r, |dom| dom.node(child).is_some()).expect("the document resolves");
        assert!(resolved);

        arena.remove_subtree(child).expect("remove");
        let r = root(&arena);
        let resolved = with_dom(&arena, &r, |dom| dom.node(child).is_some())
            .expect("the document still resolves");
        assert!(
            !resolved,
            "a stale handle must not resolve into a view; this is §4.1's whole point"
        );
    }

    /// ADR 027's requirement, asserted where the type is defined.
    #[test]
    fn view_is_copy_and_one_pointer_wide() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<StyleNode<'_>>();
        assert_eq!(
            size_of::<StyleNode<'_>>(),
            size_of::<usize>(),
            "ADR 027: stylo's sharing cache asserts the element type is pointer-sized"
        );

        let arena = Arena::new();
        let r = root(&arena);
        with_dom(&arena, &r, |dom| {
            let view = dom.node(arena.document()).expect("document resolves");
            let copied = view;
            assert_eq!(view, copied, "a copy must compare equal to its source");
        })
        .expect("the document resolves");
    }

    /// Views from different arenas cannot be compared at all any more.
    ///
    /// Under the old representation a view carried `&Arena` inline, two views
    /// from different arenas had the same type, and `PartialEq` had to compare
    /// arena pointers to stop a traversal confusing one document for another.
    ///
    /// ADR 027's records make that a *type* error instead: each `with_dom` scope
    /// has its own lifetime, so a view from one cannot be passed into another's
    /// closure — the borrow checker rejects it with "borrowed data escapes
    /// outside of closure". The earlier version of this test compared views
    /// across two nested `with_dom` calls and no longer compiles, which is the
    /// stronger outcome and the reason this test now reads the way it does.
    ///
    /// What is still worth asserting is the within-arena half: distinct nodes are
    /// distinct views, and the same node is the same view.
    #[test]
    fn views_are_equal_exactly_when_they_are_the_same_node() {
        let mut arena = Arena::new();
        let doc = arena.document();
        let a = text(&mut arena, "a");
        let b = text(&mut arena, "b");
        arena.append_child(doc, a).expect("append");
        arena.append_child(doc, b).expect("append");

        let r = root(&arena);
        with_dom(&arena, &r, |dom| {
            let va = dom.node(a).expect("resolves");
            let vb = dom.node(b).expect("resolves");
            let va_again = dom.node(a).expect("resolves");

            assert_eq!(va, va_again, "the same handle must give an equal view");
            assert_ne!(va, vb, "different nodes must not compare equal");
        })
        .expect("the document resolves");
    }

    /// The record must carry the live generation, not a fabricated one.
    ///
    /// `to_opaque` packs index *and* generation into stylo's `OpaqueNode`, so a
    /// record holding the wrong generation would hand out an identity no arena
    /// lookup agrees with — and it would do it silently.
    #[test]
    fn view_id_round_trips_through_the_arena() {
        let mut arena = Arena::new();
        let doc = arena.document();
        let a = text(&mut arena, "a");
        let b = text(&mut arena, "b");
        arena.append_child(doc, a).expect("append");
        arena.append_child(doc, b).expect("append");

        let r = root(&arena);
        with_dom(&arena, &r, |dom| {
            for id in [doc, a, b] {
                let view = dom.node(id).expect("live handle resolves");
                assert_eq!(
                    view.id(),
                    id,
                    "the record must hold the handle it was built for"
                );
                assert!(
                    view.arena().get(view.id()).is_some(),
                    "the handle a view reports must still resolve against the arena"
                );
            }
        })
        .expect("the document resolves");
    }
}
