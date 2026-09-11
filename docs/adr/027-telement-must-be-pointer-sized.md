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

**Recommended: B.** It costs 24 bytes per slot and keeps the property the whole
design was chosen for. A is smaller to write and gives up the compiler's
guarantee, which is the one thing this project has consistently refused to trade.

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
could be carried *inline in the view*. Under design B it is carried one
indirection away and everything else stands.

**A 24-byte-per-slot cost, on top of ADR 026's table.** Both are per-slot,
both are built at the start of a pass, and both exist because stylo's traits
want things the DOM does not store. They should be built together and measured
together.

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

Wrong about design B if the per-slot record cannot be built without a
self-referential borrow, in which case A is the fallback and the lost compile-time
guarantee has to be replaced with something — most plausibly a debug assertion
that the arena pointer in the thread-local matches the one a view was created
from.
