//! The tab strip as a state machine.
//!
//! The browser owns the list of tabs and which is active; the strip mirrors it
//! ([`TabStrip::set_tabs`]) and reports what the user asked for as
//! [`AppRequest`]s. Local state is only what the pointer is doing: hover, a
//! pressed button, a drag in progress. `TabId`s are minted by the browser; the
//! chrome never invents or trusts one from outside (root CLAUDE.md).

use crate::geom::Point;
use crate::input::{AppRequest, Button, Effect, PointerEvent, PointerKind};
use crate::layout::{Layout, Region};
use crate::text;

/// Identifies a tab. Opaque to the chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TabId(pub u64);

/// Most tabs the strip will hold; the rest of a longer list is dropped.
pub const MAX_TABS: usize = 512;

/// Movement under which a press on a tab becomes a drag.
const DRAG_THRESHOLD: i32 = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tab {
    pub id: TabId,
    /// Sanitised page title.
    pub title: String,
    pub loading: bool,
}

impl Tab {
    /// Titles are page-controlled: control and bidi-override characters are
    /// dropped and the length capped. An empty title shows the brand string.
    pub fn new(id: TabId, title: &str, loading: bool) -> Tab {
        Tab {
            id,
            title: text::sanitize(title),
            loading,
        }
    }

    pub fn display_title(&self) -> &str {
        if self.title.trim().is_empty() {
            px_brand::ui::NEW_TAB_TITLE
        } else {
            &self.title
        }
    }
}

/// What the pointer is over, for hover styling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hover {
    #[default]
    None,
    Tab(usize),
    Close(usize),
    NewTab,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pressed {
    Tab { index: usize, start_x: i32, dragging: bool },
    MiddleTab(usize),
    Close(usize),
    NewTab,
}

#[derive(Clone, Debug, Default)]
pub struct TabStrip {
    tabs: Vec<Tab>,
    active: Option<usize>,
    hover: Hover,
    pressed: Option<Pressed>,
}

impl TabStrip {
    pub fn new() -> TabStrip {
        TabStrip::default()
    }

    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    pub fn active_index(&self) -> Option<usize> {
        self.active
    }

    pub fn active_id(&self) -> Option<TabId> {
        self.active.and_then(|i| self.tabs.get(i)).map(|t| t.id)
    }

    pub fn hover(&self) -> Hover {
        self.hover
    }

    /// Whether the strip is mid-drag (the platform may want a grab cursor).
    pub fn is_dragging(&self) -> bool {
        matches!(self.pressed, Some(Pressed::Tab { dragging: true, .. }))
    }

    /// The browser's truth. An `active` that is not in `tabs` leaves nothing
    /// active (fail closed: no guessing which tab was meant).
    pub fn set_tabs(&mut self, mut tabs: Vec<Tab>, active: Option<TabId>) {
        tabs.truncate(MAX_TABS);
        self.active = active.and_then(|id| tabs.iter().position(|t| t.id == id));
        self.tabs = tabs;
        self.hover = Hover::None;
        self.pressed = None;
    }

    // -- keyboard-driven commands ------------------------------------------

    fn activate(&mut self, index: usize) -> Vec<Effect> {
        let Some(tab) = self.tabs.get(index) else {
            return Vec::new();
        };
        let id = tab.id;
        if self.active == Some(index) {
            return Vec::new();
        }
        self.active = Some(index);
        vec![Effect::App(AppRequest::ActivateTab(id))]
    }

    /// Next (`+1`) or previous (`-1`) tab, wrapping.
    pub fn select_relative(&mut self, delta: i32) -> Vec<Effect> {
        let n = self.tabs.len() as i32;
        let Some(cur) = self.active else {
            return Vec::new();
        };
        if n == 0 {
            return Vec::new();
        }
        let next = (cur as i32 + delta).rem_euclid(n) as usize;
        self.activate(next)
    }

    /// Ctrl+1..8 select that tab; Ctrl+9 (`None`) selects the last.
    pub fn select_nth(&mut self, n: Option<usize>) -> Vec<Effect> {
        let index = match n {
            Some(i) => i,
            None => self.tabs.len().saturating_sub(1),
        };
        self.activate(index)
    }

    pub fn close_active(&mut self) -> Vec<Effect> {
        match self.active_id() {
            Some(id) => vec![Effect::App(AppRequest::CloseTab(id))],
            None => Vec::new(),
        }
    }

    /// Move the active tab one place; no wrap at the ends.
    pub fn move_active(&mut self, delta: i32) -> Vec<Effect> {
        let Some(cur) = self.active else {
            return Vec::new();
        };
        let to = cur as i32 + delta;
        if to < 0 || to >= self.tabs.len() as i32 {
            return Vec::new();
        }
        self.move_tab(cur, to as usize)
    }

    fn move_tab(&mut self, from: usize, to: usize) -> Vec<Effect> {
        if from == to || from >= self.tabs.len() || to >= self.tabs.len() {
            return Vec::new();
        }
        let id = self.tabs[from].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active = self.active.map(|a| {
            if a == from {
                to
            } else if from < a && a <= to {
                a - 1
            } else if to <= a && a < from {
                a + 1
            } else {
                a
            }
        });
        vec![Effect::App(AppRequest::MoveTab { tab: id, to })]
    }

    // -- pointer -------------------------------------------------------------

    /// Handle a pointer event that `layout` says is over the strip (or that
    /// follows a press that started there). Returns the effects; the caller
    /// decides whether the event was consumed.
    pub fn pointer(&mut self, ev: &PointerEvent, layout: &Layout) -> Vec<Effect> {
        let region = layout.hit(ev.pos);
        match ev.kind {
            PointerKind::Leave => {
                self.hover = Hover::None;
                Vec::new()
            }
            PointerKind::Move => {
                self.hover = Self::hover_of(region);
                self.drag_to(ev.pos, layout)
            }
            PointerKind::Down(Button::Primary) => {
                self.hover = Self::hover_of(region);
                match region {
                    Region::Close(i) => {
                        self.pressed = Some(Pressed::Close(i));
                        Vec::new()
                    }
                    Region::Tab(i) => {
                        self.pressed = Some(Pressed::Tab {
                            index: i,
                            start_x: ev.pos.x,
                            dragging: false,
                        });
                        self.activate(i)
                    }
                    Region::NewTab => {
                        self.pressed = Some(Pressed::NewTab);
                        Vec::new()
                    }
                    _ => Vec::new(),
                }
            }
            PointerKind::Down(Button::Middle) => {
                if let Region::Tab(i) | Region::Close(i) = region {
                    self.pressed = Some(Pressed::MiddleTab(i));
                }
                Vec::new()
            }
            PointerKind::Down(Button::Secondary) => Vec::new(),
            PointerKind::Up(button) => {
                let pressed = self.pressed.take();
                match (button, pressed, region) {
                    (Button::Primary, Some(Pressed::Close(i)), Region::Close(j)) if i == j => {
                        self.tabs
                            .get(i)
                            .map(|t| vec![Effect::App(AppRequest::CloseTab(t.id))])
                            .unwrap_or_default()
                    }
                    (Button::Primary, Some(Pressed::NewTab), Region::NewTab) => {
                        vec![Effect::App(AppRequest::NewTab)]
                    }
                    (
                        Button::Middle,
                        Some(Pressed::MiddleTab(i)),
                        Region::Tab(j) | Region::Close(j),
                    ) if i == j => self
                        .tabs
                        .get(i)
                        .map(|t| vec![Effect::App(AppRequest::CloseTab(t.id))])
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            }
        }
    }

    fn hover_of(region: Region) -> Hover {
        match region {
            Region::Tab(i) => Hover::Tab(i),
            Region::Close(i) => Hover::Close(i),
            Region::NewTab => Hover::NewTab,
            _ => Hover::None,
        }
    }

    /// While a tab is held and has moved past the threshold, reorder live: the
    /// tab goes to the slot under the pointer.
    fn drag_to(&mut self, pos: Point, layout: &Layout) -> Vec<Effect> {
        let Some(Pressed::Tab {
            index,
            start_x,
            dragging,
        }) = self.pressed
        else {
            return Vec::new();
        };
        if !dragging && (pos.x - start_x).abs() < DRAG_THRESHOLD {
            return Vec::new();
        }
        // The slot under the pointer, among tabs that are shown.
        let shown: Vec<(usize, i32, i32)> = layout
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.rect.is_empty())
            .map(|(i, t)| (i, t.rect.x, t.rect.right()))
            .collect();
        let target = match (shown.first(), shown.last()) {
            (Some(first), _) if pos.x < first.1 => first.0,
            (_, Some(last)) if pos.x >= last.2 => last.0,
            _ => shown
                .iter()
                .find(|(_, x0, x1)| pos.x >= *x0 && pos.x < *x1)
                .map_or(index, |s| s.0),
        };
        let mut out = Vec::new();
        let mut now = index;
        if target != index {
            out = self.move_tab(index, target);
            now = target;
        }
        self.pressed = Some(Pressed::Tab {
            index: now,
            start_x,
            dragging: true,
        });
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Size;
    use crate::layout::{ControlsPlacement, LayoutInput, compute};

    fn strip(n: u64, active: u64) -> TabStrip {
        let mut s = TabStrip::new();
        s.set_tabs(
            (1..=n).map(|i| Tab::new(TabId(i), "t", false)).collect(),
            Some(TabId(active)),
        );
        s
    }

    fn lay(s: &TabStrip) -> Layout {
        compute(&LayoutInput {
            size: Size::new(920, 568),
            controls: ControlsPlacement::Left,
            tab_count: s.tabs().len(),
            active: s.active_index(),
            isolated: true,
        })
    }

    fn ev(p: Point, kind: PointerKind) -> PointerEvent {
        PointerEvent::new(p.x, p.y, kind)
    }

    fn app(e: &[Effect]) -> Vec<&AppRequest> {
        e.iter()
            .filter_map(|e| match e {
                Effect::App(a) => Some(a),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn unknown_active_leaves_none_active_and_the_list_is_capped() {
        let mut s = TabStrip::new();
        s.set_tabs(vec![Tab::new(TabId(1), "a", false)], Some(TabId(9)));
        assert_eq!(s.active_index(), None);
        s.set_tabs(
            (0..MAX_TABS as u64 + 50)
                .map(|i| Tab::new(TabId(i), "", false))
                .collect(),
            None,
        );
        assert_eq!(s.tabs().len(), MAX_TABS);
        assert_eq!(s.tabs()[0].display_title(), px_brand::ui::NEW_TAB_TITLE);
    }

    #[test]
    fn titles_are_sanitised() {
        let t = Tab::new(TabId(1), "pay\u{202E}pal\n", false);
        assert_eq!(t.title, "paypal");
    }

    #[test]
    fn clicking_a_tab_activates_it_once() {
        let mut s = strip(3, 1);
        let l = lay(&s);
        let p = l.tabs[2].rect.center();
        let e = s.pointer(&ev(p, PointerKind::Down(Button::Primary)), &l);
        assert_eq!(app(&e), vec![&AppRequest::ActivateTab(TabId(3))]);
        assert_eq!(s.active_index(), Some(2));
        let e = s.pointer(&ev(p, PointerKind::Up(Button::Primary)), &l);
        assert!(e.is_empty());
        // Clicking the already active tab asks for nothing.
        let l = lay(&s);
        let p = l.tabs[2].rect.center();
        let e = s.pointer(&ev(p, PointerKind::Down(Button::Primary)), &l);
        assert!(e.is_empty());
    }

    #[test]
    fn close_needs_press_and_release_on_the_same_button() {
        let mut s = strip(3, 1);
        let l = lay(&s);
        let c = l.tabs[0].close.center();
        let down = ev(c, PointerKind::Down(Button::Primary));
        assert!(s.pointer(&down, &l).is_empty());
        let e = s.pointer(&ev(c, PointerKind::Up(Button::Primary)), &l);
        assert_eq!(app(&e), vec![&AppRequest::CloseTab(TabId(1))]);

        // Press on close, release elsewhere: nothing.
        s.pointer(&down, &l);
        let e = s.pointer(
            &ev(l.tabs[1].rect.center(), PointerKind::Up(Button::Primary)),
            &l,
        );
        assert!(e.is_empty());
    }

    #[test]
    fn middle_click_closes_on_release_over_the_same_tab() {
        let mut s = strip(3, 1);
        let l = lay(&s);
        let p = l.tabs[1].rect.center();
        assert!(s.pointer(&ev(p, PointerKind::Down(Button::Middle)), &l).is_empty());
        let e = s.pointer(&ev(p, PointerKind::Up(Button::Middle)), &l);
        assert_eq!(app(&e), vec![&AppRequest::CloseTab(TabId(2))]);
    }

    #[test]
    fn new_tab_button() {
        let mut s = strip(2, 1);
        let l = lay(&s);
        let p = l.new_tab.center();
        s.pointer(&ev(p, PointerKind::Down(Button::Primary)), &l);
        assert_eq!(s.hover(), Hover::NewTab);
        let e = s.pointer(&ev(p, PointerKind::Up(Button::Primary)), &l);
        assert_eq!(app(&e), vec![&AppRequest::NewTab]);
    }

    #[test]
    fn dragging_a_tab_reorders_live_and_reports_each_move() {
        let mut s = strip(3, 1);
        let l = lay(&s);
        let start = l.tabs[0].rect.center();
        s.pointer(&ev(start, PointerKind::Down(Button::Primary)), &l);
        // A small wobble is not a drag.
        let e = s.pointer(&ev(Point { x: start.x + 2, y: start.y }, PointerKind::Move), &l);
        assert!(e.is_empty() && !s.is_dragging());
        // Drag over the third tab.
        let l = lay(&s);
        let over = l.tabs[2].rect.center();
        let e = s.pointer(&ev(over, PointerKind::Move), &l);
        assert_eq!(
            app(&e),
            vec![&AppRequest::MoveTab {
                tab: TabId(1),
                to: 2
            }]
        );
        assert!(s.is_dragging());
        let ids: Vec<u64> = s.tabs().iter().map(|t| t.id.0).collect();
        assert_eq!(ids, vec![2, 3, 1]);
        assert_eq!(s.active_id(), Some(TabId(1)));
        // Far left clamps to the first slot.
        let l = lay(&s);
        let e = s.pointer(&ev(Point { x: 0, y: 20 }, PointerKind::Move), &l);
        assert_eq!(app(&e).len(), 1);
        assert_eq!(s.tabs()[0].id, TabId(1));
        // Release ends the drag with no extra request.
        let e = s.pointer(&ev(Point { x: 0, y: 20 }, PointerKind::Up(Button::Primary)), &l);
        assert!(e.is_empty() && !s.is_dragging());
    }

    #[test]
    fn keyboard_selection_wraps_and_moves_do_not() {
        let mut s = strip(3, 3);
        let e = s.select_relative(1);
        assert_eq!(app(&e), vec![&AppRequest::ActivateTab(TabId(1))]);
        let e = s.select_relative(-1);
        assert_eq!(app(&e), vec![&AppRequest::ActivateTab(TabId(3))]);
        let e = s.select_nth(Some(1));
        assert_eq!(app(&e), vec![&AppRequest::ActivateTab(TabId(2))]);
        assert!(s.select_nth(Some(7)).is_empty(), "out of range is ignored");
        let e = s.select_nth(None);
        assert_eq!(app(&e), vec![&AppRequest::ActivateTab(TabId(3))]);
        assert!(s.move_active(1).is_empty(), "no wrap at the end");
        let e = s.move_active(-1);
        assert_eq!(app(&e), vec![&AppRequest::MoveTab { tab: TabId(3), to: 1 }]);
        assert_eq!(s.active_index(), Some(1));
        assert_eq!(app(&s.close_active()), vec![&AppRequest::CloseTab(TabId(3))]);
    }

    #[test]
    fn an_empty_strip_does_nothing_for_every_command() {
        let mut s = TabStrip::new();
        assert!(s.select_relative(1).is_empty());
        assert!(s.select_nth(None).is_empty());
        assert!(s.close_active().is_empty());
        assert!(s.move_active(1).is_empty());
    }

    #[test]
    fn active_index_follows_a_move_of_another_tab() {
        let mut s = strip(4, 2); // active is index 1
        s.move_tab(0, 3);
        assert_eq!(s.active_id(), Some(TabId(2)));
        s.move_tab(3, 0);
        assert_eq!(s.active_id(), Some(TabId(2)));
    }
}
