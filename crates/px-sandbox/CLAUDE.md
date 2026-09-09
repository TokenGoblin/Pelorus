# px-sandbox

Process sandboxing; the only crate permitted unsafe. Implemented in phase 2.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- The sole exception to forbid(unsafe_code). This crate carries
  `#![deny(unsafe_op_in_unsafe_fn)]` instead.
- Every unsafe block carries a `// SAFETY:` comment justified against the
  OS documentation, not against what happened to work.
- Fail closed. If a policy cannot be applied, the process does not launch
  (invariant 8). Never a silent fallback to unsandboxed.
- ASAN and TSAN builds of this crate run in CI (§4.5).
- Adversarial review required (§10).
