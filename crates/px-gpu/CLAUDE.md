# px-gpu

The GPU process boundary: the backend trait, the software rasterizer, the
`px-gpu` binary and the `GpuSession` supervisor. Phase 8. Wgpu is not here; it
comes later by its own ADR (ADR 034), and the process boundary below is the same
whichever backend sits inside it.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Runs in its own sandboxed process with no ambient filesystem access and no
  network. Anything it opens (a render node, driver libraries) is passed in by the
  broker or granted read-only by the sandbox policy; it opens nothing else and
  makes no network connection. Killing it must fall back to software without
  losing the session. (The gate's "no filesystem capability" is read this way: a
  real driver cannot literally have none. ADR 033, decided 2026-11-04.)
- **The process confines itself first** (ADR 035): `main` calls
  `px_sandbox::enter_gpu_profile` before it reads a byte, and exits if that
  fails. The broker does not believe the process's `Ready`; it asks the kernel
  (`SandboxedChild::verify_gpu_profile`) and sends nothing until that agrees. Do
  not add a thread, an open, or a socket to the binary before the profile
  lands; it refuses a threaded caller, and an open after it fails by design.
- The software rasterizer is not a legacy path: it is what makes CI
  rendering deterministic, and it is exercised on every run. The session's
  fallback frame and the GPU process's frame are the same function of the same
  list, so "the next frame equals the software baseline" is a byte comparison.
- `GpuSession` owns no pixels and holds nothing the broker depends on (no
  `ChannelId`, frame or tab). Any failure of the process (spawn refused, not
  confined, died, killed, hung, malformed, oversized, a lie about the frame)
  drops it, logs a `Cause`, and renders the frame in software in the same call.
  Replacement is on the next frame, bounded by `max_restarts` (default 8, as
  `ContentProcess`), over the session's life; then software for good.
- Everything the broker reads from the process is hostile bytes. Strips are
  reassembled by `wire::FrameAssembler`, which accepts exactly the one valid
  sequence the receiver's own arithmetic allows. Memory follows strips as they
  arrive, never a declared size. Do not add a path that sizes anything from a
  value the process sent.
- Test-only capabilities (the probe of forbidden operations, fault injection)
  are behind `cfg(feature = "testing")`, here and in `px-ipc` and `px-sandbox`,
  and `cargo xtask sandbox` scans the release `px-gpu` for the probe's marker.
  The boundary tests turn the feature on through a self dev-dependency, so a
  plain `cargo test` runs them rather than compiling them away.
- wgpu and the C drivers beneath it are where memory safety ends. The
  sandbox is the control here, not the type system (§4).

## Status (honest)

Proven on Linux with the software rasterizer: the real binary under the two-stage
profile (kernel-verified), every forbidden operation denied, kill / malformed /
oversized / hang / lying frames fall back with the next frame byte-identical.
Not proven: anything with a real GPU or driver; the Windows profile (it refuses,
so Windows renders in software); Landlock independently of seccomp.
