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
- No unwrap/expect/panic/indexing. This crate holds attacker-controlled
  markup and every tree walk in the engine runs over it, so it takes the
  workspace lints even though the root agreement does not list it.

## Not true yet

- **Miri does not run this crate's tests.** This file claimed it did, from
  Phase 0, and it never has. Recorded here rather than quietly deleted
  because the claim is worth making true: the crate is
  `#![forbid(unsafe_code)]`, so Miri finds nothing in *our* code, but once
  the html5ever integration lands the tests exercise `tendril`,
  `smallvec` and `string_cache` — roughly 800 unsafe tokens of dependency
  (ADR 017) that nothing else here checks. See docs/backlog.md.
