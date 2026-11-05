# UI direction

*Design only. The chrome is built in Phase 18 and its layout is locked by
screenshot-diff tests from then (invariant 3), so this is the look to build
toward, not code. Mockups: `ui-mockup-dark.svg`, `ui-mockup-light.svg`.*

**Clean and quiet, at home on macOS and Linux.** Soft squircle shapes, one
accent, no ornament.

## Shape

- Squircles (superellipse corners, exponent about 4.6), not plain rounded
  rectangles: window 22, address field 13, tab 11, buttons 10, search field 18.
- Window controls are neutral squircle dots with no OS-specific glyphs, so the
  chrome reads the same on both platforms. Follow each OS's placement rules when
  building.
- One hairline border (1 px) and one soft shadow. No gradients in the chrome.

## Layout

Tab strip (40), toolbar (52), page. Back, forward and reload on the left, the
address field filling the middle, one overflow menu on the right.

## Security shown calmly

A small "Isolated" chip at the start of the address field: a ring-dot and one
plain word, in the teal that means fine. It appears only for what the product
can honestly say (the page runs in a sandbox, invariant 1). It never shows a
padlock or shield, and never turns amber or red without a reason the user can
read. Colour is never the only signal.

## Silence

The new-tab page is the mark and one search field. No feed, no tiles, no
promotions (invariant 5).

## Type and spacing

System UI font stack at 13 px for chrome and 14 px for the address text; 4 px
grid (4, 8, 12, 16, 24). Colours come from `crates/px-brand/src/palette.rs`.

## Open

Real window-control behaviour per OS, high-contrast and reduced-motion modes,
and the small-size mark for the taskbar. Done in Phase 18 polish: focus rings
and Tab-order traversal, integer HiDPI scale, tab-strip scrolling. Window-control
dots are neutral grey until the pointer is over them (`ControlDots::Neutral`;
`Coloured` restores the always-coloured mockup look); the choice is pending the
user's preference. Previews: `ui-preview-before-*.png`, `ui-preview-after-*.png`.
