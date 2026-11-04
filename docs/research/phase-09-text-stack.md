# Phase 9 text stack: what the candidates measure

*Research only. No `Cargo.toml`, `Cargo.lock` or source file was changed.
Date: 2026-11-04. Measured against `origin/main` at `d12071e`, Rust 1.98.1.
Nothing here is a decision; ADR 036 (proposed) turns it into one.*

Build-spec Appendix A settles "reuse `html5ever`, `stylo`, `rustls`, `swash`".
This document does not re-argue that. It measures what swash is, what it does
not do, and what it costs, and in one place (section 2) it reports a result the
user should see before the choice is closed.

## 0. Method, and what was not measured

- **Scratch projects.** One throwaway cargo project per candidate in a
  directory outside the repository, versions pinned with `=`, registry cache
  shared. `cargo generate-lockfile` then `cargo tree --edges normal --prefix
  none --target T` for `T` = `x86_64-unknown-linux-gnu` and
  `x86_64-pc-windows-msvc`, de-duplicated on `name version`. Build
  dependencies and dev dependencies are not in the closure counts (they were
  checked separately for C toolchains, section 1.3).
- **"New".** Measured against this repository's `Cargo.lock` (171 third-party
  entries). Two figures: *new name+version pairs* (over-states, because Cargo
  unifies `syn 3.0.9` onto the locked `3.0.5`), and **new names** (a crate whose
  name is absent from the lock; an incompatible major would count too, none
  occurred). The second is the number that matters and is the one in the headline.
- **Unsafe.** The token `unsafe` over every `.rs` file of each *new* crate's
  registry source, with a Perl regex that is the gate's regex
  (`(?:^|[^\w])unsafe(?:[^\w]|$)`, non-overlapping, trailing delimiter consumed;
  see `crates/px-xtask/src/gates/unsafe_audit.rs`). It counts comments and
  strings, so it over-counts. The real figure comes from `cargo xtask
  unsafe-audit --write` after vendoring; that has not been run. Baseline today:
  21,675 tokens over 109 entries (`ci/unsafe-baseline.json`).
- **Behaviour.** Where a claim is about what a library *does*, it was run:
  swash and `rustybuzz` (a HarfBuzz port) were built in a scratch crate and fed
  the same strings (section 2), and `icu_segmenter` / `icu_properties` were run
  against a copy of this repository's own `Cargo.lock` with `--offline`.
- **Not measured.** Windows builds (resolved with `--target`, never compiled).
  Real Windows system fonts, TSF, IBus, Fcitx (none exist in this container).
  Subset sizes of the bundled fonts (no subsetter was run). Conformance-test
  file sizes for UAX #9 (`unicode.org` is not reachable from this container).
  Counts marked *estimate* are my judgment, not a measurement.

## 1. Headline numbers

### 1.1 Closures

| Candidate (pinned) | Crates in closure (Linux / Windows) | New names vs our lock | Unsafe tokens added | Notes |
|---|---|---|---|---|
| **swash 0.2.10, shaping + metrics only** (`default-features=false, features=["std"]`) | 11 / 11 | **6**: `swash skrifa read-fonts font-types bytemuck bytemuck_derive` | **459** (bytemuck 391, swash 54, bytemuck_derive 10, font-types 4) | drops `zeno`, `yazi` |
| swash 0.2.10, default features (shape + scale + render) | 13 / 13 | 8: the six + `zeno yazi` | 460 | `zeno` adds 1 token |
| `skrifa` 0.44 alone (outlines, cmap, metrics) | 10 / 10 | 5 | 405 | what swash sits on |
| **`skrifa` 0.49 + `harfrust` 0.15** (HarfBuzz v15 port) | 20 / 20 | **7**: `skrifa harfrust read-fonts font-types bytemuck bytemuck_derive foldhash` | **445** (bytemuck 391, harfrust 29, foldhash 11) | harfrust's `unsafe` tokens are `forbid(unsafe_code)` text; its README says it contains none |
| `rustybuzz` 0.20.1 | 12 / 12 | 7 (adds `ttf-parser unicode-script unicode-properties unicode-ccc unicode-bidi-mirroring`) | 420 | older HarfBuzz port; `harfrust` is its successor |
| swash + harfrust together | 24 / 24 | 10 | 500 | `read-fonts` appears at 0.41 **and** 0.46 |
| **`cosmic-text` 0.19, default features** | 41 / 38 | **26** | **875** | adds `memmap2` (157), `slotmap` (123), `self_cell`, `smol_str`, `fontdb`, `harfrust`, `unicode-bidi`, `unicode-linebreak`, `unicode-segmentation`, `sys-locale` |
| `cosmic-text`, `default-features=false, features=["swash"]` | 36 / 36 | 22 | 711 | still ships its own bidi, line break and layout |
| `allsorts` 0.17 (`default-features=false`) | 41 / 41 | 22 | **1,458** | `pathfinder_simd` alone 1,176; five crates have `build.rs` |
| `unicode-bidi` 0.3.18 | 1 / 1 | 1 | **1** | zero dependencies in this configuration |
| `unicode-linebreak` 0.1.5 | 1 / 1 | 1 | 0 | |
| `fontconfig-parser` 0.5.8 | 2 / 2 | 2 (`+roxmltree`) | 0 | |
| `fontdb` 0.24 | 8 / 8 | 6 | 291 | `memmap2` 157, `slotmap` 123 |
| `rustbus` (pure-Rust D-Bus) | 15 / 15 | 5 | 760 | `nix` 723 |
| `zbus` 5 | 66 / 66 (72 both) | 49 | **9,468** | `linux-raw-sys` 6,932, `rustix` 1,678 |
| `winit` 0.30, default features (Phase 18, context only) | 68 / 72 | 52 | 15,629 | arrives with the window, not here |
| **`icu_segmenter` + `icu_properties`, against our lock** | n/a | **0** | **0** | already locked at 2.3.0 with compiled data |

### 1.2 What the ICU4X result means

`icu_segmenter` 2.3.0 and `icu_properties` 2.3.0 are in `Cargo.lock` because
`stylo` (default features of `icu_segmenter`, via its `servo` feature) and `url`
(`idna_adapter` -> `icu_properties`) need them. A scratch crate with
`icu_segmenter = "=2.3.0"` and `icu_properties = "=2.3.0"` and a copy of this
repository's `Cargo.lock` built with `--offline` and needed **zero** new crates.
(A fresh resolve instead picks newer ICU and adds 14 name+versions, which is why
the pin to our lock matters.) What it ran, on the real data:

- `LineSegmenter::new_auto(LineBreakOptions::default())` broke Latin at spaces,
  CJK between nearly every ideograph and kana, Arabic, Hebrew, Cyrillic and Hindi
  at spaces, and never inside a Devanagari conjunct. UAX #14, with CSS-shaped
  options (`strictness`, `word_option`) in the API, so `line-break` and
  `word-break` map onto it.
- `GraphemeClusterSegmenter` (UAX #29): `हिन्दी` -> clusters at bytes 0, 6, 18;
  `e` + U+0301 as one cluster; a ZWJ family emoji as one cluster.
- `CodePointMapData::<BidiClass | JoiningType | Script>`: `BidiClass` L / R /
  AL / EN / ON / RLO as expected, Arabic `ع` JoiningType dual-joining, Script per
  code point. `icu_properties::props` also exports `BidiMirroringGlyph` and
  `BidiPairedBracketType`, which UAX #9 rule N0 needs.

So **every Unicode data table Phase 9 needs for line breaking, grapheme and word
segmentation, bidi classes, mirroring, paired brackets, script and joining type
is already in the binary's closure**. The only missing piece for UAX #9 is the
*algorithm*, not the data. (`unicode-bidi` carries its own 50 KB table file; it
is not needed if we use ICU's.)

### 1.3 Build requirements and reproducibility

For every candidate above except `allsorts`, the normal closure contains no `cc`,
`cmake`, `pkg-config`, `bindgen`, `-sys` crate or Python. `cargo tree --edges
build` for `cosmic-text` shows nothing beyond `version_check` (pure Rust, for
`slotmap`'s `build.rs`). `allsorts` has `build.rs` in five crates and `pathfinder_simd`.
None of `swash`, `skrifa`, `read-fonts`, `font-types`, `harfrust`, `bytemuck` has a
`build.rs`; `bytemuck_derive` is a proc-macro over `syn`/`quote`/`proc-macro2`
which are already locked. **The 100% Rust rule (ADR 032) and invariant 7 are
untouched by the recommended set.** Fonts are not build inputs unless we bundle
them, in which case they are committed bytes, which is reproducible by
construction.

### 1.4 Supply chain

`deny.toml` already allows every licence involved (MIT, Apache-2.0, Zlib,
Unicode-3.0) and its header comment names `swash` and `cosmic-text` as part of the
permitted closure. cargo-vet: I fetched the five imported audit sets
(`supply-chain/config.toml`) and searched them by crate name.

| Crate | In an imported audit set? | Latest audited version I could find | Version we would need |
|---|---|---|---|
| `skrifa` | Google, Mozilla | 0.44.0 | 0.44.0 (swash) / 0.49.0 (harfrust) |
| `read-fonts` | Google, Mozilla | 0.41.0 | 0.41.0 (swash) / 0.46.0 (harfrust) |
| `font-types` | Google, Mozilla | 0.12.1 | 0.12.7 (delta) |
| `bytemuck` | Google | 1.22.0 | 1.25.2 (delta) |
| `bytemuck_derive` | Google, Mozilla | not extracted | 1.12.1 |
| `foldhash` | Google, Mozilla, Bytecode Alliance | not extracted | 0.2.0 |
| **`swash`, `zeno`, `yazi`, `harfrust`** | **no set** | | **exemption or our own audit** |
| `unicode-bidi` | Mozilla, Google, Bytecode Alliance | not extracted | 0.3.18 |
| `icu_segmenter`, `icu_properties` | Mozilla, Google (already exemptions in our `config.toml`) | | already locked |

That is: the Fontations crates (`skrifa`, `read-fonts`, `font-types`) are Google's
own and audited up to exactly the versions swash allows; the shaper on top of them
(`swash` itself, or `harfrust`) is unaudited by any set we import. Whether
`skrifa` 0.49 / `read-fonts` 0.46 are covered was not checked.

## 2. What swash provides, and where it differs from HarfBuzz

Read from the source (`swash-0.2.10/README.md`, `src/`) and then run.

**Provides:** font parsing through `skrifa`; cmap, hmtx, metrics; OpenType GSUB and
GPOS (`src/shape/at.rs`, 1,826 lines); AAT `morx`/`kerx` (partial); variable
fonts; a cluster parser for complex scripts (`src/text/cluster/complex.rs`, a
Universal Shaping Engine style model, and `myanmar.rs`); Arabic joining; its own
Unicode property tables (`text/unicode_data.rs`, 5,491 lines, includes
`BidiClass`, joining type and script). `ShaperBuilder::size(0.0)` (the default)
returns **advances and offsets in font units**, which makes integer layout
possible: scale once, in integers.

**Does not provide:** text layout, bidi resolution (it only reports
`needs_bidi_resolution`, and its docs say "reversing runs should occur during
bidi reordering", so shaping is per directional run in logical order), line
breaking, system fonts, font fallback selection. Shaping alone is not enough, and
no one crate in the settled set supplies the rest.

**Behaviour measured.** Same string, same font, shaped by swash 0.2.10 and by
`rustybuzz` 0.20.1 (a port of HarfBuzz); glyph ids and advances compared. Two
datasets, 89 strings:

| Dataset | Strings | Identical | Different |
|---|---|---|---|
| System fonts here (FreeSerif, FreeSans, DejaVu Sans, WenQuanYi Zen Hei): Devanagari conjuncts and matras, Arabic, Hebrew, Cyrillic, Latin with `fi`, CJK | 42 | 40 | 2 |
| Noto Sans / Arabic / Hebrew / Devanagari (OFL; what we would bundle): 24 Devanagari, 16 Arabic, 3 Hebrew, 4 Latin or Cyrillic | 47 | 43 | 4 |
| **Total** | **89** | **83 (93%)** | **6** |

Identical in every case tried: Devanagari conjuncts (`क्ष`, `ज्ञ`, `द्व`),
i-matra reordering (`कि`, `हिन्दी`), reph on a medial consonant (`कर्म`, `सर्वेक्षण`),
Arabic joining forms, `لا` ligature, kashida, Hebrew without points, Cyrillic,
Latin kerning and `fi`/`ffi` ligatures, CJK ideographs and punctuation.

The six differences are one class and one outlier:

1. **Combining-mark order (5 of 6).** Arabic with stacked tashkeel (shadda + fatha,
   in `مُحَمَّد` and `ٱلسَّلَامُ عَلَيْكُمْ`, both in Noto Sans Arabic and in
   FreeSerif) and Hebrew with niqqud (`בְּרֵאשִׁית`). The base glyphs agree;
   swash emits the zero-advance mark glyphs in a different order than HarfBuzz
   (in FreeSerif a GSUB ligature of shadda + fatha that HarfBuzz applies does not
   fire in swash). Likely cause, not confirmed: HarfBuzz reorders marks by
   canonical combining class before shaping (its Arabic and Hebrew shapers do),
   and swash does not. Effect: marks can stack in the wrong order. Unmarked text,
   which is nearly all of the web, is unaffected.
2. **Devanagari reph at the end of a word (1 of 6).** `उत्कर्ष`: HarfBuzz ends
   with a zero-advance reph glyph (506); swash ends with a second full-width
   glyph (86, advance 578). One of 24 Devanagari strings in Noto and none of 12
   in the system fonts, so I cannot say how common it is.

**Reading.** swash is far closer to a HarfBuzz-grade shaper than I expected for
the gate's six scripts. It is not exactly HarfBuzz and the gap is in mark
handling and one reph case. `harfrust` (HarfBuzz 15 ported, Google-maintained
alongside the Fontations crates) closes it at the price of one more crate in the
lock (7 instead of 6) and 14 fewer unsafe tokens. Appendix A names swash; the
ADR keeps swash as the default and puts the shaper behind a trait so that this
is a one-crate change. **Whether to amend Appendix A is the user's call, section 8.**

**Where swash's own unsafe is.** Its 54 tokens are not decoration. 36 of them are
in `internal/parse.rs` (18), `internal/at.rs` (10) and `internal/cmap.rs` (8):
swash parses `cmap`, GSUB and GPOS with its **own** reader, which has
`read_unchecked` and `from_be_data_unchecked` over font bytes (`parse.rs:103,
216, 320, 330`). Those are unchecked reads of attacker-controlled data on the
shaping path, behind bounds checks that swash's authors wrote and that I did not
audit. `skrifa` and `read-fonts` (and so `harfrust`) are `unsafe`-free and
bounds-checked. Fonts are untrusted input (build-spec §4 lists "swash (font
parsing)" among the places safety ends), so this is the strongest argument for
the `harfrust` route and should be weighed with the 14-token difference, which
understates it. Mitigation inside the settled choice: the shaper runs in the
sandboxed content process, and font parsing gets a fuzz target (§4.5 requires one
for every parser).

### Determinism of glyph rasterization

`swash`'s `scale`/`render` features bring `zeno`, whose rasterizer is fixed-point
24.8 internally with one `f32` -> fixed conversion (`raster.rs:817`, one
`unsafe { to_int_unchecked() }`) and `f32` path flattening on the way in. IEEE
`f32` add, multiply and divide are deterministic across x86-64 Windows and Linux
and Rust does not contract them to FMA, so identical bytes are plausible; but
ADR 033's whole argument is "integer-only, tolerance 0", and `px-layout` forbids
`f32` by gate. The alternative costs little. `skrifa` can return outlines at
`Size::unscaled()`, which for `glyf` fonts is the font's own `i16` units held
exactly in an `f32`. We then scale in integers (`x * px_size * 64 / units_per_em`,
round half away from zero, documented) and run our own scanline coverage
rasterizer. *Estimate:* 400 to 600 lines for flattening quadratics with a
deterministic integer subdivision count, exact-area coverage accumulation into an
8-bit mask, and nonzero winding; no dependency, no unsafe. That removes `zeno`
and `yazi` from the lock and takes the swash closure to the 6-crate figure above.

## 3. What a hand-written, std-only text stack takes

All figures are **estimates** from reference implementations measured in the
sources above, for code that is `#![forbid(unsafe_code)]` and adds **0** unsafe
tokens and **0** dependencies.

| Piece | Lines (estimate) | Reference | Scripts it serves |
|---|---|---|---|
| sfnt directory, `head hhea maxp hmtx`, `cmap` formats 4 and 12 | 350 | swash `internal/cmap.rs` 275, `head.rs` | all |
| `loca` + `glyf` simple and composite outlines | 300 | swash `internal/glyf.rs` 52 (plus skrifa) | Latin, Cyrillic, CJK, Hebrew, Arabic, Devanagari glyph *drawing* |
| `name`, `OS/2` for family and weight matching | 150 | | system fonts |
| `kern` or GPOS pair adjustment | 80 / 300 | | Latin kerning |
| CFF / CFF2 Type 2 charstrings | 900 | | Noto CJK `.otf`, Inter `.otf` (common on Linux) |
| Bidi, UAX #9 over ICU data | 800 to 1,100 | `unicode-bidi` explicit+implicit+prepare+level = 1,779 lines incl. docs and tests | Hebrew, Arabic |
| GSUB lookups 1-8 and GPOS lookups 1-9, feature plan | 2,500 to 3,000 | swash `shape/at.rs` 1,826 + `internal/at.rs` 958 | **Arabic, Devanagari, ligatures** |
| Arabic joining + mark reorder | 400 | | Arabic |
| Indic / USE cluster model + reordering + reph + half forms | 1,500 to 2,000 | swash `cluster/complex.rs` 466 + `shape/engine.rs` 300 + HarfBuzz's Indic shaper | **Devanagari** |
| Shaping buffer, clusters, fallback | 800 | swash `shape/buffer.rs` 708, `mod.rs` 936 | all |

**Which gate scripts require a real shaper.** None of Latin, Cyrillic, CJK or
Hebrew do, for the *gate fixtures*: they need `cmap`, `hmtx`, `glyf`, bidi for
Hebrew, and line breaking for CJK, about 1,100 lines of font reading plus bidi.
(CSS expects default `liga`/`kern` on, so "no real shaper" means Latin text
without `fi` ligatures and without kerning from GPOS, visibly different from a
browser; acceptable for fixtures, not for a daily driver.) **Arabic** needs
contextual forms. A legacy shortcut exists, joining types from ICU plus lookup of
the Arabic Presentation Forms (U+FB50 to U+FDFF, U+FE70 to U+FEFF) in `cmap`; it
works only for fonts that carry those code points (DejaVu does; most modern fonts,
Noto Sans Arabic among them, shape through GSUB), and it never positions marks.
**Devanagari** cannot be done cheaply at all: the i-matra and reph reordering,
half forms, conjunct ligatures and mark positioning are the Indic engine, roughly
the whole of rows 7 to 10, a 6,000 to 9,000 line project with its own fuzzing
surface. So: **Arabic (properly) and Devanagari require a real shaper; the other
four do not.** An in-house route would reach four of six scripts cheaply and
then either stop or become a shaper rewrite. A shaper crate reaches all six, plus
ligatures and kerning, for 6 or 7 crates.

## 4. Bidi, line breaking, graphemes, Unicode data

| Need | Free in closure? | Option | Cost |
|---|---|---|---|
| UAX #14 line breaking | **yes**, `icu_segmenter` 2.3.0 | use it | 0 crates; `px-text` declares the dependency, which needs the ADR but adds nothing to the lock |
| UAX #29 grapheme / word | **yes**, same crate | use it | 0 |
| Bidi class, mirroring, paired brackets, joining type, script, general category | **yes**, `icu_properties` 2.3.0 | use it | 0 |
| NFC/NFD (font fallback clusters) | **yes**, `icu_normalizer` 2.3.0 | use it if needed | 0 |
| UAX #9 algorithm | no | (a) hand-written over ICU data | 800 to 1,100 lines, 0 deps; needs `BidiTest.txt` (about 490k cases) and `BidiCharacterTest.txt` (about 91k) as vendored test data, sizes from memory, not measured |
| | | (b) `unicode-bidi` 0.3.18 | 1 crate, 1 token, Servo project crate (same org as `html5ever`), audited by Mozilla, Google and Bytecode Alliance; ships its own 50 KB class table that duplicates ICU's |

`unicode-linebreak` and `unicode-segmentation` are not needed; adding them would
duplicate data already linked in.

## 5. System fonts without a C library

All font file access is the **broker's** (`/CLAUDE.md`: no filesystem access
outside `px-broker` and `px-store`); the content process has none. Two
consequences that shape Phase 9 and are not in the spec: (1) fonts are megabytes
(`wqy-zenhei.ttc` here is 16.8 MB; DejaVu Sans 760 KB) and `MAX_MESSAGE_BYTES` is
1 MiB, so a font crosses IPC in bounded chunks as the frame strips do (ADR 033
(e)4) or via handle passing (ADR 008 earmarks it); (2) glyph rasterization runs
in the GPU/raster process, which needs the same font bytes.

**Linux.** `fontconfig` is a C library and `fc-cache` files are version-specific
binary structures with no stability promise: not parsed. Pure-Rust options:

1. **Scan directories, std only.** `/usr/share/fonts`, `/usr/local/share/fonts`,
   `~/.local/share/fonts`, `~/.fonts`, plus `<dir>` elements from
   `/etc/fonts/fonts.conf` and its `conf.d` includes (extractable with about 100
   lines of line-oriented text handling, no XML parser). On each file read only the
   sfnt directory, `name`, `OS/2`, and `cmap` on demand. 0 dependencies, 0 unsafe.
   This container has 49 families under `/usr/share/fonts`.
2. `fontconfig-parser` (+`roxmltree`): 2 crates, 0 unsafe tokens; interprets the
   XML properly (`prefix`, `xdg`, `include`). Does not implement fontconfig's
   matching rules either.
3. `fontdb`: 6 new crates, 291 tokens (`memmap2`, `slotmap`). Rejected on count.

Neither option gives fontconfig's *alias and preference* rules (`sans-serif` ->
DejaVu Sans, language-specific CJK choice from `conf.d/*-latin.conf`,
`64-language-selector-*`). Pelorus must therefore own generic-family preference
lists per OS, in Rust, as data.

**Windows.** DirectWrite is COM. `windows-sys` ships the function and type
declarations but not callable interface wrappers (an interface is an opaque
pointer; you hand-write each vtable), so using it is a large `unsafe` surface in
`px-sandbox`, which ADR 008 scopes to OS primitives, not text. The `windows` crate
is 326,383 unsafe tokens (`docs/adr/034`). **Alternative with no unsafe and no new
`windows-sys` feature:** list `%SystemRoot%\Fonts` and
`%LOCALAPPDATA%\Microsoft\Windows\Fonts` with `std::fs`, read the `name` tables,
and keep a hard-coded fallback list (Segoe UI, Arial, Microsoft YaHei, Yu Gothic,
Nirmala UI for Devanagari, Segoe UI Emoji). The registry `Fonts` key adds nothing
in the common case, and `SystemLink` (Windows' own fallback data) is a refinement.
This is not measured on Windows. *Estimate:* 250 lines shared with Linux.

**Matching and fallback** (`font-family` list, then a per-character fallback
chain, by script and `lang`) is first-party in either case: *estimate* 500
lines, driven by `cmap` coverage and the Unicode `Script` property already in the
closure.

## 6. IME

Nothing below can be run here: this container has no display, no IBus or Fcitx
daemon, no Windows.

**What is platform-independent and testable here.** The composition model
(preedit string, caret and selection inside it, clause underline ranges, commit),
the IPC messages (`ImePreedit`, `ImeCommit`, `ImeCancel`, cursor-rectangle
report), `compositionstart/update/end` and `beforeinput`/`input` dispatch with
`isComposing`, caret rectangles from shaped text (needed so the IME can place its
candidate window), and the 100%-synthetic compat test "feed a scripted preedit
sequence, assert the DOM events and the painted underline". This is where almost
all Phase 9 IME code lives. There is no text control yet (no script until Phase
10, no form controls), so the composition target is a minimal editable surface
added for this phase and the first real consumer is Phase 10+.

**Windows.** TSF is COM: a text service client implements `ITextStoreACP2`
(about 30 methods) and sinks, all hand-written vtables with `windows-sys`, which
is the worst case for `unsafe`, and it belongs in the *UI* process, not the
audited `px-sandbox` core. The older IMM32 API is flat C: `WM_IME_STARTCOMPOSITION`,
`WM_IME_COMPOSITION`, `WM_IME_ENDCOMPOSITION`, `ImmGetContext`,
`ImmGetCompositionStringW`, `ImmSetCandidateWindow`, exposed by `windows-sys`
under `Win32_UI_Input_Ime`. It is still supported and modern IMEs expose an IMM
compatibility layer. `winit` (the chrome's window layer per Appendix A, Phase 18)
already implements IMM32 and surfaces `WindowEvent::Ime(Preedit | Commit | ...)`
plus `set_ime_cursor_area`, so the UI-side Windows work may be nil. Phase 9 should
therefore define the seam and build everything behind it, and treat "real TSF" as
a decision for the user (section 8), because the spec says TSF and the cheap
correct answer may be IMM32 through winit.

**Linux.** IBus and Fcitx are D-Bus services; the client must run in the UI
process (the content process has no sockets). Options:

| Route | New crates | Unsafe tokens | Notes |
|---|---|---|---|
| winit's IME (XIM on X11, `text-input-v3` on Wayland; IBus and Fcitx both provide XIM and text-input servers) | 0 beyond winit, which Phase 18 adds anyway | counted in winit's 15,629 | no D-Bus code of ours; XIM goes through `libX11` loaded at run time |
| `zbus` | 49 | 9,468 | rejected on count |
| `rustbus` | 5 | 760 (`nix` 723) | pure Rust, small; still a dependency and `nix` is the unsafe |
| **Hand-written minimal D-Bus client**, std only | 0 | 0 | *estimate* 1,200 to 1,800 lines: SASL `EXTERNAL` over a `UnixStream` (uid from `/proc/self` metadata, so no `libc`), message header and body marshalling with alignment and variants, `Hello`, method call/return/signal, then IBus: read the daemon address from `~/.config/ibus/bus/<machine-id>-<host>-<display>` (IBus runs its own bus, not the session bus), `CreateInputContext`, `ProcessKeyEvent`, handle `CommitText`/`UpdatePreeditText` (serialized `IBusText`, nested variant structs). Fcitx5 is a second, different interface (`org.fcitx.Fcitx.InputMethod1`, session bus). Real work and fully fuzzable. |

My reading: the hand-written client is feasible and keeps the dependency count at
zero, but it is the most expensive text unit after shaping and can be proven
**only against a real daemon**. winit's route is free if the Phase 18 dependency
is accepted anyway. This choice is for the user and does not need to be made in
Phase 9: the seam does.

## 7. Fonts to bundle for deterministic CI

Baselines must not depend on system fonts: ADR 033 gives one baseline set for
both OSes with tolerance 0, and that holds only if the glyph source is committed
bytes and rasterization is integer.

| Font | Source | Licence | Size | Role |
|---|---|---|---|---|
| **Ahem** 1.50 | WPT `fonts/Ahem.ttf` (also `http://dev.w3.org/CSS/fonts/ahem/`) | Public domain; CC0 where that is not recognised (stated in the font's `name` table) | **21,768 bytes**, SHA-256 `b719ecb3...b8448` | The WPT standard test font: 278 glyphs, UPM 1000, ascent 800, descent 200, every letter a 1000-unit square. `glyf` + `cmap` format 4 only, **no GSUB or GPOS**. |
| Noto Sans Regular | `notofonts` repo, `hinted/ttf` | SIL OFL 1.1, no Reserved Font Name declared in the header | full: 621,572 B | Latin, Cyrillic (and Greek) |
| Noto Sans Arabic | same | OFL 1.1 | full: 234,892 B | Arabic |
| Noto Sans Hebrew | same | OFL 1.1 | full: 26,860 B | Hebrew |
| Noto Sans Devanagari | same | OFL 1.1 | full: 243,520 B (GSUB 62,980 B) | Devanagari: a real Indic font is what exposes shaping bugs |
| A Noto CJK face | `notofonts` / Source Han | OFL 1.1 | not measured | CJK; **needs a `glyf`-flavoured face or CFF support** (section 3) |

Subsetting to the fixtures' glyphs (*estimate*: 15 to 60 KB each, about 150 to 250
KB for the lot; **not measured**) is a one-off dev action using a Rust subsetter
kept **outside the workspace** (Google's `skera`, or `subsetter`), with the
committed result being data: the subset font, its `OFL.txt`, and a provenance
file recording the source URL, commit and SHA-256 (the `tests/render/PROVENANCE`
pattern). OFL permits modification and redistribution with the licence text
attached. A modified font may not reuse a declared Reserved Font Name; the Noto
`OFL.txt` header declares none, so the subsets may keep the family name.

**Ahem matters on its own.** 285 of the 1,902 `.html` files in the vendored WPT
subsets name it: 47 of 210 in `css2`, 55 of 961 in `css-flexbox`, 183 of 731 in
`css-grid` (25% of grid). None uses `@font-face`: WPT assumes Ahem is installed
under that family name. Bundling Ahem and resolving `font-family: Ahem` to it
makes those 285 files measure real box sizes (a 20px Ahem glyph is exactly 20px
wide) instead of the stub's half-em, and it is the first font that can be wired in
before the shaper decision is made.

## 8. Findings the user should see

1. **swash vs HarfBuzz parity (section 2).** 83 of 89 strings identical; the six
   that differ are combining-mark order and one Devanagari reph. `harfrust` costs
   one more crate (7 vs 6 new) and 14 fewer unsafe tokens and is HarfBuzz itself.
   I am **not** re-proposing Appendix A. I am reporting that "swash" may not be
   enough for marked Arabic and Hebrew text, and that `harfrust` is the
   one-crate remedy, and waiting.
2. **The px-text invariant conflicts with ADR 033.** `crates/px-text/CLAUDE.md`
   and the §11 risk register say baselines are *per platform*. ADR 033's one set
   with tolerance 0 is achievable for **bundled fonts with integer rasterization**
   (swash gives font-unit advances, we scale in integers). Per-platform baselines
   are then only needed for *system* fonts, which should not be baselined at all.
   The ADR proposes the change; it does not make it.
3. **Fonts must cross the process boundary** (section 5), at 1 MiB per message.
4. **Everything Unicode is free.** The ICU4X data already in the closure covers
   line, grapheme, word, bidi class, mirroring, brackets, script and joining.
5. **No unsafe on the text path is needed** if rasterization is ours and system
   fonts are read with `std::fs` (Windows and Linux).
