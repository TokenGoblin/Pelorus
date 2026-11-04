# Phase 8 plan: paint and compositing

Scope and gate meaning are fixed by ADR 033. The wgpu question is ADR 034
(proposed, awaiting the user); this plan works either way and marks the one unit
that depends on the answer.

Gate (build-spec section 9): 25 static pages render headless; pixel-diff
baselines committed; GPU process proven to have no filesystem or network
capability; killing it falls back to software without losing the session.

Per /CLAUDE.md the gate tests are written first and fail. Unit 0 does that.

## Units, in order

**Unit 0: gate tests, failing.** `cargo xtask render` (reads
`tests/render/pages.toml`, renders, compares to `tests/render/baselines/`), and
two broker tests, `gpu_process_denied_capabilities` and `gpu_kill_falls_back`.
They fail because nothing exists. One commit. Gate items: all four, as red tests.

**Unit 1: display list builder** (`px-paint`). Walk laid-out boxes, emit
`FillRect`, `Border`, `TextRun`, `PushClip`/`PopClip` in CSS paint order
(backgrounds, floats, in-flow content, positioned by z-index). The types are
already in (commit 0b38428). Unit tests on small trees: paint order, clipping.
Gate item: feeds every page.
Risk, medium: layout may not expose per-box computed paint properties in a form
this can read without an API change. Find out first; if a change is needed it is
its own commit and may touch Phase 6/7 code.

**Unit 2: software rasterizer and PNG codec** (`px-gpu`, plus a codec module).
Integer-only fill, per-side borders, clip stack, bitmap. PNG encoder (fixed-
Huffman deflate, deterministic matcher) and full inflate decoder, CRC-32 and
Adler-32. No new dependency. Tests: round trip, corrupt-input table, known-answer
PNG, same bitmap gives same bytes, a hand-computed 5x5 border.
Gate items: baselines; the headless render.
Risk, medium: border corner joins are where integer rasterizers disagree with the
spec's picture. Support only what the 25 pages need and say so in the report.

**Unit 3: corpus and gate.** Author the 13 feature fixtures; write the Rust
selection tool for the 12 WPT-derived pages (rule in ADR 033 (a)); `pages.toml`
with provenance; `--bless`; failure output to `target/render-diff/`. Baselines
committed and looked at once by a person.
Gate items: 25 pages, baselines.
Risk, high for the schedule: selection needs units 1 and 2 working on real WPT
pages, which use text and properties the rasterizer ignores. Expect many pairs
to match because both render blank. Add a minimum-ink check: a page that paints
only a background is not a test, is skipped by the rule, and is counted in the
report.

**Unit 4: backend trait and `GpuSession`** (`px-gpu`, `px-broker`). A `Rasterizer`
trait with `SoftwareRasterizer` (unit 2) and, later, `WgpuRasterizer`. A test
backend that fails on command exercises fallback without a process.
`GpuSession` holds the current display list, tries the GPU process, and on any
failure renders the same list in software. `--disable-gpu` selects software
without spawning.
Gate item: fallback. Risk, low.

**Unit 5: the GPU process** (`px-gpu` binary, `px-sandbox`, `px-broker`).
- `px-sandbox`: a stricter profile for the GPU child (deny `open*`, `socket`,
  `connect`, `execve` after startup; Landlock empty ruleset where available;
  Windows: restricted token, job object, child-process mitigation). New unsafe:
  `// SAFETY:` comments against the OS documentation, and an adversarial review
  (spec section 10). Applied rungs recorded as today.
- Strip transport: a frame returned in row strips of 256 KiB or less, under the
  1 MiB `MAX_MESSAGE_BYTES`; the broker reassembles and size-checks.
- A `testing`-feature probe that attempts forbidden calls and reports the results
  (spec 14.4: scanned out of release artifacts).
- The supervisor reuses `ContentProcess`'s worker threads and `MAX_RESTARTS`.
Gate items: no filesystem or network capability; kill and fall back.
Risk, highest: a process that must exec and dynamically link, yet may not `open`,
is a startup-ordering problem (static binary, or a two-stage policy whose second
stage only tightens). Prototype it before building anything around it. It is the
unit most likely to overrun, and the new unsafe goes in the crate with the
highest review cost. Today Linux applies only `no_new_privs` and a 17-syscall
denylist, so none of this exists yet.

**Unit 6: `WgpuRasterizer`. Only if ADR 034 is decided as option A.** Adds `wgpu`
in its own dependency commit, with the unsafe baseline raised in the same commit;
implements the trait; headless device, render to texture, read back. Tests are
`#[ignore]` with a reason and run on a machine with a GPU. Under option B this
unit is deferred to the phase a later ADR names, and the GPU process uses
`SoftwareRasterizer`.
Risk: the device may not start under unit 5's profile. That is a finding, not a
bug to work around: raise it (ADR 033, question to the user).

## Gate items mapped

| Gate item | Units | Proven by | Where |
|---|---|---|---|
| 25 static pages render headless | 1, 2, 3 | `cargo xtask render`, 25 entries rendered | CI, both OSes |
| Pixel-diff baselines committed | 2, 3 | exact byte match against `tests/render/baselines/` | CI, both OSes |
| GPU process: no filesystem or network capability | 5 (6) | broker test spawns the real binary under the profile; probe attempts fail; `applied` lists the rungs | CI for the boundary; the real-device half only on a GPU machine |
| Killing it falls back without losing the session | 4, 5 | kill and malformed-frame tests; next frame equals the software baseline; frame tree and channel ids unchanged | CI |

## What the gate report will say

First paragraph: text is placeholder blocks, and baselines will be regenerated in
Phase 9. Then: CI proves the process boundary using the software rasterizer; it
does not prove wgpu (no GPU in CI) unless ADR 034 option A was taken and a GPU
machine ran unit 6. The gate is not reported as simply "passed" while the
real-GPU half has not run.

## Cross-cutting

- The display list needs `serde` derives to cross IPC. `serde` is already in the
  lockfile through stylo and `px-ipc`; declaring it in `px-paint` is a new direct
  declaration, not a new crate. Say so in the commit.
- `cargo xtask all-rust`, `style`, `unsafe-headers` and the brand-leak gate stay
  green; the render gate is added to `cargo xtask all`.
- Baseline size: 25 PNGs at 800x600 of flat colour should total under 1 MB.
  Measure in unit 3; if not, say so rather than shrinking the pages.
- Defects found in layout or style go to `docs/backlog.md`, not fixed here, unless
  unit 1 cannot proceed without one.
