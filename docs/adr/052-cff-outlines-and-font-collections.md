# 052 — CFF/OpenType outlines and TrueType Collections, read by our own bounded Type 2 interpreter

- **Status:** accepted for the interim single-process shell (Phase 18); it
  changes decisions 1 and 8 of ADR 050 and nothing else in it.
- **Date:** 2026-11-06
- **Phase:** 9 (text), 18 (shell integration)
- **Invariants touched:** none changed. Fail closed (a font that cannot be read
  is absent, never partly used); no new crate dependency (ADR 032 and the
  supply-chain gate are untouched); all first-party source, fixtures and the
  fixture generator are Rust.

## Context

ADR 050 made web fonts work for TrueType-flavoured sfnt, WOFF and WOFF2, and
named one gap: `OTTO` (CFF/OpenType) outlines and `ttcf` collections were
refused, "the outline reader is `glyf` only, and a CFF charstring interpreter is
a separate unit of work". The gap is wide in practice. Most Adobe and many
Google-hosted families ship CFF-flavoured `.otf` and WOFF2; macOS, Windows and
Linux distributions install `.otf` and `.ttc` system fonts (CJK faces are
mostly CFF or collections), so on such a machine every generic family fell back
to the bundled face. Shaping already worked (`harfrust`/`skrifa` read CFF), and
`cmap`, `hmtx` and the rest are ordinary sfnt tables; what was missing was our
own outline path, which the display list and rasterizer use.

## Decisions

1. **A Type 2 charstring interpreter in `px-text/src/cff.rs`, safe Rust, std
   only.** It reads the CFF 1.0 header, INDEXes, Top DICT, Private DICTs (local
   subroutines, `defaultWidthX`, `nominalWidthX`), global subroutines, and
   CID-keyed fonts (`FDArray`, `FDSelect` formats 0 and 3). It executes every
   path operator (`rmoveto`, `hmoveto`, `vmoveto`, `rlineto`, `hlineto`,
   `vlineto`, `rrcurveto`, `hhcurveto`, `hvcurveto`, `vhcurveto`, `vvcurveto`,
   `rcurveline`, `rlinecurve`, `hflex`, `flex`, `hflex1`, `flex1`), skips stems
   and `hintmask`/`cntrmask` bytes (hints are never executed), takes the width
   from the first stack-clearing operator, supports `callsubr`/`callgsubr`/
   `return` with the spec's bias, and the few deprecated arithmetic operators
   (`abs add sub div neg mul drop dup exch`). Refused by name:
   CFF2 (variable fonts), the accent form of `endchar` (`seac`), the deprecated
   storage and conditional operators, Type 1 charstrings. The Top DICT
   `FontMatrix` is ignored (`head.unitsPerEm` governs).

2. **Integer arithmetic only.** Operands are 16.16 fixed in `i32`, the current
   point is 16.16 in `i64`, saturating; each emitted point is rounded half up and
   clamped to the `i16` range `glyf` can express. Nothing is floating point, so
   outlines are a pure function of the bytes on every platform (the render
   gate's twice-and-compare and Windows/Linux baselines rely on this).

3. **Cubics become the path vocabulary the rasterizer already has.** `Outline`
   and `PathCmd` (lines and quadratics) are consumed by five other crates through
   exhaustive matches; adding a cubic variant would touch all of them. Instead each
   cubic is split at `t = i/k` into `k <= 16` pieces and each piece replaced by the
   quadratic with the same end points and control point
   `(3 (c1 + c2) - (p0 + p3)) / 4`, with `k` the smallest value for which
   `|p0 - 3 c1 + 3 c2 - p3| <= 4 k^3` (error about `0.048 d / k^3` font units,
   under a quarter unit before rounding). Sub-cubic control points come from
   integer de Casteljau blossoms scaled by `k^3`; every division rounds half up.
   Adjacent pieces share a computed end point exactly. Checked once, outside the
   repository, against `skrifa`'s exact cubic outlines on 29 real CFF fonts
   (Inter in 36 styles' worth of files, Loma, GNU Unifont; about 400 glyphs
   each, sampled): no point of our path was more than 1.5 font units from theirs
   at 2048 units per em, with the tolerance constant then at 8 (0.9 typical); it
   was tightened to 4 afterwards, which only adds pieces. The 0.5-unit rounding
   of the points themselves is most of the deviation. The fixtures (also read by
   `skrifa`) deviate by at most 0.71. Nothing from that check is committed.

4. **Bounds.** Every offset and size is checked against the table before use; an
   INDEX is validated when parsed (its last offset must lie in the table) and each
   object again when read; DICT operand stacks are capped at 64. Per glyph:
   operator stack 96 (the spec's 48 with slack), subroutine nesting 10 (the spec's
   limit), 200,000 operators and operands in total *across all subroutines* (so an
   exponential call tree stops, tested), 20,000 output points (the `glyf` cap);
   exceeding one is `FontError::TooComplex`. The only allocation proportional to
   anything in the file is the output, which the point cap bounds. A font's glyph
   count is checked against `maxp`. No `unsafe`, no indexing, `unwrap`, `expect`
   or `panic` (the crate's clippy denies stand).

5. **Collections.** `ttcf` is read with `Directory::parse_face(data, index)`:
   at most 256 faces, the face offset and every table range bounds-checked, a face
   index at or past the count an error. For the web path and the registry,
   `woff::to_sfnt_face(data, index)` copies the chosen face's tables into a
   standalone sfnt, so everything downstream (`Font::from_bytes`, the shaper at
   face 0, the GPU font chunks, the content-hash dedupe) sees an ordinary single
   font and a collection's other faces cost nothing. `Font::from_bytes` itself
   reads face 0 of a collection. CSS cannot name a face, so a web `@font-face` to a
   `.ttc` uses face 0; the system scan uses the face the family/weight match found.
   WOFF2 collections (flavour `ttcf` in a WOFF2) stay refused by name.

6. **Wired in.** `woff::load_font`, `to_sfnt` and `px_layout::fontset::
   register_font` accept `OTTO` sfnt, WOFF 1.0 and WOFF 2.0 around CFF (the
   WOFF2 `glyf` transforms simply do not apply to a CFF font), and collections.
   `px_layout::fontset::register_font_face` takes a face index.
   `px_shell::fonts::resolve_generics` no longer skips `.otf` files or faces
   with index other than 0 (the scan already listed them); a file `px-text`
   refuses (CFF2, corrupt) is still skipped for the next preference. The
   `format('opentype')` hint was already accepted.

7. **Fixtures from Rust.** `crates/px-text/examples/cff_fonts.rs` writes
   `tests/fonts/PxCff-Regular.otf`, `PxCff-Bold.otf`, `PxCff.ttc` (two faces),
   `PxCff-Regular.woff` (stored tables) and `PxCff-Regular.woff2` (a Brotli stream
   of stored blocks; our decoder reads it, `skrifa` reads the unwrapped font), so
   the containers are exercised without a Python tool. The glyphs use every path
   operator family the interpreter has except `flex`/`flex1`, which the unit tests
   cover with hand-assembled charstrings. Render baselines `webfont-cff` and
   `webfont-cff-containers` pin the drawn shapes; `PAGE_FLOOR` rises 86 -> 89, the
   real total.

## Alternatives rejected

* **Rendering CFF with `skrifa`'s outline drawing.** It is already linked and
  correct. But the display-list outline path is ours on purpose (integer font
  units, `PathCmd`, panic-free by lint, one place that bounds hostile glyphs),
  `skrifa` outlines are floating point (platform-divergent rounding risk for exact
  baselines), and a second path would double what must be audited. The shaper
  still reads advances and GSUB/GPOS through it; only outlines are ours.
* **Adding `CubicTo` to `PathCmd`.** Touches `px-paint`, `px-gpu`, `px-shell`,
  `px-ui`, `px-display` and their tests for a shape the rasterizer would flatten
  anyway; the conversion lives in one file instead. Revisit if exact cubic
  rasterization (stroked text, very large glyphs) is wanted.
* **Flattening to line segments.** More points per glyph, slightly rougher at
  large sizes; the quadratic form keeps outlines the same shape of data as
  `glyf`.
* **A CFF crate** (`ttf-parser`, `cff-parser`). New dependency (ADR 032); we
  already have the safe, bounded reading discipline in this crate.

## Consequences

* Real-world `.otf` and CFF WOFF2 web fonts and system `.otf`/`.ttc` generics
  render. Hinting is ignored (as for `glyf`), so very small CFF text is the
  unhinted outline.
* Outlines are approximated to a quarter font unit before rounding; at 2048
  units per em and 16 px that is invisible, at 500 px a maximum of about 0.1 px.
* A hostile CFF font is a new parser's worth of attack surface in safe Rust: its
  failure mode is a refused font or an empty glyph, never memory unsafety.
* `FontError::Unsupported("CFF outlines")` no longer exists; callers that
  matched on it (none in the tree) must not.
* Not done: CFF2 and variable fonts, `seac`, FontMatrix, face selection from CSS,
  WOFF2 collections, `.otf` with TrueType outlines inside `OTTO` is just read as
  TrueType.

## Verification

* Wrong if a truncated, bit-flipped, stomped or random CFF/collection file panics,
  hangs or allocates beyond the caps (`tests/cff.rs` runs every truncation and
  thousands of flips on every `cargo test -p px-text`; `src/cff/tests.rs` covers
  each operator, the width forms, bias boundaries, nesting 10 vs 11, the
  exponential tree, CID `FDSelect` formats 0 and 3, and INDEX corruption).
* Wrong if a real CFF font draws differently from `skrifa` beyond rounding (the
  one-off comparison above; repeat when the interpreter changes).
* Wrong if two renders of the CFF pages differ (the render gate renders twice).
