# px-websec

SOP, CORS, CSP, mixed content, referrer, COOP/COEP. Implemented in phase 12.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- A same-origin bypass needs no memory corruption at all, which makes this
  the highest-severity bug class in the project — above sandbox escapes.
- Enforcement points are enumerated and tested individually. A check that
  exists but is not reached is not a check.
- Fail closed: if a decision cannot be reached, deny.
- The testdriver.js shim WPT needs is test-only and must never compile into
  a release artifact (§14.4).
- Adversarial review required (§10).
