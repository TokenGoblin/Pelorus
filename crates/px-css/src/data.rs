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
use style::properties::PropertyDeclarationBlock;
use style::shared_lock::Locked;
use style::values::AtomIdent;

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
    /// stylo's per-element traversal bits, which it sets through `&self`.
    ///
    /// These live here rather than on the node for the same reason
    /// `ElementData` does (ADR 026): putting them on `px_dom::Node` would make
    /// the DOM crate depend on the CSS engine and would change the arena's slot
    /// layout, which is ADR 021's tripwire.
    ///
    /// `Cell`, not `AtomicBool`, because ADR 023 runs stylo's traversal
    /// sequentially -- `traverse_dom` is passed `pool: None`. The day the pool
    /// is turned on these have to become atomics, and they are grouped here so
    /// that change is one struct rather than a search. Servo gets this wrong in
    /// the other direction today: `stylo-requirements.md` notes a live FIXME
    /// admitting a non-atomic `Cell` read-modify-write from parallel style
    /// threads. Sequential-first is what makes `Cell` honest here.
    flags: Flags,
    /// The interned `id` attribute, and the interned `class` list.
    ///
    /// Here rather than computed on demand because `TElement::id` returns
    /// `Option<&AtomIdent>` — a *reference* to an interned atom. html5ever stores
    /// attribute values as string tendrils, not atoms, so answering that question
    /// means interning, and interning on demand behind `&self` would need the
    /// same stable-address machinery `ElementData` needed.
    ///
    /// Populated when the table is built, which is the one moment the whole arena
    /// is in hand and nothing is mid-traversal. The cost is one pass over the
    /// nodes at the start of a style pass, and it replaces re-interning the same
    /// class list once per selector that mentions it.
    names: Names,
}

/// The interned identity attributes of one element.
#[derive(Debug, Default)]
struct Names {
    id: Option<AtomIdent>,
    classes: Vec<AtomIdent>,
    /// The parsed `style` attribute.
    ///
    /// Here for the same reason `id` is: `TElement::style_attribute` returns an
    /// `ArcBorrow`, so the parsed block must already exist somewhere with a
    /// stable address. Parsed once when the table is built rather than on every
    /// cascade, which also means a malformed attribute is reported once instead
    /// of re-parsed per pass.
    style_attribute: Option<style::servo_arc::Arc<Locked<PropertyDeclarationBlock>>>,
}

/// The traversal bits stylo keeps per element.
///
/// Hand-written `Debug` and `Default` rather than derived: selectors'
/// `ElementSelectorFlags` implements neither in 0.40, so a derive on this struct
/// does not compile. Both are written in terms of `empty()`, which is the right
/// default anyway -- an element starts with no selector flags set.
struct Flags {
    /// `has_dirty_descendants` / `set_dirty_descendants`.
    dirty_descendants: Cell<bool>,
    /// `handled_snapshot` / `set_handled_snapshot`.
    handled_snapshot: Cell<bool>,
    /// `store_children_to_process` / `did_process_child`.
    ///
    /// stylo's parallel traversal uses this as a countdown: a parent stores the
    /// number of children, each child decrements it, and the one that reaches
    /// zero owns the parent's post-order work. Sequential traversal still drives
    /// it, so it is implemented rather than stubbed.
    children_to_process: Cell<isize>,
    /// `selectors::Element::apply_selector_flags`.
    ///
    /// The selector engine sets these on an element's *parent* or *siblings* as
    /// a side effect of matching -- "this element's children are order-sensitive,
    /// so invalidate them all if one moves". Losing them does not produce a wrong
    /// first paint; it produces a wrong *incremental* restyle later, which is
    /// much harder to trace back. So they are stored from the start rather than
    /// when incremental restyle arrives.
    selector_flags: Cell<selectors::matching::ElementSelectorFlags>,
}

impl Default for Flags {
    fn default() -> Self {
        Self {
            dirty_descendants: Cell::new(false),
            handled_snapshot: Cell::new(false),
            children_to_process: Cell::new(0),
            selector_flags: Cell::new(selectors::matching::ElementSelectorFlags::empty()),
        }
    }
}

impl core::fmt::Debug for Flags {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Flags")
            .field("dirty_descendants", &self.dirty_descendants.get())
            .field("handled_snapshot", &self.handled_snapshot.get())
            .field("children_to_process", &self.children_to_process.get())
            .field("selector_flags", &self.selector_flags.get().bits())
            .finish()
    }
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
        let mut table = Self { slots };
        table.intern_names(arena, None);
        table
    }

    /// Size a table for `arena`, parsing inline `style` attributes as well.
    ///
    /// Separate from [`Self::for_arena`] because parsing needs a base URL, a
    /// quirks mode and the shared lock — the caller's, not ours — and most of this
    /// crate's own tests have no opinion about any of them.
    #[must_use]
    pub fn for_arena_with_style_attributes(
        arena: &Arena,
        parser: &StyleAttributeParser<'_>,
    ) -> Self {
        let mut slots = Vec::new();
        slots.resize_with(arena.slot_count(), Slot::default);
        let mut table = Self { slots };
        table.intern_names(arena, Some(parser));
        table
    }

    /// `TElement::style_attribute`.
    #[must_use]
    pub fn style_attribute_of(
        &self,
        id: NodeId,
    ) -> Option<style::servo_arc::ArcBorrow<'_, Locked<PropertyDeclarationBlock>>> {
        self.slot(id)?
            .names
            .style_attribute
            .as_ref()
            .map(style::servo_arc::Arc::borrow_arc)
    }

    /// Intern every element's `id` and `class` once, before the pass.
    ///
    /// Walks the arena from the document rather than scanning slots, so a retired
    /// slot's leftover contents cannot be read as a live element's names.
    fn intern_names(&mut self, arena: &Arena, style_attrs: Option<&StyleAttributeParser<'_>>) {
        let document = arena.document();
        let ids: Vec<NodeId> = core::iter::once(document)
            .chain(arena.descendants(document))
            .collect();
        for id in ids {
            let Some(node) = arena.get(id) else { continue };
            if node.element_name().is_none() {
                continue;
            }
            let Some(attrs) = node.attrs() else { continue };
            let Some(slot) = self.slots.get_mut(id.index() as usize) else {
                continue;
            };
            for attr in attrs {
                if attr.name.ns != html5ever::ns!() {
                    continue;
                }
                if attr.name.local == html5ever::local_name!("id") {
                    slot.names.id = Some(AtomIdent::from(&*attr.value));
                } else if attr.name.local == html5ever::local_name!("class") {
                    slot.names.classes = attr
                        .value
                        .split_ascii_whitespace()
                        .map(AtomIdent::from)
                        .collect();
                } else if attr.name.local == html5ever::local_name!("style") {
                    // Parsed only if the caller supplied a parser. A table built
                    // without one simply has no inline style, which is what
                    // `StyleData::for_arena` produces for the unit tests that do
                    // not care about the cascade.
                    if let Some(parser) = style_attrs {
                        slot.names.style_attribute = Some(parser.parse(&attr.value));
                    }
                }
            }
        }
    }

    /// `TElement::id`.
    #[must_use]
    pub fn id_of(&self, id: NodeId) -> Option<&AtomIdent> {
        self.slot(id)?.names.id.as_ref()
    }

    /// `TElement::each_class`.
    #[must_use]
    pub fn classes_of(&self, id: NodeId) -> &[AtomIdent] {
        self.slot(id).map_or(&[], |s| s.names.classes.as_slice())
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

    /// `TElement::has_dirty_descendants`.
    #[must_use]
    pub fn has_dirty_descendants(&self, id: NodeId) -> bool {
        self.slot(id)
            .is_some_and(|s| s.flags.dirty_descendants.get())
    }

    /// `TElement::set_dirty_descendants` / `unset_dirty_descendants`.
    pub fn set_dirty_descendants(&self, id: NodeId, dirty: bool) {
        if let Some(slot) = self.slot(id) {
            slot.flags.dirty_descendants.set(dirty);
        }
    }

    /// `TElement::handled_snapshot`.
    #[must_use]
    pub fn handled_snapshot(&self, id: NodeId) -> bool {
        self.slot(id)
            .is_some_and(|s| s.flags.handled_snapshot.get())
    }

    /// `TElement::set_handled_snapshot`.
    pub fn set_handled_snapshot(&self, id: NodeId) {
        if let Some(slot) = self.slot(id) {
            slot.flags.handled_snapshot.set(true);
        }
    }

    /// `selectors::Element::apply_selector_flags`, which only ever adds bits.
    pub fn insert_selector_flags(
        &self,
        id: NodeId,
        flags: selectors::matching::ElementSelectorFlags,
    ) {
        if let Some(slot) = self.slot(id) {
            slot.flags
                .selector_flags
                .set(slot.flags.selector_flags.get() | flags);
        }
    }

    /// `TElement::has_selector_flags`.
    #[must_use]
    pub fn has_selector_flags(
        &self,
        id: NodeId,
        flags: selectors::matching::ElementSelectorFlags,
    ) -> bool {
        self.slot(id)
            .is_some_and(|s| s.flags.selector_flags.get().contains(flags))
    }

    /// `TElement::store_children_to_process`.
    pub fn store_children_to_process(&self, id: NodeId, n: isize) {
        if let Some(slot) = self.slot(id) {
            slot.flags.children_to_process.set(n);
        }
    }

    /// `TElement::did_process_child`, returning the count that remains.
    ///
    /// Returns -1 for a stale table rather than 0. Zero is the value stylo reads
    /// as "you are the last child, do the parent's work", and inventing it for a
    /// node the table does not cover would hand out that responsibility twice.
    pub fn did_process_child(&self, id: NodeId) -> isize {
        let Some(slot) = self.slot(id) else {
            return -1;
        };
        let remaining = slot.flags.children_to_process.get() - 1;
        slot.flags.children_to_process.set(remaining);
        remaining
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
            // The traversal bits go too. A cleared element that kept
            // `dirty_descendants` would have the next pass walk into a subtree
            // whose style it just threw away.
            slot.flags.dirty_descendants.set(false);
            slot.flags.handled_snapshot.set(false);
            slot.flags.children_to_process.set(0);
            slot.flags
                .selector_flags
                .set(selectors::matching::ElementSelectorFlags::empty());
        }
    }
}

/// What parsing an inline `style` attribute needs, collected so the table does
/// not have to know about URLs or locks.
///
/// The lock must be the engine's: stylo reads the resulting declarations through
/// a guard derived from `TDocument::shared_lock`, and a block wrapped in a
/// different lock is a block the cascade cannot read. That is the same trap
/// `StyleRoot::new` documents, one level down.
pub struct StyleAttributeParser<'a> {
    url_data: &'a style::stylesheets::UrlExtraData,
    shared_lock: &'a style::shared_lock::SharedRwLock,
    quirks_mode: style::context::QuirksMode,
}

impl<'a> StyleAttributeParser<'a> {
    /// Collect what parsing needs.
    #[must_use]
    pub fn new(
        url_data: &'a style::stylesheets::UrlExtraData,
        shared_lock: &'a style::shared_lock::SharedRwLock,
        quirks_mode: style::context::QuirksMode,
    ) -> Self {
        Self {
            url_data,
            shared_lock,
            quirks_mode,
        }
    }

    /// Parse one attribute value into a lock-wrapped declaration block.
    ///
    /// No error path. A malformed declaration inside a `style` attribute is
    /// discarded and the rest applies, which is what CSS Syntax requires — an
    /// attribute that failed as a unit would drop working declarations because of
    /// a neighbour using a property this engine has not implemented.
    fn parse(&self, value: &str) -> style::servo_arc::Arc<Locked<PropertyDeclarationBlock>> {
        let block = style::properties::declaration_block::parse_style_attribute(
            value,
            self.url_data,
            // No error reporter: parse errors in a page's markup are the page's
            // business, and a browser that logged every one would be unusable on
            // the real web.
            None,
            self.quirks_mode,
            style::stylesheets::CssRuleType::Style,
        );
        style::servo_arc::Arc::new(self.shared_lock.wrap(block))
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
