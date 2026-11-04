# Phase 17 syscall audit: the content process after stage two (Linux)

Groundwork for the allow list the Phase 17 gate wants (build-spec section 9
says "seccomp-bpf allowlist"). Measured, not inferred. Nothing here switches
the profile to an allow list.

## Method

- `strace` 6.8, `strace -f -qq -o <file> <test binary>` over the test binaries
  of `px-content` built with `--features testing` (unoptimised, x86_64, glibc,
  kernel 6.18.44, Landlock ABI 7).
- Workload: the Phase 10 tests that drive the real content binary over its
  pipes: `script_process` (`1+1`, protocol), `script_clock`, `script_loop`
  (timers, microtasks), `script_limits` (recursion, runaway loops, the
  watchdog's abort, regex, array fill, the isolation tests) and, for the
  sandboxed case, `sandbox_escape`. About 50 content processes in total.
- Reduction: for each process whose `execve` is `px-content`, every syscall it
  and its threads make after the `prctl(PR_SET_SECCOMP)` that installs stage
  two. (The unsandboxed test peers install only stage two; the sandboxed ones
  have stage one already in place from `pre_exec`. The reduction is the same.)
  The `awk` used is not committed (the repository is all Rust); the commands
  above reproduce the traces.
- The memory-cap tests are excluded: their processes die by allocation failure
  and add nothing.

## Result: what the process does after stage two

Twenty-four syscalls, plus two that belong to stage two's own self-check:

| group | syscalls |
|---|---|
| memory | `mmap`, `munmap`, `mprotect`, `mremap`, `brk`, `madvise` (`MADV_DONTNEED`) |
| threads and sync | `clone3`, `futex`, `set_robust_list`, `rseq`, `gettid`, `sched_getaffinity`, `prctl` (`PR_SET_NAME` only), `exit` |
| time | `clock_nanosleep` (`clock_gettime` is vDSO and makes no syscall) |
| channel | `read`, `write` (the inherited pipes and stderr) |
| signals / abort | `rt_sigaction`, `rt_sigprocmask`, `sigaltstack`, `getpid`, `tgkill`, `exit_group` |
| entropy | `getrandom` |

The self-check in `enter` adds one `openat("/proc/self/status")` (refused with
`EPERM`, which is its purpose) and `prctl(PR_GET_SECCOMP)` per process. Nothing
else opens, stats, polls, creates a socket or executes after stage two: the
process uses the descriptors it was handed with `read` and `write`.

Before stage two (loader, runtime start, Boa not yet running) the process makes
`access`, `arch_prctl`, `brk`, `close`, `execve`, `fcntl`, `fstat`,
`getrandom`, `gettid`, `landlock_*`, `mmap`, `mprotect`, `munmap`, `openat`,
`poll`, `prctl`, `pread64`, `prlimit64`, `read`, `rseq`, `rt_sigaction`,
`sched_getaffinity`, `set_robust_list`, `set_tid_address`, `sigaltstack`,
`statx`. That is the set a creation-time (stage one) allow list would have to
admit, and it includes `openat`, `execve` and `statx`: so an allow list is
necessarily two-stage as well, with the strict one applied after startup.

## How far the current profile is from an allow list

Stage one denies 17 syscalls; stage two adds about 55 (opens, directory
walks, path mutation, sockets, exec, io_uring, the new mount API, pidfd). The
kernel's x86_64 table has well over 300 entries, so the process is currently
permitted roughly 250 syscalls it has never made. The measured set is 24. What
that leaves allowed and unneeded includes `kill` and `tkill` (and `tgkill` to
any process: see below), `ptrace`-adjacent calls that are not on the list,
`statx` / `newfstatat` / `stat` (an existence leak), `memfd_create`,
`clone` of arbitrary flags, `setrlimit` and `prlimit64`, `keyctl`,
`add_key`, `mmap` with `PROT_EXEC` on anonymous memory (the process has no JIT
and could be refused it), `ioctl`, `fcntl`, `socket`-family calls on kernels
that add new ones, and every syscall a future kernel introduces.

## What the measured set suggests, and why it was not switched here

The set is small, but it is not yet shown to be stable:

- It was measured on one libc, one architecture, one kernel and an unoptimised
  build. A release build, aarch64 and a different glibc or musl change which
  of `rseq`, `clone3`, `set_robust_list`, `sched_getaffinity` and `madvise`
  appear.
- It covers the paths the tests reach. Paths that make a syscall only rarely
  (a panic with a backtrace, stack overflow's `SIGSEGV` handler returning
  through `rt_sigreturn`, `sched_yield`, `membarrier`, a late `close`) are the
  ones an allow list kills the process on. The default action must be
  `SECCOMP_RET_KILL_PROCESS`, which makes a missed entry a crash, so the
  measured set needs a release-build, multi-libc, fault-path pass before it is
  load-bearing.
- `tgkill` cannot be allowed by number alone without letting a process signal
  the broker: the allow list needs an argument check (`tgid == own pid`) or a
  PID namespace. The first is a few BPF instructions; the second is the
  unapplied user-namespace rung.

Suggested next step, for the unit that owns the allow list: take the table
above, add `rt_sigreturn`, `sched_yield`, `close`, `fcntl`, `membarrier` and
`exit` as the conservative superset, run the sandbox tests under it as
`SECCOMP_RET_TRAP` in CI to log the misses on release builds on both
architectures, then flip to kill. The test `sandbox_escape_*` and
`sandbox_memory_cap_*` are the regression suite for it.

## Not measured

- Release build, aarch64, musl.
- The GPU process (software rasterizer). Same method applies; not run here.
- Windows. There is no equivalent: its process-creation mitigations are a
  different mechanism (docs/backlog.md).
