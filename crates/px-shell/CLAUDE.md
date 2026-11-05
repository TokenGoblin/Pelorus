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
- No network. A link whose target is not a local file or an `about:` page does
  nothing. Local files only, named on argv, typed in the address bar or linked.
- No script execution, no images, no forms: whatever the engine does not yet do
  is simply absent from the picture.
- Page-controlled text (titles) reaches the chrome only through `px_ui::tabs::Tab`,
  which sanitises it. The page never draws a pixel of chrome.
- Fonts are the bundled set through `px-layout` (never system fonts); the chrome
  and the page share them.
- 100% Rust, workspace-path dependencies only.
