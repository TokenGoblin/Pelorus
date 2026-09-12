# 027 — stylo requires `TElement` to be pointer-sized, and the borrowed view is not

- **Status:** accepted
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

**B is the decision, and this ADR was wrong about it twice before getting
there.** The draft recommended B. The first revision rejected it as impossible:
`TNode::parent_node` and the sibling accessors have only `&self`, so a record
must reach the *table* to turn a neighbour's `NodeId` back into a reference,
which makes the record hold a reference to the collection it lives in — a
self-referential structure. That reasoning was right about the shape and wrong
about the conclusion.

It is expressible without `unsafe`, and the cycle closes in two steps:

```rust
let ctx = DomCtx { arena, root, entries: OnceCell::new(), document };
let entries: Vec<NodeEntry<'_>> = (0..arena.slot_count())
    .map(|_| NodeEntry { ctx: &ctx, id: document })
    .collect();
ctx.entries.set(&entries).ok()?;
```

`OnceCell::set` takes `&self`, so closing the cycle needs no mutable borrow while
the records already hold a shared one. Neither type implements `Drop`, so dropck
permits two locals that reference each other. Verified by compiling a
thirty-line probe before touching the crate, rather than by reasoning about it a
third time — which is the actual lesson here: the first rejection was a
conclusion reached by thinking, and thinking was what had been wrong the time
before.

So `StyleNode` is `&'a NodeEntry<'a>` — **eight bytes** — the arena and style
root live in a per-pass `DomCtx`, and the lifetime keeps doing exactly what it
did. Nothing is given up.

The construction has one consequence worth stating: the context and the records
are two locals that reference each other, so neither can outlive the call that
made them and a `Dom` cannot be returned. The entry point is therefore
`with_dom(arena, root, |dom| ...)`, a scope rather than a constructor. That is
forced by the lifetimes, not a stylistic choice.

**Design A, the scoped thread-local, is not needed and was not taken.** It would
have traded the compile-time guarantee — that no `&mut Arena` can exist while a
view does — for a convention plus a debug assertion, and it would have wanted a
new dependency (`scoped-tls` is not in the closure) because a thread-local cannot
hold a borrowed reference without `unsafe`. Both costs are avoided.

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
could be carried *inline in the view*. It moves one indirection away and
everything else stands — including the compile-time guarantee, which was the
whole reason for choosing a borrowed view.

**Cross-arena confusion becomes a type error rather than a runtime check.**
Under the old representation two views from different arenas had the same type,
and `PartialEq` compared arena pointers to keep a traversal from mistaking one
document for another. Now each `with_dom` scope has its own lifetime, so a view
from one cannot be passed into another's closure at all. A test that compared
views across two nested scopes stopped compiling, which is the better outcome.

**A per-slot record table, 24 bytes each, built once per pass.** The second such
table after ADR 026's style data, built at the same moment and for the same
reason: stylo's traits want things the DOM does not store. They should be
measured together.

**The entry point is a scope, not a constructor.** `with_dom(arena, root, f)`,
because the context and the records reference each other and neither can outlive
the call. Callers cannot hold a `Dom` across passes, which is a constraint the
old design did not have.

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

This ADR was wrong about its own decision twice — first recommending B without
checking it, then rejecting B on reasoning that was itself unchecked. Both are
recorded above rather than tidied away, because the pattern is the point: the
step that settled it was compiling a thirty-line probe, and that step was
available on the first day.

Wrong now if a later phase needs a view to outlive a pass — a cached reference to
a styled element held across restyles, say. `with_dom`'s scope forbids it by
construction, and the fix would be to key such a cache by `NodeId` and re-resolve,
which is what §4.1 would want anyway.

Wrong if the per-slot record table shows up in a memory profile on a real page.
Cheap to measure and it shares its shape with ADR 026's table, so the two would be
fixed together.
