# 044 — Phase 18 window and input stack: hand-written over winit, with the numbers

- **Status:** accepted, 2026-11-05. The user chose option 1 of the ranking below
  (hand-written Win32 + X11, XWayland for Wayland sessions; no `winit`, `wgpu` or
  `softbuffer`, no native Wayland in v1) and approved the third unsafe crate,
  `px-display`. Appendix A of the build spec and the `/CLAUDE.md` unsafe rule are
  amended accordingly. Decisions 3 to 5 below keep their recommendations
  (broker process for Phase 18, XWayland first, D-Bus client as its own ADR).
- **Date:** 2026-11-04
- **Phase:** 18 (decision needed before Phase 9 units 9b/9c, which build the IME
  backends in the same layer)
- **Invariants touched:** 1 (no ambient authority: who may hold a display
  connection), 3 (the UI does not move: how pixels are produced), 7 (reproducible
  builds: `x11-dl`), 8 (fail closed). Appendix A row "Hand-rolled widgets on
  winit + wgpu" and the `/CLAUDE.md` rule "two crates are excepted from
  `forbid(unsafe_code)`".
- **Numbers:** `docs/research/phase-18-window-stack.md`. Every figure below is
  from there unless marked *estimate*.

## Context

Phase 18 needs a native window on Windows and Linux that shows pixels and delivers
keyboard, text, IME, pointer, resize, scale and clipboard events. Appendix A settles
that the chrome is hand-rolled widgets, not a toolkit, and names `winit + wgpu` as
the base. The user's standing directive is as few dependencies as possible; ADR 034
already moved `wgpu` out of Phase 8 on that ground, and the Phase 9 research assumed
`winit` would supply IME "for free once Phase 18 adds it".

**I do not think Appendix A's `winit` half survives its price, and I am saying so
rather than working around it.** The `wgpu` half is already deferred by ADR 034 and
is not needed by anything below. Per `/CLAUDE.md` this ADR proposes and stops.

What was measured (Linux / Windows, against this repository's lock, unsafe counted
by the gate's own function):

- **winit 0.30.13, smallest useful** (`x11` + `wayland` + `wayland-dlopen`, no
  Adwaita decorations): 58 / 13 crates in the closure, **39 / 8 new names,
  +14,762 / +1,022 unsafe tokens** on a baseline of 21,675 (**+68%** on Linux).
  Default features: 48 / 8 new names, +14,947. X11 only: 21 / 8 new names, +13,538.
  80% of the Linux figure is `rustix` and `linux-raw-sys`, each in two versions.
  Shrinking features does not shrink the cost.
- **It also needs a C world at run time**: up to eight C libraries (`libX11`,
  `libXcursor`, `libXi`, `libxcb`, `libxkbcommon`, `libxkbcommon-x11`,
  `libwayland-client`, `libwayland-cursor`) loaded into the process that holds the
  display, plus `pkg-config` at build time, and `x11-dl`'s build script writes the
  build machine's library directories into the binary (an invariant 7 question).
- **Presenting a CPU framebuffer needs a second crate** (`softbuffer`): together
  with winit, 42 / 9 new names and +15,005 / +1,109.
- **The hand-written minimum is small and real.** X11 over `std::os::unix::net`:
  **364 lines, zero dependencies, zero unsafe, run against a real X server**
  (window, `PutImage` presentation read back pixel-exact, keys, keysyms, clipboard
  round trip). Win32 through `windows-sys` 0.61.2, which is already locked: **270
  lines, 10 `unsafe` tokens, compile-checked for `x86_64-pc-windows-msvc`, never
  run**, covering window, `StretchDIBits` presentation, DPI, keyboard, mouse, IMM32
  composition and clipboard. Neither adds a crate; neither moves the baseline
  (the gate counts all of `windows-sys` whatever features are on).
- **Wayland is where hand-written gets expensive.** `std` cannot pass descriptors
  on stable (`SocketAncillary` is unstable, `E0658`, checked), and the keyboard
  arrives as an XKB keymap in an fd. *Estimate* 2,250 to 2,550 lines and 100 to 250
  unsafe tokens, against winit's 4,198 lines plus `smithay-client-toolkit`'s 8,619.

Two facts about the surroundings shape every option:

1. **Only the process that holds the display connection can present**, and that
   cannot be the GPU process (its stage-two profile denies `socket` and `open`,
   ADR 033 (e)1 and ADR 035), nor any content process (invariant 1). So the
   pixel path is the same for every option: content process, display list, GPU
   process (software rasterizer today), strips (`MAX_STRIP_BYTES` = 256 KiB) to the
   browser process, composited with the chrome, uploaded by the window layer.
2. **The Phase 18 gate cannot be met by a GPU-drawn chrome.** "Identical rendering
   on both OSes; a one-pixel change fails CI" is ADR 033 (b)'s standard (integer
   rasterizer, tolerance 0, one baseline set), which GPUs do not meet. The chrome
   is drawn by the software rasterizer, which today has no anti-aliasing, curves or
   shadows (ADR 033 (f)); the approved squircle direction needs all three. That
   work is real, is the same under every option, and is out of this ADR; it is
   recorded so Phase 18 is not surprised by it.

## Decision

**Proposed: hand-written window layers behind one trait, Windows and X11 first,
native Wayland only when measured need appears.** Concretely:

1. A `PlatformWindow` seam in `px-ui` (events, IME, clipboard, present with damage;
   the exact surface is in the research document, section 6) with three
   implementations chosen by `cfg`: Win32, X11, and later Wayland. The seam is
   what Phase 9's 9b/9c backends implement; it exists whichever option is chosen.
2. **Windows**: `windows-sys` 0.61.2 (already a dependency of `px-sandbox`), with
   seven more features enabled. The unsafe lives in a **new crate, `px-display`**,
   `#![deny(unsafe_op_in_unsafe_fn)]`, a `// SAFETY:` comment on every block,
   reviewed against the Win32 documentation: the same rule as `px-sandbox`
   (ADR 008). Linked only by the browser binary. *Not* inside `px-sandbox`, for
   the reason below.
3. **Linux**: an X11 core-protocol client in safe Rust over a `UnixStream` that
   **`px-broker` opens** (it owns every OS handle) and hands over together with the
   Xauthority cookie it read. Pure-Wayland sessions run through XWayland. Native
   Wayland is a later unit with its own ADR; its fd passing is the same primitive
   ADR 008 earmarked for handle passing and would live in the audited core.
4. **One hand-written D-Bus client** (*estimate* 1,200 to 1,800 lines, 0
   dependencies, fuzzable), written once for IBus/Fcitx (IME), AT-SPI (Phase 16)
   and portals; it is a separate decision in its own unit, not part of this ADR's
   cost.
5. **Present path**: the compositor in the browser process produces one RGBA
   `Canvas` plus a damage rectangle per frame and the window layer uploads it
   (`StretchDIBits`, `PutImage`, `wl_shm`). Strips gain dirty rectangles (ADR 033
   (f) excluded damage tracking from Phase 8).

### Ranking, with the numbers (Linux / Windows)

| Rank | Option | New crates | Unsafe tokens added | C libs loaded at run time | Build needs |
|---|---|---|---|---|---|
| 1 | Hand-written Win32 + X11 (XWayland for Wayland sessions) | 0 / 0 | 0 / 0 baseline (first-party: 10 measured, *est.* 30 to 80) | 0 | nothing |
| 2 | Rank 1 plus hand-written native Wayland | 0 / 0 | *est.* 100 to 250 more | 0 | nothing |
| 3 | winit 0.30 (x11 + wayland-dlopen) + softbuffer | 42 / 9 | 15,005 / 1,109 | up to 8 | `pkg-config`; `x11-dl` bakes paths |
| 4 | winit default + wgpu (Appendix A literally) | 48 + 43 (ADR 034) / 8 + 43 | 14,947 + 342,729 / 1,022 + 342,729 (ADR 034: the `windows` crate is in both) | up to 8 + GPU drivers | `pkg-config` |

Rank 4 sums two separate measurements; their overlap was not measured. Ranks 3
and 4 are what the directive exists to avoid. Rank 2 is not cheaper than 3
in engineer time (about 2,400 lines against a library that exists) and is cheaper
in every other column; it is rank 2 because nothing needs it until a user does.

### Why a new crate rather than a module in `px-sandbox`

ADR 008 puts every unsafe OS operation in `px-sandbox` and says to revisit "if the
sandbox policy and the OS primitives are genuinely two jobs". Windowing is the
first thing that is not sandboxing. Putting it in `px-sandbox` means the crate
linked by every content, GPU and network process also carries the window FFI, and
the property "no sandboxed process can create a window" stops being a `cargo tree
-p px-content` assertion and becomes a feature-flag argument (Cargo unifies features
across a workspace build, and `px-sandbox` already takes a `testing` feature that is
scanned for in release artifacts because a flag being off is not evidence, §14.4).
A separate crate that no child process depends on makes the property structural.
The cost is a third entry in the unsafe exceptions: the `unsafe-headers` gate, the
`/CLAUDE.md` hard rule and ADR 008 each change. **That is the amendment the user
must approve.**

## Alternatives rejected

- **winit 0.30 as Appendix A names it, with `softbuffer` to present** (smallest
  feature set: +39 names / +14,762 tokens for the window, +42 / +15,005 with
  softbuffer). Rejected on the directive: it is the largest single increase to the
  unsafe baseline after wgpu, it puts eight C libraries in the privileged
  process, and it does not remove the work that matters (the chrome rasterizer,
  the IME seam, the D-Bus client, the compositor). It remains the **fallback if
  the user rejects the amendment above**: it needs an ADR for the baseline
  increase (README: "any increase in the dependency-tree unsafe-line count").
- **winit 0.31.0-beta.4.** Same family, 52 / 12 new names, 10,305 / 770 tokens, and
  a beta API. Not for a stack whose gate freezes the UI.
- **`x11rb` / `wayland-client` with their Rust backends.** 5 and 8 new crates and
  8,694 / 8,880 tokens, all `rustix` + `linux-raw-sys`, to replace 364 lines we can
  read. `x11rb-protocol` alone (1 crate, zero unsafe tokens) is the only candidate
  with no I/O stack, and it is 129,220 generated lines; hand-encoding the 12
  requests the prototype uses is the more auditable trade. Kept as the escape hatch
  if the hand-written X11 client proves harder than measured.
- **`minifb`.** 20 new names, 9,402 tokens, a framebuffer-only API with its own
  event loop; its IME support was not checked.
- **wgpu as the presenter.** Rejected twice: ADR 034 (priced and deferred), and the
  GPU process cannot present (above). It stays the rasterization decision for page
  content only.
- **Window code in the content or GPU process.** Violates invariant 1.
- **A separate UI process in v1.** The spec diagram draws `px-ui` as a child; the
  brief for this research says the chrome runs in the browser process. See the
  decision below. The seam is written so the move is mechanical.

## Consequences

- **Cost**: Win32 and X11 production layers *estimated* 600 to 900 and 1,200 to
  1,800 lines respectively (the prototypes measured 270 and 364), the D-Bus
  client 1,200 to 1,800, XIM or IBus on top, UIA providers 500 to 900. None of it is
  small. It is cheaper than winit in every currency the directive counts
  (crates, unsafe tokens, C libraries, build requirements) and more expensive in
  the one it does not: our own hours, and our own bugs in code winit has had a
  decade of users exercise.
- **Linux is X11 first.** On a pure-Wayland session without XWayland the browser
  does not start, and says why (invariant 8). Under XWayland the scale factor is
  integer. Both are from memory, not measured. The first user bug report that says
  "blurry" is the trigger for the Wayland unit.
- **X11 has no isolation between clients.** The browser process can see every
  window and keystroke on the display, and so can anything else on it. Not
  fixable here; Wayland is better. Stated in the threat model's residuals when
  this ADR is accepted.
- **The unsafe baseline does not move on Linux and moves by 0 on Windows**
  (the 10 first-party tokens in the prototype are in a new crate that the gate
  will list as a new entry; *estimate* 30 to 80 in production). The ratchet
  needs a line added for `px-display`, not a raised ceiling.
- **The window layer is the largest hand-written parser of untrusted bytes in the
  privileged process** (X replies, selection data from other applications, input
  method strings). It is safe Rust on X11, and must get fuzz targets like every
  other decoder (§4.5): an X11 reply decoder fed from a recorded socket is the
  cheapest fuzz target in the project.
- **Reproducibility improves**: no `pkg-config`, no baked library paths.
- **Phase 9's plan changes**: 9b/9c implement the seam in `px-ui`; nothing there
  assumed winit except the cost estimate, which this ADR replaces.
- **Phase 16 is unblocked by hand-written Win32** (`WM_GETOBJECT` is ours) and
  blocked-then-hacked by winit (it has no hook; grep returns nothing).

## What the user must decide

1. **Keep `winit` (Appendix A as written), or approve the hand-written layers?**
   If winit stays, accept the baseline increase it needs by ADR. If hand-written,
   approve the next two.
2. **A third unsafe crate (`px-display`), or windowing unsafe inside
   `px-sandbox` by ADR 008's letter?** Recommendation: the new crate. Either way
   `/CLAUDE.md` and ADR 008 change.
3. **Where does the window layer run: in the browser (broker) process as the
   brief says, or in a separate low-privilege UI process as the spec diagram
   draws?** In the broker, the window code, X reply parsing and the clipboard
   run in the process that "may not die" and holds every handle. As a child, it
   needs a profile that allows one inherited display descriptor and nothing else,
   which the hand-written X11 client suits and winit (dlopen of eight C libraries,
   file reads) does not. Recommendation: broker process for Phase 18, behind the
   seam, and revisit at Phase 17 with the sandbox ladder in hand.
4. **Pure-Wayland sessions: XWayland only for v1, or native Wayland in the first
   cut?** The estimate is about 2,400 lines and an XKB keymap reader. Recommendation:
   XWayland first.
5. **IME on Linux**: the D-Bus client (IBus/Fcitx) versus XIM. Shared with
   AT-SPI, so recommendation: the D-Bus client, as its own ADR.

## Verification

Wrong if: the Windows prototype, once run on a real machine, cannot present or
cannot deliver `WM_IME_COMPOSITION` for a real IME (then 270 lines was a compile
check, not a window); the X11 client's `PutImage` cannot sustain a window-sized
repaint at an interactive rate without MIT-SHM on the maintainer's hardware (then
MIT-SHM is in, adding a `shmat` and its unsafe); native Wayland turns out to be
needed on day one (then winit's 4,198 lines are cheaper than ours); or `cargo
xtask unsafe-audit` on the real `px-display` counts more than 100 tokens (the
estimate's upper bound). Not falsifiable here: IME on a real Japanese or Korean
input method, UIA with a screen reader, pure-Wayland and XWayland behaviour.
