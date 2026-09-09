# px-a11y

Platform-independent accessibility tree, UIA and AT-SPI. Implemented in phase 16.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Correct on its own merits, and a hard dependency of px-mcp: read_tab
  returns this tree, not raw DOM.
- Find-in-page is built on this tree, not on a second traversal.
