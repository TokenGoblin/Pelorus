# px-update

Signed manifest fetch and data-file updates. Implemented in phase 20.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- The update URL and the signing key live HERE, not in px-brand. They are
  brand-independent and permanent: a rename must never change them, or every
  installed copy silently stops receiving security updates (§14.1).
- The manifest request carries no identifiers: no unique ID, no cookies, no
  detailed user-agent, no query parameters, over a connection sharing
  nothing with browsing state (invariant 4).
- A tampered manifest is rejected before anything it names is fetched.
- data/ updates (PSL, HSTS preload, root store) ride this same channel. A
  stale PSL means wrong security boundaries.
