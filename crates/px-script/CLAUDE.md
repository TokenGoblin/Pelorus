# px-script

Boa host, event loop, task and microtask queues. Implemented in phase 10.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Interpreter only. No JIT, ever, without reopening invariant 6 by ADR.
- Timers are coarsened to 100us with jitter (Spectre mitigation).
- boa_gc is a tracing GC and contains real unsafe. Safety ends there (§4).
