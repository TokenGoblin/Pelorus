# px-display

The window layer: hand-written X11 (safe Rust over a `UnixStream`) and Win32
(`windows-sys`), implementing `px_ui::platform::PlatformWindow`. ADR 044,
accepted by the user 2026-11-05. No `winit`, `wgpu`, `softbuffer`; no native
Wayland (XWayland covers Wayland sessions).

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Third unsafe-permitted crate, same rule as `px-sandbox`:
  `#![deny(unsafe_op_in_unsafe_fn)]` and a `// SAFETY:` comment on every unsafe
  block. All unsafe lives in `src/win32.rs`; the `x11` module has none and must
  stay that way. `cargo xtask unsafe-headers` enforces the header, the comments
  and the next rule.
- Only the browser binary (`px-broker`) may depend on this crate. A content,
  GPU or network process never holds a display connection (invariant 1).
- The X11 decoder reads bytes from the server and from other applications. It
  carries the workspace panic lints (no unwrap/expect/panic/indexing); every read
  is bounds-checked and every length is capped (`proto::MAX_MESSAGE`). Fail
  closed: a protocol error closes the window.
- The socket and the Xauthority cookie come from the broker (`x11::discover` is
  that code, kept separate so the move is a file move). Nothing else here opens
  files or connects.
- `src/win32.rs` was written on Linux and has never run. Treat it as unverified
  until Windows CI has exercised it.
- Tests: `tests/xvfb.rs` starts a real Xvfb per test and skips (loudly) without
  one. `px-chrome-demo` (`--features demo`) shows the chrome in a window.
