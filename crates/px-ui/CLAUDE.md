# px-ui

Browser chrome: thin hand-rolled widgets. Implemented in phase 18. The window
and input stack is ADR 044 (proposed); the windowing-independent core (state
machines, layout, painter, `platform::PlatformWindow` seam) is here and depends
on no window crate. Render baselines: `tests/baselines/`, re-bless with
`PX_UI_BLESS=1 cargo test -p px-ui --test baselines` and review the PNGs.

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
