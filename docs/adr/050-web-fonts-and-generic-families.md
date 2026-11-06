# 050 — Web fonts (`@font-face`: TTF, WOFF, WOFF2) and generic font families, with no new dependency

> **Note, 2026-11-06:** decision 1 (CFF/OpenType and collections refused) and the
> `.otf` clause of decision 8 are superseded by ADR 052, which reads CFF outlines
> and `ttcf` collections. The rest of this ADR stands as written.

- **Status:** accepted for the interim single-process shell (Phase 18); the
  process placement in decision 9 is a Phase 19 obligation the user should
  confirm.
- **Date:** 2026-11-06
- **Phase:** 9 (text), 18 (shell integration)
- **Invariants touched:** none changed. Fail closed (a font that cannot be
  fetched, decoded or validated is absent, never partly used); no new network
  path (fonts go through `px-net`'s `SecureFetcher`); no new crate dependency
  (ADR 032 and the supply-chain gate are untouched: `px-text` gains a
  *workspace* path dependency on `px-image` for zlib, nothing external).

## Context

Real sites bring their own typefaces and icon fonts (private-use code points in
a WOFF2), and `font-family: serif | monospace | system-ui` should not all be the
same face. Before this change the engine had the bundled set only (Ahem and four
Noto faces, ADR 036), ignored `@font-face`, and mapped every generic family to
whatever the list's next name or the default gave. The system font scan
(`px-text::sysfonts`, ADR 036 (f)) existed but fed nothing.

Font files are hostile input and a classic attack surface. The engine's text
stack is already safe Rust end to end (our own `sfnt`/`glyf`/`cmap` reader,
`harfrust` and `skrifa` for shaping, neither with `unsafe`), so the work is to
unwrap the containers and bound everything, not to add a parser written in C.

## Decisions

1. **Containers.** `px_text::woff::load_font` accepts raw sfnt with TrueType
   outlines, **WOFF 1.0** and **WOFF 2.0** (including the `glyf`/`loca` and
   `hmtx` transforms), and unwraps them to a plain sfnt that the existing
   `Font::from_bytes` validates. **CFF/OpenType (`OTTO`) outlines and font
   collections are refused with a named error** (`FontError::Unsupported`); a
   page whose first source is CFF falls to its next source or family. This is the
   one gap: the outline reader is `glyf` only, and a CFF charstring interpreter
   is a separate unit of work.

2. **No new dependency; one decoder written, one reused.**
   * zlib for WOFF 1.0 is `px_image::inflate` (made `pub`): the bounded,
     panic-free inflater the PNG path already uses, reached through a workspace
     path dependency. A second copy would be a second thing to keep hostile-safe.
   * Brotli for WOFF 2.0 is `px-text/src/brotli.rs`, **hand-written** from RFC
     7932, about 700 lines of safe Rust. The only data it embeds are RFC 7932's
     own constants: the 122,784-byte static dictionary (`px-text/data/`, SHA-256
     `20e42eb1...f35c70`, pinned by a test), the 121 transforms with their
     prefix/suffix strings, and two context-lookup tables (`brotli_tables.rs`).
     Those were extracted mechanically from the MIT-licensed
     `brotli-decompressor` 5.0.3 source for this repository and are *data*, not
     code; the decoder itself was written against the RFC and checked against the
     reference encoder's output (six vectors at qualities 1 to 11 and windows
     16 to 24, including dictionary references, context modelling, block
     splitting and stored meta-blocks) and, once, against real-world fonts
     (a Google Fonts WOFF2 and DejaVu Sans, 6,253 glyphs, compared glyph by glyph
     with `fontTools`' reconstruction; nothing from that check is committed).
   * Not supported in Brotli: the large-window extension (no WOFF2 encoder uses it).

3. **Bounds, enforced before allocation.** A font file is at most 8 MiB
   (`MAX_FONT_BYTES`, checked by the loader and again by `woff::to_sfnt`); an
   unwrapped sfnt is at most 32 MiB; at most 64 tables; WOFF1 table sizes are
   checked against the declared total and each table's inflate is capped at its
   declared length; WOFF2 `totalSfntSize`, the sum of declared table sizes and
   the Brotli output are all capped, the output at the declared size *and
   checked before every append* (a bomb stops at the cap); the glyph, point,
   contour and component counts are limited by the streams that carry them; all
   coordinate arithmetic is checked. Nothing indexes, unwraps or panics
   (`px-text` denies those lints outside tests). Hostile tests: every truncation
   and every single-bit flip of the WOFF/WOFF2 fixtures, pseudo-random garbage
   behind valid signatures, header size lies, a zlib bomb, a Brotli cap test.
   Per process: at most 256 registered web faces and 128 MiB of unwrapped bytes
   (`px_layout::fontset`); over either, a font is refused and the page falls
   back. A font is validated by `Font::from_bytes` (every table it reads is
   range-checked) and parsed by the shaper's reader; hinting programs are never
   executed.

4. **Fetching.** `px_net::browse::Loader::font` is a `Destination::Font` fetch in
   **CORS mode with `same-origin` credentials** (CSS Fonts 4 section 4.3): a
   cross-origin font is used only if the server's `Access-Control-Allow-Origin`
   admits the document. It runs the whole secure path (partitioned state, HSTS,
   CSP `font-src`, mixed content, referrer policy, redirect rules). A local page
   reads fonts only from its own directory or below (`Resources::key`, the rule
   pictures use); it cannot reach the web or `file:` elsewhere. `data:` URLs are
   not fetched (as for pictures).

5. **`local()` sources are never honoured, and `format()` hints are respected.**
   Reading an installed font by name lets a page learn which fonts exist (it can
   measure whether text changed width), a fingerprinting oracle, and makes the
   page depend on the machine. A `url()` whose `format()` is something this
   engine cannot read (`svg`, `embedded-opentype`, `collection`, unknown) is
   dropped so the next source is tried, as the spec directs.

6. **No indefinite wait; deterministic fallback; no late swap.** The shell loads
   the page's fonts in one bounded phase before layout (`PHASE_BUDGET`, 3 s,
   CSS Fonts 4's block period; at most 32 `@font-face` rules; each URL once). A
   font not in hand by then is dropped and text uses the next family in its
   `font-family` list, so `font-display` reduces to: `auto`, `block`, `swap`,
   `fallback` use the font if it arrived within the phase; `optional` only if
   that one fetch took at most 100 ms. The page is laid out once and a font
   arriving later is never swapped in, so no page relays out on a late font and
   the result depends only on which fetches succeeded. (The loader itself is
   synchronous and has its own connect/read timeouts; the budget is checked
   before each fetch and after it returns. A genuinely non-blocking load is
   Phase 19.)

7. **Matching.** CSS Fonts 4 section 5.2 for style and weight, in
   `px_layout::fontset`: `font-style` preference first (italic, oblique, normal
   in the order the request implies), then the weight rules, then the **later
   rule wins** a tie. All faces of a family stay in the chain, ordered, so
   `unicode-range` composes several files into one family and a glyph missing from
   the best face falls to the next. No synthetic bold or italic, no `font-stretch`
   matching, no variable-font axes (a variable face is used at its default
   instance).

8. **Generic families.** `serif`, `sans-serif`, `monospace`, `cursive`,
   `fantasy` and `system-ui` resolve to **installed faces**: the browser binary
   scans the system font directories once at startup (`px-text::sysfonts` over
   the broker's `FsReader`, which never follows symlinks and caps reads), walks
   each generic's preference list in order, and takes the first installed name
   whose file registers as a TrueType font (a CFF-flavoured or collection file is
   skipped for the next name). A generic with no installed match is *skipped*, so
   the list continues: `font-family: monospace, "Noto Sans"` is Noto Sans, and
   with no list left the policy of `resolve_font` applies (bundled Noto Sans on
   the web, Ahem in conformance mode). Conformance mode installs nothing, so WPT
   expectations are unchanged. The deterministic render corpus must not read the
   machine's fonts, so a corpus page maps generics itself with a harness-only
   `<meta name="px-test-generics" content="serif=NAME;...">` to files in
   `tests/fonts`; a browser ignores that tag.

9. **Where it runs.** Today everything is in one process (ADR 044 and the shell's
   CLAUDE.md). The font code is pure: no file system, no network, no `unsafe`,
   which is what makes it confinable. In the Phase 19 split the **network
   process fetches the bytes and the content process unwraps and validates them**
   (`woff::to_sfnt` and `Font::from_bytes` run inside the content sandbox), and
   the GPU process receives only the *unwrapped* sfnt through the existing font
   chunk path (`px-gpu::FontSet::insert`, which rejects WOFF, and re-validates
   with `Font::from_bytes`), under the web ids (>= 65,536) layout assigns. Until
   then the shell parses in-process; a parser panic is contained by
   `catch_unwind` in `page::layout` and the font is a pure function with no
   shared mutable state beyond the bounded store.

10. **Lifetime.** A registered face is `Box::leak`ed so layout can keep handing
    out `&'static Font` and a background tab's display list stays valid. The store
    is capped (decision 3) and deduplicated by content, so the leak is bounded
    and reloading a page adds nothing.

## Alternatives rejected

* **A Brotli crate** (`brotli-decompressor`, `brotli`). Rejected by the user's
  constraint (no new dependency; ADR 032). It also carries `unsafe` in its
  default build and a `no_std` allocator abstraction we do not need. The hand
  decoder is bounded and tested against it.
* **A font sanitizer in C (OTS, as browsers use).** Not Rust (ADR 032), and our
  parse path never hands the bytes to a native library.
* **Honouring `local()`.** See decision 5.
* **Re-laying-out when a late font arrives (a real swap period).** It would make
  rendering a function of timing, and the shell's loader is synchronous. Revisit
  with the Phase 19 non-blocking loader.
* **Per-tab web font registries that free on navigation.** Needs ownership
  threaded through layout, paint and the GPU boundary. The capped, content-
  deduplicated, leaked store is smaller and cannot grow past a fixed bound.
* **Mapping generics to bundled faces only.** The bundled set has one sans face;
  "distinct serif and monospace" needs the machine's fonts. The fallback to the
  bundled face keeps a machine with none deterministic.

## Consequences

* A WOFF2 decoder and a Brotli decoder are new attack surface in safe Rust. Their
  failure mode is a refused font, never memory unsafety; a logic error could
  produce wrong glyphs from a hostile font, which is the same class as any
  rendering bug.
* The render baselines `webfont-*` and `font-generics` pin outlines, matching
  and fallback exactly; `PAGE_FLOOR` rises 75 -> 83 (it was already 4 below the
  real total of 79, now equal).
* Text in a web font is shaped by `harfrust` like any other, so ligatures,
  kerning and marks work when the font has the tables. CFF fonts do not render
  (decision 1).
* `px-shell` links `px-broker` (for the system scan reader only; no sandbox or
  IPC use). Until the process split this is the single place the shell touches
  the installed font directories.
* On a machine whose preferred system fonts are all CFF-flavoured (OpenType
  `.otf`), generics fall back to the bundled face.

## Verification

* Wrong if a fuzzed or truncated font panics, hangs, or allocates beyond the caps
  (the hostile tests run on every `cargo test -p px-text`); if a cross-origin
  font without `Access-Control-Allow-Origin` renders (shell web test); if two
  runs of one page differ (the render gate renders twice).
* Wrong if a real-world WOFF2 fails to decode. Only two were checked by hand
  (above); the Phase 20 corpus run should add the top sites' fonts.
