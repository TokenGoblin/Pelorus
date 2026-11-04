# px-image

First-party, std-only image decoders: PNG (including APNG), GIF, JPEG
(baseline and progressive), BMP and ICO. Phase 15. ADR 042.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` -- it applies here too; this file only adds what is local.

## Local invariants

- No dependencies. std only (ADR 042). The `image` crate named in the build
  spec section 3 is replaced by this crate; do not add it, or any codec crate.
- Every byte decoded here is hostile. Held to the panic lints
  (`[lints] workspace = true`): no unwrap, expect, panic or indexing in
  non-test code. Use `get`, iterators and checked arithmetic.
- Allocation happens only after the limit check. Every buffer whose size comes
  from the input is obtained through `Budget` (`src/limits.rs`), which checks
  `Limits` first and uses `try_reserve_exact`. A new decoder allocating from a
  header field without `Budget` is a bug.
- Deterministic output. No floating point anywhere in a decode path: the same
  bytes give the same pixels on every platform. JPEG uses the libjpeg "islow"
  integer IDCT, fancy (triangle) chroma upsampling and fixed-point YCbCr.
- Output is always RGBA8 (straight alpha). Animated frames are full-canvas,
  already composited.
- Unsupported is an error, never a guess: arithmetic-coded, 12-bit, lossless
  and CMYK JPEG, WebP, and other unsupported features return `Unsupported`.
- `inflate.rs` and the PNG decoder started as a copy of
  `crates/px-paint/src/png.rs`; the duplication is recorded in
  `docs/backlog.md` and is to be removed by making px-paint depend on this
  crate. Do not make this crate depend on px-paint.
- Every format has hostile-input tests (truncation at every byte, byte
  corruption, bombs, random bytes) and a fuzz target `image_decode`.
