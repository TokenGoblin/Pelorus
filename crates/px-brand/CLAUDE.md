# px-brand

The only crate that carries the product name. Implemented in phase 0.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- This crate is the single source of truth for the product name
  (build-spec §2.2). Every other crate takes the constant from here.
- The update URL and the release signing key do NOT belong here. They are
  brand-independent and permanent (§14.1); they live in px-update and are
  explicitly out of scope for a rename. ci/gate-structure.sh enforces this.
- LEGACY_CONFIG_DIRS grows by one entry at every rename and never shrinks.
  A dropped entry silently orphans a user's profile.
