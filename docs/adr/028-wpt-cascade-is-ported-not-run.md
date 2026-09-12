# 028 — Phase 5's WPT gate item is met by ported assertions, not by running WPT

- **Status:** proposed
- **Date:** 2026-09-12
- **Phase:** 5
- **Invariants touched:** none. This reinterprets a §9 gate item, which is why it
  is proposed rather than taken.

## Context

§9 Phase 5's second gate item is *"WPT `css/css-cascade` subset passes"*.

**No test in that directory can run in Phase 5**, and the obstruction is
structural rather than a matter of effort. Every file there is one of two kinds:

- **reftests**, carrying `<link rel="match" href="reference/...">`. Passing one
  means rendering two documents and comparing the result. That needs layout
  (Phase 6), paint (Phase 8) and text shaping (Phase 9).
- **testharness.js tests**, which load `/resources/testharness.js` and assert
  with `assert_equals(getComputedStyle(el).opacity, "0.5")`. Passing one means
  executing JavaScript against DOM bindings — Phase 10 for the engine, Phase 11
  for the bindings.

Read rather than recalled. The directory listing was fetched and three files were
read in full: `all-prop-unset-color.html` is a reftest, `important-vs-inline-001.html`
is testharness, and `parsing/all-valid.html` — the most promising candidate for a
runnable subset, since parsing tests need no rendering — is **also** testharness. It
loads `parsing-testcommon.js` and calls `test_valid_value("all", "initial")`, which
round-trips through CSSOM. There is no third category and no subset that avoids
JavaScript.

This is the same shape as the `compat-list` gate item, which has been red since
Phase 0 because it depends on something outside the phase. The difference is that
`compat-list` announces its dependency and this one does not.

## Decision

**Proposed.** Phase 5 meets this item by **porting** a named subset of WPT
`css/css-cascade` tests into Rust fixtures — `crates/px-css/tests/cascade.rs` —
each of which:

1. asserts the same behaviour the WPT test asserts,
2. reads it from computed style through `TElement::borrow_data`, and
3. names the WPT file it came from in a doc comment.

Seven are ported, on two axes.

Five test the **cascade**: `!important` against inline style, `inherit` and
`initial`, `all: unset` followed by a later declaration, specificity ordering
across id/class/type with and without `!important`, and `@layer` ordering
including the unlayered-wins rule.

Two test the **parser**, from `parsing/all-valid.html` and `all-invalid.html`:
that `all` accepts exactly the CSS-wide keywords and nothing else, and that an
invalid declaration is discarded while its neighbours survive. The second is worth
having on its own — CSS Syntax §5 requires it, it is what makes a browser usable on
pages using features this engine has not implemented, and the tempting
implementation (fail the block) passes every test that only ever feeds it valid
CSS.

**The gate report must say "ported" and not "passes".** What is verified is the
cascade. What is not verified is WPT's harness, its edge cases, or the roughly
145 files not ported. A report that said "WPT css-cascade subset passes" would be
describing a test run that did not happen, and this project has already shipped
two gate items whose green meant less than it read as.

When JavaScript lands, these fixtures should be **replaced** by the real tests,
not kept beside them. A ported assertion that has drifted from its source still
looks like coverage.

## Alternatives rejected

**Declare the item blocked and move it to Phase 11.** Honest, and it was the
alternative I weighed longest. Rejected because it would leave the cascade with
no behavioural test at all for six phases, and the cascade is what this phase
exists to deliver — layout in Phase 6 would be built on computed values nothing
had checked. The ports cost an afternoon and catch real regressions now.

**Write a minimal `getComputedStyle` and a testharness shim.** Lets the real test
files be read rather than transcribed. Rejected: the shim still cannot execute
the `test()` bodies, which are JavaScript. Without an engine it would only handle
tests whose assertions are structurally trivial, and selecting for those would
bias the subset towards the tests that prove least.

**Extract assertions from the WPT files mechanically.** A parser that lifted
`assert_equals(getComputedStyle(...))` calls into generated Rust. Rejected as
the worst of both: it looks automatic, so nobody reviews the extraction, and it
breaks on any test that computes its expectation rather than writing it literally.
Transcribing seven tests by hand and naming the source is more honest than
generating fifty whose provenance nobody checked.

## Consequences

**The gate item's green is weaker than its wording.** Seven behaviours,
hand-ported, against a directory of ~150 files. `ci/gate-style.sh` cannot tell the difference —
it checks that `cascade_*` tests exist, are in the named file, and are not
ignored — so the honesty has to live in the report and in the test file's own
documentation. Both say it.

**A ported test can drift from its source silently.** WPT changes; a transcribed
assertion does not. Nothing detects that. The mitigation is the replacement rule
above, and it is a rule rather than a mechanism, which is weaker.

**Inline `style` had to be implemented to port one test**, and that was worth the
trade on its own: `important-vs-inline-001` is meaningless without it, and
`TElement::style_attribute` returning `None` was the largest honest gap in the
phase. The port forced the gap closed rather than letting it be recorded and
left.

## Verification

Wrong if some subset of `css/css-cascade` turns out to be runnable without
JavaScript or rendering. `parsing/` was the obvious candidate and has now been
checked — all six files there are testharness — so the claim is stronger than when
this ADR was drafted. What remains unchecked is whether a WPT runner could be
driven far enough by Phase 11's bindings to run these for real *before* layout
exists, which would let the cascade ports be replaced two phases earlier than the
reftests can be.

Wrong in the other direction if the ports turn out to pass while a real WPT run
later fails on the same behaviour, which would mean the transcription was wrong
rather than incomplete. That is detectable only when JavaScript lands, and it is
the reason the replacement rule says replace rather than augment.
