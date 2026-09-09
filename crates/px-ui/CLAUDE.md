# px-ui

Browser chrome: winit and wgpu, thin hand-rolled widgets. Implemented in phase 18.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Every user-visible string comes from px-brand. No literals.
- The UI does not move (invariant 3). After Phase 18 the layout is frozen by
  screenshot diff; a change needs an ADR with a user-facing reason.
- No page-controlled content anywhere in the chrome region, ever.
- Origin display: eTLD+1 emphasised, the rest de-emphasised, punycode for
  mixed-script labels. Mark the absence of security, not its presence — no
  padlock theatre.
- Keyboard first.
