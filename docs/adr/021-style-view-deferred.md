# 021 — The borrowed style view is deferred to Phase 5

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 4
- **Invariants touched:** none.

## Context

`docs/research/stylo-requirements.md` §4 item 2 asks Phase 4 to build a
borrowed style view:

> **A borrowed style view type. Yes, `px-dom` needs a parallel-safe snapshot
> type.** `DomTree::style_view(&self) -> StyleView<'_>`, yielding
> `StyleNode<'dom>` / `StyleElement<'dom>` — `Copy`, two words, `Send + Sync`,
> constructed only through a fallible generation-checked lookup, statically
> incapable of outliving the immutable borrow of the tree. […]
> *Build it in Phase 4 even though nothing consumes it until Phase 5.*

Every other Phase 4 item in that note has been taken: the chunked arena
(ADR 020), the `NodeId` layout (ADR 018), the `OpaqueNode` packing (§3.5), the
mutation-side snapshots (item 6), the tree validator (item 7), and the atom
agreement so far as it can be checked without stylo (item 4).

This is the one that has not, and skipping something a research deliverable
explicitly says to build needs a recorded reason rather than a silence.

## Decision

**Do not build the view type in Phase 4. Build it in Phase 5, against the real
traits.**

The note gives one reason for the Phase 4 timing, and states it as the whole
reason:

> Its whole value is that it forces the arena and slot layout decisions early,
> which is precisely what this note exists to do.

**That value has been banked without the type.** The decisions it was meant to
force are made and recorded:

| Decision the view type would have forced | Where it was made |
|---|---|
| Stable slot addresses, so `StyleNode` can hold `&'dom Slot` | ADR 020 — chunked arena, measured |
| `NodeId` layout and width | ADR 018 — 32/32, measured |
| Non-zero, injective, allocation-free opaque identity | §3.5 packing, five tests |
| Generations that never wrap, so identity stays injective | §14.3, ADR 018 |

`ADR 020` in particular was decided *because* of §3.3's argument about
`StyleNode<'dom>` holding `&'dom Slot`, and paid a measured 25% traversal
regression to satisfy it. The forcing function worked; the artefact it was
supposed to work through is not needed for it to have worked.

## What building it now would cost

The note's own evidence argues against writing this type against today's stylo:

> Version churn is real and is a Phase 5 planning input: 24 published versions
> in ~28 months, with breaking trait changes in most. The `TElement` shape
> described below is a snapshot, not a contract. `dom.rs` changed as recently
> as 2026-06-30 in a way that *added* a supertrait (`ElementContext`).

And §3.3's two-layout comparison — the argument this type's shape rests on — is
marked **[I]**, inferred, not **[V]**, verified against a working integration.
The note is careful about that distinction throughout and it is doing work
here: `StyleView` is a type whose entire purpose is to satisfy a trait nobody
has compiled against this DOM.

So building it now means writing an unconsumed type against an inferred shape
of a churning trait, with no compiler and no test able to say whether it is
right. The likely outcome is that it is written twice, and the second writing
happens in Phase 5 anyway — which is the cost the note was trying to avoid,
paid in the other direction.

## Alternatives rejected

**Build it as specified.** The honest version of this ADR is that it is a close
call and the note's author may simply be right. What tips it is that the stated
benefit is already realised and the stated risk — trait churn — is documented
in the same note, by the same evidence-gathering, with a `[V]` marker on the
churn and an `[I]` on the design.

**Build a reduced version** — the lifetime and `Copy`/`Send` shape without the
stylo-specific accessors. Rejected because the shape *is* the stylo-specific
part; a two-word `Copy` handle that cannot outlive a tree borrow, with no
traits on it, is `NodeId` plus a lifetime, and `px-dom` already has the half of
that which is load-bearing.

## Consequences

**Phase 5 starts with more to do than the note planned.** That is the cost, and
it is real: the phase §9 already calls the highest-risk gains a task. It is
mitigated by every *decision* the type depended on being settled, so what
remains is writing a type against traits that can be compiled, not making
layout choices under deadline.

**This ADR is the tripwire.** If Phase 5 finds that the arena cannot support
the view type — that stable addresses are not enough, or that `NodeId`'s width
or the opaque packing is wrong for the real `TElement` — then this decision was
wrong, and specifically the claim that the forcing function had been banked was
wrong. That is the thing to check first and to say plainly, rather than
absorbing it as ordinary Phase 5 difficulty.

## Verification

Wrong if Phase 5 has to change the arena's slot layout, `NodeId`'s
representation, or the opaque packing in order to write `StyleNode`. Any of
those falsifies the central claim above. Cheap to detect: it is the first thing
that happens when the trait impls are attempted.

Wrong in the other direction if the view type turns out to be writable
unchanged against the real traits — which would mean the inference was sound
all along and a phase of lead time was given up for nothing. Also cheap to
detect, and worth recording either way, because the two failures point at
opposite lessons about how much to trust an `[I]` marker.

It is **not** falsified by Phase 5 being difficult. §9 says it will be.
