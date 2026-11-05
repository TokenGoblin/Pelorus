# 044 — Phase 17 (Windows): job limits, low integrity, mitigation policies

- **Status:** proposed (written and cross-compiled on Linux, **never run on
  Windows**; CI on Windows is the first execution. The user should confirm
  decision 1.)
- **Date:** 2026-11-05
- **Phase:** 17
- **Invariants touched:** 8 (fail closed), 9 (authority comes from the channel)

## Context

ADR 043 built the Linux half of Phase 17. The Windows half of build-spec section
9 names: AppContainer, restricted token, job object limits including memory
caps, Win32k lockdown, mitigation policies. ADR 007 floored Windows at a
restricted token and a job object and called AppContainer and Win32k lockdown
"Phase 17's tightening". `windows::spawn` already created the process with a
restricted token (`DISABLE_MAX_PRIVILEGE`), in a job (by
`PROC_THREAD_ATTRIBUTE_JOB_LIST`) with an exhaustive handle list, but the job
carried no limits.

`windows-sys` 0.61 is already this crate's dependency (ADR 010, listed KEEP in
`docs/research/dependency-audit.md`); this change adds **no** crate and no
feature, only more of its items.

## Decisions

1. **Low integrity now; AppContainer is a decision for the user.** The token is
   lowered to the Low integrity level (`SetTokenInformation(TokenIntegrityLevel)`
   on the restricted token, before the process is created). AppContainer was
   not built because (a) an AppContainer process cannot read its own executable
   or DLLs unless they carry an ACE for `ALL APPLICATION PACKAGES`, so it
   requires an install-time and a test-time ACL step on `px-content.exe`,
   which `px-install`/packaging does not have and a developer build directory
   will not satisfy; (b) it needs `userenv` profile creation and cleanup
   (persistent per-user state the crate has avoided); (c) none of it can be
   run here, and a first execution that fails to start the process is a total
   outage. What Low integrity does: mandatory integrity blocks **writes** to
   everything not labelled Low, regardless of DACLs. What it does **not** do:
   it does not stop reads of files the user's DACLs allow, and does not remove
   network access. So the Windows gate item "open a file" is met for writes and
   **not** for reads, and "connect a socket" is not met at all, until the
   AppContainer (or a restricted-SID set, which would stop reads) is chosen.
2. **Job limits are set before the process exists** (`apply_limits`, on the
   freshly created job, before `CreateProcessAsUserW`): per-process commit limit
   (`JOB_OBJECT_LIMIT_PROCESS_MEMORY`) = `Policy::memory`; `KILL_ON_JOB_CLOSE`;
   `ACTIVE_PROCESS` = 1; `DIE_ON_UNHANDLED_EXCEPTION`; and every
   `JOB_OBJECT_UILIMIT_*` (desktop, display settings, exit windows, global
   atoms, USER handles, read and write clipboard, system parameters). This
   covers the gate's clipboard and spawn-a-process items structurally.
   `SandboxedChild::kill` now terminates the job, so a tree dies together.
3. **The memory cap is committed memory, not address space.**
   `ProcessMemoryLimit` bounds commit charge. The 1 GiB figure was measured as
   *address space* on Linux (218 MiB peak, ADR 043); commit is smaller than
   address space for the same process, so the same figure is at least as
   generous. It has **not** been measured on Windows, so no claim is made that
   1 GiB is right there. `Rung::MemoryCap` is spelled "commit limit" on Windows.
4. **Mitigation policies** are passed at creation
   (`PROC_THREAD_ATTRIBUTE_MITIGATION_POLICY`): DEP, ATL thunk, SEHOP,
   heap-terminate-on-corruption, bottom-up and high-entropy ASLR, strict handle
   checks, **Win32k system call disable**, extension points disabled, font
   loading disabled, no remote images, no low-label images. Plus
   `PROC_THREAD_ATTRIBUTE_CHILD_PROCESS_POLICY` = restricted (belt and braces with
   the active-process limit). Not included: prohibit-dynamic-code (sanitizer
   builds and some allocators need executable memory; measure first) and
   control-flow-guard (needs a binary built for it). A policy the running
   Windows does not know makes `CreateProcess` fail, so the launch fails closed.
5. **Everything that can be read back is read back; the rest is not reported.**
   After creation the parent checks, from the OS: `IsProcessInJob`; the job's
   extended limits and UI restrictions via `QueryInformationJobObject`; and the
   integrity RID of the child's own token (opened with `OpenProcessToken` on the
   process handle). Any mismatch kills the child and fails the spawn. Only then
   are `RestrictedToken`, `JobObject`, `LowIntegrity` and `MemoryCap` in
   `applied()`, and `memory_limit()` is `Some`. Mitigation policies cannot be
   queried from outside a process (`GetProcessMitigationPolicy` reads only the
   caller), so they are applied-or-fail but **not** listed in `applied()`.
6. **`Rung::LowIntegrity` joins the Windows `FLOOR`.** A machine that cannot
   lower a token's integrity level refuses to launch. Detection makes the
   token and lowers it, and applies and reads back the limits on a probe job,
   rather than assuming either works.

## Consequences

- Windows has a memory cap, a tree kill, UI/clipboard isolation, no child
  processes and write isolation. It still has file read and network access;
  stage two (`enter_content_profile`) still refuses on Windows, so
  `px-content` on Windows has only the creation-time profile.
- The `sandbox_escape_*` suite and the runaway-tab memory test are Linux-only
  and were **not** ported: they need `common::sandboxed`, `px_sandbox::probe`
  Windows bodies, and a Windows run to be believed. See
  `docs/research/phase-17-windows-unverified.md` for what CI must show.
- `px-sandbox`'s unsafe grows by several blocks, each with a `SAFETY:`
  comment; first-party crates are not in `ci/unsafe-baseline.json`.
