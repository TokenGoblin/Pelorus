# 043 — Phase 17 (Linux): the memory cap, the content process's stage two, and the escape suite

- **Status:** proposed (built and tested on Linux; Windows is not done and the
  gate is not met there; the user should confirm the figures below)
- **Date:** 2026-11-04
- **Phase:** 17
- **Invariants touched:** 8 (fail closed), 9 (authority comes from the channel)

## Context

Phase 17's gate asks for an escape-attempt suite that fails every attempt, a
syscall audit, and "memory cap kills a runaway tab and only that tab". Going in:

- A content process was created with stage one only (no-new-privs plus a deny
  list of 17 escalation primitives). It could still `open`, `socket`, `connect`,
  `execve` and fork+exec, read `/proc/<broker>/mem`, and `pidfd_open` the broker.
  Only the GPU process had a stage two (ADR 035).
- Nothing capped a content process's heap (docs/backlog.md, Phase 10: a doubling
  string reached about 1.1 GiB in 1.5 s; the watchdog bounds time, not memory).

## Decisions

1. **The memory cap is `RLIMIT_AS`, applied in the child before `exec`.**
   `px_sandbox::spawn` takes a `Policy` carrying a `MemoryLimit`; the Linux spawn
   sets both soft and hard limit with the raw `setrlimit` syscall inside the
   existing `pre_exec` (async-signal-safe: one syscall, no allocation), before
   the seccomp filter. One new `unsafe` block, `linux::limit_address_space`,
   with a `// SAFETY:` comment; the existing `pre_exec` block's comment is
   extended. Both limits are set so the child cannot raise its own cap.
2. **The cap is reported only when the kernel confirms it.** After `spawn`
   returns (so after `exec`; rlimits survive it) the parent reads
   `/proc/<pid>/limits` and requires the `Max address space` row to show soft
   and hard equal to the request. Otherwise the child is killed and `spawn`
   fails (`PolicyError::Failed { rung: MemoryCap }`). `Rung::MemoryCap` appears
   in `applied()` and `SandboxedChild::memory_limit()` is `Some` only then. It is
   not in `FLOOR`: a machine that cannot cap still sandboxes, but a policy that
   carries a cap refuses to launch without it.
3. **Figures.** Content: **1 GiB**, the build specification's figure.
   Measured address-space high-water mark (`VmPeak`) of the content process,
   unoptimised build, x86_64: 24 MiB at startup, **218 MiB** after the first
   script runs (the engine's thread and the allocator's arenas map most of
   that), unchanged through a 16-MiB-character string, 200,000 objects and the
   clock and timer tests. So the specification's figure leaves roughly 800 MiB
   for a page's heap and **nothing was weakened or tuned**. GPU: **1 GiB**, a
   software rasterizer today; its test frames run in under 8 MiB of address
   space (fail under 1 MiB). A hardware driver reserves far more address space
   than it touches, so this figure is to be revisited when one is linked.
   `RLIMIT_DATA` was not added: it is `<=` `RLIMIT_AS` and adds a second failure
   mode without a second ceiling worth having.
4. **The content process confines itself first, reusing ADR 035's stage two.**
   `px_sandbox::enter_content_profile()` is `enter_gpu_profile(&[])`: nothing
   granted, Landlock where available, the stage-two deny list. `px-content`
   calls it first in `main` (Linux only), before a thread exists, and exits if
   it fails. The broker-side check is `SandboxedChild::verify_stage_two()`
   (`verify_gpu_profile` is now an alias): the kernel's `Seccomp_filters >= 2`,
   never the child's report. `ContentProcess::verify_stage_two` exposes it; the
   spawn path does not call it yet (docs/backlog.md).
   After stage two the process still runs script: measured, it makes 24
   distinct syscalls (docs/research/phase-17-syscall-audit.md).
5. **The escape suite is a probe inside the content binary, behind `testing`.**
   A testing-only `ScriptRequest::EscapeProbe` / `ScriptResponse::EscapeProbe`
   pair; the probe is `px_sandbox::probe::escape_attempts` (28 attempts) so the
   unsafe calls stay in `px-sandbox`. Each attempt is built so that an
   unconfined process fails for an ordinary reason or succeeds harmlessly, and a
   confined one answers `EPERM`; an unconfined control test proves the probe can
   succeed. A second marker, `ESCAPE_MARKER`, is scanned for in every release
   binary alongside the existing one.
6. **Permitted operations are listed in the test, not hidden.** `stat` (a
   deny list cannot split it from `fstat`) and, on kernels without Landlock ABI
   6, signalling the broker. Both are in docs/backlog.md.

## Consequences

- The Linux half of the "runaway tab" and escape-attempt gate items is met for
  the content profile; the Windows half is not, and the backlog says so.
- The profile is still a deny list. The audit says the measured set is small
  but not yet shown stable, and what pass would make it so.
- `px-content` now depends on `px-sandbox` (a workspace path, no third-party
  crate). Windows `px-content` does not call stage two.
- Address space is not resident memory, and no cgroup is used: unprivileged
  cgroup delegation is not generally available, and a limit the process could
  evade by being in the wrong cgroup would be worse than none. A host that runs
  out of RAM before a tab reaches the cap can still OOM.
- Array-heavy scripts grow slowly enough that the watchdog ends them before the
  cap on an unoptimised build; the cap is still a hard ceiling.
