//! Chrome to display list.
//!
//! [`paint`] appends the window's contents (everything inside the window outline)
//! to a [`DisplayList`] in window coordinates shifted by `origin`;
//! [`paint_preview`] adds the backdrop and the soft shadow around it, which is
//! what the render baselines and the mockups show. Only `FillRect`, `PushClip`,
//! `PopClip` and `GlyphRun` come out, so the list rasterizes identically on every
//! platform with the integer software rasterizer.
//!
//! No page-controlled pixel is drawn: tab titles are sanitised text, and a
//! favicon is never fetched or shown (the dot is the chrome's own).

use px_brand::palette::Rgb;
use px_layout::geom::px;
use px_paint::{Color, DisplayList, Item, Rect as PaintRect};

use crate::address::Emphasis;
use crate::chrome::Chrome;
use crate::geom::{Point, Rect};
use crate::layout::{self as lay, Layout};
use crate::shape::{self, Circle, Path, SUB, Squircle, color, fill};
use crate::tabs::Hover;
use crate::text::{self, AU_PER_PX, TextShaper};
use crate::theme::{Colors, Theme};
use crate::toolbar::ToolButton;

struct Painter<'a> {
    list: &'a mut DisplayList,
    origin: Point,
    c: Colors,
    shaper: &'a dyn TextShaper,
}

impl Painter<'_> {
    fn at(&self, r: Rect) -> Rect {
        Rect::new(r.x + self.origin.x, r.y + self.origin.y, r.w, r.h)
    }

    fn squircle(&mut self, r: Rect, radius: i32, col: Color) {
        let s = Squircle::new(self.at(r), radius);
        fill(self.list, &s, col);
    }

    fn rect(&mut self, r: Rect, col: Color) {
        let r = self.at(r);
        if r.is_empty() {
            return;
        }
        self.list.push(Item::FillRect {
            rect: PaintRect {
                x: px(r.x),
                y: px(r.y),
                width: px(r.w),
                height: px(r.h),
            },
            color: col,
        });
    }

    fn stroke(&mut self, r: Rect, radius: i32, width: i32, col: Color) {
        let s = Squircle::new(self.at(r), radius).stroke(width);
        fill(self.list, &s, col);
    }

    fn circle(&mut self, cx8: i32, cy8: i32, r8: i32, col: Color) {
        let c = Circle::disc(
            cx8 + self.origin.x * SUB,
            cy8 + self.origin.y * SUB,
            r8,
        );
        fill(self.list, &c, col);
    }

    fn ring(&mut self, cx8: i32, cy8: i32, r8: i32, thick8: i32, col: Color) {
        let c = Circle::ring(
            cx8 + self.origin.x * SUB,
            cy8 + self.origin.y * SUB,
            r8,
            thick8,
        );
        fill(self.list, &c, col);
    }

    /// Polyline with points in 1/8 px relative to `(cx8, cy8)`.
    fn path(&mut self, cx8: i32, cy8: i32, pts: &[(i32, i32)], half8: i32, col: Color) {
        let ox = cx8 + self.origin.x * SUB;
        let oy = cy8 + self.origin.y * SUB;
        let abs: Vec<(i32, i32)> = pts.iter().map(|(x, y)| (ox + x, oy + y)).collect();
        fill(self.list, &Path::polyline(&abs, half8), col);
    }

    fn text(&mut self, s: &str, size: i32, x: i32, band: Rect, col: Color) -> i32 {
        let shaped = self.shaper.shape(s, size);
        let base = text::baseline_in(self.shaper, size, band.y, band.h);
        text::push(
            self.list,
            &shaped,
            x + self.origin.x,
            base + self.origin.y,
            col,
        );
        shaped.width
    }

    fn clip(&mut self, r: Rect) {
        let r = self.at(r);
        self.list.push(Item::PushClip {
            rect: PaintRect {
                x: px(r.x),
                y: px(r.y),
                width: px(r.w),
                height: px(r.h),
            },
        });
    }

    fn pop(&mut self) {
        self.list.push(Item::PopClip);
    }
}

fn rgb(c: Rgb, a: u8) -> Color {
    color(c, a)
}

/// `(cos, sin)` of 0, 30, ..., 330 degrees, times 1000. Exact table: no trig at
/// run time, so the reload arc is the same bytes everywhere.
const UNIT: [(i32, i32); 12] = [
    (1000, 0),
    (866, 500),
    (500, 866),
    (0, 1000),
    (-500, 866),
    (-866, 500),
    (-1000, 0),
    (-866, -500),
    (-500, -866),
    (0, -1000),
    (500, -866),
    (866, -500),
];

/// Paint the window's contents. See the module docs.
pub fn paint(chrome: &Chrome, shaper: &dyn TextShaper, theme: Theme, origin: Point, list: &mut DisplayList) {
    let layout = chrome.layout();
    let mut p = Painter {
        list,
        origin,
        c: theme.colors(),
        shaper,
    };
    let s = p.c.surfaces;
    let win = Rect::new(0, 0, layout.size.w, layout.size.h);
    let r = chrome.config().window_radius;

    // Surfaces: window body, then the three bands with the window's corners.
    p.squircle(win, r, rgb(s.window, 255));
    let strip = Squircle::with_radii(p.at(layout.strip), [r, r, 0, 0]);
    fill(p.list, &strip, rgb(s.bar, 255));
    p.rect(layout.toolbar, rgb(s.bar, 255));
    let page = Squircle::with_radii(p.at(layout.page), [0, 0, r, r]);
    fill(p.list, &page, rgb(s.page, 255));
    p.rect(
        Rect::new(0, lay::CHROME_H - 1, layout.size.w, 1),
        rgb(s.hairline, 255),
    );

    paint_controls(&mut p, chrome, &layout);
    paint_tabs(&mut p, chrome, &layout);
    paint_toolbar(&mut p, chrome, &layout);
    paint_address(&mut p, chrome, &layout);

    // The one hairline round the window.
    p.stroke(win, r, 1, rgb(s.hairline, 255));
}

/// Backdrop, soft shadow, then the window at `(margin, margin_top)`. The
/// returned canvas size is `size + 2 * margin` wide and `size + margin +
/// margin_top` tall; with the mockups' 40/36 margins and a 920 x 568 window this
/// is their 1000 x 640.
pub fn paint_preview(chrome: &Chrome, shaper: &dyn TextShaper, theme: Theme, margin: Point) -> (DisplayList, crate::geom::Size) {
    let size = chrome.size();
    let canvas = crate::geom::Size::new(size.w + 2 * margin.x, size.h + 2 * margin.y);
    let mut list = DisplayList::new();
    let c = theme.colors();
    list.push(Item::FillRect {
        rect: PaintRect {
            x: px(0),
            y: px(0),
            width: px(canvas.w),
            height: px(canvas.h),
        },
        color: rgb(c.surfaces.backdrop, 255),
    });
    shape::shadow(
        &mut list,
        Rect::new(margin.x, margin.y, size.w, size.h),
        chrome.config().window_radius,
        14,
        18,
        c.surfaces.shadow_alpha,
    );
    paint(chrome, shaper, theme, margin, &mut list);
    (list, canvas)
}

fn paint_controls(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let cols = [p.c.red, p.c.amber, p.c.teal];
    for (i, r) in layout.controls.iter().enumerate() {
        if r.is_empty() {
            continue;
        }
        let strong = chrome.hover_control() == Some(i) || !chrome.window_focused();
        // The mockups draw them at 90%; an unfocused window dims them to 40%.
        let a = if !chrome.window_focused() {
            102
        } else if strong {
            255
        } else {
            230
        };
        p.squircle(*r, 5, rgb(cols[i], a));
    }
}

fn paint_tabs(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let strip = chrome.tabs();
    let c = p.c;
    for (i, (tab, slot)) in strip.tabs().iter().zip(&layout.tabs).enumerate() {
        if slot.rect.is_empty() {
            continue;
        }
        let active = strip.active_index() == Some(i);
        let hovered = matches!(strip.hover(), Hover::Tab(j) | Hover::Close(j) if j == i);
        if active {
            p.squircle(slot.rect, lay::TAB_RADIUS, rgb(c.surfaces.raised, 255));
            p.stroke(slot.rect, lay::TAB_RADIUS, 1, rgb(c.surfaces.hairline, 255));
        } else if hovered {
            p.squircle(slot.rect, lay::TAB_RADIUS, rgb(c.surfaces.raised, 150));
        }
        // The favicon slot: the chrome's own dot, a ring while loading.
        let (cx, cy) = ((slot.rect.x + 16) * SUB, (slot.rect.y + lay::TAB_H / 2) * SUB);
        let dot = if active { rgb(c.teal, 255) } else { rgb(c.muted, 128) };
        if tab.loading {
            p.ring(cx, cy, 36, 13, dot);
        } else {
            p.circle(cx, cy, 36, dot);
        }
        // Title.
        let show_close = !slot.close.is_empty() && (active || hovered);
        let right = if show_close { slot.close.x - 4 } else { slot.rect.right() - 12 };
        let tx = slot.rect.x + 30;
        let room = right - tx;
        if room >= 8 {
            let title = text::fit(p.shaper, tab.display_title(), lay::TEXT_CHROME, room);
            let col = if active { c.text } else { c.muted };
            p.text(&title, lay::TEXT_CHROME, tx, slot.rect, rgb(col, 255));
        }
        if show_close {
            let close_hover = strip.hover() == Hover::Close(i);
            if close_hover {
                p.squircle(slot.close, 7, rgb(c.surfaces.hairline, 255));
            }
            let m = slot.close.center();
            let (cx, cy) = (m.x * SUB, m.y * SUB);
            let d = 4 * SUB / 1; // arm length
            let col = rgb(if close_hover { c.text } else { c.muted }, 255);
            p.path(cx, cy, &[(-d, -d), (d, d)], 7, col);
            p.path(cx, cy, &[(-d, d), (d, -d)], 7, col);
        }
    }
    // New tab.
    let nt = layout.new_tab;
    if strip.hover() == Hover::NewTab {
        p.squircle(nt, lay::BUTTON_RADIUS, rgb(c.surfaces.raised, 255));
    }
    let m = nt.center();
    let (cx, cy) = (m.x * SUB, m.y * SUB);
    let col = rgb(if strip.hover() == Hover::NewTab { c.text } else { c.muted }, 255);
    p.path(cx, cy, &[(-5 * SUB, 0), (5 * SUB, 0)], 8, col);
    p.path(cx, cy, &[(0, -5 * SUB), (0, 5 * SUB)], 8, col);
}

fn paint_toolbar(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let c = p.c;
    let tb = chrome.toolbar();
    for (b, r) in [
        (ToolButton::Back, layout.back),
        (ToolButton::Forward, layout.forward),
        (ToolButton::Reload, layout.reload),
        (ToolButton::Overflow, layout.overflow),
    ] {
        let enabled = tb.enabled(b);
        if enabled && tb.pressed() == Some(b) {
            p.squircle(r, lay::BUTTON_RADIUS, rgb(c.surfaces.hairline, 255));
        } else if enabled && tb.hover() == Some(b) {
            p.squircle(r, lay::BUTTON_RADIUS, rgb(c.surfaces.raised, 255));
        }
        let col = if !enabled {
            rgb(c.muted, 90)
        } else if tb.hover() == Some(b) {
            rgb(c.text, 255)
        } else if b == ToolButton::Back || b == ToolButton::Forward || b == ToolButton::Reload {
            rgb(c.text, 255)
        } else {
            rgb(c.muted, 255)
        };
        let m = r.center();
        let (cx, cy) = (m.x * SUB, m.y * SUB);
        let u = SUB;
        match b {
            ToolButton::Back => p.path(cx, cy, &[(3 * u, -6 * u), (-3 * u, 0), (3 * u, 6 * u)], 8, col),
            ToolButton::Forward => p.path(cx, cy, &[(-3 * u, -6 * u), (3 * u, 0), (-3 * u, 6 * u)], 8, col),
            ToolButton::Reload if tb.is_loading() => {
                let d = 5 * u;
                p.path(cx, cy, &[(-d, -d), (d, d)], 8, col);
                p.path(cx, cy, &[(-d, d), (d, -d)], 8, col);
            }
            ToolButton::Reload => {
                // 300 degrees of a 6.5 px circle, then an arrowhead at its start.
                let rad = 13; // 6.5 px in half-pixels
                let pts: Vec<(i32, i32)> = (1..=11)
                    .rev()
                    .map(|k| {
                        let (cs, sn) = UNIT[k];
                        (rad * u / 2 * cs / 1000, rad * u / 2 * sn / 1000)
                    })
                    .collect();
                p.path(cx, cy, &pts, 8, col);
                if let Some(&(hx, hy)) = pts.first() {
                    p.path(
                        cx,
                        cy,
                        &[(hx + u, hy - 5 * u), (hx, hy), (hx - 5 * u, hy - u)],
                        8,
                        col,
                    );
                }
            }
            ToolButton::Overflow => {
                for dy in [-7, 0, 7] {
                    p.circle(cx, cy + dy * u, 14, col);
                }
            }
        }
    }
}

fn paint_address(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let c = p.c;
    let a = chrome.address();
    let f = layout.field;
    p.squircle(f, lay::FIELD_RADIUS, rgb(c.surfaces.raised, 255));
    if a.is_editing() {
        // The focus ring: blue, 2 px, drawn inside the field. Colour is not the
        // only cue: it is also a different shape from the idle field's edge.
        p.stroke(f, lay::FIELD_RADIUS, 2, rgb(c.blue, 255));
    }
    // Isolated chip.
    if a.is_isolated() && !layout.chip.is_empty() {
        let ch = layout.chip;
        p.stroke(ch, lay::CHIP_RADIUS, 1, rgb(c.teal, 115));
        let (cx, cy) = ((ch.x + 15) * SUB, (ch.y + ch.h / 2) * SUB);
        p.ring(cx, cy, 34, 13, rgb(c.teal, 255));
        p.circle(cx, cy, 12, rgb(c.teal, 255));
        p.text(px_brand::ui::ISOLATED, lay::TEXT_CHIP, ch.x + 27, ch, rgb(c.teal, 255));
    }
    let band = Rect::new(layout.text_x, f.y, (layout.text_right - layout.text_x).max(0), f.h);
    if band.is_empty() {
        return;
    }
    p.clip(band);
    if a.is_editing() {
        let (visible, caret) = a.visible_text();
        let shaped = p.shaper.shape(&visible, lay::TEXT_ADDRESS);
        let x_of = |i: usize| layout.text_x + shaped.x_at(i) / AU_PER_PX;
        let inner = Rect::new(0, f.y + 7, 0, f.h - 14);
        if let (Some((s0, s1)), None) = (a.selection(), a.preedit_range()) {
            let (x0, x1) = (x_of(s0), x_of(s1));
            p.rect(Rect::new(x0, inner.y, x1 - x0, inner.h), rgb(c.blue, 90));
        }
        if visible.is_empty() {
            p.text(px_brand::ui::ADDRESS_PLACEHOLDER, lay::TEXT_ADDRESS, layout.text_x, f, rgb(c.muted, 255));
        } else {
            p.text(&visible, lay::TEXT_ADDRESS, layout.text_x, f, rgb(c.text, 255));
        }
        if let Some((a0, a1)) = a.preedit_range() {
            let (x0, x1) = (x_of(a0), x_of(a1));
            p.rect(Rect::new(x0, f.y + f.h - 9, x1 - x0, 1), rgb(c.text, 255));
        }
        if chrome.window_focused() {
            p.rect(Rect::new(x_of(caret), inner.y, 1, inner.h), rgb(c.text, 255));
        }
    } else if a.display().is_empty() {
        p.text(px_brand::ui::ADDRESS_PLACEHOLDER, lay::TEXT_ADDRESS, layout.text_x, f, rgb(c.muted, 255));
    } else {
        let mut x = layout.text_x;
        for seg in &a.display().segments {
            let col = match seg.emphasis {
                Emphasis::Strong => c.text,
                Emphasis::Muted => c.muted,
            };
            let w = p.text(&seg.text, lay::TEXT_ADDRESS, x, f, rgb(col, 255));
            x += w / AU_PER_PX;
            // Keep sub-pixel remainders honest across segments.
            let _ = w;
        }
    }
    p.pop();
}
