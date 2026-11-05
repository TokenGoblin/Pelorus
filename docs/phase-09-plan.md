# Phase 9 plan: text and input

Decisions are ADR 036 (proposed; nothing below that needs a dependency starts
before it is accepted) and the measurements in
`docs/research/phase-09-text-stack.md`. Phase 8's ADR 033 (c) promised that every
text baseline is regenerated once, in a single reviewed `--bless` commit; unit 10
is that commit.

Gate (build-spec section 9): multi-script fixtures (Latin, Cyrillic, CJK, Arabic,
Devanagari, Hebrew) baselined; IME composition test in the compat suite on both
OSes. ADR 036 (g) changes "per platform" to one set, tolerance 0, for bundled
fonts; that needs the user's yes.

Per /CLAUDE.md the gate tests are written first and fail (unit 0).

## Units

**Unit 0: gate tests, failing.** Extend `tests/render/pages.toml` with six
script fixtures (one per gate script, bundled fonts only) and a `text-` set (mixed
direction, a long CJK paragraph, a Devanagari conjunct page, Ahem boxes); an
`ime_composition` test in `px-content` driven by scripted events; a
`shaper_parity` test comparing the shaper with a reference on a fixed string list
(ADR 036 Verification). All red. One commit. Gate: both items.

**Unit 1: font core and bundled fonts** (`px-text`, `tests/fonts`). `Font` handle
over `Arc<[u8]>`, `FontId`, a std-only reader for what Ahem needs (sfnt directory,
`head hhea hmtx cmap` format 4, `loca glyf`), about 300 lines, and the committed
fonts with licences and a provenance file (Ahem verbatim; Noto subsets). Resolve
`font-family: Ahem`. Gate: Latin baselines; unblocks 285 WPT files. Needs no
dependency, so it can start now.
Risk, low. The subsetting recipe must be recorded or the Noto files are
unreproducible data; the subsetter stays outside the workspace (ADR 032).

**Unit 2: the shaper** (`px-text`). `Shaper` trait, swash implementation in
shaping-and-metrics mode (or `harfrust` if the user chooses), font-unit integer
output, one documented scaling to `Au`. Glyph cache keyed by (font, size, glyph).
Fuzz target over font parsing (section 4.5). Gate: all six scripts shape.
Blocked on ADR 036 accepted. Risk, medium: swash differs from HarfBuzz on mark
order and one reph (research section 2); the parity test is the guard.

**Unit 3: layout integration** (`px-layout`). Replace `text.rs` (`measure`,
`words`, `char_advance`, used at `block.rs:2061` and `:2120`) with shaped runs;
line boxes and baselines from font metrics (ascent, descent, line gap), `font-family`,
`font-size`, `line-height`, `white-space` read from computed style. Still integer
only (`cargo xtask layout`). Gate: every fixture. Depends on 2 and 6, and on 7 for
RTL. Risk, **high**: this is Phase 6/7 code with ~3,600 lines in `block.rs`; the
WPT thresholds for `css2`, `css-flexbox`, `css-grid` may fall before they rise,
because the stub metric was uniformly wrong and a real one is not. Record the
counts before and after, in the gate report, not just a pass.
Check first: does `px-css` expose `font-family` and its fallback list today? If
not, that is a commit in `px-css` before this unit.

**Unit 4: glyph runs in the display list** (`px-paint`, `px-ipc`, `px-gpu`).
`Item::GlyphRun { font, size, glyphs, color }` replaces `Item::TextRun`; postcard
types and size limits; font bytes reach the GPU process in bounded chunks from the
broker (ADR 033 (e)4 shape, 1 MiB messages); a cache by `FontId`. Gate: baselines.
Risk, medium: the boundary tests from Phase 8 must still pass with fonts in
flight, and a malformed font must kill only its process.

**Unit 5: integer glyph rasterizer** (`px-gpu`). Outlines from `skrifa` unscaled,
integer scaling, exact-area scanline coverage into 8-bit masks, nonzero winding,
no `f32`, blended into the software rasterizer. 400 to 600 lines. Tests: hand-
computed coverage for a rectangle glyph (Ahem), a triangle, a quadratic; same
glyph twice gives the same bytes. Gate: baselines; this is the cross-OS identity
claim. Risk, medium: exact-area coverage has a classic off-by-one at pixel edges.
Needs units 1 and 4 types only, so it runs in parallel with 2.

**Unit 6: line breaking** (`px-text`). `icu_segmenter` `LineSegmenter` with the
CSS `line-break` and `word-break` options, `overflow-wrap`, `white-space`, soft
hyphen. Gate: CJK and Devanagari paragraphs. Parallel with 2 and 7. Risk, low
(data is already in the lock); the CSS mapping is the work.

**Unit 7: bidi** (`px-text`). UAX #9 over `icu_properties` (or `unicode-bidi`,
user's choice), run splitting, shaping per directional run, visual reordering at
line breaks, mirroring, `direction` and `unicode-bidi` CSS. Conformance against
`BidiTest.txt` and `BidiCharacterTest.txt`, vendored as third-party data (the
files could not be fetched from this container; fetch them from a machine that
can, record the Unicode version). Gate: Hebrew, Arabic fixtures. Parallel with
2 and 6. Risk, medium: line-end reordering with floats and inline-blocks.

**Unit 8: system fonts and fallback** (`px-broker`, `px-text`). Directory scan
(Linux roots plus `fonts.conf` `<dir>` entries; Windows `Fonts` directories),
`name`/`OS/2` index, generic-family and per-script preference lists as Rust data,
per-character fallback by `cmap` coverage and `Script`, lazy chunked delivery of
the chosen file. Gate: none directly (fixtures use bundled fonts); the spec's
"system font enumeration". Parallel with everything. Risk, medium: this container
has Latin, Arabic, Hebrew, Thai and CJK fonts, no Devanagari-complete one except
FreeSerif/FreeSans; Windows is untested here.

**Unit 9: IME.** 9a (here): composition model, IPC messages, `compositionstart/
update/end` and `beforeinput`/`input` with `isComposing`, caret rectangles from
shaped text, a minimal editable surface, the scripted compat test. 9b (real
Windows): backend for the user's choice (IMM32 / winit, or TSF). 9c (real Linux):
backend for the user's choice (winit XIM and `text-input-v3`, or a hand-written
IBus/Fcitx D-Bus client). 9b and 9c implement the seam in the UI process only.
Gate: IME composition test. Risk, **high** for 9b and 9c; 9a is low.

**Unit 10: bless and report.** One `--bless` commit regenerating every text-bearing
Phase 8 baseline plus the new fixtures; the diff reviewed by a person as PNGs; the
gate report whose first paragraph says what was and was not proven on each OS.

## Gate items to units

| Gate item | Units |
|---|---|
| Latin baselined | 0, 1, 3, 4, 5, 10 |
| Cyrillic | same, with the Noto subset |
| CJK | 6 (breaking) plus the above |
| Hebrew | 7 plus the above |
| Arabic | 2 (joining), 7 plus the above |
| Devanagari | 2 (Indic cluster), 6 plus the above |
| Baselined per platform (see ADR 036 (g)) | 5, 10, and the cross-OS CI job |
| IME composition test, both OSes | 9a here; 9b and 9c on real machines |

Cheapest order for the six scripts, each step adding one capability: Latin (1, 4,
5, 3 with Ahem), Cyrillic (2 with Noto Sans), CJK (6), Hebrew (7), Arabic (2's
joining, which is where mark order bites), Devanagari (2's Indic engine).

## Parallelism

0, then 1, then {2, 4, 5, 6, 7} in parallel, then 3, then 10. Units 8 and 9a run
alongside everything; 9b and 9c are last. Units 2, 4, 5, 6 and 7 touch different
crates and can be separate sessions; unit 3 is the integration point and is one.

## What can be proven where

| Here (no display, no IME, Linux) | Only on real hardware |
|---|---|
| Units 0 to 8, 9a, 10 for Linux | Windows runs of everything (CI has a Windows runner; none here) |
| Shaping parity, rasterizer identity, fixtures, WPT counts | **Byte identity of bundled-font renders on Windows and Linux** (the cross-OS job) |
| System font scan on this machine's fonts | System fonts on Windows; fonts under paths we do not scan |
| Scripted composition test | **A real IME** (TSF/IMM32, IBus, Fcitx, Wayland text-input): 9b, 9c, the gate's actual IME item |
| | Candidate-window placement from the reported caret rectangle |

## Honest risk

- **Gate item 2 cannot be met by this container.** A scripted IME test proves the
  model and the DOM events, not that a Japanese user can type. The gate report must
  say "model proven, backends unproven on this runner", as Phase 8 said of wgpu.
- **Unit 3 is where the phase can stall.** Real metrics expose layout code only
  ever tested with a uniform stub (ADR 029 relies on that uniformity); expect
  Phase 6 and 7 defects, and inline layout is in phase for this one.
- **The dependency decision blocks units 2 and 7.** Units 0, 1, 4, 5, 6, 8 and 9a
  do not; start those, and the phase loses little if the user takes a week.
- **First-party size**: roughly 5,000 to 7,000 lines before tests (an estimate; the
  Phase 8 rasterizer and PNG codec were the closest comparable and ran over their
  estimate). Plan for the upper end.
- **Fonts as attack surface.** Parsing runs in the content and GPU processes, both
  sandboxed; the fuzz target is part of unit 2, not a follow-up.

## Status, 2026-11-04 (built, not closed)

Written from what is on `main`, verified by running it; nothing here has run in
GitHub CI yet.

| Unit | State |
|---|---|
| 1 font core, Ahem | **Done.** std-only sfnt reader (head, hhea, maxp, hmtx, cmap 4 and 12, loca, glyf with caps), hostile-input tests, Ahem committed verbatim with licence. CFF fonts and collections return `Unsupported` |
| 2 shaper | **Done for structure, unproven for quality.** harfrust 0.15 + skrifa 0.49 behind a `Shaper` trait, integer font units, bounded. The parity tests skip unless font files are supplied, so **complex-script shaping has never been compared with a HarfBuzz reference** in this repo |
| 3 layout integration | **Done, Ahem only.** Shaped runs, bidi reordering, CSS line breaking, real metrics. Reftests moved css2 54 to 55, flexbox 150 to 151, grid 71 to 67 (four pairs read, each in the floor's comment). Inline elements were still flattened into their block's run at this point; see the next row |
| 3b inline elements | **Done, Ahem only** (branch `phase9/inline-elements`). An inline item list per run (text tagged with its element's style, box start and end edges, `<br>` as a forced break, `inline-block`, `inline-flex` and `inline-grid` as atomic inlines), shaped per element and directional run, broken over the concatenated text, line boxes from the strut and every inline box and atomic (CSS 2.1 10.8.1), one `FragmentKind::Inline` fragment per line for a decorated box (`box-decoration-break: slice`), paint order for inline backgrounds, borders and per-element glyph colour. `vertical-align`: `top`, `bottom`, `sub`, `super`, lengths and percentages for atomics, shifts only for text; `middle`, `text-top`, `text-bottom` are `baseline`. Reftests css2 55 to 56, flexbox 151 to 156, grid 67 to 66 (pairs read in each floor's comment). Not done: `text-decoration`, `letter-spacing`, `word-spacing`, `text-transform`, `unicode-bidi` embeds on spans, an absolutely positioned child's static position after text on a line, `position: relative` offsets on an inline box's own text, percentage margins on inline boxes |
| 4 glyph runs, transport | **Done.** `Item::GlyphRun`, fonts cross the GPU boundary in bounded chunks, a malformed font kills only its process, fallback to software |
| 5 glyph rasterizer | **Done.** Integer exact-area coverage, hand-derived expectations. Where contours cross inside one pixel the coverage is the clamped sum (an approximation); pixels fully inside or outside are exact |
| 6 line breaking, 7 bidi | **Done** over data already in the lockfile plus `unicode-bidi` |
| wiring into paint | **Done.** Text renders as real glyphs; 11 baselines re-blessed, each read; 3 new text fixtures (RTL, centered wrap, mixed direction) |
| 8 system fonts | **Not started** |
| 9 IME | **Not started.** Needs real Windows and Linux machines to prove |
| 10 cross-OS baselines | **Not started.** One byte-identical set is the claim; Windows has never run it |
| bundled Noto subsets | **Not started** (Cyrillic, CJK, Arabic, Hebrew, Devanagari) |

### What the gate needs, and the honest distance

The gate is six scripts baselined plus an IME composition test on both OSes.

- **Latin:** the pipeline works end to end, but Ahem draws every letter as a solid
  square, so today's baselines prove *geometry* (advances, baselines, wrapping,
  direction), not glyph shapes. A real Latin face is needed for the claim.
- **Cyrillic, CJK, Arabic, Hebrew, Devanagari:** blocked on bundled fonts. Hebrew
  currently renders Ahem's hollow `.notdef` box. Nothing for these scripts is
  baselined.
- **IME:** not started and not provable in a container without a display or an
  input method.

Phase 9 is therefore **not closed**, and the number of gate items met is zero. What
exists is the machinery the gate runs on.
