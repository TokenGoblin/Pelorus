# Phase 4 gate report

*DOM. Branch `phase/04-dom`.*

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| html5lib-tests ≥99% | `crates/px-dom/tests/html5lib.rs`, 62 vendored `.dat` files, 1,952 cases | **Pass at 99.43%**, with two causes set aside — ADR 019 |
| Stale-handle fuzz target proves every stale lookup returns `None` | `dom_stale_handle`, plus `dom_stale_*` ×5 | **Pass** |
| 24h mutation fuzz clean | `fuzz-campaign` run [34483936230](https://github.com/TokenGoblin/Pelorus/actions/runs/34483936230) — 19 shards, `dom_mutation` given six of them for 24 CPU-hours of that target | **Pass**, clean, `-max_len` verified |
| 100,000-level nesting without stack overflow | `dom_depth_*` ×6, on a 256 KB stack | **Pass**, and it found a hang |

Ranges are named in the phase but in none of the four gate items. They were
missing, are now built, and are gated anyway — see below.

`ci/gate-dom.sh` passes on Windows and Linux, locally and in CI. The `dom` CI
job is now required — `continue-on-error` removed, since a job allowed to fail
that does not is no longer telling anybody anything.

**An earlier draft of this line said the job was green when it was red**, and
what made that possible is worth reading before Phase 5 — see *The green that
was not green* below.

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

## The campaign, which closes the last gate item

Run [34483936230](https://github.com/TokenGoblin/Pelorus/actions/runs/34483936230):
**19 shards, `-max_total_time=14400` each, 28.2 billion executions, no crashes
and no artifacts written.** Four hours of wall clock, 76 CPU-hours, and
`dom_mutation` holds six of those shards — **24 CPU-hours of the target the
gate item names**, which is what the previous run could not say.

| Target | Shard | Runs | cov | ft | corpus | lim | exec/s |
|---|---:|---:|---:|---:|---|---:|---:|
| `dom_mutation` | 1 | 1,653,401 | 870 | 6,093 | 820 / 205 KB | 4,096 | 114 |
| `dom_mutation` | 2 | 1,983,351 | 870 | 6,113 | 830 / 217 KB | 4,096 | 137 |
| `dom_mutation` | 3 | 2,126,293 | 865 | 6,093 | 866 / 200 KB | 4,096 | 147 |
| `dom_mutation` | 4 | 2,093,659 | 870 | 6,113 | 857 / 238 KB | 4,096 | 145 |
| `dom_mutation` | 5 | 1,657,658 | 870 | 6,141 | 856 / 222 KB | 4,096 | 115 |
| `dom_mutation` | 6 | 1,603,241 | 870 | 6,113 | 867 / 205 KB | 4,096 | 111 |
| `dom_stale_handle` | 1 | 3,765,973 | 441 | 2,936 | 405 / 45 KB | 4,096 | 261 |
| `dom_stale_handle` | 2 | 3,902,787 | 441 | 2,936 | 423 / 44 KB | 4,096 | 271 |
| `dom_parse` | 1 | 397,678 | 4,472 | 28,448 | 3,693 / 51 MB | 500,000 | 27 |
| `dom_parse` | 2 | 320,293 | 4,438 | 27,509 | 3,498 / 40 MB | 500,000 | 22 |
| `dom_parse` | 3 | 130,038 | 4,241 | 24,654 | 2,718 / 24 MB | 500,000 | 9 |
| `frame_request` | 1 | 10,951,797,106 | 170 | 208 | 92 / 1,155 b | 1,100,000 | 760,488 |
| `frame_response` | 1 | 6,253,695,283 | 152 | 190 | 83 / 1,038 b | 1,100,000 | 434,254 |
| `channel_stream` | 1 | 2,439,981,073 | 178 | 655 | 242 / 1,553 KB | 1,100,000 | 169,431 |
| `channel_stream` | 2 | 1,950,043,208 | 178 | 698 | 256 / 1,138 KB | 1,100,000 | 135,410 |
| `broker_sequence` | 1 | 3,188,215 | 512 | 3,344 | 1,005 / 233 KB | 19,064 | 221 |
| `http_response` | 1 | 1,163,433,348 | 411 | 1,541 | 590 / 97 KB | 8,500,000 | 80,788 |
| `http_response` | 2 | 949,740,224 | 413 | 1,563 | 585 / 108 KB | 8,500,000 | 65,949 |
| `http_chunked` | 1 | 4,431,836,714 | 127 | 471 | 213 / 8,483 b | 8,500,000 | 307,745 |

`dom_parse` is the shard to read first. It reaches **4,472 coverage points and
28,448 features** — five times any other target here, and roughly nine times
`dom_mutation` — because it is the only one that sends HTML through html5ever
and the `TreeSink`, which is the surface a page actually reaches. Nine to
twenty-seven executions per second, against a 51 MB corpus of large documents.
The previous campaign covered none of this.

The six `dom_mutation` shards agree with each other to within five coverage
points (865–870) and 48 features. Independent seeds converging on the same
plateau is the useful reading: the target's reachable state space is being
saturated rather than sampled, and a seventh shard would be unlikely to say
anything new.

Execution rates in the hundreds are the harnesses working as designed — they
assert every structural invariant after every operation. The rate is the price
of the precision that turned three range defects into two-second reproductions.

### `-max_len` was verified applied, not assumed

The check Phase 1 taught, and this time it needed more than reading the final
line.

- The flag appears on all 19 `Running` command lines.
- `-max_len is not provided` appears **zero** times in 104,368 log lines.
- libFuzzer's own `lim:` field is present on every progress line and on all 19
  `DONE` lines.

**The third check needed interpreting rather than pattern-matching.** For
`dom_parse` the final `lim:` reads 500,000 against a configured 1,200,000, and
for `broker_sequence` 19,064 against 1,100,000 — which looks exactly like the
flag being ignored. It is not. libFuzzer's `-len_control` ramps the mutation
ceiling up from the largest seed rather than starting at `-max_len`:
`dom_mutation` begins at `lim: 66`, which is its largest seed to the byte, and
climbs. Those two targets simply never needed the whole ceiling.

For the target the gate item is about, the ramp completes and the evidence is
unambiguous. **All six `dom_mutation` shards reach `lim: 4096` — 43 to 70
minutes in — and hold it to the final line**, 3,352 log lines at the ceiling:

| shard | fuzzing began | ceiling reached | lines at ceiling | final |
|---|---|---|---:|---|
| 1 | 13:42:34 | 14:52:45 | 432 | `lim: 4096` |
| 2 | 13:40:30 | 14:37:50 | 661 | `lim: 4096` |
| 3 | 13:49:13 | 14:51:15 | 737 | `lim: 4096` |
| 4 | 13:41:26 | 14:31:27 | 746 | `lim: 4096` |
| 5 | 13:42:21 | 14:37:49 | 397 | `lim: 4096` |
| 6 | 13:42:34 | 14:25:09 | 379 | `lim: 4096` |

A flag on a command line proves it was passed; `lim:` reaching the configured
value and staying there proves libFuzzer acted on it.

**The 1.1 MB nesting seed survived loading**, which is the thing `dom_parse`'s
larger ceiling exists to protect. Its startup line reads `seed corpus: files:
12 min: 33b max: 1100000b` — the 100,000-level seed §4.4 asks for, loaded
whole. Had `-max_len` been left at the 1,100,000 default the seed would have
sat exactly at the boundary; at 1,200,000 it does not, and no truncation
warning appears anywhere in the run.

### What the previous run could not say, and this one can

The run recorded in earlier drafts of this report — 11 shards, 30.7 billion
executions, clean — was real coverage of the arena targets and is superseded
rather than contradicted. It failed the gate item on two counts, both known
before it finished:

- **It predated `dom_parse`.** Launched at `a599761`, before that target
  existed. Not one byte of HTML reached html5ever. Now three shards do.
- **`dom_mutation` had eight CPU-hours, not twenty-four.** Two shards at four
  hours, with the campaign's 44-hour total standing in for a per-target item.
  Counting a campaign total against a per-target item is how a gate gets
  satisfied on paper. Now the target itself has its twenty-four.

Three earlier campaigns were cancelled before either of these, each because
bugs were still being found in the code under test. That is the lesson worth
carrying to Phase 5: **finish the code, then start the four-hour job.** A
campaign launched against moving code measures nothing except how recently you
edited it.

## The green that was not green

`every_committed_corpus_seed_replays_clean` was added in `f504c2d` to run the
committed corpus on every push. It passed on this machine and failed in CI from
the moment it landed, and the `dom` job was red for five commits — through the
README update, the docs commit and the overnight log, each of which stated the
job was green.

**The cause is that an empty directory is invisible to git.** Git cannot store
one, and `git status` does not report it, so `fuzz/corpus/dom_stale_handle`
existed on the machine that ran the gate and existed nowhere else. The test
reads the directory with `read_dir`, found it, iterated zero files, and passed.
In CI the directory was simply absent and the same test panicked with *the
dom_stale_handle corpus is missing*. Nothing was ignored and no output was
misread: the two machines were running against different trees, and only one of
them was the repository.

It also failed `sandbox` on both platforms, because that job runs the workspace
suite. One missing directory, four red jobs, and a summary line that read *21
of 22 green* because it was written from the last run anybody had looked at.

**Two things are fixed, and only the first is about the corpus.**

`dom_stale_handle` now has seven committed seeds, hand-written the way
`dom_parse`'s are rather than harvested, because this target has never crashed
and so has no ADR 006 reproducers to commit. Between them they drive all five
branches of the harness — create, remove, detach, move, and ADR 018's forced
generation exhaustion, which ordinary churn cannot reach — plus operations
against an empty tree and a trailing op byte with no selector after it. The
count is pinned at 25 across the three DOM targets for the same reason ADR 019
pins its exclusions.

`ci/gate-structure.sh` now enumerates the fuzz targets from `fuzz/Cargo.toml`
and asserts each has at least one corpus file **in the git index**, which is
the only view of the tree that is identical on every machine. Reading the
filesystem is what hid this; a check that reads the filesystem would hide it
again. `http_response` and `http_chunked` are named in that gate's
`corpus_empty` list — they genuinely have no seeds, which is a Phase 3 gap now
in the backlog — and the gate fails if the list and the index disagree in
either direction, so the exception cannot quietly grow.

**This is the fourth time this project has recorded a check that passed locally
and failed in CI**, after Phase 1's product binary exiting failure behind a
green gate, Phase 1's campaign against an out-of-reach `-max_len`, and Phase
3's HTTP targets fuzzed inside their own limits. The shape is constant: the
check was real, and the thing it read was not what CI would read. Worth stating
plainly because the previous three were each treated as a one-off.

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

In CI the whole job takes **122 seconds**, against 50 for the `dom` gate on the
same runner — measured on the first run, so ADR 022's "wrong if the CI cost is
more than the table says" is a checked fact rather than an open question.

Verified in both directions: a failing test in a covered suite turns the gate
red, and so does a suite disappearing. The second one was worth checking — the
first version reported a deleted file as *"miri found undefined behaviour in
snapshots"*, which is untrue and sends somebody looking in the wrong place.

## What the mutation harness found once it covered all three surfaces

`px-dom` has three mutation surfaces. The harness exercised one.

Tree structure was fuzzed; **live ranges and attribute writes were not** — and
both are mutation-path features built during this phase. A range is updated by
every insertion and removal; an attribute write records a prior-state
snapshot. Fuzzing the tree and calling that "mutation fuzz" is the same shape
of gap as fuzzing the arena and calling it parser coverage.

Teaching it all three found **four defects in the first second**, none of which
the 13 hand-written range tests or the 12 snapshot tests had reached.

**The document node could be removed.** `remove_subtree(document)` succeeded,
freed every node, and left `document()` handing out a stale handle — and
`validate()` still returned `Ok(())`. The arena's most basic invariant, broken,
with the validator reporting healthy.

**The document node could be given a parent.** `append_child(orphan, document)`
succeeded. Same silence from `validate()`.

**A text node could have children.** Nothing stopped `append_child(text, x)`.
That matters beyond tidiness: a boundary point's offset means *children* for a
node that has them and *bytes* for character data, so a text node with a child
list has two incompatible notions of its own length and every range pointing
into it is nonsense. This is the DOM's `HierarchyRequestError`, and it is now
`TreeError::CannotHaveChildren`.

**`precedes` contradicted itself.** It walked from the document and returned on
the first node found — so with `a` detached and `b` attached, it answered "a
does not precede b", *and* answered "b precedes a". `compare_boundary_points`
inverts the mirrored answer, so an inconsistent `precedes` produced ranges that
compared as following themselves. Nodes outside the document tree are not
comparable in document order, and `None` is that answer.

Plus one behaviour that turned out to be correct and undocumented: a removed
element **keeps** its snapshot, because a restyle still needs to know it
changed. Safe here only because the key is a generational `NodeId` — with
Servo's pointer keys it would be the bug rather than the design. Now written
down where it looks like a leak.

### A question that was open twice, and had three answers

The harness produced a range whose start compared as *following its own end*.
Whether that was a defect here or inherent to `(node, offset)` boundary points
was not established, so it went in as an `#[ignore]`d test with the
reproducer, per `/CLAUDE.md`'s rule for an ambiguity.

Then it was settled the way that test said to settle it — delta-reduce and read
each step. **34 operations to 16, and the answer was a defect.** Two of them:

- `compare_boundary_points` ended in a bare `Some(Before)`, justified by an
  earlier step having mirrored the call — sound only when `precedes` gives an
  answer. When it returns `None`, the fall-through turns *"I cannot order
  these"* into *"a comes first"*.
- `precedes` walked from `document()` alone, so every pair inside a detached
  subtree was incomparable, and the fall-through fired constantly.

Both fixed, the assertion restored, the ignored test graduated to a regression
test. **And that was premature.** The next campaign found the same assertion
failing in minutes, on a 25-byte input, from all six `dom_mutation` shards.

The third defect was in the *constructor*, not the comparison. `new_range`
refused a pair that compared as `After` — but two boundary points in different
trees do not compare at all, and `None` is not `Some(After)`, so the pair was
accepted. The range is well-formed right until the two trees are joined, at
which point it is inverted and **nothing moved either endpoint**. Eight
operations: create a detached comment, build a range from it to the document,
append the comment to the document.

That is why it kept reading as a mutation-rules problem for two rounds. The
DOM never holds such a range either — `setStart` and `setEnd` collapse when
handed a node in a different tree — so `new_range` now requires the ordering to
be *establishable*, not merely "not backwards yet".

All six crash inputs are committed to `fuzz/corpus/dom_mutation/`, which is
what ADR 006 asks for and what makes the 60-second smoke run meaningful.

**The lesson recorded rather than the conclusion.** Twice I called this
answered on the evidence available and twice the next run disagreed. The
harness's 600 pseudorandom sequences never reached any of the three; libFuzzer
found the last one in minutes with coverage feedback. A property that only a
guided fuzzer can falsify is one to stop reasoning about and start running.

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

**All four gate items pass.** The last one to close was the mutation
campaign, run 34483936230, whose numbers are above. Nothing in §9 Phase 4 is
outstanding — but the phase is done when the gate is green *in CI on both
operating systems*, and that had not been true for five commits. See *The green
that was not green*.

**Deferred by decision, with the reasoning recorded**

- The borrowed `StyleView` type — ADR 021, with an explicit tripwire for
  Phase 5.
- `whatwg/html#12118` upstream to html5ever; until then, 88 conformance cases
  are set aside by ADR 019 with their count pinned.
- Six conformance cases needing script execution, which Phase 11 inherits.

**Known gaps, still open**

- html5ever's quadratic tree builder: mitigated inside `px_dom::parse`, not
  fixed. Anything driving html5ever without going through it gets the old
  behaviour back with no warning.
- `string_cache`'s process-global interner is a cross-document timing channel
  until Phase 14 makes one-process-per-site real.
- `Arena::force_generation_to_last` has no release-artifact scan, because no
  shipping binary links `px-dom` yet and the scan could not fail.
- Miri covers five suites; the deep-nesting, generation-exhaustion and
  conformance suites are out of reach, and its guarantee over `tendril` is
  weakened by that crate's integer-to-pointer casts. `px-ipc` and `px-store`,
  which §4.5 also names, have no Miri job.
- `web_atoms` agreement with stylo cannot be checked until stylo is a
  dependency. Two versions in the tree would fail as a type error at the
  `TElement` boundary in the first hour of Phase 5.

**Fixed during the phase, listed because the report above claims it**

- Miri now runs on this crate. Its CLAUDE.md claimed so from Phase 0 while it
  did not; that claim is now true, and narrower than it sounds — the file says
  which suites and what is weakened.
