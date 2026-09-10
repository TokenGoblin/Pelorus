# Phase 4 gate report

*DOM. Branch `phase/04-dom`.*

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| html5lib-tests ≥99% | `crates/px-dom/tests/html5lib.rs`, 62 vendored `.dat` files, 1,952 cases | **Pass at 99.43%**, with two causes set aside — ADR 019 |
| Stale-handle fuzz target proves every stale lookup returns `None` | `dom_stale_handle`, plus `dom_stale_*` ×5 | **Pass** |
| 24h mutation fuzz clean | `dom_mutation` target exists and runs as a test; the campaign has not been run for this phase yet | **Incomplete** — see below |
| 100,000-level nesting without stack overflow | `dom_depth_*` ×6, on a 256 KB stack | **Pass**, and it found a hang |

Ranges are named in the phase but in none of the four gate items. They were
missing, are now built, and are gated anyway — see below.

`ci/gate-dom.sh` passes locally on Windows. The `dom` CI job is now required —
`continue-on-error` removed, since a job allowed to fail that does not is no
longer telling anybody anything.

## Conformance: two numbers, on purpose

```
html5lib (graded):       1935/1946 = 99.43%
  set aside: 6 needing script execution (Phase 11)
  set aside: 88 whatwg/html#12118, see ADR 019
html5lib (whole corpus): 1847/1952 = 94.62%
```

The gate grades the first. The second is measured every run and floored, so it
cannot rot quietly while the first stays green.

The 105 failures, by cause:

| Cause | Count |
|---|---:|
| [whatwg/html#12118](https://github.com/whatwg/html/pull/12118) — `<?target data?>` became a `ProcessingInstruction` in 2025; html5ever 0.39 predates it | 88 |
| Needs script execution with DOM bindings, mid-parse (Boa is Phase 10, bindings Phase 11) | 6 |
| html5ever tree-builder gaps — counted against us | 11 |

**None is `px-dom`'s sink**, and that was checked rather than assumed. For the
`<frameset>` failures, a throwaway minimal `TreeSink` that logs its calls shows
html5ever never creating a frameset element and never calling
`remove_from_parent` for that input — the decision is made before anything
reaches us. For the processing instructions, html5ever calls `create_comment`
with `"?something"`, never `create_pi`.

The eleven tree-builder gaps are **not** excluded. "Our dependency is
imperfect" is a reason, and a reason is not an exemption.

### The corpus moved, and the reader was wrong

Two things had to be fixed before a number existed at all.

`html5lib/html5lib-tests` is now a one-line README pointing at
web-platform-tests. The corpus here is WPT at `2b6223e2`,
`html/syntax/parsing/resources` — same tests, same format, new address,
recorded in `tests/html5lib/README.md` rather than substituted quietly.

And the `.dat` reader stripped a trailing newline that is part of the input:
`<!doctype html><table>\n` became `<!doctype html><table>`, which has no
trailing text node and therefore a different expected tree. Two tests were
failing because of the harness rather than the parser.

### What stops the exclusions from laundering the number

Both counts are **pinned** — 6 and 88. The exclusion cannot absorb one more
test than it does today without failing, whether from a widened predicate or a
real regression falling into a bucket. Verified by widening the predicate to
swallow `template.dat` and watching the assertion fire at 94 against 88.

A cause must have an external reference to exist: a spec PR, a filed upstream
bug. There is no bucket for "this one seems fine."

The corpus is pinned too: file and case counts asserted, so a file
disappearing is a failure rather than a smaller denominator. Fragment tests
are run rather than skipped and their count is asserted at 199 — they are the
ones needing a second entry point, and so the easiest to drop.

## The nesting bomb, which was a hang

The gate item says "100,000-level nesting handled without stack overflow". The
tree half is §4.4's depth limit and was straightforward. The time half was not,
and it is the one that was dangerous.

html5ever's tree builder is **quadratic in nesting depth**: its stack of open
elements grows however shallow the tree we build, and the spec's "has an
element in scope" tests scan it on every start tag. The depth limit bounds our
tree and does nothing about that stack.

Release measurements, nested `<div>`s — this crate's arena against the whole
parse:

| nesting | arena | parse |
|---:|---:|---:|
| 2,000 | 2 ms | 11 ms |
| 4,000 | 4 ms | 44 ms |
| 8,000 | 9 ms | 192 ms |
| 16,000 | 19 ms | 686 ms |

The arena doubles with the input. The parse quadruples. A five-megabyte file of
nothing but `<div>` — one line to write — extrapolates to about **forty-five
minutes of CPU**. That is the exhaustion §4.4 exists to prevent, arriving
through a dependency rather than through our own code.

`px_dom::parse` now feeds the parser in 8 KB chunks and stops once the tree has
refused eight pieces of content, reporting `Dom::abandoned`. A million-deep
document costs **65 ms**, flat in input size.

Tested in both directions, because a bound that fires early silently eats real
pages: 100,000 shallow paragraphs all survive, 400 levels of legal nesting all
survive, and neither is abandoned.

## §14.3, closed by measurement

ADR 018. `NodeId` is 32/32, eight bytes — the *more expensive* option, chosen
deliberately.

| | 32/32 | 24/8 |
|---|---:|---:|
| handle | 8 B | 4 B |
| a node's five links | 40 B | 20 B |
| exhaustion at 1,000 mutations/sec | never | **64 days** |
| exhaustion at 1,000,000/sec | never | **71 minutes** |

Packing saves 20 bytes per node — 5 MB on a 250,000-node document. It sells a
scripted tab-kill in 71 minutes, a two-month ceiling under ordinary use, and a
hard 16.7-million-node document limit. The 64 days decided it: a dashboard left
open for two months reaches the ceiling with nobody attacking, and the user
sees a tab that dies for no reason they can connect to anything.

`crates/px-dom/tests/layout.rs` asserts every number above, including the ones
arguing against the decision taken.

## What the tests found

**A depth limit with a gate in it.** `check_insertable` only asked where the
*moved node* would land, not its subtree's deepest node — bypassable in one
move where every step looks legal: build the deep part detached, where each
insertion is genuinely shallow, then attach its root somewhere deep. 512
becomes a suggestion. Now measures subtree height as well.

**A gate check that could not fail.** `gate-dom.sh`'s non-vacuity guard passed
on any `Option<...Node>` anywhere, which the arena's own `node: Option<Node>`
slot field satisfies — so it would have gone green on a crate with storage and
no accessors at all, exactly the half-started state it exists to catch. Found
by deleting the real accessors and watching it pass. Now anchored on `->`.
That anchor then broke the scan silently, because a pattern starting with `->`
is read by grep as options.

**The same gate matched its own prose.** The script's header names `Vec<Node>`
as the thing it bans; px-dom's module docs explain why nodes must not own each
other. Both tripped the ban. A rule that cannot be written down without
tripping is a rule people comply with by deleting the explanation.

Every check verified in both directions: fires on an injected violation, silent
on the clean tree.

## The 24h campaign: running, not yet reported

`dom_stale_handle` and `dom_mutation` exist, compile, and run as ordinary tests
on every push — about a tenth of a second for twelve hundred operation
sequences. What has **not** happened is the 24-hour campaign the gate item
names.

They have also never been run under libFuzzer on this machine: cargo-fuzz will
not link here (no MSVC ASAN runtime, and `--sanitizer=none` leaves sancov
symbols undefined). That is why the bodies live in `px_dom::harness` rather
than in `fuzz_targets/` — Phase 1 shipped a campaign that reported clean while
never reaching the code it was built for, and two targets committed having only
ever compiled would be the same failure from the other direction.

Verified by breaking the arena on purpose: `detach` made to skip its
`last_child` fixup, and both harnesses failed with the assertion written for
that exact corruption.

**This item is incomplete and the phase should not be called closed on it.**

Run [34434827530](https://github.com/TokenGoblin/Pelorus/actions/runs/34434827530)
is in flight: 11 shards × 4 hours, three of them the DOM targets. The matrix
did not know these targets existed until this phase added them, so a campaign
run before that would have reported clean while never touching them — the
Phase 1 failure again, arrived at by omission rather than by a wrong flag.

Their `-max_len` is 4 KB against the 1.1 MB the IPC targets get and the 8.5 MB
the HTTP ones get. These harnesses assert the whole tree's invariants after
every operation and one byte is roughly one operation, so cost grows with the
square of the input: 1 KB is 0.4 ms, 4 KB 2.9 ms, 16 KB 20 ms, 64 KB 170 ms. At
the default a single execution would take minutes and the campaign would
explore almost nothing.

The numbers belong in this section when they exist, the way Phase 3's did.

## Ranges, which the gate never asked for

§9 Phase 4 names four deliverables — *"mutation-safe iteration, tree ordering,
ranges, depth limits"* — and its gate covers three. Ranges were missing
entirely, and nothing was red. They were found by re-reading the phase
description after the gate had already gone green, which is precisely the
failure a gate exists to prevent.

`ranges` is now a gate suite despite not being a §9 gate item. This gate has
always checked more than §9 enumerates — the no-infallible-accessor and
no-owned-children source scans are not gate items either.

Thirteen tests, mostly about **liveness**: what a range does when the tree
moves underneath it. The DOM does not invalidate a range whose node was
removed, it *moves* it, so "the handle stopped resolving" and "the range is
meaningless" are different states and are kept apart.

Both mutation rules verified load-bearing by disabling each and watching the
tests written for it fail — 2 for insertion, 4 for removal.

### And the quadratic I put in

The parse suite went from 3 seconds to 145. `append_child` was calling
`child_ids(parent).count()` to hand the insertion rule an index — a walk of
the whole child list on every append. 100,000 shallow paragraphs took **145
seconds**, against 2 for a million-deep nesting bomb.

The fix was not a faster count. An append lands at the end, the DOM's rule
moves only offsets *greater* than the insertion index, and the largest valid
offset in a parent is exactly that index — so an append cannot move a boundary
point, and the notification was never needed. Back to 3 seconds, with a
wall-clock ceiling on that test so the next one fails rather than merely
crawls.

## Stylo's snapshots, built now because Phase 5 cannot afford to

`docs/research/stylo-requirements.md` item 6, written during Phase 1: stylo's
invalidation needs prior-state records captured *at mutation time*, and it is a
mutation-path feature — every attribute setter has to record one. The note ends
*"Retrofitting it in Phase 5 means touching every mutation site twice. Add it
to the Phase 4 scope and gate."*

It was not in the gate, and it was not built. `px-dom` has three attribute
mutation sites today; it will have dozens once there is a scripting surface.

Built in this crate's own types, not stylo's: stylo is a Phase 5 dependency
needing an ADR, and a Phase 4 crate depending on the thing Phase 5 exists to
*try* is backwards — §9 calls Phase 5 the phase most likely to force a `px-dom`
redesign.

The rule that makes a snapshot useful is that it holds the element as of the
**last restyle**, not the last mutation: the first write since a flush
captures, every write after only updates the change flags. Getting it backwards
records a change from the second-most-recent value to the most recent — a
change that never happened — and it is invisible, because the flags are right
and the values are plausible.

### A test that could not see what it claimed

The guard here is a **source check**, not a test, and the reason is worth
keeping. A sink that reaches into `NodeData::Element { attrs }` and pushes
directly builds exactly the right tree and passes the entire conformance
corpus. It is wrong only in that nothing recorded what the attribute used to
be — which nothing observes until Phase 5 turns recording on, months later,
with no way to connect symptom to cause.

The first attempt at testing it failed instructively: the test called the arena
method directly, so rewriting the sink to bypass the arena entirely left it
green. That test has been renamed to say what it actually covers, and
`ci/gate-dom.sh` now checks the source.

Twelve tests besides. One of them exists because `snapshot()` returns the first
match, so a bug that pushes a fresh record per write is invisible through that
accessor — the first entry still holds the oldest values and still looks right.
It shows up only in the count.

## Dependencies added

ADR 017. `html5ever` 0.39: **+21 crates, +967 unsafe tokens**.

The finding is where the unsafe is not. `html5ever` contributes 4 tokens and
`markup5ever` 1. **539 — more than half — is `parking_lot`**, reached only
because `string_cache` wants a process-global interner and therefore a lock.
178 more is `redox_syscall`, for an operating system this project never
compiles for.

The figure that reaches a user is **789, of which a mutex is 539**.

`redox_syscall` is the project's first `cargo vet` exemption rather than a
trust entry: trusting its publisher would assert we vouch for a maintainer
whose code we never build.

## Carried out of this phase

- The 24h mutation campaign (above) — the one gate item not met.
- html5ever's quadratic tree builder: mitigated in `px_dom::parse`, not fixed.
  Anything driving html5ever without going through it gets the old behaviour
  back with no warning.
- whatwg/html#12118 upstream to html5ever.
- Six conformance exclusions that Phase 11 inherits.
- `Arena::force_generation_to_last` has no release-artifact scan, because no
  shipping binary links px-dom yet and the scan could not fail.
- `string_cache`'s process-global interner is a cross-document timing channel
  until Phase 14 makes one-process-per-site real.
- Miri still does not run this crate's tests, which its CLAUDE.md claimed since
  Phase 0. Corrected in place under a "Not true yet" heading.
