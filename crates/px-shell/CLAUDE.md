# px-shell

The interim browser binary for Phase 18: the `px-ui` chrome in a `px-display`
window, with a real page rendered by our own engine in the content area.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- **Everything runs in one process.** Parse, style, layout, paint and raster
  happen in-process here, next to the window. That is a deliberate stand-in, not
  the architecture: the real split (a content process that never holds a display
  connection, the broker owning the window and the files, frames crossing IPC)
  is Phase 19 integration. Nothing here weakens a rule in another crate; no
  sandbox, IPC or broker code is touched or bypassed, and `px-content` still does
  not link `px-display` (`cargo tree -p px-content`).
- Network only through `px-net`. `https://` and `http://` addresses (argv, address
  bar, links on a web page) load via `px_net::browse`, which runs every request
  through `SecureFetcher` (partitioned state, HSTS, CSP, mixed content, CORS,
  referrer policy, SRI, redirect rules). This crate never opens a socket and has
  no TLS code; the symbol audit (`cargo xtask network`) keeps it so. Only the
  navigation, its `<link rel=stylesheet>` files and its pictures are fetched
  (`<img>`, `<picture>`/`<source>`, `srcset`, `background-image`; see
  `src/images.rs` for the selection rule and the caps: 64 images, 8 MiB each
  compressed, 8 Mpx and 192 MiB of pixels decoded). Pictures go through
  `Loader::image`, a `Destination::Image` fetch on the same secure path, and are
  decoded by `px-image` (PNG/APNG, GIF, JPEG, BMP, ICO; first frame only; WebP
  and SVG are refused). A picture that fails for any reason is skipped and the
  page shows its `alt` text. Web fonts (`@font-face` `url()` sources) are fetched
  too, by `Loader::font`: a CORS-mode request, so a cross-origin font needs
  `Access-Control-Allow-Origin`; at most 32 rules, 8 MiB each, a 3 s phase budget
  and a 100 ms one for `font-display: optional`; unwrapped (TTF, WOFF, WOFF2) and
  validated by `px-text`; a font that fails or arrives late is absent and text
  falls to the next family (ADR 049). `local()` sources are never read. Scripts,
  frames and forms are never requested and never run. Nothing persists:
  the cookie jar lives for one page load, there is no cache and no history store.
  A local page cannot link out to the web. Any failure shows `px-pages`' error
  page. The loader is synchronous on the UI thread, in-process; the content /
  network process split and a non-blocking load are Phase 19.
- A local page reads pictures only from its own directory or below (`Resources::key`:
  no scheme, no absolute path, no `..`). A web page's pictures are `http(s)` URLs only;
  `data:` and `file:` are never fetched.
- The `testing` feature (`px-net/testing`) adds proxy support and an extra trust
  anchor file for sandboxes that only allow egress through a TLS-terminating
  proxy (`HTTPS_PROXY`, `PX_TEST_CA_BUNDLE`). It is scanned for in release
  artifacts and never enabled for a shipping build; verification is never off.
- No script execution, no forms, no animation of images: whatever the engine does not yet do
  is simply absent from the picture.
- Page-controlled text (titles) reaches the chrome only through `px_ui::tabs::Tab`,
  which sanitises it. The page never draws a pixel of chrome.
- Fonts: the bundled set through `px-layout` for the chrome and as the page's
  fallback, plus the page's own web fonts, plus the machine's installed faces for
  the six generic families only (`fonts::install_system_generics`, once at
  startup, in the browser binary; never in tests, which map generics per page).
  A page cannot probe for an installed font by name (`local()` is ignored); it
  can only see what a generic keyword resolves to.
- 100% Rust, workspace-path dependencies only.
