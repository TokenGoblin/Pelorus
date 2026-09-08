# px-ipc

Typed channels, postcard codec, handle passing. Implemented in phase 1.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- postcard over typed enums, with size limits. No serde_json in IPC.
- No length prefix from untrusted bytes drives an allocation without a
  bound check (§4.4).
- Every message type needs a cargo-fuzz target for its deserializer (§4.5).
- Lock-free code here is checked with loom.
- Adversarial review: this crate gets a session whose only job is attacking
  the previous session's output (§10).
