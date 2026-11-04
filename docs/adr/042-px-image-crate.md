# 042 — Image decoding is a first-party, std-only crate: px-image

- **Status:** accepted, by delegation of the user's standing directive that the
  project take "as few dependencies as possible" (ADR 032, ADR 036). No
  external crate is added; this ADR records the decision not to add one.
- **Date:** 2026-11-04
- **Phase:** 15
- **Invariants touched:** none. It adds a crate, not a dependency, and the
  crate is `#![forbid(unsafe_code)]`. Build-spec section 3 is amended: its
  ecosystem list names the `image` crate; this ADR replaces that line.

## Context

Phase 15 needs raster image decoding for PNG, JPEG, GIF (animated) and the
small legacy formats, in a process that parses bytes from any web page.
Section 3 of the build spec lists the `image` crate. `image` is a facade: with
default features its closure includes the `png`, JPEG and `gif` codec crates,
`weezl`, the deflate crates, `bytemuck`, `byteorder` and others, several of
which carry `unsafe` and whose maintainers and audit status would each have to
be established. The project already has a PNG encoder and a bounded,
full-inflate PNG decoder in `px-paint` written for its own baselines, so a PNG
path exists and is understood.

## Decision

Write the decoders in a new workspace crate `crates/px-image`, std only, with
`#![forbid(unsafe_code)]` and the panic lints of section 4.3 (`unwrap`,
`expect`, indexing, `panic` denied). Scope: PNG with APNG, GIF with animation,
JPEG baseline and progressive Huffman, BMP and ICO. Output is RGBA8; animation
frames are full-canvas and composited. A single `Limits` value (pixels,
frames, total bytes) bounds every allocation and is checked before it.

Determinism is a design requirement, not a hope: no floating point appears in
any decode path. JPEG uses the libjpeg "islow" integer IDCT, libjpeg's
triangle ("fancy") chroma upsampling and 16-bit fixed-point YCbCr, so the
pixels are identical on every platform and, as measured on the fixtures,
identical to libjpeg-turbo (see the test header in `tests/jpeg.rs`).

`inflate` and the PNG decoder are copied from `px-paint` rather than shared:
`px-paint` depends on the style engine and a decoder crate must not. The
duplication is recorded in `docs/backlog.md`; later `px-paint` should depend
on `px-image`.

WebP is out of scope. VP8 and VP8L are large decoders; a later ADR decides.
Arithmetic-coded, 12-bit, lossless and CMYK JPEG are rejected as
`Unsupported`, never decoded wrongly.

## Alternatives rejected

- **The `image` crate and its codec closure.** Violates the as-few-
  dependencies directive; unsafe in the closure that this crate would then
  have to trust with hostile input; versions to track.
- **One external codec crate per format (`png`, `gif`, a JPEG crate).**
  Smaller closure than `image` but still several maintainers and an `unsafe`
  audit each, for formats whose decoders are a few hundred lines of safe Rust.
- **Platform decoders (WIC, gdk-pixbuf).** Not deterministic across
  platforms, would need capabilities the image process must not have, and
  break reproducible baselines.

## Consequences

- We own the bugs. Four decoders of hostile input is real attack surface; it
  is mitigated by the panic lints, the single allocation chokepoint and
  fuzzing, not removed.
- Performance is that of safe, straightforward code: a bit-at-a-time Huffman
  walk in inflate and JPEG, no SIMD. Acceptable for page images; it is not
  libjpeg-turbo.
- Not here yet: CMYK/YCCK JPEG, ICC profiles and EXIF orientation (parsed
  past, ignored), WebP. 16-bit PNG samples are reduced to 8 bits by taking the
  high byte.
- The `Limits` defaults are a policy choice (see `lib.rs`); an embedder with
  more memory may raise them.

## Verification

Wrong if any of these is observed: a fuzz campaign (24 h, the Phase 15 gate)
finds a panic or an unbounded allocation; a fixture decoded by this crate
differs from the independent oracle (Pillow) beyond the documented tolerance;
or users hit real images this crate rejects often enough that carrying the
decoders costs more than the closure it avoids.
