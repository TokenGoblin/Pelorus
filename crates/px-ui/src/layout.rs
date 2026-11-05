//! Where everything sits, in logical pixels, window coordinates.
//!
//! Numbers are the mockups' (`docs/brand/ui-mockup-*.svg`) on a 4 px grid: tab
//! strip 40, toolbar 52, 32 px tabs, 30 px buttons, a 34 px address field. They
//! are the frozen layout of invariant 3: changing one is a UI change.

use crate::geom::{Point, Rect, Size};

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
pub const TAB_MIN_W: i32 = 36;
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabSlot {
    pub rect: Rect,
    /// Empty when the tab is too narrow for one.
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
                let x = w - CONTROLS_MARGIN - CONTROL_DOT
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
    let avail = (tabs_right - tabs_left - NEW_TAB_SIZE - TAB_GAP).max(0);
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
    let mut first_visible = 0usize;
    if natural <= avail {
        for i in 0..input.tab_count {
            widths.push(if Some(i) == input.active {
                TAB_ACTIVE_W
            } else {
                TAB_INACTIVE_W
            });
        }
    } else {
        // Shrink evenly; the leftover pixels go one each to the first tabs.
        let capacity = if n == 0 {
            0
        } else {
            ((avail + TAB_GAP) / (TAB_MIN_W + TAB_GAP)).clamp(0, n)
        } as usize;
        let shown = capacity.max(1).min(input.tab_count) as i32;
        let share_total = avail - TAB_GAP * (shown - 1);
        let share = (share_total / shown).max(0);
        let extra = (share_total - share * shown).max(0);
        // A stand-in for scrolling: keep the active tab in the shown window.
        if let Some(a) = input.active {
            first_visible = (a + 1).saturating_sub(shown as usize);
        }
        for i in 0..input.tab_count {
            let in_window = i >= first_visible && i < first_visible + shown as usize;
            let k = (i - first_visible.min(i)) as i32;
            widths.push(if in_window {
                share + i32::from(k < extra)
            } else {
                0
            });
        }
    }
    let mut x = tabs_left;
    let mut tabs = Vec::with_capacity(widths.len());
    for tw in &widths {
        if *tw == 0 {
            tabs.push(TabSlot {
                rect: Rect::default(),
                close: Rect::default(),
            });
            continue;
        }
        let rect = Rect::new(x, TAB_TOP, *tw, TAB_H);
        let close = if *tw >= TAB_CLOSE_MIN_W {
            Rect::new(
                rect.right() - 8 - CLOSE_SIZE,
                rect.y + (TAB_H - CLOSE_SIZE) / 2,
                CLOSE_SIZE,
                CLOSE_SIZE,
            )
        } else {
            Rect::default()
        };
        tabs.push(TabSlot { rect, close });
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
        new_tab,
        back,
        forward,
        reload,
        overflow,
        field,
        chip,
        text_x,
        text_right: field.right() - 14,
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
    /// Chrome background (draggable in a frameless window).
    Bar,
    Page,
    Outside,
}

impl Layout {
    pub fn hit(&self, p: Point) -> Region {
        if !Rect::new(0, 0, self.size.w, self.size.h).contains(p) {
            return Region::Outside;
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
                    let l = compute(&i);
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
