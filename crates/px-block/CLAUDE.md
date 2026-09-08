# px-block

Content blocking and tracker mitigations. Implemented in phase 19.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Declarative rules only. No remote rule fetch without consent (invariant 4).
- CNAME-cloaking detection, query-parameter stripping on navigation, and
  bounce-tracking mitigation live here.
