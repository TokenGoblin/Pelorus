# px-a11y

Platform-independent accessibility tree, UIA and AT-SPI. Implemented in phase 16.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- Correct on its own merits, and a hard dependency of px-mcp: read_tab
  returns this tree, not raw DOM.
- Find-in-page is built on this tree, not on a second traversal.

## What is built (phase 16, platform-independent half)

- `role`: ARIA 1.2 concrete roles, fallback-list parsing, abstract roles rejected,
  name-from and children-presentational tables. `doc`: HTML-AAM implicit roles.
- `accname`: accname 1.2 steps 2A-2I iteratively (explicit frame stack), with budgets.
- `tree`: `AccessibilityTree::build`, generational `A11yId`, navigation, `aria-owns`,
  tab order. `dump`: the test serialisation. `diff`: `diff(old, new) -> Vec<Change>`.
- `wire`: `A11yUpdate` (bounded, validated). px-ipc owns the encoding; no serde here.
- Style and layout arrive via `StyleSource` / `BoundsSource`; px-a11y depends on px-dom only.

## Not built (do not assume)

UIA and AT-SPI bridges; ARIA 1.3 roles; CSS generated content in names; shadow DOM and slots;
SVG/MathML internals (SVG `title`); `aria-activedescendant`, `aria-controls`, `aria-flowto`,
`aria-details`, `aria-roledescription`, `aria-keyshortcuts`, `aria-autocomplete`,
`aria-orientation`; required owned/context role repair; table row/column indices; radio
grouping by `name`; localised default strings (`Submit`, `Reset` are English); `display: none`
from stylesheets until px-css exposes it (docs/backlog.md); incremental rebuild; find-in-page
itself (the tree is its substrate, the feature is not here).
