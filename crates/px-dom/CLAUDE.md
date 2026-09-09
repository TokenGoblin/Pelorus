# px-dom

Generational arena DOM. Implemented in phase 4.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- NodeId is generational and every accessor returns Option. There is no
  infallible index API, not even a private one (§4.1).
- On generation overflow a slot is retired permanently. It never wraps —
  wrapping reintroduces exactly the use-after-free this design prevents
  (§14.3).
- Tree walks are iterative with an explicit work stack, never recursive.
- Depth limits are explicit and start at 512 (§4.4).
- Miri runs this crate's unit tests in CI.
