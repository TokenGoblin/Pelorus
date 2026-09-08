# px-gpu

Wgpu backend and software rasterizer fallback. Implemented in phase 8.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Runs in its own sandboxed process with no filesystem and no network
  capability. Killing it must fall back to software without losing the
  session.
- The software rasterizer is not a legacy path: it is what makes CI
  rendering deterministic, and it is exercised on every run.
- wgpu and the C drivers beneath it are where memory safety ends. The
  sandbox is the control here, not the type system (§4).
