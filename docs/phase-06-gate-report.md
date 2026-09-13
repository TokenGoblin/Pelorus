# Phase 6 gate report

*Block and inline layout. Branch `phase/06-layout`.*

## The three gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| WPT CSS2 reftest subset at threshold | `crates/px-layout/tests/reftest.rs`, 105 pairs, threshold pinned at 54 | **Pass** — and read "What 54 of 105 means" below before the tick |
| identical box tree on repeat runs | `crates/px-layout/tests/determinism.rs`, 4 tests | **Pass** |
| iterative (non-recursive) tree walks verified by a deep-nesting test | `crates/px-layout/tests/depth.rs`, 4 tests, plus 28 WPT crashtests in `crashtest.rs` | **Pass** |

`ci/gate-layout.sh` passes on Windows and Linux. **25 of 26 CI jobs are green; the
only red is `compat-list`**, which has been red since Phase 0 and is the user's
forty-site list. The `layout` job's `continue-on-error` was set in the phase's
first commit and removed in its last.

One run in this phase was **26 of 26 minus two**: `sanitizers (windows-latest)`
failed on `sandbox_policy_repeated_spawns_do_not_leak_handles`, in `px-sandbox`,
on a branch that does not touch it, and passed again on the next commit. It is
Phase 2's test and it is already in `docs/backlog.md` — the assertion reports
"leaks roughly 0 per launch" while failing, which is the interesting part. Recorded
here because an intermittent red that somebody has to recognise each time is worse
than a permanent one, and because "the only red is `compat-list`" would otherwise
be a slightly cleaner claim than the truth.

A second caveat on what "25 of 26 green" is worth: **Phase 3's `network` job still
carries `continue-on-error: true`** and has been green for three phases. The
workflow's own comment on the `dom` job states the principle — a job that is
allowed to fail and does not is not protecting anything. It is Phase 3's line to
delete rather than Phase 6's, so it stays in `docs/backlog.md`.

Not found here, though this phase rediscovered it from scratch while removing the
same line from its own job, and filed a duplicate entry before noticing Phase 5
had already written one. Both phases did the right thing and the defect is still
there, which is the more useful observation than either filing: a defect belonging
to a closed phase has nobody to fix it, and "whenever somebody is in that file
anyway" has now failed twice.

One hundred and five tests, which is a coincidence with the corpus size and not a
correspondence: 92 in the crate, 4 determinism, 4 depth, 3 reftest, 2 corpus.
`#![forbid(unsafe_code)]`, no `f32` or `f64` anywhere in the crate, and every
geometric walk iterative.

## What 54 of 105 means, and what it does not

The threshold is a **pinned count that may not fall**, not an externally given
number — build-spec §9 says "at threshold" without one, so the ratchet is the
threshold. It started at 25, was corrected down to 7, rose to 54, fell to 52 and
returned to 54. Every move has its evidence in `MATCH_FLOOR`'s own doc comment,
because a number that can go down is a number somebody has to be able to audit.

**The 51 that do not match are not 51 layout bugs, and the harness says so.**

| Category | Count | Why |
|---|---|---|
| Need a script host | 12 | The test mutates its own DOM on load. Laying out the file as written and comparing it to a reference showing the result is the wrong input, not a wrong answer. Phase 10. |
| Comparison-method exclusions | 3 | `EXPECTED_FAILURES`, each read rectangle by rectangle. |
| Everything else | 36 | Features: tables, multi-column, `vertical-align` (needs baselines, Phase 9), paint order (Phase 8), `direction: rtl` (Phase 9), `inline-block`, inline boxes. |

The scripted category is **detected, not listed** — the harness asks the parsed
document for a `<script>` element or an `on*` attribute. A hand-written list would
go stale as the corpus is re-vendored and would be somewhere to park a real
failure. It found two that the grep which prompted it had missed.

None of these shrink the denominator. ADR 019's pattern is that a conformance
figure must not be met by removing tests, and "we cannot run this one" is exactly
the argument that would remove it.

## The oracle has a shape it cannot see, and Phase 6 measured it

ADR 029 runs the reftests by comparing box geometry, and named two kinds of pair
the method would get wrong. The second — "a reference that reaches the same
rendering through deliberately different geometry" — turned out to have a shape,
and the shape is common.

A CSS 2.1 reference is very often *one green box of the right size*. Where the
test builds its rendering out of a parent, a child and a footer that happen to be
contiguous, the reference draws one box covering all three. Every rectangle the
reference draws is present in the test, and the test draws more. **The engine can
be exactly right and the comparison still fails.**

For out-of-flow features it is unavoidable rather than stylistic: a reftest for
`float` or `position: absolute` is written by putting the same box *in* flow in
the reference, so the reference's ancestors contain it and the test's do not.

This is not theoretical. Two concrete cases:

- **`absolute-non-replaced-min-max-001`** — the green square the test is about
  lands at `(8, 51, 16, 16)` on both sides, `min-width` beating `max-width` and
  all. The test's `body` is 19 tall and the reference's is 51, because one has the
  square in flow and the other does not.
- **`margin-collapse-min-height-001`** — five of the reference's rectangles are
  present and identical; the three extra are the test's parent, child and footer,
  contiguous at `(8, 51, 100, 100)` then `(8, 151, 100, 50)` against the
  reference's single `(8, 51, 100, 150)`.

**Absolute positioning cost three matching pairs and gained none.** That is the
clearest statement of the limit available: a correct implementation of a named
Phase 6 feature moved the score down. The harness marks the shape `SUPERSET` in
its report now, eight failures carry it, and ADR 029 has a new section saying that
if the mark keeps climbing while the engine gets more correct, the decision is the
thing that is wrong.

## Two corrections to the instrument, both worth exactly two pairs

The first was ADR 029's own argument applied a third time. It said to compare
flattened geometry rather than tree shape, "because a test and its reference
deliberately differ in structure". Depth was removed on that argument, then order.
**Multiplicity is tree shape too**: two boxes at the same rectangle and one box at
that rectangle render identically, and what the count carries is how many levels
of nesting came to rest in the same place. Margin collapsing made it concrete — a
parent with no border or padding ends up on exactly its child's rectangle.

The second was a conflation. `TRIVIAL_FRAGMENTS` — "did the engine lay anything
out" — was reading the same deduplicated vector as the comparison, so a layout
with four boxes at three distinct rectangles was being rejected as empty. Those
are two questions and `layout_file` returns two numbers now.

Net 52 → 54. **The number is the check on the reasoning.** Both pairs were
identified and read before the instrument was touched; a change to the oracle that
bought a dozen would have been evidence of bending it to fit rather than
correcting it.

## What this phase built

`Au`, a flat fragment tree, block layout, inline layout, floats, absolute
positioning, and a stub text metric with its limits written down.

- **Box generation (§9.2.1.1)** — a container's content is partitioned into inline
  runs and block-level children *in document order*, with anonymous block boxes
  generated only where the two are mixed. Floats and absolutely positioned boxes
  are block-level and out of flow, so they neither end a run nor force a wrapper.
- **Block layout** — §10.3.3 widths, §10.6.3 heights, §8.5.1's border rule,
  auto-margin centring, and margin collapsing between siblings, through a parent
  and through a self-collapsing box.
- **Inline layout** — greedy line breaking at ASCII spaces, §16.6.1 trailing-space
  removal, §9.4.2 overflow of an unbreakable word, and line boxes shortened
  against floats.
- **Floats (§9.5)** — `crates/px-layout/src/float.rs`: band queries, placement
  against earlier floats, shrink-to-fit from a new intrinsic-sizing pass, `clear`,
  and §10.6.7 containment by formatting context roots.
- **Absolute positioning (§9.6, §10.1, §10.3.7)** — including the static position,
  in a pass after the tree has coordinates.
- **`min-width`, `max-width`, `min-height`, `max-height`** (§10.4, §10.7).

Three parts of `display` that are easy to get wrong and were: `display: none`
generates no box **and no content**, an unimplemented formatting context generates
no box but its content still belongs to the run around it, and `display: flow-root`
is an ordinary block container that happens to establish a formatting context.

## Five defects the gate found that reading would not have

**`display: none` was being flattened through.** The partition walks through
non-block elements to collect their text, and `display: none` is not block-level,
so the user-agent stylesheet's `title { display: none }` put every document's
`<title>` into an anonymous block at the top of the page. Every file in the corpus
has a title and a test's is never its reference's, so no pair could agree about
its first line whatever the engine did below it. Four lines, and worth **44 of the
45 pairs that commit gained** — anonymous block generation, which was the point of
the commit, was worth the other one.

**`display: flow-root` was being discarded whole.** `is_block_level` compared
against `Display::Block`, so a `<div style="width: 150px; display: flow-root">` —
which is how the corpus wraps its float fixtures — generated no box at all and the
floats inside it were placed against the body's 784px. Reading `display`'s outside
and inside halves separately is the fix, and it is also where
`establishes_formatting_context` gets its predicate.

**Inline layout ran on the way down and had to run on the way up.** A line box
shortens to the space a float leaves it, and a float's size is not resolved until
its own exit visit — which happens *after* its container's children are
scheduled. Built on the way down, a line box is measured against a container that
has not yet met its own floats.

**The static position was one collapsed margin too high.** An absolutely
positioned box with no insets takes the position it would have had in flow, and
the margin waiting from the previous sibling is part of that. A reference put its
square at 51 and the test put it at 35; the difference was a paragraph's 16px
bottom margin.

**`min-height` does less to margin collapsing than it looks like it does.** CSS 2.2
§8.3.1 makes a non-zero `min-height` block the bottom margin from collapsing out
only when the child's bottom margin would *also* reach the box's top margin. What
produces the rendering `margin-collapse-min-height-001` asserts is §10.7's re-run
rule: applying `min-height` means using it as the computed value for `height`, and
a box whose height is no longer `auto` fails §8.3.1's first condition. A 30px child
with a 550px bottom margin in a `min-height: 100px` parent puts the next sibling at
100 — not at 580, not at 650. **This was read from the spec after the engine and a
reftest disagreed, rather than inferred from the reference**, which is the working
agreement's rule and the reason the answer is right.

## Tests that passed for the wrong reason, twice, in the same way

Through-collapsing moves a **parent** onto its child's rectangle and leaves the
child where it was. So `blocks.contains(child_rect)` is true whether or not the
parent collapsed, and three margin tests asserted exactly that. Removing the
feature failed only one of them.

The same trap caught a `min-height` test a commit later. Both sets count
rectangles now, and `count_rect` carries the story so the next one is written
correctly.

**Every behavioural test in this phase was verified by breaking the thing it
tests.** Laying floats in flow fails six; ignoring `clear` fails one; making a
`flow-root` stop containing its floats fails one; treating an absolutely
positioned box as in-flow fails seven; never changing the containing block fails
one; applying `min` before `max` fails one; nothing self-collapsing fails one;
reading the unclamped cursor instead of the used height fails seventeen.

## The gate caught me trying to get past it

The reftest diagnostic — the tool that says which pair to look at — was added as
an `#[ignore]`d test, and `ci/gate-layout.sh` refused it: *gate item
'css2-reftests' has 1 #[ignore]d test(s)*. The check matches on the
`layout_reftest_` name prefix, so renaming the function would have made it pass
while leaving an ignored test sitting in the gate's own file — which is the thing
the check exists to notice.

It is a plain function the gate test calls when asked now, and there is nothing
left to ignore.

## Live web, which is not a gate item and is the most convincing evidence

`cargo run -p px-broker --features testing --bin px-fetch -- <url> --find <text>`
fetches a real URL through `px-net`, parses it, applies its stylesheets, lays it
out and searches the result.

`https://rust-lang.org` — 242 elements, 640 nodes, **0 parse errors**, 200 styled
from 4 stylesheets, 217 fragments 7 deep, and `--find "performance"` returns the
three occurrences a reader would see. `https://example.com` likewise.

This is behind `required-features = ["testing"]` and is a diagnostic, not a
product surface. Nothing is distributed before Phase 20.

## Carried out of this phase

In `docs/backlog.md`:

- **`position: relative`'s offsets (§9.4.3).** The containing-block half is in —
  which is most of why anyone writes it. The offsets are two lines for a block box
  and not implementable for an inline one until inline boxes exist, and shipping
  the block half alone made a reference whose `<div>` moved disagree with a test
  whose `<span>` could not.
- **Absolute positioning's remaining gaps** — `height: auto` resolved from `top`
  and `bottom` together, auto margins absorbing the slack, and the static
  position's inline axis.
- Wikipedia returns **403** because `px-net` sends no `User-Agent`. Phase 19 owns
  the decision and Phase 23's gate will discover it otherwise.

Not yet written down as backlog because they are features rather than defects:
inline boxes (a `<span>`'s own borders and padding, and `display: inline-block`),
tables, and `vertical-align`, which needs the font metrics Phase 9 brings.

## 28 crashtests this phase nearly missed entirely

`ci/vendor-css2-reftests.sh` pairs a test with a `-ref.html` of the same name.
A **crashtest** has no reference -- it passes if the engine does not crash on it --
so every crashtest in the seven vendored directories was silently skipped for the
whole phase. There are 28 of them, and they are the inputs that made Chrome or
Firefox crash, for exactly the features Phase 6 implements.

They were found by surveying Phase 7's corpus, not this one. `css-flexbox` and
`css-grid` have 14 between them, which prompted the obvious question about the
directories already vendored.

They are worth more than a count. For a crate that is `#![forbid(unsafe_code)]`,
"does not crash" has exactly three meanings and `crashtest.rs` checks all three:

- **A panic** — caught by the worker thread's `join` returning `Err`.
- **A stack overflow** — each file runs on the same 256 KB stack `depth.rs` uses,
  so a recursive walk reached by a real document fails here as well as there.
- **Non-termination** — float placement and line breaking both step through
  positions until a condition holds, and both have an *argument* for why they
  terminate. An argument is not a test. A hung worker cannot be killed from Rust,
  so a timeout reports the filename and ends the process rather than hanging until
  CI's own deadline loses it.

All 28 pass, and all 28 produce boxes, which is asserted — a crashtest that lays
out nothing cannot crash, so a run where every file was empty would pass and mean
nothing. The harness was verified by injecting a panic (reports the file, fails)
and an infinite loop (reports the file, exits).

Pinned in `ci/gate-layout.sh` by a count **read from the git index**, which is the
Phase 4 lesson: that phase lost five commits to a fuzz corpus that was an empty
directory git cannot store and `git status` does not report. The check earned its
keep immediately — it failed on first run because the files were on disk and not
yet staged.

This is not a fourth gate item. It is a robustness floor under the third one, which
until now was a single hand-written document.

## The `.xht` question ADR 029 left open, now closed

ADR 029 held the subset to `.html` files -- 105 pairs against a possible 1,400 --
because whether html5ever's HTML parser produces the tree an XML parser would was
an empirical question it declined to guess at. Thirty `.xht` files were sampled
across the six directories at the pinned commit, and the answer is **no**, for a
reason that is not the expected one.

There are **no self-closing non-void elements in the sample at all**. `<div/>`
closing in XML and not in HTML is the divergence everyone reaches for, and every
`/>` in these files is on a void element where the two parsers agree.

What there is, in fifteen of thirty, is a bare `<![CDATA[` immediately inside
`<style type="text/css">` -- not the comment-wrapped `/*<![CDATA[*/` idiom that
survives both parsers. HTML parses `<style>` content as raw text, so the markers
reach the CSS parser as stylesheet text. Measured with px-dom and px-css rather
than reasoned about:

```
with CDATA: stylesheet text begins "<![CDATA[
div { height: "
with CDATA: [(800, 0), (800, 0), (800, 0), (800, 0), (800, 0)]
without:    [(800, 250), (800, 250), (800, 250), (200, 200), (800, 50)]
```

**The whole stylesheet is discarded, not the first rule.** A `div` that asked for
200x200 comes out 800x0, along with everything else.

That is worse than a corpus that fails, because it is one that does not: a test
and its reference stripped of their stylesheets are both the user-agent skeleton,
and two skeletons agree. Adding ten thousand files this way would move the
conformance number without measuring anything -- the same agreement-in-brokenness
that made this subset's own count fall from 25 to 7 once boxes were being
generated properly.

A vendoring-time transform stripping the markers from `<style>` contents would
work and is mechanical and auditable, but it edits vendored test files. That is a
decision for its own ADR rather than a line in the fetch script.
