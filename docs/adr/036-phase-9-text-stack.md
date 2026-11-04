# 036 — Phase 9 text stack: a library shaper behind a trait, our own integer rasterizer, the Unicode data we already carry

- **Status:** proposed. Nothing in `Cargo.toml` or `Cargo.lock` has been changed.
  Section "What the user must decide" lists the questions that block acceptance.
- **Date:** 2026-11-04
- **Phase:** 9
- **Invariants touched:** 7 (reproducible builds) and §4 (the unsafe claim) are
  examined and **not** changed; ADR 033 (c) (baselines) is extended, see Decision (g).
  Appendix A is **not** re-argued; Decision (a) keeps swash and names the one
  place it may be wrong.

## Context

§9 gives Phase 9 "shaping, font fallback, bidi, line breaking, system font
enumeration, and IME integration" and a gate of "multi-script fixtures (Latin,
Cyrillic, CJK, Arabic, Devanagari, Hebrew) baselined per platform; IME
composition test in the compat suite on both OSes". Today `px-text` is empty and
`px-layout/src/text.rs` measures every character as half the font size; ADR 033
(c) paints text as placeholder blocks and says every text baseline is regenerated
in one reviewed `--bless` commit when this phase lands. The project wants as few
dependencies as possible (ADR 032, `docs/research/dependency-audit.md`) and
`/CLAUDE.md` forbids a dependency without an ADR.

The facts below are measured in `docs/research/phase-09-text-stack.md`; section
numbers refer to it.

- **Unicode data is already here.** `icu_segmenter` 2.3.0 (UAX #14 line, UAX #29
  grapheme and word, with CSS-shaped options) and `icu_properties` 2.3.0 (bidi
  class, mirroring, paired brackets, joining type, script) are in `Cargo.lock`
  through `stylo` and `url`. Declaring them directly in `px-text`, pinned to our
  lock, adds **0** crates and **0** unsafe tokens (§1.2, tested offline against a
  copy of our lockfile).
- **swash shapes well, not perfectly.** swash 0.2.10 with `default-features=false`
  adds 6 crates and 459 unsafe tokens (+2.1% on the 21,675 baseline); 391 of the
  459 are `bytemuck`. Against a HarfBuzz port on 89 strings across the six gate
  scripts, 83 are identical, including Devanagari conjuncts and reordering,
  Arabic joining and the lam-alef ligature. The 6 differences are the order of
  stacked Arabic and Hebrew marks (5) and one Devanagari reph (§2). swash also
  parses cmap and GSUB/GPOS with its own reader that uses `read_unchecked` over
  font bytes (36 of its 54 tokens).
- **`harfrust` + `skrifa` is the same size.** 7 new crates, 445 tokens, no
  `unsafe` in the shaper, safe bounds-checked parsing, HarfBuzz v15 behaviour.
  `skrifa`/`read-fonts`/`font-types` are Google-audited (`skrifa` 0.44.0 and
  `read-fonts` 0.41.0 exactly); swash, `zeno`, `yazi` and `harfrust` are in no
  imported audit set (§1.4).
- **`cosmic-text` is much bigger.** 26 new crates, 875 tokens (22 and 711 with
  defaults off), `memmap2` and `slotmap`, a second copy of `skrifa`,
  `read-fonts` and `font-types`, and its own layout, bidi and line breaking that
  `px-layout` would have to bypass.
- **A real shaper is needed for two of six scripts.** Latin, Cyrillic, CJK and
  Hebrew need `cmap`, `hmtx`, `glyf`, bidi and line breaking (about 1,100 lines
  of font reading, an estimate). Arabic needs contextual forms and Devanagari the
  Indic engine; hand-writing them is a 6,000 to 9,000 line project (§3).
- **System fonts need no C and no unsafe**: directory scan on Linux and Windows
  (§5). DirectWrite is COM with no wrappers in `windows-sys`.
- **Fonts are a broker concern**, megabytes against a 1 MiB message limit (§5).
- **The test fonts exist and are free to carry.** Ahem 1.50: 21,768 bytes, public
  domain / CC0, named by 285 vendored WPT files (25% of `css-grid`). Noto Sans,
  Arabic, Hebrew, Devanagari: OFL 1.1, no Reserved Font Name (§7).

## Decision

**(a) Shaping and font parsing: swash, behind a trait, with `harfrust` as the named
replacement.** `px-text` owns a `Shaper` trait (input: a font handle, a script, a
direction, a `lang`, features, a run of UTF-8; output: glyph ids, cluster byte
offsets, advances and offsets **in font units**, integers). swash implements it
first, in shaping-and-metrics mode only: `default-features = false, features =
["std"]`, which drops `zeno` and `yazi`. Appendix A names swash and is
respected. The measured gap (marked Arabic and Hebrew, one reph) is reported, not
acted on: the trait makes a later swap to `harfrust` a one-crate ADR, and
section "What the user must decide" asks whether to make it now.

**(b) Outlines: `skrifa`, pinned at the version swash allows (`=0.44`), unscaled.**
It is already beneath swash, so it adds nothing, but `px-text` declares it
directly. Outlines are requested at `Size::unscaled()` (the font's own integer
units) and scaled by us in integers, one documented rounding.

**(c) Glyph rasterization: first-party, integer-only, in `px-gpu`'s software
rasterizer.** Exact-area scanline coverage into 8-bit masks, nonzero winding,
deterministic subdivision of quadratics, no `f32`. It removes `zeno` and `yazi`,
keeps ADR 033's "tolerance 0, one baseline set" true for text, and extends
`cargo xtask layout`'s no-`f32` rule to `px-text`. Estimate 400 to 600 lines.

**(d) Unicode: use what is in the lock.** `icu_segmenter` for line, grapheme and
word segmentation (`LineSegmenter::new_auto` with the CSS `line-break` and
`word-break` options); `icu_properties` for bidi class, mirroring, paired
brackets, joining type, script. `px-text` declares both as direct dependencies at
`=2.3.0`, version-pinned to the lock. No `unicode-linebreak`, no
`unicode-segmentation`.

**(e) Bidi: UAX #9 written first-party over ICU's data** (800 to 1,100 lines, 0
dependencies), tested against `BidiTest.txt` and `BidiCharacterTest.txt`, which
are Unicode test data and must be vendored under `tests/` as third-party corpora
(exempt from ADR 032). Alternative `unicode-bidi` is rejected only on the
dependency count; the user may flip this, see below.

**(f) System fonts: directory scan with `std::fs`, in the broker.** Linux:
`/usr/share/fonts`, `/usr/local/share/fonts`, `~/.local/share/fonts`, `~/.fonts`
and `<dir>` entries of `/etc/fonts/fonts.conf`. Windows: `%SystemRoot%\Fonts`
and `%LOCALAPPDATA%\Microsoft\Windows\Fonts`. Generic-family and per-script
fallback preference lists are Rust data owned by `px-text`; fontconfig's
alias rules are not interpreted. No DirectWrite, no `windows` crate, no new
`windows-sys` feature, no `fontdb`. The broker sends font bytes to the content and
GPU processes in bounded chunks (the ADR 033 (e)4 strip precedent); handle
passing waits for the window phase.

**(g) Bundled fonts and baselines.** `tests/fonts/` commits Ahem 1.50 verbatim and
Noto Sans, Noto Sans Arabic, Noto Sans Hebrew, Noto Sans Devanagari and one CJK
face **subset to the fixtures' glyphs**, each with its licence text and a
provenance file (source URL, commit, SHA-256, how subset). A `--test-fonts` mode
(and the render gate) resolves `font-family: Ahem` and the fixtures' named
families to these fonts only and never reads system fonts. Text baselines are
**one set for both OSes with tolerance 0**, as ADR 033 (b) already has it,
because bundled-font glyph output is integer end to end. System-font rendering
is **not baselined**. This amends the "per platform" wording of build-spec §9
Phase 9, §11 and `crates/px-text/CLAUDE.md`; the first two are not edited by this
ADR.

**(h) IME: a platform-independent seam first, real input methods behind it.**
Phase 9 builds the composition model, the IPC messages, DOM composition events,
caret-rectangle reporting and a scripted-IME compat test, all runnable in CI
with no display. The real Windows and Linux IME backends implement the seam in
the UI process; which technology (TSF or IMM32 on Windows; winit's XIM and
`text-input-v3`, or a hand-written D-Bus client, on Linux) is a user decision
below and not needed to start.

**(i) Direct-dependency list this ADR would add** (the full-closure effect is
the table in "Consequences"): `swash` (shaping), `skrifa` (outlines),
`icu_segmenter` and `icu_properties` (already locked). **Nothing is added until
this ADR is accepted.**

## Options, ranked

1. **Recommended: this ADR.** swash (shaping only) + `skrifa` + own integer
   rasterizer + ICU4X from the lock + own bidi + directory-scan fonts. **6 new
   crates, +459 unsafe tokens, 0 build requirements beyond Rust.** Six scripts
   reachable; the two hard ones (Arabic, Devanagari) are shaped by a library.
2. **Same, with `harfrust` + `skrifa 0.49` as the shaper.** 7 new crates, +445
   tokens, shaper has no `unsafe`, HarfBuzz-exact. Technically stronger than 1;
   costs an amendment to the word "swash" in Appendix A and one unaudited crate
   (`harfrust`) in place of one (`swash`). If the user is willing to amend
   Appendix A, **this is my recommendation over 1**, and Decision (a) becomes a
   one-line change.
3. **Option 1 with `unicode-bidi` instead of hand-written bidi.** +1 crate, +1
   token, saves 800 to 1,100 lines and the conformance-vendoring work; the crate
   comes from the Servo project and is in three imported audit sets.
4. **`cosmic-text`.** 26 new crates (22 with defaults off), +875 (+711) tokens,
   duplicate font crates, and it solves layout, which is ours (§3, "write
   yourself: layout"). Rejected.
5. **In-house for Latin, Cyrillic, CJK, Hebrew; a real shaper for Arabic and
   Devanagari.** Needs the shaper anyway, which already covers the first four, so
   it is strictly more code. It survives only as a *bootstrap*: a 300-line
   Ahem-only reader lets glyph runs, the rasterizer and the 285 Ahem WPT files
   land before the dependency is accepted (plan unit 1).
6. **All in-house, zero dependencies, zero tokens.** 6,000 to 9,000 lines for the
   shaper alone, a new fuzz surface, ligatures and kerning missing until it is
   done. Not chosen; recorded as the way to honour "as few dependencies as
   possible" literally.

## Alternatives rejected

- **`allsorts`:** 22 new crates, **1,458** tokens (`pathfinder_simd` 1,176),
  `build.rs` in five crates.
- **`rustybuzz`:** older HarfBuzz port on `ttf-parser`, 7 new crates; `harfrust`
  is its successor on `read-fonts` and shares Fontations with `skrifa`.
- **swash with `scale`/`render`:** keeps `zeno` (an `f32` -> fixed conversion with
  one `unsafe`, and `f32` flattening) in the path that ADR 033 wants integer.
- **DirectWrite for Windows fonts:** COM with no wrappers in `windows-sys`; the
  `windows` crate is 326,383 tokens (ADR 034).
- **`fontdb`/`fontconfig-parser` for Linux fonts:** 6 crates and 291 tokens, or 2
  crates for an XML reader we can replace with 100 lines.
- **`zbus` for IBus/Fcitx:** 49 crates, 9,468 tokens.

## Consequences

Effect on the claims, by number.

| | Today | After this ADR (option 1) | Option 2 |
|---|---|---|---|
| New lock entries | | +6 (`swash skrifa read-fonts font-types bytemuck bytemuck_derive`) | +7 |
| Unsafe tokens (whole source, gate method; to be re-measured) | 21,675 | ~22,134 (+459, +2.1%) | ~22,120 (+445) |
| Build requirements | Rust (+ Python/Mako for stylo, ADR 023) | **unchanged** | unchanged |
| 100% Rust (ADR 032) | holds | holds; one-off font subsetter lives outside the workspace; subset fonts are data | same |
| Reproducible builds (invariant 7) | holds | holds; bundled fonts are committed bytes | same |
| Unaudited by imported cargo-vet sets | | `swash` (plus bytemuck/font-types deltas) | `harfrust` (plus `skrifa`/`read-fonts` 0.49/0.46 unchecked) |

- The §4 sentence "swash (font parsing)" among the places safety ends gets a
  number: swash's own `cmap`/GSUB/GPOS reader has unchecked reads of untrusted
  bytes. It runs in the sandboxed content and GPU processes; it needs a fuzz target
  (§4.5) and the unsafe baseline must be raised by this ADR when accepted.
- First-party text code is **large**: rasterizer 400-600, bidi 800-1,100, font
  matching and fallback 500, directory scan 250, `px-layout` inline integration
  (replace `text.rs`, line breaking, baseline and line-box from font metrics),
  display-list glyph runs, IPC for fonts and IME. Together, an estimate of 5,000
  to 7,000 lines before tests.
- Every text-bearing Phase 8 baseline changes once, in one `--bless` commit
  (ADR 033 (c)).
- Per-platform text divergence is removed for fixtures by bundling, and moved to
  real pages, which are not pixel-compared.
- A font crosses an IPC boundary at 1 MiB per message and is parsed in at least
  two processes (content for shaping, GPU for rasterization).
- IME can be built and tested only as a model and a seam in this container; the
  Windows and Linux backends, and the gate's "IME composition test on both OSes",
  need real machines (see `docs/phase-09-plan.md`).
- Subsets of OFL fonts are committed to a public repository (ADR 002): the licence
  text travels with each, and the subsetting recipe is recorded.

## Verification

Wrong if any of these is observed.

- Cross-OS CI renders a bundled-font text fixture to different bytes on Windows
  and Linux: integer-only was not true and the first differing glyph is the
  finding (same test as ADR 033).
- A fixture in the gate's six scripts shapes differently from `harfrust` (run in a
  scratch crate, as in §2) in a way a reader can see: the swash choice cost the
  gate and Decision (a)'s replacement slot should be used. Concretely: add the 89
  strings of §2 plus any fixture strings to a test that compares swash with
  `harfrust` output and **fails** on any difference outside a written allow-list.
- The fuzz target for font parsing finds an out-of-bounds read in swash within the
  phase's fuzz budget.
- `cargo xtask unsafe-audit` after vendoring reports a delta that differs from
  459 (or 445) by more than a few tokens: the method here was wrong.
- Real pages in Ahem-using WPT files (285) do not move toward their references
  once Ahem is wired: text metrics were not the blocker.
- There is no observation that would show the directory-scan decision wrong
  except real machines with fonts in places we do not scan; the gate cannot see it.

## For a dependency ADR (build-spec §3, §5)

- **Crates and versions:** `swash =0.2.10` (`default-features=false`,
  `features=["std"]`); `skrifa =0.44`; transitive `read-fonts 0.41`,
  `font-types 0.12.7`, `bytemuck 1.25.2`, `bytemuck_derive 1.12.1`. Direct
  re-declarations of locked crates: `icu_segmenter =2.3.0`, `icu_properties =2.3.0`.
- **What they do:** swash: OpenType and AAT shaping, font metrics. skrifa and
  read-fonts: font table parsing and glyph outlines (Google Fontations). bytemuck:
  safe casting of plain data, used by the font crates.
- **Why not write it:** Arabic joining and the Indic engine are 6,000 to 9,000
  lines (§3); settled by Appendix A for swash.
- **Maintainer count:** not checked (crates.io owners were not queried; do not
  read this as a finding).
- **Audit status:** `skrifa` 0.44.0 and `read-fonts` 0.41.0 audited by Google and
  Mozilla; `font-types` audited to 0.12.1 and `bytemuck` to 1.22.0 (deltas
  needed); `swash`, `zeno`, `yazi`, `harfrust` in none of the five imported sets.
- **Unsafe-line count:** +459 tokens by the gate's method (swash 54, bytemuck 391,
  bytemuck_derive 10, font-types 4); baseline to be re-measured with
  `cargo xtask unsafe-audit --write`.
- **Transitive dependencies added:** 6 lock entries, listed above; none has a
  `build.rs`, a `-sys` crate or a Python/C requirement.

## What the user must decide

1. **Shaper.** Keep swash as Appendix A names it (this ADR's default), or amend
   Appendix A to allow `harfrust` (+1 crate, -14 tokens, no unchecked reads, exact
   HarfBuzz behaviour, one more unaudited crate in place of another). I am saying
   swash may be insufficient for marked Arabic and Hebrew and **waiting**.
2. **Bidi.** Write UAX #9 (0 dependencies, about 1,000 lines plus conformance
   data) or take `unicode-bidi` (+1 crate, +1 token).
3. **Baselines.** Accept one cross-platform baseline set, tolerance 0, for bundled
   fonts, and amend the "per platform" wording in §9, §11 and `px-text/CLAUDE.md`.
4. **IME technology.** Windows: literal TSF (COM, `unsafe` in the UI process) or
   IMM32 / winit's IME. Linux: winit's XIM and `text-input-v3` (free once Phase 18
   adds winit), a hand-written D-Bus client (0 dependencies, about 1,500 lines,
   provable only against a real IBus/Fcitx), or `rustbus` (+5 crates, +760 tokens).
   Not needed to start; needed before the IME backend unit.
5. **Bundled fonts.** Accept committing OFL subsets (and which CJK face), or limit
   fixtures to Ahem plus fonts the CI image installs.
6. **System fonts.** Accept directory scan with Pelorus-owned preference lists
   instead of fontconfig and DirectWrite semantics.

This ADR is accepted when 1 to 3 are answered; 4 to 6 can wait for their units.
