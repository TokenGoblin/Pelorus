//! Back, forward, reload/stop and the overflow menu button.

use crate::input::{AppRequest, Button, Effect, PointerEvent, PointerKind};
use crate::layout::{Layout, Region};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolButton {
    Back,
    Forward,
    /// Reload, or stop while a load is in progress.
    Reload,
    Overflow,
}

impl ToolButton {
    fn of(region: Region) -> Option<ToolButton> {
        match region {
            Region::Back => Some(ToolButton::Back),
            Region::Forward => Some(ToolButton::Forward),
            Region::Reload => Some(ToolButton::Reload),
            Region::Overflow => Some(ToolButton::Overflow),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Toolbar {
    can_back: bool,
    can_forward: bool,
    loading: bool,
    hover: Option<ToolButton>,
    pressed: Option<ToolButton>,
}

impl Toolbar {
    pub fn new() -> Toolbar {
        Toolbar::default()
    }

    pub fn set_nav_state(&mut self, can_back: bool, can_forward: bool, loading: bool) {
        self.can_back = can_back;
        self.can_forward = can_forward;
        self.loading = loading;
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn hover(&self) -> Option<ToolButton> {
        self.hover
    }

    pub fn pressed(&self) -> Option<ToolButton> {
        self.pressed
    }

    pub fn enabled(&self, b: ToolButton) -> bool {
        match b {
            ToolButton::Back => self.can_back,
            ToolButton::Forward => self.can_forward,
            ToolButton::Reload | ToolButton::Overflow => true,
        }
    }

    /// Accessible name / tooltip, from px-brand.
    pub fn label(&self, b: ToolButton) -> &'static str {
        match b {
            ToolButton::Back => px_brand::ui::BACK,
            ToolButton::Forward => px_brand::ui::FORWARD,
            ToolButton::Reload if self.loading => px_brand::ui::STOP,
            ToolButton::Reload => px_brand::ui::RELOAD,
            ToolButton::Overflow => px_brand::ui::MORE,
        }
    }

    /// What activating a button asks the browser for, if it is enabled.
    pub fn activate(&self, b: ToolButton, layout: &Layout) -> Vec<Effect> {
        if !self.enabled(b) {
            return Vec::new();
        }
        let req = match b {
            ToolButton::Back => AppRequest::Back,
            ToolButton::Forward => AppRequest::Forward,
            ToolButton::Reload if self.loading => AppRequest::Stop,
            ToolButton::Reload => AppRequest::Reload,
            ToolButton::Overflow => AppRequest::OpenMenu(layout.overflow),
        };
        vec![Effect::App(req)]
    }

    /// A click fires on release over the button it was pressed on.
    pub fn pointer(&mut self, ev: &PointerEvent, layout: &Layout) -> Vec<Effect> {
        let over = ToolButton::of(layout.hit(ev.pos));
        match ev.kind {
            PointerKind::Leave => {
                self.hover = None;
                Vec::new()
            }
            PointerKind::Move => {
                self.hover = over;
                Vec::new()
            }
            PointerKind::Down(Button::Primary) => {
                self.hover = over;
                self.pressed = over.filter(|b| self.enabled(*b));
                Vec::new()
            }
            PointerKind::Up(Button::Primary) => {
                let pressed = self.pressed.take();
                match (pressed, over) {
                    (Some(p), Some(o)) if p == o => self.activate(p, layout),
                    _ => Vec::new(),
                }
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::Size;
    use crate::layout::{ControlsPlacement, LayoutInput, compute};

    fn layout() -> Layout {
        compute(&LayoutInput {
            size: Size::new(920, 568),
            controls: ControlsPlacement::Left,
            tab_count: 1,
            active: Some(0),
            isolated: true,
        })
    }

    fn click(t: &mut Toolbar, l: &Layout, r: crate::geom::Rect) -> Vec<Effect> {
        let c = r.center();
        t.pointer(&PointerEvent::new(c.x, c.y, PointerKind::Down(Button::Primary)), l);
        t.pointer(&PointerEvent::new(c.x, c.y, PointerKind::Up(Button::Primary)), l)
    }

    #[test]
    fn disabled_buttons_do_nothing_enabled_ones_fire_on_release() {
        let l = layout();
        let mut t = Toolbar::new();
        assert!(click(&mut t, &l, l.back).is_empty());
        t.set_nav_state(true, false, false);
        assert_eq!(click(&mut t, &l, l.back), vec![Effect::App(AppRequest::Back)]);
        assert!(click(&mut t, &l, l.forward).is_empty());
        assert_eq!(click(&mut t, &l, l.reload), vec![Effect::App(AppRequest::Reload)]);
        assert_eq!(
            click(&mut t, &l, l.overflow),
            vec![Effect::App(AppRequest::OpenMenu(l.overflow))]
        );
    }

    #[test]
    fn reload_becomes_stop_while_loading() {
        let l = layout();
        let mut t = Toolbar::new();
        t.set_nav_state(false, false, true);
        assert_eq!(t.label(ToolButton::Reload), px_brand::ui::STOP);
        assert_eq!(click(&mut t, &l, l.reload), vec![Effect::App(AppRequest::Stop)]);
    }

    #[test]
    fn dragging_off_a_pressed_button_cancels_the_click() {
        let l = layout();
        let mut t = Toolbar::new();
        t.set_nav_state(true, true, false);
        let c = l.back.center();
        t.pointer(&PointerEvent::new(c.x, c.y, PointerKind::Down(Button::Primary)), &l);
        assert_eq!(t.pressed(), Some(ToolButton::Back));
        let f = l.forward.center();
        let e = t.pointer(&PointerEvent::new(f.x, f.y, PointerKind::Up(Button::Primary)), &l);
        assert!(e.is_empty());
        assert_eq!(t.pressed(), None);
    }

    #[test]
    fn hover_tracks_and_clears() {
        let l = layout();
        let mut t = Toolbar::new();
        let c = l.reload.center();
        t.pointer(&PointerEvent::new(c.x, c.y, PointerKind::Move), &l);
        assert_eq!(t.hover(), Some(ToolButton::Reload));
        t.pointer(&PointerEvent::new(0, 0, PointerKind::Leave), &l);
        assert_eq!(t.hover(), None);
    }
}
