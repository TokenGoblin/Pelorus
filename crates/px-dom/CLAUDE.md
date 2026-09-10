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

## Miri

Miri runs over five of this crate's test suites — `parse`, `ranges`,
`handles`, `snapshots`, `order` — on the nightly ADR 006 pins. See ADR 022 and
`ci/gate-miri.sh`.

This file claimed Miri ran from Phase 0, when it did not. What it covers is
narrower than "Miri runs on px-dom" suggests, so:

- **Not covered:** the deep-nesting, generation-exhaustion, conformance and
  fuzz-harness suites. Miri interprets at roughly a thousandfold slowdown and
  those build documents of 100,000 nodes and up.
- **Weakened for `tendril`:** it packs a tag bit into a pointer and casts back,
  so Miri warns that it "might miss pointer bugs". A green run says less about
  `tendril` than about the rest of the closure, and `tendril` holds every
  string in the DOM.

The point of it is not this crate's own code, which is `forbid(unsafe_code)`.
It is the ~800 unsafe tokens ADR 017 brought in — `parking_lot`, `tendril`,
`smallvec`, `string_cache` — which the unsafe audit counts and nothing else
runs.
