# 027 — stylo requires `TElement` to be pointer-sized, and the borrowed view is not

- **Status:** proposed
- **Date:** 2026-09-10
- **Phase:** 5
- **Invariants touched:** none. Bears directly on ADR 021 and on §4.1's
  generational-handle rule; see *Relationship to ADR 021's tripwire*.

## Context

`StyleEngine::resolve` runs stylo's cascade over `px-dom`. The first time it ran
it panicked inside stylo, not in this crate:

```
thread '...' panicked at stylo-0.21.0/sharing/mod.rs:611:9:
assertion `left == right` failed
  left: 10256
 right: 9488
```

That is `StyleSharingCache::new`, and the assertion is this:

```rust
struct FakeCandidate {
    _element: usize,
    _validation_data: ValidationData,
    _may_contain_scoped_style: bool,
}

type SharingCache<E>      = SharingCacheBase<StyleSharingCandidate<E>>;
type TypelessSharingCache = SharingCacheBase<FakeCandidate>;

thread_local! {
    static SHARING_CACHE_KEY: &'static AtomicRefCell<TypelessSharingCache> = ...;
}

pub fn new() -> Self {
    assert_eq!(size_of::<SharingCache<E>>(), size_of::<TypelessSharingCache>());
    assert_eq!(align_of::<SharingCache<E>>(), align_of::<TypelessSharingCache>());
    let cache = SHARING_CACHE_KEY.with(|c| c.borrow_mut());
    ...
}
```

The style-sharing cache lives in a thread-local whose type has the element
erased to `usize`, and it is `transmute`d back to `SharingCache<E>`. The
assertion is what keeps that transmute honest — and it means **`E` must be
exactly the size of a `usize`.**

Measured: `StyleElement` is **32 bytes** against the required **8**. The
arithmetic checks out exactly — 24 bytes of excess across 32 cache entries is
768, and 10256 − 9488 = 768.

This is a hard requirement that `docs/research/stylo-requirements.md` did not
find, and it is not documented in the trait. `TElement`'s declared bounds are
`Eq + PartialEq + Debug + Hash + Sized + Copy + Clone + SelectorsElement +
ElementContext`. Nothing says pointer-sized. It is enforced by an `assert_eq!`
in a constructor three modules away, at runtime, on the first style pass.

Servo satisfies it without noticing, because its element type *is* a pointer
into the DOM.

## Decision

**Proposed, and blocking Phase 5's first gate item.** The view types must shrink
to one machine word. `NodeId` is already exactly 8 bytes (ADR 018), so a view
that carries *only* the handle is the right size — which means the arena and the
style root have to be reached from somewhere other than the view itself.

Two designs, and I recommend the second:

**A. A scoped thread-local holding the `Dom`.** `StyleNode` becomes
`NodeId`, and every accessor reads the arena from a thread-local set for the
duration of `resolve()`. This is small and it is what an embedder usually does.
The cost is that the borrow relationship stops being expressed in the type
system: the guarantee that no `&mut Arena` exists during a traversal — the thing
[`crate::view`] currently gets from the borrow checker, and the thing that makes
§4.1's "resolve once and borrow thereafter" sound — becomes a runtime
convention again. It also needs per-thread setup the day ADR 023's `pool: None`
becomes `Some`.

**B. A per-slot node record, and the view is a reference to it.** The pass owns
a `Vec<NodeEntry>` with one entry per arena slot, each holding the arena
reference, the style-root reference and the `NodeId`. `StyleNode<'a>` becomes
`&'a NodeEntry<'a>` — one pointer, 8 bytes — and the lifetime keeps doing exactly
what it does today. The table is built once at the start of a pass, when the
whole arena is in hand, which is the same moment and the same justification as
ADR 026's style-data table; it can be built alongside it.

**B does not work, and the reason is worth writing down.** It was the
recommendation when this ADR was drafted, on the strength of costing 24 bytes per
slot and keeping the compile-time guarantee. Working through it kills it:
`TNode::parent_node` and the sibling accessors have only `&self`, so an entry must
be able to reach the *table* to turn a neighbour's `NodeId` back into a
`&'a NodeEntry`. That makes `NodeEntry` hold a reference to the collection it
lives in — `Table<'a>` containing `Vec<NodeEntry<'a>>` containing
`&'a Table<'a>` — which is a self-referential structure. The two-phase
construction that usually rescues this (`OnceCell` entries filled in through
shared access after the table exists) still requires the table to outlive a
lifetime that borrows it, so it is not expressible without `unsafe` — and ADR 024
forbids `unsafe` in this crate.

**So the decision is A, with the lost guarantee replaced rather than written
off.** `StyleNode` becomes the `NodeId` alone and the arena is read from a
thread-local established for the duration of `resolve()`. What the borrow checker
was providing — no `&mut Arena` can exist while a view does — becomes a
convention, so it gets a check: the thread-local stores the arena pointer a pass
was entered with, and every view records nothing but the handle, so a debug
assertion can confirm a view is being resolved against the arena it came from.
That is weaker than a type error and it is what is available.

The day ADR 023's `pool: None` becomes `Some`, the thread-local needs per-thread
establishment, and stylo's traversal already hands each worker its own
`ThreadLocalStyleContext` — so the seam exists, but it is work that has to be
done deliberately rather than inherited.

## Alternatives rejected

**Turn style sharing off.** There is no flag. `StyleSharingCache::new` is called
unconditionally from `ThreadLocalStyleContext::new`, so any traversal
constructs one and any traversal hits the assertion.

**Keep the fat view and make the *element* type a thin wrapper.** The assertion
is on `E` itself, which is the type passed to `DomTraversal` and stored in the
candidate. Every `TElement` method would have to reconstitute the fat view from
the thin one, which is design B with the table hidden inside the methods and no
table to hide it in.

**Ask stylo to relax it.** The assertion is load-bearing for a `transmute`; it
cannot be relaxed, only made unnecessary by a different cache design. Worth
raising upstream as a documentation bug — the requirement belongs in
`TElement`'s doc comment, not in an assert in a constructor — but that does not
unblock this phase.

## Consequences

**The research note's central design survives, but not its representation.**
`stylo-requirements.md` §3.2's resolution — resolve once at the boundary, borrow
thereafter — is still right, and is still what makes §4.1's generational handles
coexist with an infallible traversal. What was wrong was assuming the borrow
could be carried *inline in the view*. It moves to a pass-scoped thread-local and
everything else stands.

**The guarantee moves from the compiler to a convention, and that is the real
cost.** Today no `&mut Arena` can exist while a view does, because the view holds
`&'a Arena` and the borrow checker says so. Under A the view holds a handle and
nothing else, so the rule becomes "do not mutate the arena during a pass" plus a
debug assertion. Every other safety property survives — the generation check
still happens, the accessors still return `Option` — but this one is downgraded,
and it was the property that made the borrowed-view design attractive.

**No per-slot memory cost**, which is the one thing A is better at: the views
shrink from 32 bytes to 8 and nothing new is allocated. ADR 026's style-data
table remains the only per-slot table.

**The size requirement needs a permanent test.** It is invisible in the type
system and enforced at runtime in a dependency, so a future field added to the
view would be caught by a panic on the next style pass rather than by the
compiler. `crates/px-css/tests/sizes.rs` asserts it directly.

## Relationship to ADR 021's tripwire

**ADR 021's tripwire, read literally, does not fire — and reading it literally
would be the wrong lesson.**

Its falsification condition was *"Phase 5 has to change the arena's slot layout,
`NodeId`'s representation, or the opaque packing in order to write
`StyleNode`."* None of those changes. `NodeId` stays 8 bytes and is in fact
*exactly* the right size, the slot layout is untouched, and the opaque packing is
untouched. `ci/gate-style.sh`'s pinned blob hashes still match.

But ADR 021's central *claim* was that every layout decision `StyleNode` depended
on had already been banked, so writing it would be mechanical. That claim was
wrong. A decision it did not know about — how large the view may be — was not
banked, could not have been banked from the research note, and is the one that
forces a redesign of the type ADR 021 deferred.

So the honest report is: the tripwire as specified was too narrow. It watched the
three things the research note had identified, and the requirement that actually
bit was a fourth nobody had. That is worth recording precisely, because the
lesson is not "ADR 021 was fine" and not "ADR 021 was wrong about `NodeId`" — it
is that a tripwire can only watch the risks you already know, and this one was
enumerated from a document that had missed this.

## Verification

Wrong if `size_of::<StyleElement>() == size_of::<usize>()` is not in fact
sufficient — if the sharing cache's alignment assertion or some other erased
type imposes a further constraint. Cheap to detect: the same panic, one
assertion later.

This ADR already recorded one thing wrong with itself: design B was the
recommendation and does not work, for the self-referential reason given above.
That was found by working the design through rather than by compiling it, which
is the weaker kind of evidence — if `unsafe`-free two-phase construction turns out
to be possible after all, B is better than A and should replace it.

Wrong about A if the thread-local turns out to be reachable from a context where
no pass is active — a `Debug` impl called from a logger, say — in which case the
accessor has to answer `None` rather than panic, and the `Option`-returning shape
§4.1 already requires is what absorbs it.
