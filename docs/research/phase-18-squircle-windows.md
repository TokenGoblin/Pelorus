# Phase 18: squircle top-level windows

Directive (ui-direction.md): every window is a squircle, anti-aliased, with no
hard-edged box. Covers the main window, dialogs, popups, menus and tooltips.

One shape, one mask: `px_display::shape::WindowShape` (radius per window kind;
exponent 37/8 = 4.625) produces an 8-bit coverage mask with
`px_ui::shape::coverage`, the chrome's own arithmetic (4 x 4 samples, integer
geometry, correctly rounded operations only), so OS windows and chrome corners
agree pixel for pixel and the mask is bit-identical on every platform. Tests pin
its sum, symmetry, determinism and equality with the chrome's coverage.

## X11 (verified under Xvfb, with and without xcompmgr)

| Situation | Mode | Edge |
| --- | --- | --- |
| compositor owns `_NET_WM_CM_S0` and a 32-bit TrueColor ARGB visual exists | `Argb` | anti-aliased (premultiplied alpha) |
| no compositor, SHAPE present | `ShapeExt` | aliased (mask thresholded at half coverage) |
| neither | `Software` | rectangle, warning on stderr |

- ARGB: `CreateColormap` on the ARGB visual, `CreateWindow` depth 32 with
  BorderPixel and Colormap (else BadMatch), `PutImage` depth 32 of the frame
  premultiplied under the mask. The compositor check is made once at creation.
- Input: SHAPE input region (rectangles from the mask) so the server itself
  gives transparent corners to the window below; a software filter drops presses
  and turns moves into one Leave for servers without SHAPE and for synthetic
  events. Tooltips get an empty input region.
- Frameless through `_MOTIF_WM_HINTS`; popups, menus and tooltips are
  override-redirect with `_NET_WM_WINDOW_TYPE` set.
- Screenshots: `docs/brand/ui-squircle-argb.png`, `-shape-fallback.png` and the
  12x corner zooms, produced by `PX_SQUIRCLE_SHOTS=docs/brand cargo test -p
  px-display --test squircle`.

## Win32 (compile-checked for x86_64-pc-windows-gnu, never run: UNVERIFIED)

Written from the documentation. Unverified until Windows CI runs it:

- `WS_EX_LAYERED` + `UpdateLayeredWindow(ULW_ALPHA, AC_SRC_ALPHA)` with a 32-bit
  top-down DIB section holding premultiplied BGRA (`Mask::compose_bgra`, which
  is unit-tested on Linux). Layered windows are DWM-composited, so no separate
  DWM path is needed.
- `WS_POPUP | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX` (no `WS_CAPTION`);
  `WM_NCCALCSIZE` returns 0 so the frame is not drawn or reserved. Resize grips
  come from `WM_NCHITTEST` (6 px, invisible).
- Menus/popups/tooltips: `WS_EX_TOOLWINDOW | TOPMOST | NOACTIVATE`, shown with
  `SW_SHOWNOACTIVATE`; tooltips add `WS_EX_TRANSPARENT`.
- Questions for the first Windows run: does `UpdateLayeredWindow` from inside
  `WM_PAINT` flicker; is the resize loop smooth (every present rebuilds a DIB);
  does a maximised thick-frame popup overshoot the work area; per-monitor DPI
  moves (the mask follows `WM_DPICHANGED`).
