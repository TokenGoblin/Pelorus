# px-bindings

WebIDL parser and Rust code generation. Implemented in phase 11.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- The generator is the deliverable. Hand-written binding glue count is zero;
  if something cannot be generated, fix the generator.
