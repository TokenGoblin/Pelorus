# px-pages

The browser's own internal pages (`about:newtab`, `about:privacy`,
`about:version`, `about:error`) as plain HTML and CSS, rendered by our engine.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- No script, no external URL, no `url()`, no `@import`, no remote font. A page
  is one self-contained file. A test enforces it.
- Silence (invariant 5): no feed, no tiles, no promotions, no onboarding.
- No product name in these files (brand-leak gate). Anything that names the
  product comes from `px-brand`, not from here.
- Voice: `docs/brand/guide.md`. Calm, plain, no blame, no exclamation marks, no
  claim whose gate is not green.
- Both colour schemes are authored with `prefers-color-scheme`; the colour
  tokens block (`/* tokens */`) is identical in every page, and a test says so.
- Render baselines: `tests/render/baselines/ui-page-*.png`, listed in
  `tests/render/pages.toml` with source in `pages/` here (`#dark` suffix for the
  dark scheme). Re-bless with `cargo xtask render --bless` and look at the PNGs.
