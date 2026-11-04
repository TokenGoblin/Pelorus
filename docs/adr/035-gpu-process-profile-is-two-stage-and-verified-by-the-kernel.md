# 035 — The GPU process profile is two-stage, and the kernel, not the process, vouches for it

- **Status:** proposed (the design is built and tested on Linux; the user should
  confirm it, and decide what Windows does until its profile exists)
- **Date:** 2026-11-04
- **Phase:** 8
- **Invariants touched:** 8 (fail closed), 9 (authority comes from the channel)

## Context

ADR 033 (e) requires a GPU-process profile that denies `open*`, `socket`,
`connect` and `execve`, and left open how a process that must `execve` and
dynamically link, both of which open files, can start under a policy that denies
`open`. The plan (unit 5) calls this the highest risk in the phase and names two
candidates: a static binary, or a policy whose second stage only tightens.

Seccomp filters are inherited across `execve` and cannot be removed. A filter
denying `openat` installed between `fork` and `exec` kills the process in the
dynamic loader. A static binary was not considered further: it would need a
different target and libc for this one binary, and it would not be the binary a
real driver stack could ever use, so the property proven would not be the
property shipped.

What was measured, on this container's kernel (6.18, Landlock ABI 7, glibc):
after `main` starts, and after the process has read its first message, the
process makes only `read`, `write`, `brk`, `mmap`, `munmap`, `mremap` and
`exit_group`. Startup is where the files are opened.

A second finding: the existing Landlock detection read the active-LSM list
under securityfs, which is not mounted here. It reported Landlock as
inconclusive, so unavailable, on a kernel that enforces it.

## Decision

**Two stages, the second applied by the process to itself.**

1. At creation, between `fork` and `exec`, exactly as for a content process:
   `no_new_privs` and the escape-primitive deny list.
2. As the first act of `main`, single-threaded, before the first byte is read
   from the channel: `px_sandbox::enter_gpu_profile`. Landlock where the kernel
   has it (every filesystem right handled, so none is allowed except read access
   to paths explicitly passed in, which for the software backend is none; TCP
   bind and connect and abstract-socket and signal scope where the ABI has
   them), then a second seccomp filter that adds the calls that reach the
   filesystem or the network (opens, directory walking and mutation, sockets,
   `execve`, io_uring, the new mount API, pidfds). It then tries to open a file
   and requires that to be denied.

**The broker does not take the process's word.** After the process says it is
ready and before any display list is sent, the broker calls
`SandboxedChild::verify_gpu_profile`, which reads `/proc/<pid>/status` and
requires `NoNewPrivs: 1`, `Seccomp: 2` and at least two filters. A kernel that
does not report the filter count cannot be verified and the process is not
used. Landlock cannot be verified this way (the kernel reports nothing about it
in `/proc`); it is recorded as the process's report, kept out of the verified
list, and shown by behaviour in the tests.

**The failure of any step is a failure to start,** which the session treats as
any other failure: render in software, replace within the bound, then stay on
software.

**Landlock detection asks the kernel** (`landlock_create_ruleset` with the
version flag, which takes no pointer and changes nothing) instead of reading
securityfs. Inconclusive is still unavailable.

**Windows:** `enter_gpu_profile` and `verify_gpu_profile` refuse. The restricted
token and job object a content process gets at creation are applied to the GPU
process too, but they are not a denial of files and sockets, and there is no
Windows code here that could be run to show otherwise. A Windows session
therefore renders in software, and says why in its event log. This is the
fail-closed reading, and it is a real gap, not a pass.

## Alternatives rejected

- **A static binary.** Rejected above: it proves a property of a binary that
  would not ship.
- **Deny `open` in stage one and pre-open what startup needs.** Not possible:
  the dynamic loader opens by path, before any of our code runs.
- **`SECCOMP_RET_USER_NOTIF`** or a stateful filter ("allow open until a marker
  syscall"): a supervisor deciding per call is a larger trusted component than
  the problem, and a filter that changes meaning at runtime is harder to review
  than two that each mean one thing.
- **The process reports its own confinement and the broker believes it.**
  Rejected by invariant 9. The one party that gains from lying is the one asked.
- **An allow list in stage two.** The measurement above says it would be about
  ten syscalls for this binary. It is rejected for now only because the
  allocator, thread runtime and glibc differ across the distributions we will
  ship on, and a missing entry kills the process on a user's machine in a way
  this container cannot show. Phase 17 owes the allow list and this measurement
  is its starting point. Until then the filter is a deny list, the weaker shape,
  and says so.

## Consequences

- The process runs unconfined between `exec` and the profile: the dynamic
  loader and `std` startup. It has read nothing from the channel. This is the
  window ADR 007 wanted closed for content processes, and it is open here, by
  necessity, for first-party startup code only.
- The profile applies per thread, so it refuses to run in a process that has
  started a second thread. The GPU binary starts none before it.
- Verification costs a file read per process start. Nothing per frame.
- New unsafe in `px-sandbox`: three Landlock syscalls and one descriptor
  adoption, plus the version probe, and a testing-only probe module. All carry
  SAFETY comments against `landlock(7)` and `socket(2)`/`connect(2)`/`execve(2)`.
  `ci/unsafe-baseline.json` counts the vendored dependency tree only and does
  not move; first-party unsafe is held by `unsafe-headers` and review.
- aarch64 numbers come from `libc` constants and were not compiled here.

## Verification

This decision is wrong if a GPU process can be shown to open a file or create a
socket after `Ready`; the probe in `px-gpu/tests/process_boundary.rs` is that
observation, run against the real binary. It is also wrong if a process that
never entered stage two is ever used; `gpu_process_unconfined_is_not_used` runs
the verification against a child that did not. What is not shown: the real
driver half (no GPU here), Landlock verified independently of seccomp, and
anything on Windows.
