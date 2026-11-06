# 049 — SVG is rendered statically by a first-party renderer in px-image

- **Status:** accepted, by delegation of the user's standing directive that the
  project take "as few dependencies as possible" and be 100% Rust (ADR 032,
  ADR 042). No crate is added; this ADR records the decision not to add one
  (`resvg`, `usvg`, `tiny-skia`, `roxmltree`, `svgtypes` and their closure) and
  the safety rules for the code that is written instead.
- **Date:** 2026-11-06
- **Phase:** 15 (Media)
- **Invariants touched:** none. Adds a module to an existing crate and one
  workspace dependency edge (`px-paint` to `px-image`), no third-party crate.

## Context

ADR 042 left SVG out: `px-net` refused it, `px-image` never saw it, so
`<img src=x.svg>`, `background-image: url(x.svg)` and inline `<svg>` drew
nothing. SVG is the most common icon and logo format on the web.

SVG is also the image format with the most ways to hurt a browser. As a
document it can carry script, event handlers, `<image>`/`<use>`/`<foreignObject>`
references that reach the network or the filesystem, CSS `@import` and
`url()`, XML entities and external DTDs, animation, and cheap constructs that
cost a lot to draw (exponential `<use>` trees, enormous dash patterns, paths
with millions of segments, huge canvases). The browser treats SVG used as an
image as a picture: static, with no script and no loads. That is the line this
decision draws too.

## Decision

Render SVG with `px-image::svg`, hand-written, std only, `forbid(unsafe_code)`,
under the same panic lints as the other decoders. An SVG document decodes (via
`px_image::decode`/`sniff`, `Format::Svg`) to RGBA8 at its intrinsic size like
any other still image, so `<img>`, CSS background images, `<picture>` and the
image loader work unchanged. Inline `<svg>` elements are replaced elements
whose picture is made from their own subtree (`px_paint::svg::inline_images`),
with `currentColor` taken from the element's computed `color`.

**Safety rules** (each has a test in `crates/px-image/tests/svg.rs`):

- The renderer never runs script and never reads event attributes, `<script>`,
  `<animate>`/`<set>`, `<image>`, `<foreignObject>` or `<text>`. They are not
  rendered, children included.
- Nothing is loaded. `href` is followed only when it is `#id` in the same
  document; `url()` in a paint or `clip-path` likewise. Anything else is as if
  absent (a paint falls back to its fallback colour, else paints nothing).
  Stylesheet at-rules, `@import` included, are skipped. `px-image` has no
  network, filesystem or loader code at all.
- XML entities: only the five predefined and numeric references exist. A
  `DOCTYPE` is skipped unread, so declared entities (including `SYSTEM` ones)
  are never defined; a reference to one is a parse error. There is nothing to
  expand, so there is no expansion bomb.
- Hard bounds, each a `DecodeError::LimitExceeded`: source 16 MiB; 50,000
  elements; nesting depth 64; 128 attributes per element; 256 KiB of style
  text and 1024 rules; 100,000 path segments, 400,000 flattened points per
  shape, 4,000,000 per document; 2,000,000 raster edges per shape; 100,000
  dash pieces; 200,000 element visits (this stops `<use>` trees; `use` chains
  are also capped at depth 12 and cycles are refused); four nested compositing
  layers; a work counter on the rasterizer; canvas pixels and bytes through
  `Limits`/`Budget`. A document that passes a bound is refused whole, and the
  page shows the `alt` text.
- Inline `<svg>`: at most 256 per page and 64 MiB of pixels; its subtree gets no
  more authority than an SVG file would (page stylesheets reach it from
  2026-11-06, ADR 051, through the same selector subset and extra bounds).

**Numbers.** No floating point (ADR 042's rule for decode paths, applied to the
whole renderer). Lengths and matrices are Q16.16 in an `i64` that saturates;
angles are Q30 radians from a CORDIC with a constant table; square roots are
integer. The rasterizer samples 16 scanlines per pixel row with exact 1/256
pixel horizontal spans and a real winding count (non-zero and even-odd), so
overlaps and self-intersections are right and the output is identical on every
platform. Strokes are unions of per-segment quads and per-join and per-cap
polygons filled non-zero.

**Supported:** `svg` (nested), `g`, `a`, `switch`, `defs`, `symbol`, `use`,
`rect`, `circle`, `ellipse`, `line`, `polyline`, `polygon`, `path` (all
commands, arcs included); fill, stroke, width, caps, joins, miter limit, dash
array and offset, opacity (as a group), fill rule; `transform`; `viewBox` and
`preserveAspectRatio`; colours (hex, `rgb()`, `hsl()`, names), `currentColor`;
linear and radial gradients (units, transform, spread, `href` inheritance);
`clip-path` to a user-space `clipPath`; presentation attributes, `style=""` and
`<style>` with type, class, id and descendant/child selectors.

**Not supported (skipped, not approximated):** `text`, `image`, `mask`,
`pattern`, `marker`, `filter`, `foreignObject`, bounding-box `clipPath` units,
nested-`svg` overflow clipping, radial focal points, animation.

**Two limitations of the bitmap approach** (superseded 2026-11-06 by ADR 051:
the document is rendered again at the used size at paint time, a viewBox-only
SVG fills its container, and page CSS reaches inline `<svg>`; what follows is
the state as first accepted). The picture is rendered once at the
intrinsic size, before layout, and then scaled like any raster. So (1) an SVG
shown larger than its intrinsic size is blurry, and (2) an `<img>` given a
different aspect ratio is stretched, where a browser would re-run the SVG's
`preserveAspectRatio` in the new viewport. Fixing both needs the used size
before the picture is made, a layout-to-paint feedback this phase does not have;
it is recorded in `docs/backlog.md` as the next step. Inline `<svg>` with a
`viewBox` and no `width`/`height` is 300x150 contained, not 100% wide.

## Alternatives rejected

- **`resvg` (with `usvg` and `tiny-skia`).** The best-known renderer, but it is
  a closure of a dozen crates (`roxmltree`, `svgtypes`, `kurbo`, `rctree`,
  `tiny-skia` with its `unsafe` SIMD paths, `strict-num`, ...), uses floating
  point in its pixel path (different results across CPUs and compilers, so
  exact baselines cannot be pinned), and implements much more than a static
  subset, including text and filters, whose attack surface we do not want
  here. Violates the as-few-dependencies directive.
- **A smaller XML crate (`roxmltree`, `quick-xml`) only.** Saves about 250
  lines of parser, adds a maintainer and an audit, and the parser is where the
  entity and depth rules must be exactly ours.
- **Reuse `px-dom` (html5ever) as the XML parser.** It parses HTML, not XML:
  it is not strict, applies HTML error recovery and foreign-content rules, and
  would put a very large dependency inside the image crate. It is used for
  inline `<svg>` (the DOM is already there), by copying that subtree into
  `px-image`'s small document model.
- **Draw into `px-paint`'s display list.** Vector items would make scaling
  exact, but need path and gradient items in the display list, GPU-process and
  IPC encodings and rasterizer changes in a crate others are editing. Revisit
  with the used-size feedback above.
- **Wire SVG through `Content-Type` only.** The sniffer decides, never the
  label, as for every other image; `image/svg+xml` merely passes the "is an
  image label" check.

## Consequences

- We own the bugs. A hand-written XML parser, CSS subset and rasterizer on
  hostile input is real attack surface; it is mitigated by the panic lints, the
  saturating arithmetic (release builds trap on overflow), the bounds above,
  the hostile-input battery (truncation, corruption, random bytes, mutated
  attributes, bombs) and the `image_decode` fuzz target, not removed.
- `px-paint` now depends on `px-image` (a workspace crate; ADR 042 had said it
  eventually should). `px-net` gains an SVG text sniff (a few lines, no new
  edge); `px-layout` skips the children of a replaced element with a picture.
- Rendering speed is that of straightforward integer code: tens of
  milliseconds for an icon, a second or so for a full-page illustration.
- SVG fonts and `text` are absent, so many logos that rely on text show
  without it.

## Verification

Wrong if any of these is observed: the fuzz campaign finds a panic, a hang or
an unbounded allocation in `svg`; an SVG used as an image causes a network
request, a filesystem read or script execution (the render corpus page
`svg-hostile` and `tests/svg.rs` pin the known vectors); two platforms render
the same document to different bytes; or real icon sets lean on unsupported
features (text, masks, patterns) so often that the subset is not worth its
code.
