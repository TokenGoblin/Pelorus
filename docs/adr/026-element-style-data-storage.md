# 026 — stylo's `ElementData` lives in a pre-sized side table, not in the DOM

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 5
- **Invariants touched:** none. Bears on ADR 021's tripwire by *avoiding* it —
  see Consequences.

*(ADR 025 is reserved for §9 Phase 5's third gate item, the decision on whether
stylo survived contact, which cannot be written until the phase ends.)*

## Context

stylo does not keep computed style itself. It writes an `ElementData` back into
the consumer's DOM through `TElement` and reads it back through five methods:

```rust
unsafe fn ensure_data(&self) -> ElementDataMut<'_>;
       fn borrow_data(&self) -> Option<ElementDataRef<'_>>;
       fn mutate_data(&self) -> Option<ElementDataMut<'_>>;
unsafe fn clear_data(&self);
       fn has_data(&self) -> bool;
```

Every one takes `&self`. Two of them create or destroy storage through a shared
reference, and three return a guard borrowing it. That combination is what
constrains the design, and it rules out the first thing anybody reaches for.

## Decision

**A `StyleData` table owned by `px-css`, sized once from `Arena::slot_count()`
before the style pass, with one entry per arena slot.** Entries are indexed by
`NodeId`'s slot index, so a lookup is a bounds-checked array read.

The table never grows, and that is not a hope — it is already guaranteed. A
style traversal holds `StyleNode<'a>`, which holds `&'a Arena`, so the DOM
cannot be mutated while the traversal runs and the node count cannot change.
A `Vec` that never grows hands out references with the `Vec`'s own lifetime
rather than a borrow guard's, which is exactly what `borrow_data`'s signature
needs.

Consequently **the container needs no interior mutability**. The mutability
stylo asks for already exists inside `ElementDataWrapper`, which wraps its
`ElementData` in an `UnsafeCell` and hands out both `&` and `&mut` from `&self`.
That is stylo's unsafe, in stylo's crate. The only interior mutability this adds
is a `Cell<bool>` per entry, because `has_data` and `clear_data` need one bit
settable through `&self`.

**The result is that `px-css` needs no `unsafe` block and no new dependency** —
ADR 024's rule holds with room to spare, and `ci/gate-style.sh` checks it.

## Alternatives rejected

**`RefCell<HashMap<NodeId, ElementDataWrapper>>`.** The obvious design, and it
does not compile. `borrow_data` must return a guard that outlives the map's own
`Ref`, and any reference derived from a `RefCell` borrow dies with it. The
stored data needs an address stable independently of the container's borrow
state; a hash map behind a `RefCell` cannot provide one.

**A field on `px_dom::Node`.** What Servo does, and it would work. Rejected on
two grounds. Layering: `px-dom` would have to name a `stylo` type, so the DOM
crate would depend on the CSS engine — §3's diagram has that arrow the other
way, and a DOM that cannot be compiled without a CSS engine cannot be tested
without one. And it would walk straight into ADR 021's tripwire: a field on
`Node` changes the arena's slot layout, one of the three things that ADR named
as its own falsification. Taking a design that trips a tripwire, in order to
avoid a table, would be paying a real price for a smaller allocation.

**An append-only arena crate (`typed-arena`, `elsa`) keyed by `NodeId`.** These
exist precisely to hand out stable references from `&self`, and would work
without pre-sizing. Rejected because it is a new dependency — needing its own
ADR — to buy a property the borrow on `&Arena` already provides for free. If
the no-growth guarantee ever stops holding, this is the alternative to revisit.

**Sizing the table by `Arena::len()`.** Not a design so much as the bug this
nearly was. `len()` counts *live* nodes; the table is indexed by slot index,
which runs to the high-water mark including retired slots. The two diverge the
moment anything is removed, and the failure would be an out-of-bounds read in
the middle of a traversal. `slot_count()` is the right number and is now public
for this reason; `data_table_covers_retired_slots_not_just_live_nodes` is the
test that would have caught it.

## Consequences

**Memory is paid per slot, not per element.** One `ElementDataWrapper` for every
arena slot, whether or not it holds an element, and whether or not it is live.
`ElementData` is 24 bytes by stylo's own `size_of_test!`, so a 100,000-node
document pays a few megabytes for a table it can index without hashing. On a
document that has churned heavily the retired slots are paid for too.

**The table can go stale, and that is the one failure mode.** It is sized for an
arena at a moment in time. If nodes are added and the table is not rebuilt, the
new ones index past the end. Every accessor returns `Option` and returns `None`
there rather than silently vending a fresh empty entry — losing every computed
style past the end would be far harder to see than a `None`.

**`ensure_data`'s documented race is designed out rather than documented.**
stylo declares it `unsafe fn` because "it can race to allocate and leak if not
used with exclusive access to the element". Here there is no allocation to race
on: the entry already exists, and establishing data is a `Cell` write. The
method is still `unsafe fn` because the trait says so, which is the whole of
ADR 024's point.

**ADR 021's tripwire did not fire, and this is the decision where it most
nearly did.** `px-dom` changed in exactly one way: `slot_count()` became `pub`.
That is a visibility keyword, not the slot layout, not `NodeId`'s
representation, and not the opaque packing. `tests/layout.rs` and
`tests/opaque.rs` are untouched, which `ci/gate-style.sh` verifies by pinned
blob hash rather than by taking this paragraph's word for it.

## Verification

Wrong if a later phase needs style data for nodes created *during* a style pass
— incremental restyle driven by script, most likely — because then the table
does have to grow and the no-growth guarantee that makes it safe is gone. The
append-only arena alternative is what to reach for, and it is a dependency ADR
away rather than a redesign.

Wrong on cost if the per-slot allocation shows up on a real page. Cheap to
measure and cheap to fix in the other direction: a map from slot index to a
dense style index would pay a hash to save the empty entries.

Wrong in a way that would be caught immediately if `Arena` ever hands out a
`NodeId` whose index exceeds `slot_count()`, which would make the bounds check
in `StyleData::slot` start returning `None` for live nodes.
