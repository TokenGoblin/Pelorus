# What is actually runnable in WPT `css-flexbox` and `css-grid`

*Measured at the end of Phase 6, before Phase 7 starts. Pinned commit
`b875b21fd9e6b688e37c10d1b6366cab7418f91e`, the same one the CSS2 subset is
vendored from.*

## Why measure before starting

Phase 7's gate is "WPT `css-flexbox` and `css-grid` subsets at threshold". Phase 5
had a gate of the same shape — "WPT `css/css-cascade` subset passes" — and
discovered *during the phase* that **not one file in that directory can run before
JavaScript exists**. Every one is either a reftest needing layout and paint or a
`testharness.js` test needing DOM bindings. ADR 028 exists because that was found
late.

This is the same question asked early. The answer is much better.

## The counts

Top level of each directory, files only:

| | `css-flexbox` | `css-grid` |
|---|---|---|
| `.html` files | 799 | 103 |
| of which `-ref.html` | 212 | 26 |
| **test files (non-ref)** | **587** | **77** |
| of which `-crash.html` | 6 | 8 |
| subdirectories | 6 | 15 |

## What kind of test they are

Thirty test files sampled from each directory, classified by whether they carry
`<link rel="match">` (a reftest) or load `testharness.js`:

| | reftest | testharness | crashtest | both |
|---|---|---|---|---|
| `css-flexbox` | **12 / 30 (40%)** | 18 / 30 | 0 | 0 |
| `css-grid` | **21 / 30 (70%)** | 3 / 30 | 6 / 30 | 0 |

No file was both, and no file was neither except the crashtests.

## Three kinds, and only one of them has to wait

**Reftests — runnable now.** ADR 029's machinery already does this: lay both sides
out, compare box geometry, and the equality survives the missing paint and text
shaping. Everything that ADR knows applies unchanged, including its blind spot —
a reference that draws the same picture with fewer boxes cannot be matched, which
Phase 6 measured and marked `SUPERSET`.

**Crashtests — runnable now, and nearly free.** A crashtest has no reference and
no assertions: it passes if the browser does not crash. For a layout engine that
is a real gate item rather than a consolation, and this project is unusually well
placed to take it seriously — `px-layout` has no `unsafe`, so "does not crash"
means "does not panic, does not overflow its stack, does not loop". 14 files
between the two directories, and they are the ones Chrome and Firefox actually
crashed on, which is a better-chosen corpus than anything hand-written.

**testharness.js — not before Phase 10/11.** 60% of flexbox and 10% of grid. Same
situation ADR 028 recorded for the cascade, and the same rule should apply:
port a named few if the phase needs them, say "ported, not run" in those words,
and replace them with the real tests when JavaScript lands rather than keeping
both.

## What this implies for Phase 7's threshold

The runnable denominator is roughly **235 flexbox reftests and 54 grid reftests**
if the sample rate holds, plus 14 crashtests — before the subdirectories, which
were not counted and which hold more.

That is twice the size of the CSS2 subset Phase 6 worked against, and it is
reftests rather than a mixture, so ADR 029's comparison is the right oracle
throughout. **The flexbox/grid bias runs the other way from CSS2's**: these are
recently specified features with modern tests, so the `.xht` problem that cost
Phase 6 92% of `css/CSS2` does not arise here at all.

## Two things to check when the phase opens

**Do these directories use `<link rel="match">` with a `-ref` *naming* convention,
or arbitrary reference filenames?** The CSS2 vendoring script pairs on the name.
212 `-ref.html` against 587 tests in flexbox suggests references are shared between
tests, which the script's one-ref-per-test assumption does not handle. Read the
`rel="match"` hrefs rather than the filenames.

**The crashtests should land first, not last.** They need no reference pairing, no
threshold argument and no oracle: run the file, assert the process survives. They
would have been the cheapest possible Phase 6 gate item and nobody had looked.
