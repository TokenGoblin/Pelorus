# px-store

Partitioned storage and config-directory migration. Implemented in phase 13.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- One of only two crates permitted filesystem access (px-broker is the
  other). CI enforces by symbol audit.
- Everything is keyed by (eTLD+1 of top-level, origin). There is no
  unpartitioned code path to leave enabled by accident (invariant 2).
- Reads CONFIG_DIR plus LEGACY_CONFIG_DIRS from px-brand and migrates on
  first run (§2.3). The migration must be exercised against a populated
  profile, not an empty one.
- Miri runs this crate's unit tests in CI.
