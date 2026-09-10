# 020 — The arena is chunked, so slot addresses are stable

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 4
- **Invariants touched:** none.

## Context

`docs/research/stylo-requirements.md` §3.3 calls this "the actual Phase 4
decision", and it was made by default rather than on purpose: `px-dom`'s arena
was a flat `Vec<Slot>`, which is the layout that note rejects.

A `Vec` reallocates when it grows and moves every slot with it. That is sound
in safe Rust — nothing can grow the arena while a `&Node` is outstanding,
because the borrow checker says so. The problem is what that soundness costs:

> Within a traversal that cannot happen (the view holds `&DomTree`), so this is
> sound as written — but it silently forbids ever handing out a `StyleNode`
> that outlives a mutation window, and it means the borrow of the whole tree is
> the unit of safety.

Phase 5 implements stylo's `TElement` over a `StyleNode<'dom>` that is meant to
be `Copy`, two words, `Send + Sync`, and to hold `&'dom Slot` directly. The
alternative — carrying `(NodeId, &'dom Arena)` and re-resolving on every
accessor — was considered in the note and rejected, because `local_name()` then
needs an `expect()` on a lookup that cannot fail, which puts a panic path back
into selector matching.

The note's recommendation is unambiguous:

> **Recommendation: chunked, stable-address arena, decided in Phase 4.**
> Retrofitting stable addresses onto a flat `Vec` design touches every
> accessor, every iterator, and the Miri tests. This single choice is most of
> the "px-dom redesign" the risk register is worried about, and it is fully
> decidable today.

§9 calls Phase 5 the phase most likely to force a `px-dom` redesign. This is
the part of that redesign that can be done before it is needed.

## Decision

**Slots live in chunks of 1024, as `Vec<Box<[Slot]>>`.**

Growing pushes a `Box` onto a `Vec`, which moves pointers, never the slots
behind them. A slot's address is fixed for the life of the arena.

1024 is a power of two, so the index split is a shift and a mask. It is 48 KB
of `Slot` at present — large enough that per-chunk overhead is noise, small
enough that a document of a few hundred nodes does not pay for a page it never
fills.

## What it costs

Measured on a release build, a 200,000-paragraph document (~470,000 nodes),
median of five runs, three runs of each layout:

| | flat `Vec` | chunked | change |
|---|---:|---:|---:|
| parse | 109 ms | 107 ms | **−2%** |
| full document-order walk | 14.4 ms | 18.0 ms | **+25%** |
| 1.9M handle resolutions | 8.9 ms | 10.0 ms | **+12%** |

Parse is unchanged, marginally in the chunked layout's favour — a flat `Vec`
growing to 470,000 slots reallocates and copies repeatedly, and not doing that
roughly pays for the extra indirection.

The traversal cost is real and is the honest headline: **a quarter more time in
document-order walking.** Every accessor now does a bounds check, a shift, a
mask and a second pointer hop instead of one indexed load.

## Alternatives rejected

**Keep the flat `Vec`.** Cheaper on every measurement above, and it forecloses
the `StyleNode<'dom>` design Phase 5 is planned around. The note's argument is
about *when* the cost is paid, not whether: retrofitting stable addresses later
touches every accessor and every iterator in the crate, at the point in the
schedule where `px-dom` is also absorbing stylo's trait stack. Doing it now
costs an afternoon and a benchmark.

**A flat `Vec` plus `StyleNode { id, tree }`,** re-resolving per accessor.
Rejected in the research note and not revisited: it reintroduces a panic path
into selector matching, in a crate whose entire discipline is that lookups
return `Option`.

**Larger or smaller chunks.** 1024 was not tuned, and this is worth saying
plainly rather than implying a measurement that did not happen. The traversal
regression is dominated by the extra indirection, which no chunk size removes;
chunk size trades allocation count against waste in the last chunk, and neither
is currently near mattering.

## Consequences

**Traversals are a quarter slower, before anything has needed them to be
fast.** No layout, style or paint work exists to measure against, so this is
being accepted on the research note's reasoning rather than on a profile. If
document-order traversal later shows up hot, the fix is not to un-chunk it —
it is that `Descendants` re-resolves a handle per step and could hold a chunk
reference across a run of siblings.

**`Slots` is a private type with an invariant the compiler does not check:**
every chunk is exactly `CHUNK_SLOTS` long, because the index arithmetic assumes
it. It is built in one place and never resized.

**A test asserts the property, and would fail on the old layout.**
`dom_mutation_slot_addresses_are_stable_across_growth` records slot addresses
as integers, grows the arena past twenty thousand nodes and several chunk
boundaries, and compares. Verified in both directions: it passes chunked and
fails flat. Comparing integers rather than holding references, because holding
them across the growth is exactly what the borrow checker refuses — and that
refusal is why the flat layout looks fine right up until Phase 5 needs it not
to be.

## Verification

Wrong if Phase 5's `StyleNode` turns out not to want `&'dom Slot` after all —
if stylo's trait stack forces the `(NodeId, &Arena)` shape for some other
reason, this bought a 25% traversal regression for nothing. The research note
marks §3.3's two-layout comparison **[I]** rather than **[V]**: it is inferred
from stylo's requirements, not verified against a working integration. That is
the weakest link in this ADR and it is the note's own assessment, not a
qualification added here.

Wrong if the traversal regression turns out to matter more than the note
assumes. Testable as soon as there is a style traversal to profile, which is
Phase 5 — and the numbers above are the baseline to compare against.

It is **not** falsified by the arena being slower. That was measured before
accepting and is in the table.
