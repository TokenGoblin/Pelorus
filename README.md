# Pelorus

A ground-up, memory-safe web browser for Windows and Linux.

> **Working name.** A pelorus is a sighting compass with no magnet — it reads
> bearings relative to your own vessel rather than to any external reference.
> Provisional; the codebase is built so it can be replaced in a day.
> See `docs/adr/000-name.md` (Phase 0 deliverable).

## The memory-safety claim

Memory-safe application code, over a small audited unsafe core, with process
isolation for the parts that cannot be made safe.

Not "written in Rust, therefore safe". The dependency closure contains real
unsafe — stylo, boa_gc, wgpu and the C GPU drivers beneath it, swash. Safety
ends at those boundaries, which is why the sandbox is the primary control and
Rust is what reduces how often it has to save you. See `docs/build-spec.md` §4.

## Status

**Pre-Phase 0.** No code yet. Nothing is distributed to anyone before Phase 20
completes (invariant 10) — the update channel and signed release pipeline must
exist before a build leaves this machine.

## Where things are

| | |
|---|---|
| `CLAUDE.md` | Working agreement. Loaded every session. |
| `docs/build-spec.md` | Full specification. Read §9 for the current phase. |
| `docs/adr/` | Architecture decision records. |
| `docs/backlog.md` | Out-of-phase defects. |

## License

Undecided — open decision 9 in `docs/build-spec.md` §13, along with whether
this repository is public before Phase 20.
