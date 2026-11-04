# Phase 8 gate report

*Paint and compositing. Built on `main` through branches `phase8/*`.*

Read the second table before the first. The gate has four items; **two are met on
Linux, one is met only for its software half, and one is met only on Linux.**
Nothing here has run in GitHub CI yet (the queue was backed up when this was
written), so every result below is a local run.

## The four gate items (build-spec section 9)

| Item | Where | Result |
|---|---|---|
| 25 static pages render headless | `tests/render/pages.toml`, `cargo xtask render` | **Met.** 25 pages through the real pipeline (parse, style, layout, display list, software raster, PNG) |
| pixel-diff baselines committed | `tests/render/baselines/`, 25 PNGs, 272,428 bytes | **Met.** Exact match, tolerance 0; every page rendered twice and compared byte for byte |
| GPU process proven to have no filesystem or network capability | `crates/px-gpu`, `crates/px-sandbox/src/gpu_linux.rs`, tests `gpu_process_denied_capabilities` | **Met on Linux, read as "no ambient access" (ADR 033, user decision).** Not met on Windows |
| killing it falls back to software without losing the session | `GpuSession`, tests `gpu_kill_falls_back` | **Met on Linux** for kill, hang, malformed and oversized frames |

## What each tick does and does not mean

**"25 pages" are fixtures, not websites.** 13 are hand-written feature pages and 12
are chosen by a stated rule from the vendored WPT corpus (ADR 033). No real-site
archive was possible: `tests/compat` holds only the empty forty-site list, and
`tests/net/corpus` is 200 synthetic framing fixtures. So "renders 25 static pages"
does not say "renders 25 real pages". It says the pipeline draws what the fixtures
describe.

**Text is placeholder blocks.** Phase 9 supplies fonts and shaping. Every baseline
containing text will be regenerated then, which is a stated cost, not a surprise.

**The baselines record known-wrong output.** Written when the display-list builder
had no stacking contexts, no `position: relative` offset, and no backgrounds on
inline boxes. `positioned.png` shows a relatively positioned box at its un-offset
position. A baseline that matches is a baseline that *has not changed*, not one
that is *correct*. See "Known defects" and `phase8/positioning`.

**Cross-platform byte identity is a claim, not a result.** The rasterizer is integer
arithmetic only, so Linux and Windows should produce identical bytes, and the CI job
`render` is wired for both (with `continue-on-error`, as every phase's job starts).
Windows has not run it.

**The GPU process runs the software rasterizer.** ADR 034 (user decision): wgpu is
added later by its own ADR. So "GPU process" proves the *boundary* (sandbox, strip
transport, kill and fallback) and says nothing about a real driver. Whether a real
Vulkan or DX12 device starts under the profile, and how its output compares with
software, can only be checked on a machine with a GPU.

**The sandbox profile is two-stage, and the first stage is unconfined.** ADR 035
(proposed, awaiting the user): the process must exec and dynamically link, which a
filter denying `open` would prevent, so it applies the strict filter to itself as the
first act of `main`. Between exec and that call it runs only first-party startup
code, unconfined. The broker does not take the process's word: it reads the kernel's
view (`/proc/<pid>/status`) before sending any work. Landlock is applied but cannot be
shown to deny anything independently of seccomp.

## Not met or not proven

- **Windows GPU profile.** `enter_gpu_profile` and `verify_gpu_profile` refuse on
  Windows, so a Windows session renders in software and says why. The creation-time
  restricted token and job object apply, but file and socket denial and the probe do
  not exist. This is a real gap in the gate's third item.
- **A real GPU or driver.** Not run.
- **aarch64, kernels before 5.9.** Not run (syscall numbers uncompiled; older kernels
  cannot be verified, so the session uses software).
- **Fuzz targets** `gpu_request` and `gpu_response` compile; they were not run
  (nightly libFuzzer).
- **CI.** None of this has run on GitHub's runners.

## Known defects the corpus exposed

- `position: relative` offsets are not applied (layout).
- No stacking contexts or `z-index`; paint order is tree order.
- No backgrounds or borders on inline boxes (layout makes no fragment for them).
- `background-clip: content-box` paints as padding-box; non-solid border styles
  paint as solid.
- Not drawn at all: shadows, rounded corners, opacity, transforms, images, gradients.

The first two are being fixed on `phase8/positioning`; the baselines change there
and each change is read before it is blessed.

## What was built

| Unit | Result |
|---|---|
| Display list types and builder | `px-paint`; fragments carry their DOM node; style read through a plain-data layer in `px-css` so no stylo type crosses |
| Software rasterizer | integer only, edges snapped once by one rule, mitred borders, bounded canvas (16384 per side, 2^25 pixels) |
| PNG codec | first-party encoder (fixed-Huffman, per-row filter choice) and decoder (full inflate), bounded and panic-free |
| Render gate | `cargo xtask render`: exact compare, floor of 25 pages, blank-page check, determinism, `--bless` refuses under CI |
| GPU process | `px-gpu` binary, `GpuSession` with 8 bounded restarts, strip transport (256 KiB), kernel-verified profile, probe behind `testing` and scanned for in release |

Dependencies added in this phase: none third-party. `px-paint` depends on the
workspace crates `px-layout`, `px-css` and `px-dom`.

Local results at the last merge: 1,190 workspace tests pass, 0 fail, 2 ignored;
fmt and clippy clean; gates `all-rust`, `structure`, `brand-leak`, `unsafe-headers`,
`unsafe-audit`, `sandbox`, `ipc`, `render` pass. `compat-list` is red, as it has been
since Phase 0, because the forty-site list is the user's to supply.

## Closing the phase

Remove `continue-on-error` from the `render` job in the phase's last commit, after CI
has run green on both operating systems. Until then the phase is **built, not
closed**.
