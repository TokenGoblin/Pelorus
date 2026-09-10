# Phase 4 gate report

*DOM. Branch `phase/04-dom`.*

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| html5lib-tests ≥99% | `crates/px-dom/tests/html5lib.rs`, 62 vendored `.dat` files, 1,952 cases | **Pass at 99.43%**, with two causes set aside — ADR 019 |
| Stale-handle fuzz target proves every stale lookup returns `None` | `dom_stale_handle`, plus `dom_stale_*` ×5 | **Pass** |
| 24h mutation fuzz clean | one campaign complete (11 shards, 30.7bn executions, clean) but it predates `dom_parse` and gave `dom_mutation` 8 CPU-hours, not 24; a second is in flight | **Incomplete** — see below |
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

## The campaign: one run complete, and it does not close the item

Run [34434827530](https://github.com/TokenGoblin/Pelorus/actions/runs/34434827530)
finished: **11 shards, 4h each, 30.7 billion executions, no crashes and no
artifacts written.**

The three DOM shards:

| Target | Shard | Runs | cov | ft | corpus | exec/s |
|---|---:|---:|---:|---:|---|---:|
| `dom_stale_handle` | 1 | 5,374,062 | 466 | 3,337 | 555 / 63 KB | 373 |
| `dom_mutation` | 1 | 8,682,691 | 403 | 2,798 | 360 / 36 KB | 602 |
| `dom_mutation` | 2 | 17,236,660 | 403 | 2,797 | 370 / 31 KB | 1,196 |

Execution rates in the hundreds rather than the millions the IPC targets reach,
which is the harnesses working as designed: they assert every structural
invariant after every operation. The predicted rate was ~350/s at 4 KB; the
observed 373–1,196 says the estimate was sound and slightly pessimistic.

**`-max_len` was verified applied rather than assumed**, by the check Phase 1
taught: `lim: 4096` appears in libFuzzer's own final line for all three DOM
shards, and `-max_len is not provided` appears zero times in 53,856 log lines.
A flag on a command line proves it was passed; `lim:` proves libFuzzer acted on
it.

### Why this does not close the gate item

Two reasons, both recorded before the run rather than discovered after.

**It does not cover `dom_parse`.** The run was launched at `a599761`, several
commits before that target existed. Every line above is the arena API; not one
byte of HTML went through html5ever and the `TreeSink`.

**`dom_mutation` got eight CPU-hours, not twenty-four.** Two shards at four
hours. The campaign total is 44 hours and the gate item is "24h mutation fuzz
clean" — both numbers are true and only one of them is about the item.
Counting a campaign total against a per-target item is how a gate gets
satisfied on paper. Phase 3's report has the same shape: "24h fuzz on HTTP
framing", recorded as a pass on three shards and twelve CPU-hours.

Rather than argue which reading is right, the matrix now gives `dom_mutation`
six shards — 24 CPU-hours of that target — plus three for `dom_parse` and two
for `dom_stale_handle`. Nineteen shards, 76 CPU-hours, still about four hours
of wall clock.

Run [34453317232](https://github.com/TokenGoblin/Pelorus/actions/runs/34453317232)
is in flight against that matrix. **Its numbers, not the ones above, are what
close this item.** The table above is worth keeping because it is real
coverage of the arena targets — it is simply less than the gate asks.

## Dependencies added## Dependencies added

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

## The parser had no fuzz target, and validate() was quadratic

Found by auditing §4.1, §4.4 and §14.3 line by line against the code rather
than against the gate — after the gate was already green.

§4.4: *"Fuzz corpus includes 100,000-level nesting for each parser."*
`dom_stale_handle` and `dom_mutation` drive the arena's API with operation
bytes. **Neither sent a byte of HTML through html5ever and the `TreeSink`** —
which is the surface an attacker actually reaches, because a page is bytes
rather than a sequence of `append_child` calls. Foster parenting, the adoption
agency algorithm's reparenting, template contents, text-run merging, attribute
merging on a duplicate `<html>`, and the feed bound were all unfuzzed.

`dom_parse` closes it, with twelve committed seeds including the two the spec
names by size: 100,000 `<div>`s opened, and 100,000 opened and closed.

### The quadratic underneath it

Measuring the new harness turned up something worse than a missing target. A
1 MB shallow document cost **two seconds**, and the cost was in
`Arena::validate` — which `dom_mutation` calls after *every operation*.

Two separate problems, both mine:

- `validate` called `depth(id)` for every node, walking *up* to the root each
  time: O(nodes × depth). Replaced with one downward pass carrying depth, which
  is O(nodes) and is a strictly better cycle check — a cycle is now "a live
  node no root can reach", rather than "a walk that went too far", which was
  indistinguishable from a legitimately over-deep node.
- The duplicate-child scan was `for each child, search the rest of the list` —
  **O(children²)**, and this document gives `<body>` 131,072 children. It was
  also redundant: the downward pass marks nodes as it visits them, so a node
  appearing twice in one child list is caught the second time it is pushed.

| | before | after |
|---|---:|---:|
| 1 MB shallow document | 2002 ms | **104 ms** |
| 64 KB shallow | 15.9 ms | **6.3 ms** |
| 1.1 MB nesting bomb | 75 ms | 75 ms |

The nesting bomb was never the slow case — it is abandoned after eight
refusals. The slow case was an ordinary large page, in the check that runs
most often.

Both detection paths re-verified after the rewrite: deleting `detach`'s
`first_child` fixup still fails the harness, and removing the cycle check from
`check_insertable` is still caught.

## Miri, which §4.5 asked for and this crate had never run

build-spec §4.5: *"Miri on `px-dom`, `px-ipc`, `px-store` unit tests."* It had
never run, and `px-dom`'s CLAUDE.md claimed since Phase 0 that it did.

Deferred twice for reasons that were right at the time and stopped being right
during this phase. Phase 0: *"each covers a crate that is currently an empty
skeleton."* Phase 2, about px-ipc: *"`px-ipc` has no `unsafe` and no FFI, so
what Miri would add today is UB detection in `std` calls."*

`px-dom` is now the case neither of those describes. It is
`forbid(unsafe_code)`, so Miri finds nothing in its own code — but ADR 017
brought in a closure whose unsafe its tests **execute**: `parking_lot` 539
tokens, `tendril` 129, `smallvec` 75, `string_cache` 19. The unsafe audit
counts them; nothing ran them.

ADR 022, on the nightly ADR 006 pins, by the mechanism ADR 011 established for
the sanitizers — the root toolchain file untouched, so `gate-fuzz-smoke.sh`'s
assertion that nothing outside `fuzz/` resolves to nightly keeps holding.

### It found something on the first run

`tendril` does **integer-to-pointer casts** — it packs a tag bit into a pointer
and casts back — so Miri reports that it *"might miss pointer bugs in this
program."* Legitimate technique, specific consequence: **Miri's provenance
tracking is weakened for the crate that holds every string in the DOM.** A
green Miri run over `px-dom` says less about `tendril` than about anything else
in the closure.

`-Zmiri-strict-provenance` would make that an error and `tendril` would fail on
the first parse, which is not a defect in `tendril`. So the flag is not set,
and the limitation is written into the ADR and the crate's CLAUDE.md rather
than left for "Miri is green" to paper over.

### What it does not cover

| suite | Miri time | run |
|---|---:|---|
| `parse` (minus three) | 8.0 s | yes |
| `ranges` | 7.2 s | yes |
| `handles` | 6.9 s | yes |
| `snapshots` | 6.4 s | yes |
| `order` | 3.4 s | yes |
| `opaque` | 182 s | no |
| `depth`, `layout`, `harness`, `html5lib`, `mutation` | minutes to hours | no |

Miri interprets at roughly a thousandfold slowdown, so the deep-nesting,
generation-exhaustion and conformance paths are out of reach — and those are
where this crate's own logic is most intricate. What is covered is the
dependency unsafe, which is the part nothing else checks and the reason §4.5
names this crate.

Verified in both directions: a failing test in a covered suite turns the gate
red, and so does a suite disappearing. The second one was worth checking — the
first version reported a deleted file as *"miri found undefined behaviour in
snapshots"*, which is untrue and sends somebody looking in the wrong place.

## The one research item not taken

`stylo-requirements.md` §4 item 2 — the borrowed `StyleView` / `StyleNode`
type — says *"build it in Phase 4 even though nothing consumes it until Phase
5."* It was not built. ADR 021 records why.

The note gives one reason for the Phase 4 timing and states it as the whole
reason: *"its whole value is that it forces the arena and slot layout decisions
early."* Those decisions are made and recorded without it — stable addresses
(ADR 020, decided precisely on §3.3's argument about `StyleNode<'dom>` holding
`&'dom Slot`, at a measured 25% traversal cost), `NodeId` layout (ADR 018), and
the opaque packing (§3.5). The forcing function worked; the artefact it was
meant to work through is not needed for it to have worked.

Against that, the note's own evidence: §3.3 is marked `[I]`, inferred, not
`[V]`; and *"24 published versions in ~28 months, with breaking trait changes
in most … `dom.rs` changed as recently as 2026-06-30 in a way that added a
supertrait."* Writing an unconsumed type against an inferred shape of a
churning trait, with no compiler able to say whether it is right, most likely
means writing it twice.

It is a close call and the ADR says so. The tripwire is explicit: if Phase 5
has to change slot layout, `NodeId`, or the packing in order to write
`StyleNode`, this decision was wrong and that is the first thing to say.

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
