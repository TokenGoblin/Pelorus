# px-content

Content process binary. Implemented in phase 1.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Holds no handle it was not passed. No filesystem, no network, no
  clipboard, no GPU except through brokered IPC (invariant 1).
- Never sends its own identity, origin or partition key. The broker knows
  who it is from the channel (invariant 9).
- No unwrap/expect/panic/indexing. `[lints] workspace = true` denies them;
  do not waive them locally.
- Builds with panic = abort from Phase 1: a process that dies is contained,
  one that unwinds through a half-mutated DOM is not (§4.3).
- One process per site. Memory-capped; exceeding the cap kills this process
  and only this process.
