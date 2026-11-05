//! Chrome to display list.
//!
//! [`paint`] appends the window's contents (everything inside the window outline)
//! to a [`DisplayList`]; [`paint_preview`] adds the backdrop and the soft shadow
//! around it, which is what the render baselines and the mockups show. Only
//! `FillRect`, `PushClip`, `PopClip` and `GlyphRun` come out, so the list
//! rasterizes identically on every platform with the integer software
//! rasterizer.
//!
//! **Scale.** Layout is in logical pixels; the painter multiplies by the
//! chrome's integer scale (1, 2 or 3) and draws in physical pixels, `origin`
//! included. Shape sizes, stroke widths, radii, font sizes and baselines are all
//! computed at the physical size, so 2x is not a blown-up 1x: a 1 px hairline is
//! 2 device pixels, text is shaped at 26 px and its baseline is placed from the
//! 26 px metrics.
//!
//! No page-controlled pixel is drawn: tab titles are sanitised text, and a
//! favicon is never fetched or shown (the dot is the chrome's own).

use px_brand::palette::Rgb;
use px_layout::geom::px;
use px_paint::{Color, DisplayList, Item, Rect as PaintRect};

use crate::address::Emphasis;
use crate::chrome::{Chrome, ControlDots, Stop};
use crate::geom::{Point, Rect};
use crate::layout::{self as lay, Layout};
use crate::shape::{self, Circle, Path, SUB, Squircle, color, fill};
use crate::tabs::Hover;
use crate::text::{self, AU_PER_PX, TextShaper};
use crate::theme::{Colors, Theme};
use crate::toolbar::ToolButton;

mod icons;

/// Takes logical coordinates, draws physical ones.
struct Painter<'a> {
    list: &'a mut DisplayList,
    /// Physical pixels.
    origin: Point,
    s: i32,
    c: Colors,
    shaper: &'a dyn TextShaper,
}

impl Painter<'_> {
    /// A logical rectangle in physical pixels, origin applied.
    fn at(&self, r: Rect) -> Rect {
        Rect::new(
            r.x * self.s + self.origin.x,
            r.y * self.s + self.origin.y,
            r.w * self.s,
            r.h * self.s,
        )
    }

    fn squircle(&mut self, r: Rect, radius: i32, col: Color) {
        let sq = Squircle::new(self.at(r), radius * self.s);
        fill(self.list, &sq, col);
    }

    fn rect(&mut self, r: Rect, col: Color) {
        let r = self.at(r);
        self.rect_px(r, col);
    }

    /// A rectangle already in physical pixels.
    fn rect_px(&mut self, r: Rect, col: Color) {
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
        let st = Squircle::new(self.at(r), radius * self.s).stroke(width * self.s);
        fill(self.list, &st, col);
    }

    /// Centre and radius in 1/8 logical px.
    fn circle(&mut self, cx8: i32, cy8: i32, r8: i32, col: Color) {
        let c = Circle::disc(
            cx8 * self.s + self.origin.x * SUB,
            cy8 * self.s + self.origin.y * SUB,
            r8 * self.s,
        );
        fill(self.list, &c, col);
    }

    fn ring(&mut self, cx8: i32, cy8: i32, r8: i32, thick8: i32, col: Color) {
        let c = Circle::ring(
            cx8 * self.s + self.origin.x * SUB,
            cy8 * self.s + self.origin.y * SUB,
            r8 * self.s,
            thick8 * self.s,
        );
        fill(self.list, &c, col);
    }

    /// Polyline with points in 1/8 logical px relative to `(cx8, cy8)`.
    fn path(&mut self, cx8: i32, cy8: i32, pts: &[(i32, i32)], half8: i32, col: Color) {
        let ox = cx8 * self.s + self.origin.x * SUB;
        let oy = cy8 * self.s + self.origin.y * SUB;
        let abs: Vec<(i32, i32)> = pts
            .iter()
            .map(|(x, y)| (ox + x * self.s, oy + y * self.s))
            .collect();
        fill(self.list, &Path::polyline(&abs, half8 * self.s), col);
    }

    /// Draw `s` at logical `x`, centred in `band`, at logical `size`. Returns
    /// the advance in logical pixels, rounded up.
    fn text(&mut self, s: &str, size: i32, x: i32, band: Rect, col: Color) -> i32 {
        let shaped = self.shaper.shape(s, size * self.s);
        let base = text::baseline_in(self.shaper, size * self.s, band.y * self.s, band.h * self.s);
        text::push(
            self.list,
            &shaped,
            x * self.s + self.origin.x,
            base + self.origin.y,
            col,
        );
        (shaped.width + AU_PER_PX * self.s - 1) / (AU_PER_PX * self.s)
    }

    /// Where `text` at logical `size` has its baseline, physical, origin applied.
    #[cfg(test)]
    fn baseline(&self, size: i32, band: Rect) -> i32 {
        text::baseline_in(self.shaper, size * self.s, band.y * self.s, band.h * self.s)
            + self.origin.y
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

/// Paint the window's contents. See the module docs.
pub fn paint(
    chrome: &Chrome,
    shaper: &dyn TextShaper,
    theme: Theme,
    origin: Point,
    list: &mut DisplayList,
) {
    let layout = chrome.layout();
    let mut p = Painter {
        list,
        origin,
        s: chrome.scale(),
        c: theme.colors(),
        shaper,
    };
    let s = p.c.surfaces;
    let win = Rect::new(0, 0, layout.size.w, layout.size.h);
    let r = chrome.config().window_radius;
    let rp = r * p.s;

    // Surfaces: window body, then the three bands with the window's corners.
    p.squircle(win, r, rgb(s.window, 255));
    let strip = Squircle::with_radii(p.at(layout.strip), [rp, rp, 0, 0]);
    fill(p.list, &strip, rgb(s.bar, 255));
    p.rect(layout.toolbar, rgb(s.bar, 255));
    let page = Squircle::with_radii(p.at(layout.page), [0, 0, rp, rp]);
    fill(p.list, &page, rgb(s.page, 255));
    p.rect(
        Rect::new(0, lay::CHROME_H - 1, layout.size.w, 1),
        rgb(s.hairline, 255),
    );

    paint_controls(&mut p, chrome, &layout);
    paint_tabs(&mut p, chrome, &layout);
    paint_toolbar(&mut p, chrome, &layout);
    paint_address(&mut p, chrome, &layout);
    paint_focus(&mut p, chrome, &layout);

    // The one hairline round the window.
    p.stroke(win, r, 1, rgb(s.hairline, 255));
}

/// Backdrop, soft shadow, then the window at `margin` (logical). The returned
/// canvas size is physical: `(size + 2 * margin) * scale`; with the mockups'
/// 40/36 margins, a 920 x 568 window and scale 1 this is their 1000 x 640.
pub fn paint_preview(
    chrome: &Chrome,
    shaper: &dyn TextShaper,
    theme: Theme,
    margin: Point,
) -> (DisplayList, crate::geom::Size) {
    let k = chrome.scale();
    let size = chrome.size();
    let canvas = crate::geom::Size::new((size.w + 2 * margin.x) * k, (size.h + 2 * margin.y) * k);
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
    let win = Rect::new(margin.x * k, margin.y * k, size.w * k, size.h * k);
    let radius = chrome.config().window_radius * k;
    let peak = u32::from(c.surfaces.shadow_alpha);
    // Two layers: a wide, low key shadow that falls away from the window, and
    // a tight ambient one that keeps the edge from floating.
    shape::shadow(
        &mut list,
        win,
        radius,
        14 * k,
        36 * k,
        (peak * 9 / 10) as u8,
    );
    shape::shadow(&mut list, win, radius, 2 * k, 10 * k, (peak / 2) as u8);
    paint(
        chrome,
        shaper,
        theme,
        Point {
            x: margin.x * k,
            y: margin.y * k,
        },
        &mut list,
    );
    (list, canvas)
}

fn paint_controls(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let cols = [p.c.red, p.c.amber, p.c.teal];
    let group_hover = chrome.hover_control().is_some();
    let coloured = chrome.config().control_dots == ControlDots::Coloured;
    for (i, r) in layout.controls.iter().enumerate() {
        if r.is_empty() {
            continue;
        }
        let col = if !chrome.window_focused() {
            // An unfocused window dims them whatever they are.
            if coloured {
                rgb(cols[i], 102)
            } else {
                rgb(p.c.muted, 56)
            }
        } else if group_hover {
            rgb(cols[i], 255)
        } else if coloured {
            rgb(cols[i], 230)
        } else {
            // Neutral: one grey, no meaning, until the pointer is over them.
            rgb(p.c.muted, 105)
        };
        p.squircle(*r, 5, col);
    }
}

fn paint_tabs(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let strip = chrome.tabs();
    let c = p.c;
    p.clip(layout.tab_view);
    for (i, (tab, slot)) in strip.tabs().iter().zip(&layout.tabs).enumerate() {
        if slot.rect.is_empty() {
            continue;
        }
        let full = slot.full;
        let active = strip.active_index() == Some(i);
        let hovered = matches!(strip.hover(), Hover::Tab(j) | Hover::Close(j) if j == i);
        if active {
            p.squircle(full, lay::TAB_RADIUS, rgb(c.surfaces.raised, 255));
            p.stroke(full, lay::TAB_RADIUS, 1, rgb(c.surfaces.hairline, 255));
        } else if hovered {
            p.squircle(full, lay::TAB_RADIUS, rgb(c.surfaces.raised, 150));
        }
        // The favicon slot: the chrome's own dot, a ring while loading.
        let (cx, cy) = ((full.x + 16) * SUB, (full.y + lay::TAB_H / 2) * SUB);
        let dot = if active {
            rgb(c.teal, 255)
        } else {
            rgb(c.muted, 128)
        };
        if tab.loading {
            p.ring(cx, cy, 36, 13, dot);
        } else {
            p.circle(cx, cy, 36, dot);
        }
        // Title.
        let show_close = !slot.close.is_empty() && (active || hovered);
        let right = if show_close {
            slot.close.x - 4
        } else {
            full.right() - 12
        };
        let tx = full.x + 30;
        let room = right - tx;
        if room >= 8 {
            let title = text::fit(
                p.shaper,
                tab.display_title(),
                lay::TEXT_CHROME * p.s,
                room * p.s,
            );
            let col = if active { c.text } else { c.muted };
            p.text(&title, lay::TEXT_CHROME, tx, full, rgb(col, 255));
        }
        if show_close {
            let close_hover = strip.hover() == Hover::Close(i);
            if close_hover {
                p.squircle(slot.close, 7, rgb(c.surfaces.hairline, 255));
            }
            let col = rgb(if close_hover { c.text } else { c.muted }, 255);
            icons::close(p, slot.close, col);
        }
    }
    // Overflow cues: the tabs fade into the strip where more lie beyond.
    let v = layout.tab_view;
    let fade = [255u8, 200, 130, 60];
    if layout.scroll > 0 {
        for (k, a) in fade.iter().enumerate() {
            let w = 4;
            p.rect(
                Rect::new(v.x + k as i32 * w, 0, w, lay::STRIP_H),
                rgb(c.surfaces.bar, *a),
            );
        }
    }
    if layout.scroll < layout.scroll_max {
        for (k, a) in fade.iter().enumerate() {
            let w = 4;
            p.rect(
                Rect::new(v.right() - (k as i32 + 1) * w, 0, w, lay::STRIP_H),
                rgb(c.surfaces.bar, *a),
            );
        }
    }
    p.pop();
    // New tab.
    let nt = layout.new_tab;
    let hot = strip.hover() == Hover::NewTab;
    if hot {
        p.squircle(nt, lay::BUTTON_RADIUS, rgb(c.surfaces.raised, 255));
    }
    icons::new_tab(p, nt, rgb(if hot { c.text } else { c.muted }, 255));
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
        } else if tb.hover() == Some(b) || b != ToolButton::Overflow {
            rgb(c.text, 255)
        } else {
            rgb(c.muted, 255)
        };
        match b {
            ToolButton::Back => icons::back(p, r, col),
            ToolButton::Forward => icons::forward(p, r, col),
            ToolButton::Reload if tb.is_loading() => icons::stop(p, r, col),
            ToolButton::Reload => icons::reload(p, r, col),
            ToolButton::Overflow => icons::menu(p, r, col),
        }
    }
}

/// The keyboard focus ring: 2 px of the focus blue, one pixel clear of the
/// control, on any control Tab has reached. The address field draws its own
/// (it is the editing state).
fn paint_focus(p: &mut Painter, chrome: &Chrome, layout: &Layout) {
    let Some(stop) = chrome.focus_stop() else {
        return;
    };
    let (r, radius) = match stop {
        Stop::ActiveTab => {
            let Some(slot) = chrome
                .tabs()
                .active_index()
                .and_then(|i| layout.tabs.get(i))
            else {
                return;
            };
            if slot.rect.is_empty() {
                return;
            }
            (slot.full, lay::TAB_RADIUS)
        }
        Stop::NewTab => (layout.new_tab, lay::BUTTON_RADIUS),
        Stop::Back => (layout.back, lay::BUTTON_RADIUS),
        Stop::Forward => (layout.forward, lay::BUTTON_RADIUS),
        Stop::Reload => (layout.reload, lay::BUTTON_RADIUS),
        Stop::Overflow => (layout.overflow, lay::BUTTON_RADIUS),
        Stop::Field => return,
    };
    let blue = rgb(p.c.blue, 255);
    if stop == Stop::ActiveTab {
        p.clip(Rect::new(0, 0, layout.size.w, lay::STRIP_H));
    }
    p.stroke(r.inset(-3), radius + 3, 2, blue);
    if stop == Stop::ActiveTab {
        p.pop();
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
        p.text(
            px_brand::ui::ISOLATED,
            lay::TEXT_CHIP,
            ch.x + 27,
            ch,
            rgb(c.teal, 255),
        );
    }
    let band = Rect::new(
        layout.text_x,
        f.y,
        (layout.text_right - layout.text_x).max(0),
        f.h,
    );
    if band.is_empty() {
        return;
    }
    p.clip(band);
    let k = p.s;
    if a.is_editing() {
        let (visible, caret) = a.visible_text();
        let shaped = p.shaper.shape(&visible, lay::TEXT_ADDRESS * k);
        // Physical x of a byte index.
        let ox = p.origin.x;
        let x_of = |i: usize| (layout.text_x * k) + shaped.x_at(i) / AU_PER_PX + ox;
        let iy = (f.y + 7) * k + p.origin.y;
        let ih = (f.h - 14) * k;
        if let (Some((s0, s1)), None) = (a.selection(), a.preedit_range()) {
            let (x0, x1) = (x_of(s0), x_of(s1));
            p.rect_px(Rect::new(x0, iy, x1 - x0, ih), rgb(c.blue, 90));
        }
        if visible.is_empty() {
            p.text(
                px_brand::ui::ADDRESS_PLACEHOLDER,
                lay::TEXT_ADDRESS,
                layout.text_x,
                f,
                rgb(c.muted, 255),
            );
        } else {
            p.text(
                &visible,
                lay::TEXT_ADDRESS,
                layout.text_x,
                f,
                rgb(c.text, 255),
            );
        }
        if let Some((a0, a1)) = a.preedit_range() {
            let (x0, x1) = (x_of(a0), x_of(a1));
            p.rect_px(
                Rect::new(x0, (f.y + f.h - 9) * k + p.origin.y, x1 - x0, k),
                rgb(c.text, 255),
            );
        }
        if chrome.window_focused() {
            p.rect_px(Rect::new(x_of(caret), iy, k, ih), rgb(c.text, 255));
        }
    } else if a.display().is_empty() {
        p.text(
            px_brand::ui::ADDRESS_PLACEHOLDER,
            lay::TEXT_ADDRESS,
            layout.text_x,
            f,
            rgb(c.muted, 255),
        );
    } else {
        // Segments abut in physical pixels; track the pen in 1/60 px so no
        // sub-pixel remainder is lost between them.
        let mut pen_au = layout.text_x * k * AU_PER_PX;
        let base = text::baseline_in(p.shaper, lay::TEXT_ADDRESS * k, f.y * k, f.h * k);
        for seg in &a.display().segments {
            let col = match seg.emphasis {
                Emphasis::Strong => c.text,
                Emphasis::Muted => c.muted,
            };
            let shaped = p.shaper.shape(&seg.text, lay::TEXT_ADDRESS * k);
            let glyph_x = pen_au / AU_PER_PX;
            text::push(
                p.list,
                &shaped,
                glyph_x + p.origin.x,
                base + p.origin.y,
                rgb(col, 255),
            );
            pen_au += shaped.width;
        }
    }
    p.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::PlatformConfig;
    use crate::geom::Size;
    use crate::tabs::{Tab, TabId};
    use crate::text::testing::Mono;

    fn chrome(scale: u32) -> Chrome {
        let mut c = Chrome::new(PlatformConfig::desktop(), Size::new(920, 568));
        c.set_scale(scale);
        c.set_tabs(vec![Tab::new(TabId(1), "One", false)], Some(TabId(1)));
        c
    }

    #[test]
    fn baselines_are_exact_per_scale() {
        // The baseline at scale k is the baseline of the k-times font in the
        // k-times band: no rounding of the 1x answer up to k.
        let band = Rect::new(0, 8, 190, 32);
        for k in 1..=3 {
            let mut list = DisplayList::new();
            let p = Painter {
                list: &mut list,
                origin: Point::default(),
                s: k,
                c: Theme::Dark.colors(),
                shaper: &Mono,
            };
            let want = text::baseline_in(&Mono, 13 * k, 8 * k, 32 * k);
            assert_eq!(p.baseline(13, band), want, "scale {k}");
        }
    }

    #[test]
    fn the_frame_is_scale_times_the_window_and_still_balanced() {
        for k in 1..=3u32 {
            let c = chrome(k);
            let (list, size) = paint_preview(&c, &Mono, Theme::Light, Point { x: 40, y: 36 });
            assert_eq!(size.w, 1000 * k as i32);
            assert_eq!(size.h, 640 * k as i32);
            assert!(list.clips_balanced());
        }
    }

    #[test]
    fn a_hairline_is_scale_device_pixels_thick() {
        for k in 1..=3u32 {
            let c = chrome(k);
            let mut list = DisplayList::new();
            paint(&c, &Mono, Theme::Dark, Point::default(), &mut list);
            let canvas = px_paint::rasterize(&list, 920 * k, 568 * k).unwrap();
            let hair = crate::theme::Theme::Dark.colors().surfaces.hairline;
            let want = [hair.0, hair.1, hair.2, 255];
            let y0 = ((lay::CHROME_H - 1) * k as i32) as u32;
            let x = 600 * k;
            for dy in 0..k {
                assert_eq!(canvas.pixel(x, y0 + dy), Some(want), "scale {k} row {dy}");
            }
            assert_ne!(canvas.pixel(x, y0 + k), Some(want));
        }
    }

    #[test]
    fn controls_are_neutral_until_hovered() {
        use crate::input::{Event, PointerEvent, PointerKind};
        let render = |c: &Chrome| {
            let mut list = DisplayList::new();
            paint(c, &Mono, Theme::Dark, Point::default(), &mut list);
            px_paint::rasterize(&list, 920, 568).unwrap()
        };
        let mut c = chrome(1);
        let dot = c.layout().controls[0].center();
        let idle = render(&c).pixel(dot.x as u32, dot.y as u32).unwrap();
        let red = Theme::Dark.colors().red;
        // Neutral: blue is not far below red as it is for the coloured dot.
        assert!(
            i32::from(idle[0]) - i32::from(idle[2]) < 40,
            "grey at rest: {idle:?}"
        );
        c.event(
            Event::Pointer(PointerEvent::new(dot.x, dot.y, PointerKind::Move)),
            &Mono,
        );
        let hot = render(&c).pixel(dot.x as u32, dot.y as u32).unwrap();
        assert_eq!([hot[0], hot[1], hot[2]], [red.0, red.1, red.2]);
        let mut cfg = PlatformConfig::desktop();
        cfg.control_dots = ControlDots::Coloured;
        let mut c2 = chrome(1);
        c2.set_config(cfg);
        let always = render(&c2).pixel(dot.x as u32, dot.y as u32).unwrap();
        assert!(
            i32::from(always[0]) - i32::from(always[2]) > 40,
            "{always:?}"
        );
    }
}
