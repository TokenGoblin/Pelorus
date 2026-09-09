# px-layout

Box tree; block, inline, flex and grid. Implemented in phase 6.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Geometry is Au — i32 app units at 1/60 px — with explicit saturating
  operations, so overflow is defined and testable (§4.2).
- Layout walks are iterative, never recursive.
- The box tree must be identical on repeat runs. Non-determinism here is a
  bug even when the pixels match.
