# px-css

Stylo integration, cascade, computed values. Implemented in phase 5.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- stylo requires implementing TElement/TNode over px-dom with thread-safety
  invariants it assumes and does not check. Read what stylo assumes; do not
  infer it from what happens to compile.
- This phase may force a px-dom redesign. That is why it sits before layout.
