# 034 — wgpu for the GPU process: what it costs

- **Status:** accepted, option B (user decision, 2026-11-04): the GPU process is
  built now with the software rasterizer inside it, and wgpu is added by its own ADR
  at the first phase where a measured software frame time or the browser UI needs
  it. Appendix A is unchanged: wgpu remains the GPU path, only its date moves.
  Nothing in `Cargo.toml` or `Cargo.lock` has been changed.
- **Date:** 2026-11-04
- **Phase:** 8
- **Invariants touched:** none directly. It bears on §4 (the unsafe claim), the
  `ci/unsafe-baseline.json` ratchet, the "as few dependencies as possible"
  directive, and on Appendix A, which settles wgpu.

## Context

Appendix A and §3 name `wgpu` as the GPU path, and §9 Phase 8 says "`px-gpu` on
wgpu in its own sandboxed process". The project directive since the dependency
audit (`docs/research/dependency-audit.md`) is as few dependencies as possible.
`/CLAUDE.md` says no dependency without an ADR. This ADR is that record, written
**before** the dependency is added, with measured numbers. Appendix A is settled
and this ADR does not re-argue it; it prices it, and offers the user a way to
keep the decision while moving the cost to the phase that needs it.

`px-gpu` has no dependencies today.

## Measurements

**Method.** A throwaway crate in a scratch directory outside the repository (since
deleted, with its target directory), depending on `wgpu =30.0.1` (the newest
version that resolves with Rust 1.98.1; `cargo generate-lockfile` with `"*"`) with
`default-features = false` and `features = ["std", "parking_lot", "vulkan", "dx12",
"wgsl"]`. That is the smallest set that gives a Vulkan device on Linux and a DX12
device on Windows with WGSL shader source. Dropping `wgsl` changes nothing in the
crate list. `gles`, `metal`, `webgpu`, `angle`, `glsl`, `spirv`, `serde`, `trace`
are off. No windowing crate is needed for headless rendering. `cargo check`
passes on Linux in 39 s including download (221 MB target dir, deleted); a minimal
`Instance::new` compiled. A Windows build was not run, only resolved.

Crate lists come from `cargo tree --edges normal --prefix none --target T`, for
`T` = `x86_64-unknown-linux-gnu` and `x86_64-pc-windows-msvc`, de-duplicated on
`name version`. "New" is measured against this repository's `Cargo.lock`
(`name version` pairs read from it). Exact-version matching over-states novelty,
because Cargo would unify `libc 0.2.191` onto the locked `0.2.189`, so the
headline figure is **new lockfile entries after unification**: crates whose name
is absent from the lock, plus crates present only at an incompatible major
(`hashbrown 0.16.1` beside the locked 0.17.1; `rustc-hash 1.1.0` beside 2.1.3).
Unsafe is counted over each new crate's whole registry source, every `.rs` file,
for the token `unsafe` as a whole word. This is the same *whole-source* basis as
`crates/px-xtask/src/gates/unsafe_audit.rs`, which counts every `.rs` file in a
vendored crate whatever its features. My count uses `grep -oP` and does not
reproduce the gate's rule that a match consumes its trailing delimiter, so it can
over-count by a small amount. The real figure comes from `cargo xtask
unsafe-audit --write` after vendoring, and has not been run.

| Measure | Linux | Windows | Both (the lockfile) |
|---|---|---|---|
| Crates in the wgpu closure (name+version) | 56 | 67 | 68 |
| ... of which name+version already in our lock | 15 | 16 | 16 |
| **New lockfile entries after unification** | **32** | **43** | **43** |
| Current third-party baseline | 153 built | 154 built | 171 in lock |
| **Increase** | **+21%** | **+28%** | **+25%** |
| `unsafe` tokens added, whole source | 15,621 | 342,729 | 342,729 |
| Baseline today | | | 21,675 (109 entries) |

Two numbers need saying plainly.

- **+43 lockfile entries, +25% of the third-party tree**, for a feature that
  needs none of its own capability in Phase 8.
- **+342,729 unsafe tokens, a 16-fold rise on the baseline.** 327,108 of them
  (95%) are the `windows` crate family, which `wgpu-hal`'s DX12 backend and
  `gpu-allocator` require (`windows 0.62.2` alone is 326,383). Almost all of that
  is generated bindings, of which wgpu uses a small part; the gate cannot see
  that, because it counts source, not use. Removing DX12 does not help: with
  `vulkan` only, Windows still resolves `windows 0.62.2` (checked), so the figure
  is the price of wgpu on Windows in any configuration. The Linux-only
  figure is 15,621, of which `ash` (the Vulkan binding) is 8,798 and `wgpu-hal` and
  `wgpu-core` are 3,238. That is a 72% rise on the baseline, which is the number
  that is about wgpu rather than about the Windows bindings crate. Either way the
  ratchet will fail and the baseline must be raised by an ADR. The honest framing
  for a reviewer is "the baseline's meaning changes", since the existing
  `windows-sys` entries (15,941) are already 73% of it.

**New crates (43)**, by name: `allocator-api2 ash bit-set bit-vec bytemuck
bytemuck_derive codespan-reporting document-features foldhash gpu-allocator half
hashbrown(0.16) libloading litrs naga naga-types ordered-float presser profiling
range-alloc raw-window-handle renderdoc-sys rustc-hash(1.x) spirv unicode-width
wgpu wgpu-core wgpu-core-deps-windows-linux-android wgpu-hal wgpu-naga-bridge
wgpu-types zerocopy zerocopy-derive`, and on Windows `windows windows-collections
windows-core windows-future windows-implement windows-interface windows-numerics
windows-result windows-strings windows-threading`.

**Build-time requirements: none beyond a Rust toolchain.** The closure contains no
`cc`, `cmake`, `pkg-config`, `bindgen`, `-sys` crate (other than `renderdoc-sys`,
a pure-Rust struct crate) or Python. The only `build.rs` files in it are in
`wgpu`, `wgpu-core`, `wgpu-hal` (each a `cfg_aliases` call), `naga` (12 lines) and
`ash` (24 lines). The project's 100% Rust rule (ADR 032) is not touched, and the
build-time exception list does not grow. Reproducible builds are not made harder by
the build. `ash` and `wgpu-hal` find Vulkan and `d3d12.dll` at **run time**, through
`libloading::Library::new`, so no driver is linked and the binary's bytes do not
depend on the machine.

**What it does to the claims.**

- *Reproducible builds:* intact for our binaries. But what the GPU process
  actually executes is the user's driver, which is outside the supply chain, not
  reproducible, and not auditable by us. §4 already says so ("the C GPU drivers
  beneath it").
- *Sandbox:* the GPU process becomes the process that must run closed-source,
  native code from the driver. The loader reads ICD manifests and libraries, Mesa
  and vendor drivers open `/dev/dri`, `/sys` and shader caches, DX12 loads
  user-mode driver DLLs. So the literal "no filesystem capability" in the gate
  cannot hold for a real device; what can hold is *no ambient* filesystem access
  and no network (ADR 033, the question to the user). Without wgpu, the GPU
  process executes only our own code and the literal sentence is true.
- *Memory safety claim:* the README's "small audited unsafe core" gains a second,
  larger core that is not ours, with its own `ash` FFI. §4 already lists wgpu
  among the places safety ends; this ADR puts the size on it.

## Options

**A. wgpu in Phase 8, as Appendix A and §9 read.** The GPU process is real in
Phase 8. Costs: the table above; a real-GPU machine is needed to test the half of
the gate that matters (the container has none), so CI proves only the boundary
and not wgpu (ADR 033 (e)); the literal gate item must be reinterpreted; baseline
raised by ADR. Saves: finds out early whether wgpu fits the sandbox profile.

**B. Software rasterizer only in Phase 8; the GPU process exists and is
sandboxed, with the software rasterizer as its only backend; wgpu is added by its
own ADR in the phase that first needs it.** The process, the strict sandbox
profile, the strip transport, supervision, crash restart and fallback are all
built and proven in Phase 8, and every gate item, read literally, is provable in
CI (nothing in the process has a driver, so "no filesystem or network
capability" is true as written). The `WgpuRasterizer` slot in ADR 033 (d) stays
empty, and the trait has one implementation until then. Costs: the wgpu risk is
postponed, not removed (a device that will not start under the sandbox profile is
found later); "the GPU process" does no GPU work until then, which is true to the
spec's words ("GPU process proven", "software fallback") but not to its spirit;
and two things in the spec would change: §9's "on wgpu" and the px-gpu line in §3
are deferred, not dropped. Saves: 43 entries, 342,729 unsafe tokens (15,621 on
Linux), a real-GPU test dependency, and a deviation from the gate's wording.

**C. wgpu on Linux only for now.** Rejected up front: the gate is on both
operating systems, and a Linux-only dependency is a platform-conditional lockfile
that the supply-chain gates (ADR `proposed-gate-scope-shipped-targets`) have not
decided how to treat.

**D. Hand-written Vulkan and D3D12 bindings.** Rejected: that is a larger unsafe
core than `ash` and `windows` together, written once and reviewed by one person.

## What software-only costs in performance

Not measured, because the software rasterizer does not exist yet and there is no
GPU here. An estimate, labelled as one: Phase 8's display list is five item types
(solid fills, solid borders, text placeholder blocks, clip push and pop), all
axis-aligned and integer-snapped. A CPU fill of a full 1080p frame is a memory-
bound write of about 8 MB, a few milliseconds, comfortably inside a 16 ms frame
for pages of this kind. Software gets expensive with things this phase excludes:
text at scale, gradients, blur and shadow, large images, transforms, 4K, and
scrolling a tall page without damage tracking. Those arrive in Phases 9 and 15 and
the chrome work in 18. The decision to add wgpu can then be made with a measured
software frame time in hand, and "software is too slow for X" is a number rather
than a prediction. The chrome (`px-ui`, "winit + wgpu" in Appendix A) will need
the same closure when it is built, so option B defers the cost and does not
cancel it, unless the chrome is rasterized in software too (which Appendix A
rejects for widgets).

## Recommendation

**Option B.** Build the GPU process now with the software rasterizer inside it,
prove the whole boundary on CI, and bring wgpu in by its own ADR at the first phase
where a measured software frame time or a chrome requirement says it must. Reasons,
in order: (1) nothing in Phase 8's gate needs wgpu to pass, and the one gate item
wgpu makes *false* as written ("no filesystem capability") is true with B; (2) it
is the only option consistent with "as few dependencies as possible" that does not
re-argue Appendix A, because wgpu stays the chosen GPU path and only its *date*
moves; (3) the cost is not small (+25% of the tree, a 16-fold unsafe baseline, and
72% on Linux alone), is paid in full on Windows whatever features are chosen, and
buys no test coverage CI can run; (4) the sandbox profile is the hard, novel part,
and it is exercised fully by B. The honest downside is (A)'s one real advantage:
learning early whether a wgpu device starts inside the profile. That risk is real
and B accepts it. If the user prefers to learn it early, A with the table's costs
is a coherent choice, and the dependency ADR fields below are what to fill in.

If the user chooses A, the fields still to establish, because this ADR did not
measure them, are:

- **Crate and version:** `wgpu =30.0.1`, features as above.
- **Maintainer count, audit status (cargo-vet, Mozilla and Google audit sets):**
  not checked. Do not treat their absence here as a finding either way.
- **Unsafe-line count:** +342,729 by the method above (+15,621 Linux); the
  baseline would be raised by that, to be re-measured with `unsafe-audit --write`.
- **Transitive dependencies added:** 43 lockfile entries, listed above.

## Verification

Wrong if the first software frame time on a realistic page, measured in Phase 9
or 15, exceeds the frame budget on the project's reference hardware, in which case
the deferral cost real time and wgpu should have come in at Phase 8; or if, once
wgpu is added, it starts under the sandbox profile without needing more
filesystem access than ADR 033 allows, in which case the early risk B accepted was
smaller than feared. Neither can be observed today.
