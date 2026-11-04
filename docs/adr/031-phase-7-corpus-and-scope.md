# 031 — Phase 7's corpus is chosen by `rel="match"` hrefs, and grid-lanes is out of scope

- **Status:** accepted (user delegated the recommendation, 2026-11-04)
- **Date:** 2026-11-04
- **Phase:** 7
- **Invariants touched:** none. Like ADR 019 and 028, this fixes what a §9 gate
  item means, so it is raised with the user rather than settled in a commit.

## Context

§9 gives Phase 7 one gate item: "WPT `css-flexbox` and `css-grid` subsets at
threshold", with no threshold and no subset. Measured in full
(`docs/research/phase-07-wpt-composition.md`), the two directories hold 880 and
1,648 reftests, 62 crashtests, and 1,033 testharness tests that cannot run before
Phase 10. Roughly a third of `css-grid` is `grid-lanes`, a separate and newer
module than the grid layout §9 means.

## Decision

1. **Pair references by reading `<link rel="match" href>`**, not by filename.
   Nearly half the hrefs leave the test's directory.
2. **Exclude `css-grid/grid-lanes/`** from the subset. It is a different module
   with different algorithms, and counting it would set the threshold by how much
   of an unrelated feature is implemented. Revisit after Phase 7.
3. **Drop reftests with a `<script>` or `on*` attribute**, detected by parsing as
   Phase 6's harness does, and report the count beside the graded figure.
4. **The threshold is a ratchet**, as in Phase 6: a pinned count that may not
   fall, with the ungraded total printed next to it. Starting value to be set from
   the first measured run, not guessed now.
5. **Crashtests are a hard gate, not a threshold**: all 62 must lay out without a
   panic, stack overflow or hang. Vendored.
6. Comparison is by box geometry (ADR 029). Its known blind spot, a reference that
   draws the same picture with fewer boxes, carries over.

## Alternatives rejected

- **Include grid-lanes.** Inflates the denominator with a feature nobody has built.
- **Pair on filename.** Cheaper, and silently loses ~45% of flexbox.
- **Wait for Phase 8 and compare pixels.** Leaves Phase 7 with no signal.

## Consequences

The graded number will look small against 2,300 candidates. That is the honest
denominator; shrinking it is the failure mode ADR 019 exists to prevent.

## Verification

Wrong if, once flex and grid layout exist, the geometry comparison cannot tell
a correct flex layout from an incorrect one for a large share of the subset (the
Phase 6 blind spot, but worse). The first measured run will show it.
