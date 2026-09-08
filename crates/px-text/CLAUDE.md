# px-text

Shaping, font fallback, bidi, line breaking, IME. Implemented in phase 9.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- IME integration (Windows TSF, Linux IBus/Fcitx) is not optional polish.
  Without it, CJK, Korean and Vietnamese users cannot type at all.
- Baselines are per platform and reviewed together; Windows and Linux text
  divergence is expected and must be seen, not averaged away.
