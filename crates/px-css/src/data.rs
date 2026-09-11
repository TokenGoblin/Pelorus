//! Where stylo's per-element style data lives.
//!
//! stylo does not keep computed style itself. It writes an [`ElementData`] back
//! into the DOM through `TElement`, and hands it out again through five methods
//! whose signatures constrain the storage far more than their names suggest:
//!
//! ```text
//! unsafe fn ensure_data(&self) -> ElementDataMut<'_>;
//!        fn borrow_data(&self) -> Option<ElementDataRef<'_>>;
//!        fn mutate_data(&self) -> Option<ElementDataMut<'_>>;
//! unsafe fn clear_data(&self);
//!        fn has_data(&self) -> bool;
//! ```
//!
//! Every one takes `&self`. Two of them *create* or *destroy* storage through a
//! shared reference, and three return a guard that borrows it. That combination
//! is what makes the obvious designs fail.
//!
//! # Why not a `RefCell<HashMap<NodeId, ElementDataWrapper>>`
//!
//! Because `borrow_data` has to return a guard that outlives the map's own
//! borrow guard, and it cannot: the `Ref` from a `RefCell` dies at the end of
//! the function, taking any reference derived from it. The stored data needs an
//! address that is stable independently of the container's borrow state.
//!
//! # Why not a field on `px_dom::Node`
//!
//! It would work, and it is what Servo does. Rejected on layering: `px-dom` would
//! have to name a `stylo` type, so the DOM crate would depend on the CSS crate's
//! dependency. §3's diagram has the arrow the other way, and a DOM that cannot be
//! compiled without a CSS engine is a DOM that cannot be tested without one.
//!
//! It would also put this decision straight into ADR 021's tripwire: a field on
//! `Node` changes the arena's slot layout, which is one of the three things that
//! ADR said would falsify it.
//!
//! # What this is instead
//!
//! A table sized **once, before the style pass**, with one slot per arena slot.
//! That works because of something already true: a style traversal holds
//! `&Arena` — see [`crate::view::StyleNode`] — so the DOM cannot be mutated
//! while it runs, so the number of nodes cannot change. There is nothing to grow
//! during the pass, and a `Vec` that never grows hands out references with the
//! `Vec`'s own lifetime rather than a guard's.
//!
//! So no interior mutability is needed for the *container*. The mutability
//! stylo asks for is already inside `ElementDataWrapper`, which wraps its
//! `ElementData` in an `UnsafeCell` and hands out both `&` and `&mut` from
//! `&self`. That is stylo's unsafe, in stylo's crate, reviewed by stylo — and it
//! is the reason `px-css` needs **no `unsafe` block of its own**, which ADR 024
//! requires and `ci/gate-style.sh` checks.
//!
//! `has_data` and `clear_data` need one bit of per-element state that is
//! settable through `&self`, and that is a [`Cell<bool>`] — safe, and the whole
//! of the interior mutability this module introduces.
//!
//! The cost, stated plainly: one `ElementDataWrapper` per arena slot rather than
//! per element, allocated whether or not the slot holds an element. `ElementData`
//! is 24 bytes by stylo's own `size_of_test!`, so a 100,000-node document pays a
//! few megabytes for a table it can index without a hash.

use std::cell::Cell;

use px_dom::{Arena, NodeId};
use style::data::{ElementDataMut, ElementDataRef, ElementDataWrapper};

/// One element's style data, plus the bit that says whether stylo considers it
/// to exist.
///
/// The `Cell<bool>` is not redundant with the wrapper. `ElementDataWrapper` is
/// always physically present in the table; `has_data` asks whether stylo has
/// *established* data for this element, which is a different question and one
/// stylo's traversal depends on — it is how a node that has never been styled is
/// distinguished from one whose style was computed and found empty.
#[derive(Debug, Default)]
struct Slot {
    present: Cell<bool>,
    data: ElementDataWrapper,
}

/// Per-element style data for one arena, sized once before the style pass.
///
/// Indexed by `NodeId`'s slot index, so a lookup is a bounds-checked array read
/// rather than a hash. The generation is deliberately *not* part of the key: a
/// `StyleNode` only exists for a handle already resolved against this arena, and
/// the `&Arena` it holds forbids the mutation that would advance a generation.
/// Checking it again here would be checking a thing the borrow checker has
/// already made impossible.
#[derive(Debug)]
pub struct StyleData {
    slots: Vec<Slot>,
}

impl StyleData {
    /// Size a table for `arena`, before the style pass begins.
    ///
    /// Takes `&Arena` rather than `&mut Arena` because it reads only the slot
    /// count — but it must be called before any `StyleNode` is handed to stylo,
    /// because after that point there is no opportunity to grow it and no need.
    #[must_use]
    pub fn for_arena(arena: &Arena) -> Self {
        // `len()` is the number of live nodes; the table is indexed by slot
        // index, which runs to the high-water mark including retired slots.
        // Sizing by `len()` would under-allocate a document that has had nodes
        // removed, and the failure would be a panic in the middle of a
        // traversal rather than anything legible.
        let mut slots = Vec::new();
        slots.resize_with(arena.slot_count(), Slot::default);
        Self { slots }
    }

    /// How many slots this table can address.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// The slot for `id`, or `None` if this table was sized for a smaller arena.
    ///
    /// `None` here means the table is stale, not that the handle is. It is the
    /// one failure this design can have and it is worth being able to see:
    /// returning a fresh empty slot instead would silently lose every style
    /// computed for the nodes past the end.
    fn slot(&self, id: NodeId) -> Option<&Slot> {
        self.slots.get(id.index() as usize)
    }

    /// Whether stylo has established data for this element.
    #[must_use]
    pub fn has_data(&self, id: NodeId) -> bool {
        self.slot(id).is_some_and(|s| s.present.get())
    }

    /// Establish data for this element and borrow it mutably.
    ///
    /// This is `TElement::ensure_data`'s body. stylo declares that method
    /// `unsafe fn` because "it can race to allocate and leak if not used with
    /// exclusive access to the element" — and here there is no allocation to
    /// race on, because the slot already exists. The race stylo warns about is
    /// designed out rather than documented.
    ///
    /// `None` only if the table is stale; see [`Self::slot`].
    pub fn ensure(&self, id: NodeId) -> Option<ElementDataMut<'_>> {
        let slot = self.slot(id)?;
        slot.present.set(true);
        Some(slot.data.borrow_mut())
    }

    /// Borrow this element's data immutably, if stylo has established it.
    #[must_use]
    pub fn borrow(&self, id: NodeId) -> Option<ElementDataRef<'_>> {
        let slot = self.slot(id)?;
        slot.present.get().then(|| slot.data.borrow())
    }

    /// Borrow this element's data mutably, if stylo has established it.
    ///
    /// Note the asymmetry with [`Self::ensure`], which is stylo's and not ours:
    /// `mutate_data` returns `None` for an element with no data, where
    /// `ensure_data` creates it.
    pub fn mutate(&self, id: NodeId) -> Option<ElementDataMut<'_>> {
        let slot = self.slot(id)?;
        slot.present.get().then(|| slot.data.borrow_mut())
    }

    /// Discard this element's data.
    ///
    /// Resets the stored `ElementData` as well as clearing the bit. Leaving a
    /// stale `ElementData` behind the `present = false` flag would be invisible
    /// until something set the flag again and read style belonging to a previous
    /// generation of the tree.
    pub fn clear(&self, id: NodeId) {
        if let Some(slot) = self.slot(id) {
            if slot.present.get() {
                *slot.data.borrow_mut() = Default::default();
            }
            slot.present.set(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_dom::NodeData;

    fn arena_with(n: usize) -> (Arena, Vec<NodeId>) {
        let mut arena = Arena::new();
        let doc = arena.document();
        let mut ids = Vec::new();
        for i in 0..n {
            let id = arena
                .create(NodeData::Text {
                    contents: i.to_string().into(),
                })
                .expect("room");
            arena.append_child(doc, id).expect("append");
            ids.push(id);
        }
        (arena, ids)
    }

    #[test]
    fn data_starts_absent_and_ensure_establishes_it() {
        let (arena, ids) = arena_with(4);
        let data = StyleData::for_arena(&arena);
        let id = ids[0];

        assert!(!data.has_data(id), "nothing is styled before the pass runs");
        assert!(data.borrow(id).is_none());
        assert!(data.mutate(id).is_none(), "mutate_data does not create");

        drop(data.ensure(id).expect("the slot exists"));
        assert!(data.has_data(id));
        assert!(data.borrow(id).is_some());
        assert!(data.mutate(id).is_some());
    }

    #[test]
    fn data_clear_forgets_the_style_not_just_the_flag() {
        let (arena, ids) = arena_with(2);
        let data = StyleData::for_arena(&arena);
        let id = ids[0];

        {
            let mut d = data.ensure(id).expect("slot exists");
            d.hint
                .insert(style::invalidation::element::restyle_hints::RestyleHint::RESTYLE_SELF);
        }
        assert!(
            data.borrow(id)
                .expect("present")
                .hint
                .has_non_animation_invalidations()
        );

        data.clear(id);
        assert!(!data.has_data(id));

        // Re-establishing must not resurrect the old hint. A clear that only
        // flipped the bit would hand the next pass a restyle hint from the
        // previous tree.
        drop(data.ensure(id).expect("slot exists"));
        assert!(
            !data
                .borrow(id)
                .expect("present")
                .hint
                .has_non_animation_invalidations(),
            "clear() left stale ElementData behind the present flag"
        );
    }

    #[test]
    fn data_is_per_element_and_does_not_bleed() {
        let (arena, ids) = arena_with(3);
        let data = StyleData::for_arena(&arena);

        drop(data.ensure(ids[1]).expect("slot exists"));
        assert!(!data.has_data(ids[0]));
        assert!(data.has_data(ids[1]));
        assert!(!data.has_data(ids[2]));
    }

    #[test]
    fn data_table_covers_retired_slots_not_just_live_nodes() {
        // Sizing by live-node count under-allocates as soon as anything has been
        // removed, and the symptom would be a panic mid-traversal.
        let (mut arena, ids) = arena_with(5);
        arena.remove_subtree(ids[0]).expect("remove");
        arena.remove_subtree(ids[1]).expect("remove");

        let data = StyleData::for_arena(&arena);
        let survivor = ids[4];
        assert!(
            data.capacity() > arena.len(),
            "capacity {} must exceed live count {}",
            data.capacity(),
            arena.len()
        );
        assert!(
            data.ensure(survivor).is_some(),
            "a node past the removed ones must still have a slot"
        );
    }
}
