# Phase 18 window and input stack: what the candidates measure

*Research only. No `Cargo.toml`, `Cargo.lock` or source file in this repository
was changed. Date: 2026-11-04. Measured against `origin/main` at `7844b785`
(main later moved to `33a92651`; the only `Cargo.lock` change between them is a
first-party edge `px-block -> px-websec`, so no figure here is affected).
Nothing here is a decision; ADR 044 (proposed) turns it into one.*

Build-spec Appendix A settles "hand-rolled widgets on winit + wgpu; no general UI
toolkit". This document does not argue for a toolkit. It measures what *winit*
costs, because the project directive since the dependency audit is "as few
dependencies as possible", and it prices the hand-written alternative with code
that was actually written and run. Where the numbers suggest the winit half of
that Appendix A row looks wrong for this project, section 9 says so and stops.

## 0. Method, and what was not measured

- **Scratch projects.** One throwaway cargo project per candidate in a
  `mktemp -d` directory outside the repository, versions pinned with `=`. Each got
  a **copy of this repository's `Cargo.lock`** before resolution, so Cargo prefers
  locked versions (`libc`, `bitflags`, `windows-sys` 0.52 and 0.61, `bytemuck`
  are reused instead of re-resolved). Nothing was built except two hand-written
  prototypes (section 5) and the line counter. The scratch directory and its
  target directories were deleted afterwards. The resolver was the default
  `cargo 1.97.0`, not the pinned `1.98.1`; this can move a patch version, not a
  crate list.
- **Closure.** `cargo tree --edges normal --prefix none --target T` with `T` =
  `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`, de-duplicated on
  `name version`, root excluded. Build-script-only crates (`pkg-config`, `cc`,
  `cfg_aliases`) are not in these counts; section 3 lists them separately.
- **New.** Measured against `Cargo.lock` (271 name+version pairs, 259 names,
  including the 21 workspace crates). *New names* is the headline: a crate whose
  name is absent from the lock. *New pairs* also counts a second version of a
  name that is locked. Both are given because Cargo would unify some of the
  second kind.
- **Unsafe.** The token `unsafe` counted **by the gate's own function**:
  `count_unsafe` and `count_crate` from `crates/px-xtask/src/gates/unsafe_audit.rs`
  were copied verbatim into a scratch binary and run over each *new* crate's
  registry source, every `.rs` file regardless of features. It counts comments and
  strings and over-counts by design. The real figure comes from `cargo xtask
  unsafe-audit --write` after vendoring; that has not been run. Baseline today:
  21,675 tokens (`windows-sys` 0.61.2 = 12,530, 0.52.0 = 3,411).
- **Why these numbers differ from `phase-09-text-stack.md`.** That document, on a
  fresh resolve, gave winit default as 52 new crates and 15,629 tokens. Against
  the lock as it is now (which already holds `bytemuck`, `libc`, ...) it is
  **49 new names / 14,947 tokens**. Same crate, later lock, same method.
- **Not measured.** Anything on Windows beyond resolution and a `cargo check`
  (no Windows machine; the Windows prototype was **never run**). Any Wayland
  compositor (none here; the Wayland hand-written figures are estimates and are
  labelled). Any GPU. Real IME, real screen reader. Throughput of the Phase 8
  strip transport (never timed in this tree).

## 1. Headline

Unique crates in the closure (**total**), crates new to the lock (**new names**),
and unsafe tokens those new crates add, Linux / Windows.

| Candidate | Total L / W | New names L / W | New pairs L / W | Unsafe added L / W | % of 21,675 L / W |
|---|---|---|---|---|---|
| **winit 0.30.13**, default features | 68 / 13 | 48 / 8 | 53 / 8 | 14,947 / 1,022 | 69% / 4.7% |
| winit, `x11` only | 37 / 13 | 21 / 8 | 25 / 8 | 13,538 / 1,022 | 62% / 4.7% |
| winit, `wayland` + `wayland-dlopen` only | 51 / 13 | 34 / 8 | 39 / 8 | 14,471 / 1,022 | 67% / 4.7% |
| winit, `x11` + `wayland` + `wayland-dlopen`, no Adwaita CSD (**smallest useful**) | 58 / 13 | 39 / 8 | 44 / 8 | 14,762 / 1,022 | 68% / 4.7% |
| winit 0.31.0-beta.4, default | 71 / 17 | 52 / 12 | 53 / 12 | 10,305 / 770 | 48% / 3.6% |
| softbuffer 0.4.8, no features (needs a window handle from someone) | 5 / 7 | 4 / 4 | 4 / 4 | 157 / 157 | 0.7% |
| softbuffer, `x11` + `wayland` | 34 / 7 | 22 / 4 | 22 / 4 | 9,573 / 157 | 44% |
| winit (x11+wayland) **+** softbuffer (x11+wayland) | 62 / 16 | 42 / 9 | 47 / 9 | 15,005 / 1,109 | 69% / 5.1% |
| winit (x11) **+** softbuffer (x11) | 41 / 16 | 24 / 9 | 28 / 9 | 13,781 / 1,109 | 64% / 5.1% |
| minifb 0.29.0, `x11` + `wayland` | 32 / 4 | 20 / 4 | 20 / 4 | 9,402 / 292 | 43% |
| `x11rb` 0.14, pure-Rust connection (Linux only) | 6 / n/a | 5 | 5 | 8,694 | 40% |
| `x11rb-protocol` 0.14 alone (sans-IO encoder, Linux and Windows identical) | 1 | 1 | 1 | **0** | 0% |
| `wayland-client` 0.31.15, pure-Rust backend (Linux only) | 14 / n/a | 8 | 8 | 8,880 | 41% |
| **Hand-written, std only: X11 over `UnixStream`** | 0 | 0 | 0 | **0** | 0% |
| **Hand-written: Win32 via `windows-sys` 0.61.2 (already in the lock)** | 0 | 0 | 0 | **0 baseline change**, 10 first-party tokens in the prototype | 0% |

Reading the table:

1. **Shrinking winit's feature set barely changes its unsafe count**: 14,947 for
   the default down to 13,538 for X11-only. 80% of it is two crates, each present
   in two versions (`calloop` 0.13 pulls `rustix` 0.38.44; `polling`, `gethostname`
   and winit's `x11rb` pull 1.1.6): `linux-raw-sys` 0.12.1 (6,932) + 0.4.15 (1,775) and
   `rustix` 1.1.6 (1,678) + 0.38.44 (1,540) = **11,925 of 14,762** for
   x11+wayland. `linux-raw-sys` is generated syscall bindings; the gate counts
   text, not use. The honest framing is the same one ADR 034 used for `ash`: the
   figure measures the size of the source we would vendor, and it ratchets the
   baseline by 69% whichever way it is configured.
2. **On Windows winit costs 8 small crates and 1,022 tokens** (938 of them its
   own). It also makes `windows-sys` **0.52.0** (3,411 tokens, already in the
   baseline) a built dependency instead of a lock-only one, so the baseline
   entry stops being dead weight; nothing is added to it.
3. **Pure-Rust protocol libraries do not escape `rustix`.** `x11rb` and
   `wayland-client`'s Rust backend each bring `rustix` 1.1.6 and `linux-raw-sys`
   0.12.1 (8,610 tokens). The only candidate with zero unsafe is `x11rb-protocol`
   (1 crate), which does no I/O, and is **129,220 lines** of generated code
   (`x11rb-protocol-0.14.0/src`, non-blank non-comment) that no human will audit.
4. **`windows-sys` costs nothing to use.** The gate counts the whole crate source
   whatever features are on, so enabling `Win32_UI_WindowsAndMessaging` and
   friends changes the baseline by 0 and the lock by 0. Section 5.1.
5. The row labelled wgpu is not in the table: ADR 034 measured it (+43 lockfile
   entries, +342,729 tokens, 95% from the `windows` crate) and the user deferred
   it. Nothing in presenting a CPU framebuffer needs it (section 2).

## 2. How a software-rendered Canvas reaches a window with no GPU

The software rasterizer returns `px_paint::Canvas { width, height, pixels }`,
**RGBA8, row-major, `pixels.len() == width * height * 4`**
(`crates/px-paint/src/raster.rs:140`), at most 2^25 pixels and 16,384 per side.
Every platform has a call that takes exactly that, after a per-pixel swizzle to
BGRA:

| Platform | Call | Notes |
|---|---|---|
| Windows | `StretchDIBits` (or `SetDIBitsToDevice`) with a top-down `BITMAPINFOHEADER`, 32 bpp, `BI_RGB`, inside `WM_PAINT` | no GPU, no DWM requirement; GDI in the browser process only |
| X11 | core `PutImage`, format `ZPixmap`, depth 24 | no MIT-SHM. Request size is capped at the server's `maximum-request-length` (65,535 words = 262,140 bytes without BIG-REQUESTS), so the frame is sent in row bands. Measured section 5.2 |
| Wayland | `wl_shm` pool (memfd + `mmap`), `wl_buffer`, `wl_surface.attach/damage/commit` | needs fd passing (`SCM_RIGHTS`), section 5.3 |

`softbuffer` is this table as a library. It needs an object implementing
`raw-window-handle` (a winit window, or our own), and on Linux its `x11` +
`wayland` features add 22 new crates and 9,573 tokens on their own; on Windows 4
crates and 157 tokens. Presenting does not need `wgpu`, `ash`, `windows` or any
GPU driver. Whether to bring wgpu in is a rasterization question (ADR 034), not
a windowing one, and the window layer can present GPU-rasterized frames the same
way because the GPU process returns them as bytes (strips, section 7).

**One thing this exposes.** The Phase 8 GPU process cannot own a swapchain even
if it wanted to: its stage-two profile denies `socket`/`connect`/`open`
(ADR 033 (e)1, ADR 035), and a Vulkan or DX12 present needs a connection to the
display server. So under the current process model every frame, GPU-rasterized
or not, is read back, shipped in strips, and uploaded by the process that holds
the display connection. Zero-copy would need a shared surface (dmabuf or a
shared texture handle): ADR 033 (e)4 earmarked "in-band handle passing for a
shared surface" for "the live window (Phase 18)". It is not needed for a correct
Phase 18, and it is the thing that would make a window layer with its own
display connection and a GPU process disagree about who owns the surface.

## 3. Build requirements

Checked in the crates' `build.rs` files, not assumed.

| Crate | Build script behaviour | Consequence for "100% Rust" and invariant 7 |
|---|---|---|
| `wayland-sys` 0.31.11 | with feature `dlopen` returns at once; **without it, `pkg_config::Config::probe("wayland-client").unwrap()`** | winit's default has `wayland-dlopen`: no build-time C. Turn it off and a build needs `libwayland-dev` and `pkg-config`; it fails otherwise |
| `wayland-backend` 0.3.17 | compiles `log_shim.c` with `cc` **only if** feature `log` is on | winit does not enable `log` (checked in `cargo tree -f '{p} [{f}]'`), so no C compiler runs |
| `x11-dl` 2.21.0 | `build.rs` calls `pkg_config::get_variable` for **16** libraries (`x11`, `xcursor`, `xi`, `gl`, `xft`, ...) and writes their `libdir` into generated code; a miss is tolerated | no C compiler, but **a path from the build machine's pkg-config database is baked into the binary**. Two machines with different `libdir`s produce different bytes: this touches invariant 7 (reproducible builds) and should be checked with `cargo xtask build-and-hash` before any decision |
| `winit` 0.30.13 | `cfg_aliases` (pure Rust) | none |
| `x11rb` 0.13.2 (inside winit) | feature `dl-libxcb` dlopens `libxcb.so.1` | runtime C, below |

So the smallest winit does not need a C toolchain or cmake, but it does need
`pkg-config` on the build machine (x11-dl, wayland-sys's build-dependency) and
bakes its output in. The hand-written candidates need neither.

**Runtime C libraries loaded into the process that owns the window**, from the
sources (`grep` of the `dlopen` names; not run, no display here): winit `x11`
loads `libX11.so.6`, `libXcursor.so.1`, `libXi.so.6` (`x11-dl`, used for XIM
and cursors), `libxcb.so.1` (`x11rb`), `libxkbcommon.so.0` and
`libxkbcommon-x11.so.0` (`xkbcommon-dl`); `wayland` loads
`libwayland-client.so.0` and `libwayland-cursor.so.0` (`wayland-backend` with
`client_system`, the C implementation; the pure-Rust backend is not an option
inside winit). That is up to **eight C libraries inside the privileged browser
process** (six for X11 alone, two more for Wayland), parsing data from the display server and from other applications'
clipboards and input methods. The project's rule is "100% Rust" for first-party
source; this is not a breach of the letter, and it is the opposite of the spirit
that "memory-safe application code over a small audited unsafe core" (build-spec
section 4) is written to protect. On Windows the equivalent is `user32`, `gdi32`,
`imm32` which the OS provides to every process anyway.

## 4. winit's own surface, as shipped

Source line counts (non-blank, non-comment, `.rs`), `winit-0.30.13`:

| Directory | Lines |
|---|---|
| `platform_impl/linux/x11` | 7,406 |
| `platform_impl/linux/wayland` | 4,198 |
| `platform_impl/windows` | 7,598 |
| `softbuffer-0.4.8/src/backends` (all three platforms) | 2,828 |
| `smithay-client-toolkit-0.19.2/src` | 8,619 |
| `wayland-backend-0.3.17/src/rs` | 3,341 |

These are for scale: what a complete, battle-tested layer for the same features
weighs, against the hand-written figures in section 5.

Checked in winit's source: it has **no handling of `WM_GETOBJECT`** (grep returns
nothing), so UIA through winit means subclassing the `HWND` from outside or
taking AccessKit's adapter (section 8). It exposes IME as `WindowEvent::Ime(Enabled |
Preedit | Commit | Disabled)`, plus `Window::set_ime_cursor_area`,
`set_ime_allowed` and `set_ime_purpose` (`src/window.rs:1248, 1288`).

## 5. The hand-written minimum, measured

Two prototypes were written in the scratch directory, outside the repository
(and deleted with it). They are evidence for line counts and feasibility; they are
**not** proposed code.

### 5.1 Windows: `windows-sys` 0.61.2, already in the lock

- **Features needed**, all additive on the existing `px-sandbox` list
  (`Win32_Foundation`, `Win32_Security`, `Win32_System_Threading`,
  `Win32_System_JobObjects`, `Win32_System_Pipes`, `Win32_System_Console`,
  `Win32_Security_Cryptography`): `Win32_UI_WindowsAndMessaging`,
  `Win32_Graphics_Gdi`, `Win32_System_LibraryLoader`, `Win32_System_DataExchange`,
  `Win32_System_Memory`, `Win32_UI_Input_Ime`, `Win32_UI_HiDpi`. All verified to
  exist in `windows-sys-0.61.2/Cargo.toml`. (`CF_UNICODETEXT` is in
  `Win32_System_Ole`; the prototype defines the constant, value 13, instead.)
- **Prototype**: window class and `wndproc`, `WM_PAINT` presenting a BGRA
  framebuffer with `StretchDIBits`, `WM_SIZE`, `WM_DPICHANGED` (per-monitor-v2
  awareness), `WM_CLOSE`, `WM_KEYDOWN/UP` and `WM_CHAR` (with surrogate pairing),
  mouse buttons, motion and wheel, **IMM32 composition** (`WM_IME_COMPOSITION`
  with `GCS_COMPSTR`/`GCS_RESULTSTR` through `ImmGetCompositionStringW`, and
  `ImmSetCandidateWindow` for the caret rectangle), text clipboard get and set.
  **309 lines, 270 of code, 10 `unsafe` tokens, 8 `// SAFETY:` comments.**
  `cargo +1.98.1 check --target x86_64-pc-windows-msvc` passes with no warning.
  **It was never run: there is no Windows machine here.**
- **Not in the prototype and not free**: a frameless window with the approved
  squircle chrome needs `WM_NCCALCSIZE` and `WM_NCHITTEST` (drag, resize edges,
  caption buttons) and DWM corner and shadow attributes; `WM_GETOBJECT` for UIA;
  drag and drop; high contrast; message-loop integration with the broker's IPC
  (`MsgWaitForMultipleObjects` or a timer); clipboard formats beyond text. My
  estimate for a production Windows layer with those: **600 to 900 lines and 30
  to 80 unsafe tokens**. An estimate, not a measurement.
- **TSF is a different order of magnitude.** `ITextStoreACP2` is about 30 COM
  methods written as vtables (Phase 9 research, section 6). IMM32 is flat C and
  still works through the compatibility layer; the prototype covers it.

### 5.2 Linux X11: `std::os::unix::net::UnixStream`, no unsafe

**Written, run, and verified** against a real X server (Xvfb 1024x768x24) in this
container. The client connects, authenticates (MIT-MAGIC-COOKIE-1 from
`$XAUTHORITY`/`~/.Xauthority`, or none), creates and maps a window, interns
atoms, sets `WM_NAME` and `WM_DELETE_WINDOW`, **presents an RGBA framebuffer
with `PutImage` in row bands**, reads `Expose`, `ConfigureNotify`, key, button and
motion events, maps keycodes to keysyms with `GetKeyboardMapping`, and does the
CLIPBOARD selection both ways (owner, `SelectionRequest`, `ConvertSelection`,
`GetProperty`, UTF8_STRING).

- **364 lines of code** (411 including the test driver), `#![forbid(unsafe_code)]`,
  zero dependencies, zero `libc`.
- **What the run showed.** A 320x200 gradient written with `PutImage` read back
  through `GetImage` as `[10, 20, 128]` at (10, 20); an *independent* screenshot
  with ImageMagick `import -window root` showed `srgb(10,20,128)` at the same
  pixel. A clipboard round trip with `héllo clipboard`, served by the client to
  itself, came back byte-identical. A synthetic `KeyPress` for keycode 38 decoded
  to keysym `0x61` (`a`) and `0x41` with shift.
- **What it leaves out, and my estimate of the production figure**:
  `BIG-REQUESTS` (a 1080p frame is 32 bands of 256 KiB without it), MIT-SHM
  (optional, an optimisation), the XKB extension (layout groups, dead keys, key
  repeat state), Xauthority host and family matching, `_NET_WM_*` and Motif hints
  (frameless window, fullscreen, move/resize requests), cursor shapes,
  `RANDR`/`Xft.dpi` for scale, `INCR` transfers for clipboard payloads over a
  request's size, `TARGETS`, drag and drop. **Estimate 1,200 to 1,800 lines**,
  still no unsafe. Input methods (XIM, or an IBus/Fcitx D-Bus client) are not
  included and are the real cost on Linux (section 6).
- **Throughput was not measured.** `PutImage` of a full 1920x1080x4 frame is
  8,294,400 bytes; at 60 frames per second that is 498 MB/s through the server
  socket. Local sockets sustain more than that on commodity hardware (my memory,
  not measured here); the point is that damage rectangles, not full frames,
  are the design.
- **X11 has no isolation between clients**: any client can read any window and
  every keystroke on the display. Nothing in this stack changes that; it is the
  reason a content process must never hold a display connection, and the reason
  the choice between X11 and Wayland is a security one as well as a feature one.
- **Where the socket connect lives.** `px-broker/CLAUDE.md`: "This process owns
  every OS handle." The working agreement forbids filesystem and network symbols
  outside `px-broker`/`px-store` and `px-net`/`px-update`. I could not find that
  symbol-audit gate in `crates/px-xtask/src/gates/` (the working agreement says CI
  enforces it), so I cannot say what it would flag. Reading `$XAUTHORITY` is a
  filesystem read and connecting `/tmp/.X11-unix/X0` is a socket connect; both
  belong to the broker, which passes the connected stream and the cookie bytes
  into the protocol code. That is also exactly the shape a sandboxed UI process
  would need (an inherited descriptor, no `open`, no `connect`).

### 5.3 Linux Wayland: hand-written wire protocol (estimate, not built)

No compositor is available here, so nothing below was run. Facts first.

- **std cannot pass descriptors on stable.** `UnixStream` ancillary data
  (`SocketAncillary`, issue 76915) is **unstable in 1.98.1**: compiling a use of
  it gives `E0658` (checked). `wl_shm` needs `SCM_RIGHTS` to send the pool fd; the
  keyboard keymap arrives as an fd. So a Wayland client needs `sendmsg`/`recvmsg`
  with control messages and `memfd_create` + `mmap`: `unsafe`, through `libc`
  (already in the lock for `px-sandbox`). It is the same primitive ADR 008
  earmarked as "in-band handle passing", which has no consumer yet; Wayland would
  be its first.
- **The keyboard is the hard part.** `wl_keyboard.keymap` delivers an XKB keymap
  as text in an fd. Turning an evdev keycode into a keysym on a non-US layout means
  reading that text (types, levels, groups, modifier maps) or loading
  `libxkbcommon`. X11's `GetKeyboardMapping` hands the client the compiled result.
  A reader for the canonical text `xkbcommon` serialises is, in my estimate, 600 to
  900 lines; dead keys and Compose add more.
- **Everything the window needs from the compositor is optional and varies**:
  server-side decorations (`xdg-decoration`; GNOME does not implement it), cursor
  shape (`wp_cursor_shape_v1` or loading an Xcursor theme, which needs file
  parsing), fractional scale (`wp_fractional_scale_v1` + `wp_viewporter`),
  clipboard (`wl_data_device`), IME (`zwp_text_input_v3`, which works only when the
  compositor and an input method cooperate).
- **Estimate for a native Wayland layer**: wire format and registry 250 lines,
  `xdg-shell` + `wl_shm` 350, seat/pointer/keyboard 350, XKB reader 600 to 900,
  `data_device` clipboard 250, `text-input-v3` 150, scale and cursor 200, the
  fd-passing unsafe 100: **about 2,250 to 2,550 lines and 100 to 250 unsafe
  tokens**. Labelled estimate; the winit equivalent is 4,198 lines plus
  `smithay-client-toolkit` 8,619.
- **XWayland** is the alternative: GNOME, KDE and (optionally) Sway run X11
  clients through it, so the X11 layer above works on most Wayland sessions. The
  costs are real: integer-only scaling (blurry at 150%), and "most" is not "all":
  a session without XWayland cannot run the browser. I did not measure either
  claim; they are from memory.

## 6. Keyboard, text input and IME at the platform boundary

Phase 9 builds the platform-independent composition model (preedit, caret,
clauses, commit, `isComposing`, caret rectangles); Phase 9 units 9b and 9c are the
backends, "implement the seam in the UI process only", and are only provable on
real hardware (`docs/phase-09-plan.md`). What the window layer must expose to that
seam, whichever stack provides it:

**Events out of the window layer**

- Key: physical key (a layout-independent scancode or keycode), logical key and
  text produced, modifiers, press/release/repeat, and a `composing` flag so the
  page sees `isComposing` and a key consumed by the IME (Windows `VK_PROCESSKEY`,
  keycode 229) is not also delivered as a keydown.
- Text: committed strings, separately from keys, as UTF-8. Surrogate pairs from
  `WM_CHAR` are joined before this point (the prototype does).
- IME: `Enabled`, `Preedit { text, cursor range, clause ranges }`, `Commit { text
  }`, `Disabled`, and a cancel.
- Pointer: position in device pixels with scale, buttons, wheel (with line versus
  pixel units), enter/leave; resize with scale factor; focus gained/lost; close
  request; redraw request with a damage rectangle.
- Clipboard: read and write of UTF-8 text, asynchronous (X11 selection transfer is
  an event round trip).

**Calls into the window layer**

- `set_ime_allowed(bool)`, `set_ime_cursor_area(rect)` in window coordinates from
  the caret rectangle Phase 9 computes from shaped text, and optionally
  `set_surrounding_text(text, cursor, anchor)`. TSF and `text-input-v3` use the
  surrounding text for reconversion; IMM32 and XIM mostly do not. It should be in
  the seam from the start because adding it later changes every backend.
- `present(&Canvas, damage)`, `set_cursor(shape)`, `set_title`, `set_resizing`,
  `request_drag_move/resize` (a frameless window), `set_visible_when_ready`.

**Per stack**

| Stack | Windows IME | Linux IME |
|---|---|---|
| winit | IMM32, surfaced as `WindowEvent::Ime` (implemented: `platform_impl/windows`) | XIM through libX11 (`x11/ime/*`, `XOpenIM`, `XCreateIC`, `Xutf8LookupString`), `text-input-v3` on Wayland |
| Hand-written | IMM32 in the prototype (about 40 of its lines); TSF only if the user later wants it | **X11: XIM is a protocol over X client messages (est. 800 to 1,200 lines) or an IBus/Fcitx D-Bus client (est. 1,200 to 1,800 lines, the figure in ADR 036; it needs a D-Bus client).** Wayland: `text-input-v3` (est. 150) |

The D-Bus client is the item that does not go away: IBus and Fcitx need it, AT-SPI
needs it (section 8), and portals (file chooser, later) would. Measured by Phase 9
research: `zbus` 49 crates / 9,468 tokens, `rustbus` 5 / 760; hand-written, std
only, estimated 1,200 to 1,800 lines. One client, written once, serves three jobs.

## 7. Where windowing sits in the process model, and the pixel path

Build-spec section 3 draws `px-ui` as a child of the broker ("chrome only") and
Phase 18 says `px-ui` is "winit + wgpu". The brief for this research says the
chrome runs in the **browser (privileged) process**. Both are consistent with
what follows, and the choice is one of the things ADR 044 asks the user to make;
the data flow is the same.

```
 content process (sandboxed, one per site)        GPU process (sandboxed)
   DOM, style, layout, script                       software rasterizer today
   builds a display list (px-paint)                 wgpu later (ADR 034)
        |  DisplayList items (bounded, postcard)         ^
        v                                                |
 +--------------------------- browser process (broker, privileged) ----------------+
 | per-frame surface: GpuResponse::Strip x N  ---> reassembled, size-checked        |
 |   (<= 256 KiB each, MAX_STRIP_BYTES; receiver checks the arithmetic)             |
 | compositor: chrome layer (rasterized here, deterministic, section 7.1)           |
 |             + page surface clipped to the page rectangle                         |
 |             + (Phase 14) child-frame surfaces clipped to their rectangles        |
 |  -> one RGBA Canvas + damage rect                                                |
 | window layer (the only code holding a display connection):                       |
 |   Windows StretchDIBits / X11 PutImage / Wayland wl_shm                          |
 |   input: key, text, IME, pointer  ---> hit-test chrome vs page rectangle         |
 |        -> typed message to the content process that owns the focused frame       |
 +----------------------------------------------------------------------------------+
```

- **A content process never holds a window, a display connection, a clipboard
  handle or an input device.** It sends display lists and receives typed input
  events chosen by the broker. Authority comes from the channel: the broker
  decides which `FrameId` receives an event; a content process never states where
  on screen it is.
- **No page-controlled pixel can land in the chrome region.** The compositor
  blits a page surface only inside the page rectangle it computed itself, clipped.
  A hostile strip header that declares an offset outside its surface is rejected
  by the existing arithmetic check (the receiver works out rows and offsets); a
  test for the clipping is a Phase 18 gate item (spoofing corpus).
- **Frame size and memory.** One 1920x1080 RGBA surface is 8,294,400 bytes = 32
  strips of 256 KiB. Holding one per visible frame is cheap; holding one per tab of
  fifty is 415 MB, so hidden tabs drop their surface and repaint on activation.
- **Damage.** Phase 8 sends complete frames; damage tracking was explicitly out of
  scope (ADR 033 (f)). A live window needs dirty rectangles in both the strip
  protocol and `present`, or every mouse move re-uploads megabytes.

### 7.1 The chrome must be rasterized by the deterministic path

Phase 18's gate is "identical rendering on both OSes; UI snapshots committed and a
one-pixel change fails CI". ADR 033 (b) set the standard: the software rasterizer
is integer-only, the tolerance is 0, one baseline set serves both OSes, and GPUs
are *not* bit-exact with each other. A chrome drawn by wgpu cannot meet that gate
as stated. So the chrome is drawn by the same software rasterizer into the same
`Canvas`, and a GPU is an optimisation for page content only. Consequence the
approved direction makes concrete: `docs/brand/ui-direction.md` specifies
**squircle corners (exponent about 4.6), a soft shadow, a 1 px hairline**, and
ADR 033 (f) lists "rounded corners, anti-aliasing, shadows, gradients" as not
implemented in the rasterizer. **Phase 18 therefore needs an integer-only
anti-aliased path and blur in `px-paint`**, with exact, platform-independent
coverage arithmetic. That is the largest unbudgeted item between the window layer
and a chrome that matches the mockups. It is out of this ADR's scope and is stated
here so it is not discovered at Phase 18.

## 8. Accessibility bridges

Phase 16 builds the platform-independent tree in `px-a11y` (content-side) and
ships it to the browser process; the bridges are UIA (Windows) and AT-SPI (Linux)
in the privileged process, next to the window.

| Stack | UIA (Windows) | AT-SPI (Linux) | Conflicts |
|---|---|---|---|
| winit | no `WM_GETOBJECT` anywhere in winit 0.30.13 (grep). Needs subclassing the `HWND` from outside, or AccessKit's winit adapter | independent of winit: a D-Bus client registering on the accessibility bus (`org.a11y.Bus.GetAddress`) | AccessKit would be a second tree model beside `px-a11y`, plus `windows` (the crate with 326,383 tokens, ADR 034) on Windows and `zbus` on Linux. Not measured here beyond that |
| Hand-written Win32 | `WM_GETOBJECT` handled directly in our `wndproc`; `UiaReturnRawElementProvider` + `IRawElementProviderSimple` and friends as COM vtables over `windows-sys` (`Win32_UI_Accessibility`, `Win32_System_Com`) | same D-Bus client as IBus | none; COM vtables are the unsafe cost, estimate 500 to 900 lines |
| Hand-written X11 | n/a | the D-Bus client; display protocol is irrelevant to AT-SPI | none |

Nothing conflicts at the protocol level; the cost differs. With winit the missing
hook forces a dependency or a subclassing hack; hand-written code puts it where
the messages already arrive.

## 9. Findings the user should see before the choice is closed

1. **Appendix A's row names winit.** The measurements say winit is 62 to 69% of
   the current unsafe baseline on Linux, mostly `rustix` and `linux-raw-sys`
   twice, plus up to eight C libraries loaded into the privileged process,
   against hand-written layers that measure 364 lines (X11, run) and 270 lines
   (Win32, compile-checked) with no new crate. I am **not** changing that row. It
   may be wrong for this project's directive; ADR 044 is where the user says so.
2. **Wayland is where hand-written stops being cheap**, not X11 and not Windows.
3. **The chrome needs an anti-aliased integer rasterizer** (section 7.1) whichever
   window stack is chosen.
4. **The D-Bus client is shared** by IME, AT-SPI and portals; decide it once.
5. **`x11-dl`'s build script bakes `pkg-config` output into the binary**, which
   needs a reproducibility check if winit is chosen.

## Appendix: new crates per candidate (Linux unless noted)

- winit x11-only (21 new names): `as-raw-xcb-connection calloop cursor-icon dlib
  gethostname libloading linux-raw-sys(2) polling raw-window-handle rustix(2) slab
  smol_str thiserror(+impl) tracing tracing-core winit x11-dl x11rb x11rb-protocol
  xkbcommon-dl xkeysym`.
- winit adds for `wayland`: `ahash calloop-wayland-source downcast-rs getrandom
  memmap2 quick-xml scoped-tls smithay-client-toolkit wayland-backend
  wayland-client wayland-csd-frame wayland-cursor wayland-protocols
  wayland-protocols-plasma wayland-protocols-wlr wayland-scanner wayland-sys
  xcursor zerocopy`.
- winit default adds, for the Adwaita decorations only: `ab_glyph
  ab_glyph_rasterizer arrayref owned_ttf_parser sctk-adwaita strict-num tiny-skia
  tiny-skia-path ttf-parser` (9 crates, and a second font stack).
- winit on Windows (8): `cursor-icon dpi raw-window-handle smol_str tracing
  tracing-core unicode-segmentation winit`; `windows-sys` 0.52.0,
  `windows-targets`, `windows_x86_64_msvc` 0.52.6, `bitflags`, `pin-project-lite`
  are already in the lock.
- Build-script crates in the winit x11+wayland closure: `pkg-config` (wayland-sys,
  x11-dl), `cc` (wayland-backend, only if `log`; locked already for `ring`),
  `cfg_aliases` (winit).
- `wasmi` is measured in ADR 046, not here.
