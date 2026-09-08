# px-mcp

MCP subsystem, feature-gated and runtime-gated. Implemented in phase 21.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Off means off: not compiled without --features mcp, and when the runtime
  toggle is false the process is not spawned and no socket or pipe exists.
- Sandboxed like a content process. No filesystem, no network, no clipboard.
  The audit log is written by the broker, because this crate cannot write.
- Local transport only. No TCP listener, on any interface, ever.
- file:// and internal schemes are unreachable from every tool, permanently.
  No grant unlocks them.
- No download capability. Writing attacker-chosen bytes to disk is denied,
  not merely unlisted.
- Grants are (tool, scope, expiry). A restart drops all of them.
- Page content is untrusted input. read_tab wraps it in an explicit
  untrusted envelope and sanitises nothing: the defence is the grant model,
  not filtering prose.
- No unwrap/expect/panic/indexing. `[lints] workspace = true` denies them.
- Adversarial review required (§10).
