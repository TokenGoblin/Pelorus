# px-ipc

Typed channels, postcard codec, handle passing. Implemented in phase 1.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- postcard over typed enums, with size limits. No serde_json in IPC.
- No length prefix from untrusted bytes drives an allocation without a
  bound check (§4.4) — and the bound is not enough on its own. Read payloads
  incrementally: a declared megabyte that never arrives must cost a chunk, not
  a megabyte. Return the buffer afterwards; Vec::clear keeps its capacity.
- Every direction has a tag, and trailing bytes are rejected. Without both, a
  peer that reflects the bytes it was sent produces valid-looking replies.
- Any error that could leave the stream misaligned poisons the channel
  permanently. A channel that cannot resynchronise must not look healthy.
- Every message type needs a cargo-fuzz target for its deserializer (§4.5).
- Held to §4.3's panic lints (`[lints] workspace = true`) even though §4.3
  names only px-content, px-net and px-mcp. This crate decodes bytes written
  by a process an attacker may fully control; being unnamed in that list is an
  omission, not a permission.
- No lock-free code as of Phase 1, and therefore no loom. Add loom in the same
  commit as the first lock-free structure, not before — build-spec §4.5
  requires it for lock-free code, and there is none.
- Handle passing is NOT implemented here. ADR 005 defers it to Phase 2; it
  needs unsafe and this crate forbids it. The transport is generic over
  Read + Write so the eventual socket replaces inherited pipes without
  touching a caller.
- Nothing in the wire vocabulary may name its own sender. ci/gate-ipc.sh
  fails on such a field. Naming a resource is fine; the broker authorises it.
- Adversarial review: this crate gets a session whose only job is attacking
  the previous session's output (§10).
