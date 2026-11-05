//! Where everything sits, in logical pixels, window coordinates.
//!
//! Numbers are the mockups' (`docs/brand/ui-mockup-*.svg`) on a 4 px grid: tab
//! strip 40, toolbar 52, 32 px tabs, 30 px buttons, a 34 px address field. They
//! are the frozen layout of invariant 3: changing one is a UI change.

use crate::geom::{Point, Rect, Size};
use crate::input::ResizeEdge;

pub const STRIP_H: i32 = 40;
pub const TOOLBAR_H: i32 = 52;
/// Height of the chrome above the page.
pub const CHROME_H: i32 = STRIP_H + TOOLBAR_H;
/// Narrowest window the layout is specified for; the platform enforces it as a
/// minimum size. Below it the address field collapses and may overlap.
pub const MIN_WIDTH: i32 = 480;

pub const WINDOW_RADIUS: i32 = 22;
pub const FIELD_RADIUS: i32 = 13;
pub const TAB_RADIUS: i32 = 11;
pub const BUTTON_RADIUS: i32 = 10;
pub const CHIP_RADIUS: i32 = 8;

pub const TAB_TOP: i32 = 8;
pub const TAB_H: i32 = 32;
pub const TAB_ACTIVE_W: i32 = 190;
pub const TAB_INACTIVE_W: i32 = 140;
/// Tabs shrink to this before the strip scrolls instead.
pub const TAB_MIN_W: i32 = 72;
/// The active tab's width while the strip scrolls.
pub const TAB_ACTIVE_SCROLL_W: i32 = 130;
pub const TAB_GAP: i32 = 6;
/// Tabs at least this wide show a close button.
pub const TAB_CLOSE_MIN_W: i32 = 72;
pub const CLOSE_SIZE: i32 = 20;
pub const NEW_TAB_SIZE: i32 = 28;

pub const BUTTON: i32 = 30;
pub const FIELD_H: i32 = 34;
pub const CONTROL_DOT: i32 = 13;
pub const CHIP_W: i32 = 96;
pub const CHIP_H: i32 = 22;
/// Resize grips of a frameless window: edge thickness and corner square.
pub const GRIP: i32 = 5;
pub const GRIP_CORNER: i32 = 12;

/// Chrome text sizes (`ui-direction.md`): 13 for chrome, 14 for address text.
pub const TEXT_CHROME: i32 = 13;
pub const TEXT_ADDRESS: i32 = 14;
pub const TEXT_CHIP: i32 = 12;

/// Where the window's close/minimise/maximise dots go. The platform says; macOS
/// and Linux/Windows differ, and `ui-direction.md` says to follow each OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ControlsPlacement {
    Left,
    Right,
    /// The OS draws its own, or the window is fullscreen.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LayoutInput {
    pub size: Size,
    pub controls: ControlsPlacement,
    pub tab_count: usize,
    pub active: Option<usize>,
    /// Show the "Isolated" chip at the start of the address field.
    pub isolated: bool,
    /// How far the tab strip is scrolled, px. Clamped to what the tabs need.
    pub scroll: i32,
    /// Frameless window: the chrome supplies resize grips round its edge.
    pub grips: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSlot {
    /// The part of the tab that is visible and clickable (empty when scrolled
    /// out of the strip).
    pub rect: Rect,
    /// The whole tab, which may extend past the strip's viewport when the strip
    /// scrolls. The painter clips it to [`Layout::tab_view`].
    pub full: Rect,
    /// Empty when the tab is too narrow for one or not entirely in view.
    pub close: Rect,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    pub size: Size,
    pub strip: Rect,
    pub toolbar: Rect,
    /// Where the page surface goes. The compositor blits into exactly this
    /// rectangle and nowhere else: no page pixel in the chrome region.
    pub page: Rect,
    /// Close, minimise, maximise, in that order. Empty rects when absent.
    pub controls: [Rect; 3],
    /// One per tab, in order. A tab that did not fit has an empty rect.
    pub tabs: Vec<TabSlot>,
    /// Where tabs are visible; they scroll behind it. New-tab sits after it.
    pub tab_view: Rect,
    /// Clamped scroll offset in effect, and the most it can be.
    pub scroll: i32,
    pub scroll_max: i32,
    pub new_tab: Rect,
    pub back: Rect,
    pub forward: Rect,
    pub reload: Rect,
    pub overflow: Rect,
    pub field: Rect,
    pub chip: Rect,
    /// Left edge of the address text.
    pub text_x: i32,
    /// Right edge available to the address text.
    pub text_right: i32,
    /// Resize grips are active (frameless window).
    pub grips: bool,
}

const CONTROLS_MARGIN: i32 = 18;
const CONTROLS_GAP: i32 = 9;
const CONTROLS_RESERVE: i32 = 100;
const EDGE: i32 = 12;
const BAR_PAD: i32 = 20;

pub fn compute(input: &LayoutInput) -> Layout {
    let Size { w, h } = input.size;
    let strip = Rect::new(0, 0, w, STRIP_H);
    let toolbar = Rect::new(0, STRIP_H, w, TOOLBAR_H);
    let page = Rect::new(0, CHROME_H, w, h - CHROME_H);

    // Window controls.
    let mut controls = [Rect::default(); 3];
    let dot_y = 17;
    let (tabs_left, tabs_right) = match input.controls {
        ControlsPlacement::Left => {
            for (i, c) in controls.iter_mut().enumerate() {
                let x = CONTROLS_MARGIN + i as i32 * (CONTROL_DOT + CONTROLS_GAP);
                *c = Rect::new(x, dot_y, CONTROL_DOT, CONTROL_DOT);
            }
            (CONTROLS_RESERVE, w - EDGE)
        }
        ControlsPlacement::Right => {
            for (i, c) in controls.iter_mut().enumerate() {
                let x = w
                    - CONTROLS_MARGIN
                    - CONTROL_DOT
                    - (2 - i as i32) * (CONTROL_DOT + CONTROLS_GAP);
                *c = Rect::new(x, dot_y, CONTROL_DOT, CONTROL_DOT);
            }
            (EDGE, w - CONTROLS_RESERVE)
        }
        ControlsPlacement::None => (EDGE, w - EDGE),
    };

    // Tabs.
    let n = input.tab_count as i32;
    let gaps = TAB_GAP * (n - 1).max(0);
    let tab_view = Rect::new(
        tabs_left,
        0,
        (tabs_right - tabs_left - NEW_TAB_SIZE - TAB_GAP).max(0),
        STRIP_H,
    );
    let avail = tab_view.w;
    let natural: i32 = (0..input.tab_count)
        .map(|i| {
            if Some(i) == input.active {
                TAB_ACTIVE_W
            } else {
                TAB_INACTIVE_W
            }
        })
        .sum::<i32>()
        + gaps;
    let mut widths: Vec<i32> = Vec::with_capacity(input.tab_count);
    let mut content = natural;
    if natural <= avail {
        for i in 0..input.tab_count {
            widths.push(if Some(i) == input.active {
                TAB_ACTIVE_W
            } else {
                TAB_INACTIVE_W
            });
        }
    } else {
        let share_total = avail - gaps;
        let share = (share_total / n.max(1)).max(0);
        if share >= TAB_MIN_W {
            // Shrink evenly; the leftover pixels go one each to the first tabs.
            let extra = share_total - share * n;
            for i in 0..n {
                widths.push(share + i32::from(i < extra));
            }
            content = avail;
        } else {
            // Too many for the minimum width: keep it and scroll.
            // The active tab keeps enough room to read its title.
            for i in 0..input.tab_count {
                widths.push(if Some(i) == input.active {
                    TAB_ACTIVE_SCROLL_W
                } else {
                    TAB_MIN_W
                });
            }
            content = widths.iter().sum::<i32>() + gaps;
        }
    }
    let scroll_max = (content - avail).max(0);
    let scroll = input.scroll.clamp(0, scroll_max);
    let mut x = tabs_left - scroll;
    let mut tabs = Vec::with_capacity(widths.len());
    for tw in &widths {
        let full = Rect::new(x, TAB_TOP, *tw, TAB_H);
        let rect = full.intersect(tab_view);
        let close = if *tw >= TAB_CLOSE_MIN_W {
            let c = Rect::new(
                full.right() - 8 - CLOSE_SIZE,
                full.y + (TAB_H - CLOSE_SIZE) / 2,
                CLOSE_SIZE,
                CLOSE_SIZE,
            );
            if c.intersect(tab_view) == c {
                c
            } else {
                Rect::default()
            }
        } else {
            Rect::default()
        };
        tabs.push(TabSlot { rect, full, close });
        x += tw + TAB_GAP;
    }
    let new_tab = Rect::new(
        x.min(tabs_right - NEW_TAB_SIZE).max(tabs_left),
        TAB_TOP + (TAB_H - NEW_TAB_SIZE) / 2,
        NEW_TAB_SIZE,
        NEW_TAB_SIZE,
    );

    // Toolbar.
    let by = STRIP_H + (TOOLBAR_H - BUTTON) / 2;
    let button = |i: i32| Rect::new(BAR_PAD + i * (BUTTON + 10), by, BUTTON, BUTTON);
    let (back, forward, reload) = (button(0), button(1), button(2));
    let overflow = Rect::new(w - 24 - BUTTON, by, BUTTON, BUTTON);
    let field_x = reload.right() + BAR_PAD;
    let field = Rect::new(
        field_x,
        STRIP_H + (TOOLBAR_H - FIELD_H) / 2,
        overflow.x - 16 - field_x,
        FIELD_H,
    );
    let chip = if input.isolated {
        Rect::new(
            field.x + 7,
            field.y + (FIELD_H - CHIP_H) / 2,
            CHIP_W,
            CHIP_H,
        )
    } else {
        Rect::default()
    };
    let text_x = if input.isolated {
        chip.right() + 17
    } else {
        field.x + 14
    };
    Layout {
        size: input.size,
        strip,
        toolbar,
        page,
        controls,
        tabs,
        tab_view,
        scroll,
        scroll_max,
        new_tab,
        back,
        forward,
        reload,
        overflow,
        field,
        chip,
        text_x,
        text_right: field.right() - 14,
        grips: input.grips,
    }
}

/// What lies under a point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Control(usize),
    Close(usize),
    Tab(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Overflow,
    Field,
    /// A resize grip of a frameless window.
    Edge(ResizeEdge),
    /// Chrome background (draggable in a frameless window).
    Bar,
    Page,
    Outside,
}

impl Layout {
    /// The scroll offset that brings tab `index` fully into view, moving as
    /// little as possible from the current one.
    pub fn scroll_to_reveal(&self, index: usize) -> i32 {
        let Some(t) = self.tabs.get(index) else {
            return self.scroll;
        };
        let view = self.tab_view;
        let want = if t.full.x < view.x {
            self.scroll - (view.x - t.full.x)
        } else if t.full.right() > view.right() {
            self.scroll + (t.full.right() - view.right())
        } else {
            self.scroll
        };
        want.clamp(0, self.scroll_max)
    }

    fn edge_at(&self, p: Point) -> Option<ResizeEdge> {
        if !self.grips {
            return None;
        }
        let (w, h) = (self.size.w, self.size.h);
        let (l, r) = (p.x < GRIP_CORNER, p.x >= w - GRIP_CORNER);
        let (t, b) = (p.y < GRIP_CORNER, p.y >= h - GRIP_CORNER);
        match (l, r, t, b) {
            (true, _, true, _) => Some(ResizeEdge::NorthWest),
            (_, true, true, _) => Some(ResizeEdge::NorthEast),
            (true, _, _, true) => Some(ResizeEdge::SouthWest),
            (_, true, _, true) => Some(ResizeEdge::SouthEast),
            _ if p.y < GRIP => Some(ResizeEdge::North),
            _ if p.y >= h - GRIP => Some(ResizeEdge::South),
            _ if p.x < GRIP => Some(ResizeEdge::West),
            _ if p.x >= w - GRIP => Some(ResizeEdge::East),
            _ => None,
        }
    }

    pub fn hit(&self, p: Point) -> Region {
        if !Rect::new(0, 0, self.size.w, self.size.h).contains(p) {
            return Region::Outside;
        }
        if let Some(e) = self.edge_at(p) {
            return Region::Edge(e);
        }
        if self.page.contains(p) {
            return Region::Page;
        }
        for (i, c) in self.controls.iter().enumerate() {
            if c.contains(p) {
                return Region::Control(i);
            }
        }
        for (i, t) in self.tabs.iter().enumerate() {
            if t.close.contains(p) {
                return Region::Close(i);
            }
            if t.rect.contains(p) {
                return Region::Tab(i);
            }
        }
        for (r, region) in [
            (self.new_tab, Region::NewTab),
            (self.back, Region::Back),
            (self.forward, Region::Forward),
            (self.reload, Region::Reload),
            (self.overflow, Region::Overflow),
            (self.field, Region::Field),
        ] {
            if r.contains(p) {
                return region;
            }
        }
        Region::Bar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(w: i32, tabs: usize, active: Option<usize>) -> LayoutInput {
        LayoutInput {
            size: Size::new(w, 568),
            controls: ControlsPlacement::Left,
            tab_count: tabs,
            active,
            isolated: true,
            scroll: 0,
            grips: false,
        }
    }

    #[test]
    fn matches_the_mockup_at_920_wide() {
        // ui-mockup-dark.svg, window at (40, 36): see the numbers in the module doc.
        let l = compute(&input(920, 3, Some(0)));
        assert_eq!(l.tabs[0].rect, Rect::new(100, 8, 190, 32));
        assert_eq!(l.tabs[1].rect, Rect::new(296, 8, 140, 32));
        assert_eq!(l.tabs[2].rect, Rect::new(442, 8, 140, 32));
        assert_eq!(l.back, Rect::new(20, 51, 30, 30));
        assert_eq!(l.field.x, 150);
        assert_eq!(l.field.y, 49);
        assert_eq!(l.field.h, 34);
        assert_eq!(l.field.right(), 850);
        assert_eq!(l.overflow, Rect::new(866, 51, 30, 30));
        assert_eq!(l.chip, Rect::new(157, 55, 96, 22));
        assert_eq!(l.page, Rect::new(0, 92, 920, 476));
    }

    #[test]
    fn nothing_overlaps_and_nothing_leaves_the_strip_at_any_width() {
        for w in [200, 320, 480, 640, 920, 1400, 3000] {
            for tabs in [0usize, 1, 2, 7, 40, 500] {
                for ctl in [
                    ControlsPlacement::Left,
                    ControlsPlacement::Right,
                    ControlsPlacement::None,
                ] {
                    let mut i = input(w, tabs, if tabs == 0 { None } else { Some(tabs / 2) });
                    i.controls = ctl;
                    let mut l = compute(&i);
                    if tabs > 0 {
                        // The chrome scrolls the active tab into view.
                        i.scroll = l.scroll_to_reveal(tabs / 2);
                        l = compute(&i);
                    }
                    assert_eq!(l.tabs.len(), tabs);
                    let mut prev_right = i32::MIN;
                    let mut shown = 0;
                    for t in &l.tabs {
                        if t.rect.is_empty() {
                            continue;
                        }
                        shown += 1;
                        assert!(t.rect.x >= prev_right, "w={w} tabs={tabs}");
                        assert!(t.rect.right() <= w, "w={w} tabs={tabs} {t:?}");
                        assert!(t.rect.w >= 1);
                        prev_right = t.rect.right();
                    }
                    if tabs > 0 && w >= 480 {
                        let a = &l.tabs[tabs / 2];
                        assert!(!a.rect.is_empty(), "active tab hidden w={w} tabs={tabs}");
                        assert!(shown >= 1);
                    }
                    if w >= MIN_WIDTH {
                        assert!(l.field.w > 100 && l.field.right() < l.overflow.x);
                    }
                }
            }
        }
    }

    #[test]
    fn hit_testing_prefers_the_close_button_and_the_page_is_not_chrome() {
        let l = compute(&input(920, 3, Some(0)));
        let c = l.tabs[0].close.center();
        assert_eq!(l.hit(c), Region::Close(0));
        assert_eq!(l.hit(Point { x: 150, y: 20 }), Region::Tab(0));
        assert_eq!(l.hit(Point { x: 10, y: 20 }), Region::Bar);
        assert_eq!(l.hit(Point { x: 500, y: 300 }), Region::Page);
        assert_eq!(l.hit(Point { x: 1000, y: 300 }), Region::Outside);
        assert_eq!(l.hit(Point { x: -1, y: 5 }), Region::Outside);
        assert_eq!(l.hit(l.back.center()), Region::Back);
        assert_eq!(l.hit(l.field.center()), Region::Field);
        assert_eq!(l.hit(l.controls[2].center()), Region::Control(2));
    }

    #[test]
    fn many_tabs_keep_a_minimum_width_and_scroll() {
        let mut i = input(920, 30, Some(0));
        let l = compute(&i);
        assert!(l.scroll_max > 0);
        assert!(l.tabs.iter().enumerate().all(|(k, t)| t.full.w
            == if k == 0 {
                TAB_ACTIVE_SCROLL_W
            } else {
                TAB_MIN_W
            }));
        // Scrolled out tabs are neither visible nor clickable.
        let far = l.tabs[29];
        assert!(far.rect.is_empty() && !far.full.is_empty());
        // Reveal the last tab, then the first again.
        i.scroll = l.scroll_to_reveal(29);
        let l2 = compute(&i);
        assert_eq!(l2.scroll, l2.scroll_max);
        assert_eq!(l2.tabs[29].rect, l2.tabs[29].full);
        assert!(l2.tabs[0].rect.is_empty());
        assert_eq!(l2.scroll_to_reveal(0), 0);
        // Over-scrolling clamps; new-tab stays put.
        i.scroll = 100_000;
        let l3 = compute(&i);
        assert_eq!(l3.scroll, l3.scroll_max);
        assert_eq!(l3.new_tab, l.new_tab);
        // A tab cut by the viewport edge has no close button.
        i.scroll = 10;
        let l4 = compute(&i);
        assert!(l4.tabs[0].close.is_empty() || l4.tabs[0].close.x >= l4.tab_view.x);
    }

    #[test]
    fn grips_exist_only_on_a_frameless_window() {
        let mut i = input(920, 1, Some(0));
        assert_eq!(compute(&i).hit(Point { x: 2, y: 300 }), Region::Page);
        i.grips = true;
        let l = compute(&i);
        assert_eq!(
            l.hit(Point { x: 2, y: 300 }),
            Region::Edge(ResizeEdge::West)
        );
        assert_eq!(
            l.hit(Point { x: 917, y: 300 }),
            Region::Edge(ResizeEdge::East)
        );
        assert_eq!(
            l.hit(Point { x: 400, y: 1 }),
            Region::Edge(ResizeEdge::North)
        );
        assert_eq!(
            l.hit(Point { x: 400, y: 566 }),
            Region::Edge(ResizeEdge::South)
        );
        assert_eq!(
            l.hit(Point { x: 3, y: 3 }),
            Region::Edge(ResizeEdge::NorthWest)
        );
        assert_eq!(
            l.hit(Point { x: 916, y: 3 }),
            Region::Edge(ResizeEdge::NorthEast)
        );
        assert_eq!(
            l.hit(Point { x: 3, y: 565 }),
            Region::Edge(ResizeEdge::SouthWest)
        );
        assert_eq!(
            l.hit(Point { x: 916, y: 565 }),
            Region::Edge(ResizeEdge::SouthEast)
        );
        // The dots and tabs are well inside the grips.
        assert_eq!(l.hit(l.controls[0].center()), Region::Control(0));
        assert_eq!(l.hit(l.tabs[0].rect.center()), Region::Tab(0));
        assert_eq!(l.hit(Point { x: 10, y: 20 }), Region::Bar);
    }

    #[test]
    fn controls_follow_the_placement() {
        let mut i = input(920, 1, Some(0));
        i.controls = ControlsPlacement::Right;
        let l = compute(&i);
        assert!(l.controls[2].right() <= 920 - CONTROLS_MARGIN);
        assert!(l.controls[0].x < l.controls[2].x);
        i.controls = ControlsPlacement::None;
        assert!(compute(&i).controls.iter().all(Rect::is_empty));
    }
}
