# 029 — px-layout reads a plain-Rust style description, and its WPT gate item is met by ported geometry assertions

- **Status:** proposed
- **Date:** 2026-11-04
- **Phase:** 6
- **Invariants touched:** none. Item 2 reinterprets a §9 gate item, so it is
  flagged to the user rather than folded into a commit, as ADR 028 did.

## Context

Two questions came up before any Phase 6 code was written.

**1. What does layout read?** `px-css` produces stylo `ComputedValues`. If
`px-layout` names stylo types it inherits stylo's closure and its unsafe
surface, and layout cannot be tested without running a cascade. §5 forbids a new
dependency without an ADR, and stylo is already `px-css`'s.

**2. What does "WPT CSS2 reftest subset at threshold" mean today?** A reftest
renders a test and its reference and compares pixels. Paint is Phase 8 and text
shaping is Phase 9, so nothing in that directory can be *run* in Phase 6. This
is the same obstruction ADR 028 recorded for Phase 5.

## Decision

1. `px-layout` defines its own input, `BoxStyle`: display, margins, padding,
   borders, width, height and whether the box establishes a block formatting
   context. It depends on `px-dom` only. A later unit adds a conversion in
   `px-css` from `ComputedValues` to `BoxStyle`, so stylo stays behind that
   crate and `px-layout` can be fixture-tested with hand-written styles.
2. Geometry is `Au` throughout (§4.2): `i32`, 1/60 px, every operator
   saturating, so overflow is a defined result and not a panic or a wrap.
3. **Item 2 is met by porting**, as ADR 028 did: a named subset of WPT CSS2
   normal-flow tests is rewritten as geometry assertions on the laid-out box
   tree (border-box rectangles in `Au`), each naming the WPT file it came from.
   WPT's own harness is not run. When Phase 8 lands the ported tests are
   replaced by the real reftests, not kept beside them.
4. Text is out of scope until Phase 9 supplies metrics. Inline layout in this
   phase uses a fixed-advance test font (every glyph one em square, as WPT's own
   Ahem font is) so line-breaking fixtures are deterministic.

## Alternatives rejected

- **`px-layout` depends on stylo directly.** Fewer lines, but it ties layout's
  test setup to a cascade and widens the unsafe-bearing closure for no gain.
- **Declare the WPT item blocked until Phase 8.** Honest but leaves the phase
  with no conformance signal for months. Still available if the user prefers it.

## Consequences

- One more type to keep in step with computed style, and a conversion to write
  and test. Mismatches between the two would be a layout bug that no layout test
  can see; the conversion unit must be tested from CSS text to geometry.
- The WPT tick is weaker than it reads, exactly as in Phase 5, and the gate
  report must say so.

## Verification

Wrong if the `ComputedValues` → `BoxStyle` conversion needs more than a few
dozen properties to stay faithful, at which point the indirection costs more
than the dependency it avoids.
