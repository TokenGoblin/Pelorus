# 029 — CSS2 reftests are run for real, and compared by box geometry

- **Status:** proposed
- **Date:** 2026-09-12
- **Phase:** 6
- **Invariants touched:** none. This decides how a §9 gate item is measured.

## Context

§9 Phase 6's first gate item is *"WPT CSS2 reftest subset at threshold"*.

A reftest is a pair of documents and an assertion that they **render
identically** — `<link rel="match" href="...-ref.html">`. Passing one the way a
browser does means rasterising both and comparing pixels, which needs paint
(Phase 8) and text shaping (Phase 9). Phase 6 has neither.

That is the same obstruction ADR 028 hit for Phase 5's WPT item, where the answer
was to port assertions by hand. **Here there is a better answer, and it is worth
taking the trouble to find it**, because the CSS2 corpus is large and porting
hundreds of layout tests by hand would be both enormous and exactly the kind of
transcription that drifts from its source.

## The observation this rests on

**A reftest does not assert an absolute rendering. It asserts an *equality*
between two documents** — and an equality is preserved by any consistent
transformation of both sides.

So the missing pieces cancel:

- There is no paint, but both documents would be painted the same way.
- There is no text shaping, so glyph advances come from `InitialFontMetrics`'s
  stub. But **both documents get the same stub**, so a test whose match depends on
  two text runs being the same width still matches, at the wrong width.

Verified on a real pair rather than argued. `normal-flow/block-in-inline-align-001.html`
puts text and a `<div>` inside a `<span>`, relying on anonymous block boxes; its
reference uses three explicit `<div>`s to produce the same boxes. Both use
`width: 20ch` and `width: 10ch`. `ch` resolves against a stub here and will be
wrong in absolute terms — and identically wrong on both sides, so the equality the
test asserts is intact.

## Decision

**Proposed.** Phase 6 runs the reftest files themselves — parsing them with
`px-dom`, styling them with `px-css`, laying them out — and compares **the
flattened list of box geometries in layout order**, not pixels and not tree
shape.

Flattened rather than structural, and that is the load-bearing choice. The pair
above has *different DOM structure by design*: the test's anonymous blocks are
what the reference spells as explicit `<div>`s. Comparing tree shape would fail
every test that exercises anonymous box generation, which is a large part of what
CSS2 normal-flow is about. Comparing the sequence of `(position, size)` in layout
order is what the reftest actually means.

**The subset is `.html` files only.** Measured: `css/CSS2` holds **10,501 `.xht`
files against 816 `.html`**, so this gives up most of the corpus. It is taken
anyway because `px-dom` has no XML path — it drives html5ever's HTML parser, and
`TDocument::is_html_document` returns `true` unconditionally. Parsing XHTML with
an HTML parser usually works and *usually* is not a standard; `/CLAUDE.md` says
not to infer behaviour from what another browser happens to accept. 324 of the
816 are reference files, which is far above the gate's floor of 40, and the
directories map onto this phase's scope — floats (106), normal-flow (92),
box-display (60), visuren (40), floats-clear (39), positioning (29), linebox (28).

## Alternatives rejected

**Port the assertions by hand, as ADR 028 did for Phase 5.** Right for seven
cascade tests; wrong for hundreds of layout tests. The transcription cost scales
with the corpus, and a ported layout assertion is much harder to get right than a
ported cascade one — the cascade port asserts "this property computed to that
value", while a layout port would have to assert geometry a human worked out by
hand, which is the thing the engine is supposed to be computing.

**Compare box trees structurally.** Simpler to implement and wrong, for the
reason above: anonymous box generation means a correct engine produces different
*trees* for a test and its reference while producing the same *geometry*. A check
that fails on correct behaviour trains people to ignore it.

**Wait for Phase 8 and run real reftests.** Honest, and it would leave layout with
no conformance signal for two phases while Phases 6 and 7 build on it. The same
argument ADR 028 rejected the equivalent option on, with more force here because
the corpus exists and is runnable.

**Include the `.xht` files by parsing them as HTML.** Would multiply the corpus by
thirteen. Rejected on `/CLAUDE.md`'s rule about inferring behaviour: whether
html5ever's HTML parser produces the tree an XML parser would, for these files, is
an empirical question about a parser this project did not write.

**Checked at the end of Phase 6, and the answer is no.** Thirty `.xht` files were
sampled across the six directories this subset uses, at the pinned commit. Two
things were looked for and only one of them was there:

- **Self-closing non-void elements — none.** `<div/>` closes in XML and does not in
  HTML, and that is the divergence everyone expects. Every `/>` in the sample is on
  a void element (`br`, `img`, `link`, `meta`), where the two parsers agree. So the
  expected problem is not the problem.
- **A bare `<![CDATA[` inside `<style>` — in half of them.** Fifteen of thirty, all
  the same shape: `<style type="text/css"><![CDATA[` … `]]></style>`, not the
  comment-wrapped `/*<![CDATA[*/` idiom that survives both parsers.

HTML parses `<style>` content as raw text, so the CDATA markers reach the CSS
parser as stylesheet text. Measured rather than reasoned about, with px-dom and
px-css on a two-rule stylesheet:

```
with CDATA: stylesheet text begins "<![CDATA[\ndiv { height: "
with CDATA: [(800, 0), (800, 0), (800, 0), (800, 0), (800, 0)]
without:    [(800, 250), (800, 250), (800, 250), (200, 200), (800, 50)]
```

**The whole stylesheet is lost, not the first rule.** CSS error recovery consumes
everything after the unexpected token, so a `div` that asked for 200×200 comes out
800×0 along with everything else.

That is worse than a corpus that fails, because it is a corpus that *does not
fail*: a test and its reference stripped of their stylesheets are both the
user-agent skeleton, and two skeletons agree. Adding 10,501 files this way would
move the conformance number without measuring anything — the same
agreement-in-brokenness that made this subset's own count fall from 25 to 7 once
the boxes were being generated properly.

Revisit if a real XML path arrives. A vendoring-time transform that strips the
CDATA markers from `<style>` contents is mechanical and auditable and would work,
but it edits vendored test files, which is a decision for its own ADR rather than
a line in the fetch script.

## Consequences

**Two classes of test will be wrong, and both need excluding by name.**

*False passes*: a reftest whose difference is purely paint — colour, borders,
backgrounds — has identical geometry on both sides and passes without the engine
doing anything. Those are not layout tests and do not belong in the subset.

*False failures*: a reftest whose reference achieves the same pixels through
genuinely different geometry — text that happens to fill a box, against a solid
block of the same size — fails a geometry comparison while being correct.

Both are exclusions, and exclusions get ADR 019's treatment: named individually,
with a reason, and with the count pinned so the set cannot grow quietly. **The
gate prints the graded and ungraded numbers**, as Phase 4's conformance gate does,
so a green tick and a lower raw number are reconcilable from the output rather
than only from this document.

**The absolute numbers are meaningless; only the equality is checked.** Every
length that depends on font metrics is wrong in this phase — `ch`, `ex`, and every
text run's width. A test that passes here proves the two documents agree, not that
either is correct. Phase 9 is when the absolute figures become meaningful, and
that is when a `-ref` comparison becomes a real reftest.

**The subset is biased towards newer tests.** The `.html` files are the ones added
since WPT moved away from `.xht`, which skews towards recently-specified
behaviour and away from the CSS2.1 core the `.xht` bulk covers.

## What Phase 6 found, and what it costs

This ADR named two kinds of pair the comparison method would get wrong: a
paint-only difference that passes without the engine doing anything, and a
reference that reaches the same rendering through deliberately different geometry.
The second turned out to have a **shape**, and the shape is common enough to be
worth writing down rather than meeting one pair at a time.

A CSS 2.1 reference is very often "one green box of the right size". Where the
test builds its rendering out of several boxes — a parent, a child and a footer
that happen to be contiguous — the reference draws one box covering all three. Every
rectangle the reference draws is then present in the test, and the test draws more.
The engine can be exactly right and the comparison still fails.

The same shape arises for every out-of-flow feature, and there it is unavoidable
rather than stylistic: a reftest for `float` or `position: absolute` is written by
putting the same box *in* flow in the reference, so that the reference's ancestors
contain it and the test's do not. Absolute positioning cost three matching pairs
and gained none for this reason, with the box under test matching exactly in each.

The reftest harness marks these `SUPERSET` in its report, which is a triage aid
rather than an exclusion: a test that draws boxes the reference does not may also
be a test whose engine drew a box it should not have. Eight of the subset's
failures carried the mark when Phase 6 closed its box-generation work; two are in
`EXPECTED_FAILURES` with the specific rectangles that agree written down, and the
rest are unread.

## Verification

Wrong if the exclusion list grows past a small fraction of the subset, which
would mean geometry comparison is not a good proxy for the reftest assertion
after all rather than that a few tests are unusual. The pinned count is what makes
that visible. **Three of 105 at the end of Phase 6.** The paragraph above is the
early warning: if the `SUPERSET` count keeps climbing while the engine is getting
more correct, this decision is the thing that is wrong.

Wrong if a test passes here and fails a real reftest run at Phase 8 for a reason
other than paint. That is the check this decision is ultimately betting on, and it
is two phases away.

~~Wrong in the cheapest way if html5ever turns out to parse the `.xht` files into
the same tree an XML parser would.~~ **Checked at the end of Phase 6 and settled:
it does not, and the reason is a bare `<![CDATA[` inside `<style>` rather than the
self-closing tags everyone expects.** See the rejected alternative above for the
measurement. The subset stays at 105 pairs.
