# 018 — `NodeId` is 32/32, not 24/8

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 4
- **Invariants touched:** 8 (fail closed). The rejected layout fails closed
  correctly and that is precisely the problem; see Decision.

## Context

build-spec §14.3 leaves this open and tells Phase 4 to close it with a
measurement rather than an argument:

> `NodeId` doubles from 4 to 8 bytes, and DOM handles are everywhere, so this
> is a real memory cost at scale. Worse, **generation counters wrap**: after
> 2³² reuses, a stale handle becomes valid again — the exact bug the design
> exists to prevent.
>
> **Fix:** retire a slot permanently on generation overflow rather than
> wrapping. Consider a 24/8 index/generation split with slot retirement to
> keep handles at 4 bytes; measure both in Phase 4 and record the choice.

Two things are settled going in and are not reopened here. Handles are
generational, and a slot whose generations are spent is **retired
permanently rather than wrapped** — wrapping reintroduces the use-after-free
the whole design exists to prevent. What is open is only the bit split.

## Decision

**A `u32` index beside a `NonZeroU32` generation. Eight bytes.**

`NonZeroU32` on the generation is load-bearing rather than tidy: it gives
`Option<NodeId>` a niche, so a link field costs the same as a handle. Without
it every one of a node's five links pays a discriminant word and this
comparison is not the one that was measured.

Generation numbering starts at 1 and a slot is retired at `u32::MAX`.

## What was measured

`crates/px-dom/tests/layout.rs`. Both layouts are defined there and every
figure below is asserted, so these numbers can be re-derived rather than
believed — and a change in the tradeoff fails a test instead of quietly making
this document wrong.

### Memory — the case *for* packing

| | 32/32 | 24/8 |
|---|---:|---:|
| handle | 8 B | 4 B |
| `Option<handle>` | 8 B | 4 B |
| a node's five links | **40 B** | **20 B** |

Five links is not a guess — parent, first and last child, previous and next
sibling is the minimum for O(1) insertion and removal at either end plus
upward traversal, and it is what every arena DOM converges on.

So packing saves **20 bytes per node**:

| document | saving |
|---|---:|
| a typical article, 1,500 nodes | 30 KB |
| a heavy application view, 25,000 nodes | 500 KB |
| an extreme document, 250,000 nodes | 5 MB |

That is a real saving. §14.3 is right that it is not nothing.

### Exhaustion — the case *against*

Retirement is not free, and this is the half the memory table does not show.
Under sustained churn a slot is retired every `MAX_GENERATION` reuses, so
churn consumes *slots* permanently at `rate / MAX_GENERATION` per second.
With 8 generation bits that is one slot per 255 mutations, against a 24-bit
index space of 16,777,216.

| churn rate | 24/8 exhausts in | 32/32 exhausts in |
|---|---:|---:|
| 1,000 nodes/sec — an animated list, a live dashboard | **64 days** | never |
| 1,000,000 nodes/sec — a script trying | **71 minutes** | never |

Both numbers were a surprise, in opposite directions.

**71 minutes is a denial of service available to any page.** Not a
memory-safety failure — §4.3 treats a dead content process as routine, and the
failure is a clean refusal rather than a stale handle resolving, so invariant 8
holds. It fails closed. But it is a tab killed from script, with no cost to the
attacker, and no recovery except a reload that discards whatever the user had
typed. A browser whose entire thesis is that it does not lose to hostile
content should not ship a scripted tab-kill to save five megabytes on a
document nobody has.

**64 days is worse, because nothing is attacking.** This is the number that
actually decided it. A monitoring dashboard, a chat client, a trading screen —
something left open and mutating for two months — reaches the ceiling with no
adversary and no unusual behaviour. What the user sees is a tab that dies for
no reason they can connect to anything they did. That is the worst shape a
defect can have, and it is the same shape this project rejected a stale PSL for
in Phase 3: correct-looking, silent, and impossible to attribute.

There is also a ceiling that has nothing to do with time. **24 bits caps a
document at 16.7 million nodes, permanently.** No other browser has a hard
node limit, and "this document is too large" is not a failure mode the web
platform has, so nothing on the web is written to avoid it.

## Alternatives rejected

**24/8 packed.** Above. It buys 20 bytes per node and sells a scripted
tab-kill, a two-month ceiling on ordinary use, and a hard document-size limit.
The trade is not close in a security project — 5 MB at a quarter of a million
nodes is a rounding error beside the render tree, the style data and the
JavaScript heap that a document of that size implies.

**28/4 or other intermediate splits.** Strictly worse in the direction that
matters: fewer generation bits means faster slot burn. Moving bits the other
way (say 20/12) trims the exhaustion problem while tightening the document
ceiling, and no split of 32 bits escapes having both problems at once. The
tradeoff is a property of the width, not of where the line sits inside it.

**64-bit handles (32/32 with room to grow).** Already what this is. There is
no case for going wider; `u32::MAX` generations at `u32::MAX` slots is 2⁶⁴
churn cycles, which is not a number a script reaches.

**Wrapping instead of retiring.** Foreclosed by §14.3 and correctly. It is the
use-after-free this design exists to prevent, and at 8 bits it would arrive in
255 reuses rather than in four billion.

## Consequences

**A node's links cost 40 bytes.** Accepted, and it is the honest headline: this
ADR chose the more expensive option. If DOM memory becomes a measured problem
the answer is fewer nodes or a smaller `NodeData`, not smaller handles — a
node's payload dwarfs its links long before the links are worth 20 bytes.

**Retirement is still implemented and still reachable — just not reachably.**
`u32::MAX` generations is unreachable from script, so the retirement path
cannot be exercised by ordinary testing and would rot. §14.3 anticipates this
and asks for a fuzz target that *forces* generation exhaustion; that target
sets a slot's generation near the ceiling directly rather than churning to it.
Without it the most important branch in the arena would be the least tested.

**The measurement is a test, not a paragraph.** `tests/layout.rs` asserts
every number quoted here, including the ones that argue against the decision
that was taken. If a future change makes packing look better — a much smaller
`NodeData`, or a document-size ceiling that stops mattering — the way to
reopen this is to change the test and watch which assertions fail.

## Verification

Wrong if DOM memory turns out to be dominated by link fields rather than by
node payloads, which is the assumption the "5 MB is a rounding error" line
rests on. Testable as soon as `NodeData` is real: compare `size_of::<Node>()`
against the 40-byte link block. If links are more than about a third of a node,
this ADR was decided on a bad premise and should be reopened.

Wrong if a real document is ever seen approaching 16.7 million nodes, which
would mean the packed layout's ceiling was the binding constraint all along
and the exhaustion analysis was arguing about the wrong limit. Nothing
observed suggests this.

It is **not** falsified by px-dom using more memory than a browser with 4-byte
handles. That was measured at 20 bytes per node before choosing, and is
recorded above.
