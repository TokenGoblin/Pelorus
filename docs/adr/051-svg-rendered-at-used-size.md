# 051 — SVG pictures are rendered again at the size they are drawn

- **Status:** accepted, as the follow-up ADR 049 recorded ("the used-size
  feedback"). No dependency, no new crate edge.
- **Date:** 2026-11-06
- **Phase:** 15 (Media)
- **Invariants touched:** none. The integer rasterizer, Au fixed-point layout
  and every ADR 049 safety bound are unchanged.

## Context

ADR 049 renders an SVG once, at its intrinsic size, before layout, and scales
the bitmap like any other image. Three visible defects follow:

1. an SVG shown larger than its intrinsic size is blurry;
2. an `<img>` forced to another aspect ratio is stretched, where a browser
   re-runs the SVG's `preserveAspectRatio` in the new viewport;
3. an inline `<svg>` with a `viewBox` and no `width`/`height` is 300x150
   contained, where a browser makes it as wide as its container.

A fourth, noted in ADR 049: page style sheets (`.icon path { fill: red }`) did
not reach inside an inline `<svg>`.

## Decision

**Keep the intrinsic raster and add the document.** Layout still receives the
intrinsic size from the raster (the table `layout_document_with_images` already
takes); nothing about replaced-element sizing changes except item 3 below. For
every `<img>` or inline `<svg>` whose picture is an SVG, the embedder also hands
paint the parsed `px_image::svg::Document` (`ImageSet::vectors`, with the
`currentColor` to use). At paint time, where the element's destination rectangle
is known (`object-fit` and `object-position` already resolved from the intrinsic
size), `px-paint` renders the document at the snapped destination size and emits
an ordinary `Item::Image` at 1:1. The rasterizer's scaler copies it exactly.

- It fixes (1) and (2) together: `Document::render(w, h)` already maps the
  viewBox into a `w x h` canvas with the root's `preserveAspectRatio`, so the
  same call that makes the picture sharp also gives it the right placement in a
  box of another shape. `object-fit` is unchanged: it picks the destination from
  the intrinsic ratio, the document then fills that destination.
- No display-list, IPC or GPU-process change. The list still carries only
  `ImageData`; the GPU/raster side never sees a path. The display list is built
  where the pictures already are, so nothing crosses a process boundary that did
  not before.
- Deterministic: the renderer is the ADR 049 integer one; the destination size
  is computed with the rasterizer's own snapping in `Au` integers. No float is
  added anywhere, and `px-layout` gains none.
- **Device pixels.** The rendered size is the destination in the display list's
  pixels. Today one display-list pixel is one device pixel (there is no scale
  factor in paint); a future HiDPI scale multiplies the render size at this one
  place (`imgpaint::content_items`) and the result stays sharp.

**Bounds.** Every ADR 049 limit applies to the re-render (it is the same
`Document::render` with `Limits`). Added, in `imgpaint`: 8M pixels per
re-rendered picture, 96 MiB and 512 distinct renders per display list, and a
cache keyed by document, size and colour so an icon used many times at one size
renders once. A re-render that is over a bound or refused falls back to scaling
the intrinsic raster, which already passed every bound: a document over a bound
at the *intrinsic* size is still refused whole and the page shows the `alt`
text, as before. Nothing about a hostile document gets cheaper to abuse by being
shown large: the cost of a render is bounded by pixels and by the renderer's
work counters, not by the page's CSS size (a 100,000 px wide `<img>` is simply
over the pixel bound and is drawn from the raster, clipped as before).

**Ratio without size (item 3).** `Document::is_ratio_only()` is true for a
`viewBox` and no absolute `width`/`height`. The embedder passes those nodes to
`px_layout::replaced::layout_document_with_pictures`; when neither dimension is
specified, such an element's used width is its containing block's content width
(less its own border and padding), and the height follows the ratio, then
`min/max-width/height` as for any replaced element (CSS Sizing 3 section 5.1,
CSS 2.1 section 10.3.4). It applies to `<img src=x.svg>` as well as inline
`<svg>`. The table is a second thread-local beside the existing one, installed
and cleared by the same guard: no signature in `block.rs` changed. The
min-content pass (no containing block) still sees the intrinsic size. Margins
are not subtracted from the fill width; a ratio-only `<svg>` with a horizontal
margin and no width overflows by the margin (recorded in the backlog).

**Page CSS reaches inline `<svg>`.** The page's author CSS text is given to the
inline document (`Document::add_page_style`) with the element's HTML ancestors
(tag, id, class, innermost first, at most 32) so selectors can look past the
`<svg>` root. It is the same small selector subset the document's own `<style>`
uses: type, `*`, `.class`, `#id` and compounds joined by descendant and child
combinators. A rule using anything else (pseudo-classes, attribute selectors,
sibling combinators), every at-rule (`@media` included) and `!important`
(dropped, not honoured) is not applied. Page rules rank above presentation
attributes, as in CSS, and below the document's own `<style>` at equal
specificity. Bounds: 2 MiB of CSS text scanned, 50,000 rules parsed, then only
rules that match something in this document are kept, and at most 4,000,000
rule-by-element tests are made (over that the page CSS is not applied, the
picture still draws). It reuses the existing style path of `px-image`; the HTML
style engine is not involved, so no stylo change and no new property support.
`color` still comes from the computed style, so `currentColor` already followed
page CSS.

## Alternatives rejected

- **Vector items in the display list.** The exact answer, but it needs path,
  gradient and clip items, IPC and GPU-process encodings, and a second
  rasterizer for them, in crates others are editing. The paint-time render gives
  the same pixels for the cases that matter (it *is* the vector renderer) with
  one new function.
- **A layout-to-paint feedback that re-decodes after layout.** Layout would have
  to run twice or hand sizes back through the shell; the used size is only final
  at paint, and `object-fit` and `object-position` are paint's. Rendering in
  paint avoids a second layout.
- **Letting stylo style the SVG subtree.** The Servo build of stylo has no SVG
  presentation properties (`fill`, `stroke`); using it would mean adding them to
  `px-css`, which is another agent's area and a much wider change than the
  subset matcher that already exists.

## Consequences

- `px-paint` renders in the display-list build: a page with many large SVGs
  costs render time there (bounded as above) rather than at decode time.
- `ImageSet` and `px_shell::Resources` carry a document next to each SVG raster
  (`vectors`); memory is the parsed tree, small beside the pixels.
- SVG in `background-image` is still the intrinsic raster scaled (blurry when
  enlarged). The tile size is also known at paint; it is recorded in the backlog
  as the next step. `<picture>` and `srcset` need nothing: they resolve to the
  `<img>`'s key, which has the document.
- The corpus pages `svg-used-size`, `svg-inline-fill` and `svg-inline-css`,
  `crates/px-image/tests/svg.rs`, `crates/px-layout/tests/replaced.rs` and
  `crates/px-paint/tests/images.rs` pin the behaviour; `svg-img` and
  `svg-inline` were re-blessed (the viewBox-only chart now fills its width; the
  small icons are no longer downscaled from 150 px).

## Verification

Wrong if: a render at a large used size takes unbounded time or memory (the
per-picture and per-page bounds, the fuzz target and `tests/svg.rs`
`page_style_is_bounded` are the checks); two platforms draw the same page to
different bytes; page CSS makes an inline SVG draw something it could not draw as
a standalone file (it can change only what the document's own `<style>` could);
or real sites rely on page-CSS features (media queries, pseudo-classes, `inherit`
chains) so often that the subset is not worth keeping.
