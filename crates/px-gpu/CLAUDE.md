# px-gpu

Wgpu backend and software rasterizer fallback. Implemented in phase 8.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Runs in its own sandboxed process with no ambient filesystem access and no
  network. Anything it opens (a render node, driver libraries) is passed in by the
  broker or granted read-only by the sandbox policy; it opens nothing else and
  makes no network connection. Killing it must fall back to software without
  losing the session. (The gate's "no filesystem capability" is read this way: a
  real driver cannot literally have none. ADR 033, decided 2026-11-04.)
- The software rasterizer is not a legacy path: it is what makes CI
  rendering deterministic, and it is exercised on every run.
- wgpu and the C drivers beneath it are where memory safety ends. The
  sandbox is the control here, not the type system (§4).
