//! The chrome as a whole: input routing, focus, and the glue between the
//! tab strip, toolbar and address bar.
//!
//! [`Chrome::event`] is the only way input gets in and returns an [`Outcome`]:
//! whether the chrome consumed the event (otherwise it belongs to the page, and
//! the *browser* picks the frame) and the [`Effect`]s to carry out. It reads no
//! clock and does no I/O; the same events in the same order give the same
//! effects and the same display list.

use crate::address::{AddressBar, UrlDisplay};
use crate::geom::{Point, Rect, Size};
use crate::input::{
    AppRequest, Button, CursorShape, Effect, Event, Key, KeyEvent, Outcome, PlatformEffect,
    PointerEvent, PointerKind, PrimaryModifier, WindowAction,
};
use crate::layout::{self, ControlsPlacement, Layout, LayoutInput, Region};
use crate::shortcuts::{self, Command};
use crate::tabs::{Tab, TabId, TabStrip};
use crate::text::{AU_PER_PX, TextShaper};
use crate::toolbar::Toolbar;

/// What the platform layer tells the chrome once, at start (and again if it
/// changes, e.g. the window is maximised and loses its corners).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlatformConfig {
    pub controls: ControlsPlacement,
    pub primary: PrimaryModifier,
    /// Window corner radius in px; 0 when maximised or fullscreen.
    pub window_radius: i32,
}

impl PlatformConfig {
    /// Windows and Linux defaults: Control is primary, dots on the left as in
    /// the mockups, rounded window.
    pub fn desktop() -> PlatformConfig {
        PlatformConfig {
            controls: ControlsPlacement::Left,
            primary: PrimaryModifier::Ctrl,
            window_radius: layout::WINDOW_RADIUS,
        }
    }
}

/// Which part owns the pointer between a press and its release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capture {
    None,
    Strip,
    Toolbar,
    /// Selecting text in the address field.
    Field,
    Control(usize),
}

#[derive(Clone, Debug)]
pub struct Chrome {
    config: PlatformConfig,
    size: Size,
    tabs: TabStrip,
    toolbar: Toolbar,
    address: AddressBar,
    window_focused: bool,
    hover_control: Option<usize>,
    capture: Capture,
    cursor: CursorShape,
}

impl Chrome {
    pub fn new(config: PlatformConfig, size: Size) -> Chrome {
        Chrome {
            config,
            size,
            tabs: TabStrip::new(),
            toolbar: Toolbar::new(),
            address: AddressBar::new(),
            window_focused: true,
            hover_control: None,
            capture: Capture::None,
            cursor: CursorShape::Default,
        }
    }

    // -- state from the browser ----------------------------------------------

    pub fn set_tabs(&mut self, tabs: Vec<Tab>, active: Option<TabId>) {
        self.tabs.set_tabs(tabs, active);
    }

    pub fn set_url(&mut self, display: UrlDisplay) {
        self.address.set_display(display);
    }

    pub fn set_nav_state(&mut self, can_back: bool, can_forward: bool, loading: bool) {
        self.toolbar.set_nav_state(can_back, can_forward, loading);
    }

    /// Show the "Isolated" chip. The browser sets it only for what the product
    /// can honestly say (a sandboxed content process, invariant 1).
    pub fn set_isolated(&mut self, isolated: bool) {
        self.address.set_isolated(isolated);
    }

    pub fn set_config(&mut self, config: PlatformConfig) {
        self.config = config;
    }

    // -- state out -------------------------------------------------------------

    pub fn config(&self) -> PlatformConfig {
        self.config
    }

    pub fn size(&self) -> Size {
        self.size
    }

    pub fn tabs(&self) -> &TabStrip {
        &self.tabs
    }

    pub fn toolbar(&self) -> &Toolbar {
        &self.toolbar
    }

    pub fn address(&self) -> &AddressBar {
        &self.address
    }

    pub fn window_focused(&self) -> bool {
        self.window_focused
    }

    pub fn hover_control(&self) -> Option<usize> {
        self.hover_control
    }

    pub fn layout(&self) -> Layout {
        layout::compute(&LayoutInput {
            size: self.size,
            controls: self.config.controls,
            tab_count: self.tabs.tabs().len(),
            active: self.tabs.active_index(),
            isolated: self.address.is_isolated(),
        })
    }

    /// Where the compositor may put the page surface.
    pub fn page_rect(&self) -> Rect {
        self.layout().page
    }

    /// Does the chrome hold keyboard focus (versus the page)?
    pub fn has_keyboard_focus(&self) -> bool {
        self.address.is_editing()
    }

    // -- input -------------------------------------------------------------------

    pub fn event(&mut self, ev: Event, shaper: &dyn TextShaper) -> Outcome {
        let mut out = match ev {
            Event::Resize(size) => {
                self.size = size;
                consumed(Vec::new())
            }
            Event::WindowFocus(f) => {
                self.window_focused = f;
                if !f {
                    self.hover_control = None;
                    self.capture = Capture::None;
                }
                consumed(Vec::new())
            }
            Event::Key(k) => self.key(k),
            Event::Text(s) | Event::Clipboard(s) => {
                if self.address.is_editing() {
                    self.address.insert_text(&s);
                    consumed(Vec::new())
                } else {
                    Outcome::default()
                }
            }
            Event::Preedit(p) => {
                if self.address.is_editing() {
                    self.address.set_preedit(p);
                    consumed(Vec::new())
                } else {
                    Outcome::default()
                }
            }
            Event::Pointer(p) => self.pointer(p, shaper),
            Event::Wheel { .. } => Outcome::default(),
        };
        if out.consumed {
            if self.address.is_editing() {
                out.effects
                    .push(Effect::Platform(PlatformEffect::SetImeCursorArea(
                        self.caret_rect(shaper),
                    )));
            }
            out.effects.push(Effect::Platform(PlatformEffect::Redraw));
        }
        out
    }

    fn key(&mut self, k: KeyEvent) -> Outcome {
        if let Some(cmd) = shortcuts::resolve(k, self.config.primary) {
            return consumed(self.run(cmd));
        }
        if k.key == Key::Escape
            && !self.address.is_editing()
            && self.toolbar.is_loading()
            && k.mods == crate::input::Modifiers::NONE
        {
            return consumed(self.run(Command::Stop));
        }
        if self.address.is_editing() {
            return consumed(self.address.key(k, self.config.primary));
        }
        Outcome::default()
    }

    /// Run a command: the same path for a shortcut and (later) a menu item.
    pub fn run(&mut self, cmd: Command) -> Vec<Effect> {
        let layout = self.layout();
        let tool = |t: &Toolbar, b| t.activate(b, &layout);
        use crate::toolbar::ToolButton as B;
        match cmd {
            Command::FocusAddress => self.address.focus(),
            Command::NewTab => vec![Effect::App(AppRequest::NewTab)],
            Command::CloseTab => self.tabs.close_active(),
            Command::NextTab => self.tabs.select_relative(1),
            Command::PrevTab => self.tabs.select_relative(-1),
            Command::SelectTab(i) => self.tabs.select_nth(Some(i)),
            Command::LastTab => self.tabs.select_nth(None),
            Command::MoveTabLeft => self.tabs.move_active(-1),
            Command::MoveTabRight => self.tabs.move_active(1),
            Command::Back => tool(&self.toolbar, B::Back),
            Command::Forward => tool(&self.toolbar, B::Forward),
            Command::Reload => {
                // F5 while loading restarts; the button's stop is its click.
                if self.toolbar.is_loading() {
                    vec![Effect::App(AppRequest::Reload)]
                } else {
                    tool(&self.toolbar, B::Reload)
                }
            }
            Command::Stop => vec![Effect::App(AppRequest::Stop)],
        }
    }

    fn pointer(&mut self, ev: PointerEvent, shaper: &dyn TextShaper) -> Outcome {
        let layout = self.layout();
        let region = layout.hit(ev.pos);
        let in_chrome = !matches!(region, Region::Page | Region::Outside);
        match ev.kind {
            PointerKind::Leave => {
                self.hover_control = None;
                let mut e = self.tabs.pointer(&ev, &layout);
                e.extend(self.toolbar.pointer(&ev, &layout));
                e.extend(self.set_cursor(CursorShape::Default));
                consumed(e)
            }
            PointerKind::Move => {
                let mut e = Vec::new();
                match self.capture {
                    Capture::Field => {
                        let idx = self.index_at(ev.pos.x, &layout, shaper);
                        self.address.place_caret(idx, true);
                    }
                    Capture::Strip => e.extend(self.tabs.pointer(&ev, &layout)),
                    Capture::Toolbar => e.extend(self.toolbar.pointer(&ev, &layout)),
                    Capture::Control(_) => {}
                    Capture::None => {
                        e.extend(self.tabs.pointer(&ev, &layout));
                        e.extend(self.toolbar.pointer(&ev, &layout));
                        self.hover_control = match region {
                            Region::Control(i) => Some(i),
                            _ => None,
                        };
                    }
                }
                let shape = match region {
                    Region::Field => CursorShape::Text,
                    Region::Tab(_)
                    | Region::Close(_)
                    | Region::NewTab
                    | Region::Back
                    | Region::Forward
                    | Region::Reload
                    | Region::Overflow
                    | Region::Control(_) => CursorShape::Pointer,
                    _ => CursorShape::Default,
                };
                e.extend(self.set_cursor(if self.capture == Capture::Field {
                    CursorShape::Text
                } else {
                    shape
                }));
                Outcome {
                    consumed: in_chrome || self.capture != Capture::None,
                    effects: e,
                }
            }
            PointerKind::Down(button) => {
                let mut e = Vec::new();
                // A press anywhere but the field ends editing; a press on the
                // page is then the page's.
                if self.address.is_editing() && region != Region::Field {
                    e.extend(self.address.blur());
                }
                if !in_chrome {
                    return Outcome {
                        consumed: false,
                        effects: e,
                    };
                }
                self.capture = Capture::None;
                match region {
                    Region::Field if button == Button::Primary => {
                        if self.address.is_editing() {
                            let idx = self.index_at(ev.pos.x, &layout, shaper);
                            self.address.place_caret(idx, ev.mods.shift);
                            self.capture = Capture::Field;
                        } else {
                            e.extend(self.address.focus());
                        }
                    }
                    Region::Tab(_) | Region::Close(_) | Region::NewTab => {
                        self.capture = Capture::Strip;
                        e.extend(self.tabs.pointer(&ev, &layout));
                    }
                    Region::Back | Region::Forward | Region::Reload | Region::Overflow => {
                        self.capture = Capture::Toolbar;
                        e.extend(self.toolbar.pointer(&ev, &layout));
                    }
                    Region::Control(i) if button == Button::Primary => {
                        self.capture = Capture::Control(i);
                    }
                    _ => {}
                }
                consumed(e)
            }
            PointerKind::Up(button) => {
                let capture = std::mem::replace(&mut self.capture, Capture::None);
                let mut e = Vec::new();
                match capture {
                    Capture::Strip => e.extend(self.tabs.pointer(&ev, &layout)),
                    Capture::Toolbar => e.extend(self.toolbar.pointer(&ev, &layout)),
                    Capture::Control(i) => {
                        if button == Button::Primary && region == Region::Control(i) {
                            let action = match i {
                                0 => WindowAction::Close,
                                1 => WindowAction::Minimize,
                                _ => WindowAction::Maximize,
                            };
                            e.push(Effect::Platform(PlatformEffect::Window(action)));
                        }
                    }
                    Capture::Field | Capture::None => {}
                }
                Outcome {
                    consumed: in_chrome || capture != Capture::None,
                    effects: e,
                }
            }
        }
    }

    fn set_cursor(&mut self, shape: CursorShape) -> Vec<Effect> {
        if shape == self.cursor {
            return Vec::new();
        }
        self.cursor = shape;
        vec![Effect::Platform(PlatformEffect::SetCursor(shape))]
    }

    /// Byte index in the address text nearest to window x.
    fn index_at(&self, x: i32, layout: &Layout, shaper: &dyn TextShaper) -> usize {
        let text = self.address.text();
        let shaped = shaper.shape(text, layout::TEXT_ADDRESS);
        let rel = (x - layout.text_x).saturating_mul(AU_PER_PX);
        shaped.boundary_at(text, rel)
    }

    /// The caret as a rectangle, for IME candidate windows.
    pub fn caret_rect(&self, shaper: &dyn TextShaper) -> Rect {
        let layout = self.layout();
        let (visible, caret) = self.address.visible_text();
        let shaped = shaper.shape(&visible, layout::TEXT_ADDRESS);
        let x = layout.text_x + shaped.x_at(caret) / AU_PER_PX;
        Rect::new(x, layout.field.y + 6, 1, layout.field.h - 12)
    }

    /// For the platform: should a pointer at `p` be offered to the page?
    pub fn is_page_point(&self, p: Point) -> bool {
        self.layout().hit(p) == Region::Page
    }
}

fn consumed(effects: Vec<Effect>) -> Outcome {
    Outcome {
        consumed: true,
        effects,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::Modifiers;
    use crate::text::testing::Mono;

    fn chrome() -> Chrome {
        let mut c = Chrome::new(PlatformConfig::desktop(), Size::new(920, 568));
        c.set_tabs(
            vec![
                Tab::new(TabId(1), "One", false),
                Tab::new(TabId(2), "Two", false),
            ],
            Some(TabId(1)),
        );
        c.set_url(UrlDisplay::from_parts("https://", "", "example.com", "/a"));
        c.set_isolated(true);
        c
    }

    fn ctrl(key: Key) -> Event {
        Event::Key(KeyEvent::with(
            key,
            Modifiers {
                ctrl: true,
                ..Modifiers::NONE
            },
        ))
    }

    fn ptr(x: i32, y: i32, kind: PointerKind) -> Event {
        Event::Pointer(PointerEvent::new(x, y, kind))
    }

    fn click(c: &mut Chrome, p: Point) -> Vec<Effect> {
        let m = Mono;
        let mut e = c
            .event(ptr(p.x, p.y, PointerKind::Down(Button::Primary)), &m)
            .effects;
        e.extend(
            c.event(ptr(p.x, p.y, PointerKind::Up(Button::Primary)), &m)
                .effects,
        );
        e
    }

    fn apps(e: &[Effect]) -> Vec<AppRequest> {
        e.iter()
            .filter_map(|e| match e {
                Effect::App(a) => Some(a.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn keys_belong_to_the_page_until_the_address_is_focused() {
        let mut c = chrome();
        let m = Mono;
        let o = c.event(Event::Key(KeyEvent::new(Key::Char('x'))), &m);
        assert!(!o.consumed && o.effects.is_empty());
        let o = c.event(Event::Text("x".into()), &m);
        assert!(!o.consumed);
        let o = c.event(ctrl(Key::Char('l')), &m);
        assert!(o.consumed && c.address().is_editing() && c.has_keyboard_focus());
        let o = c.event(Event::Text("hi".into()), &m);
        assert!(o.consumed);
        assert_eq!(c.address().text(), "hi");
        // While editing, keys never leak to the page.
        let o = c.event(Event::Key(KeyEvent::new(Key::Char('q'))), &m);
        assert!(o.consumed);
    }

    #[test]
    fn typing_and_enter_navigates() {
        let mut c = chrome();
        let m = Mono;
        c.event(ctrl(Key::Char('l')), &m);
        c.event(Event::Text("rust lang".into()), &m);
        let o = c.event(Event::Key(KeyEvent::new(Key::Enter)), &m);
        assert_eq!(
            apps(&o.effects),
            vec![AppRequest::Navigate("rust lang".into())]
        );
        assert!(!c.address().is_editing());
        assert!(
            o.effects
                .contains(&Effect::Platform(PlatformEffect::Redraw))
        );
    }

    #[test]
    fn shortcuts_work_while_typing_and_ignore_a_dead_back_button() {
        let mut c = chrome();
        let m = Mono;
        c.event(ctrl(Key::Char('l')), &m);
        let o = c.event(ctrl(Key::Char('t')), &m);
        assert_eq!(apps(&o.effects), vec![AppRequest::NewTab]);
        let alt_left = Event::Key(KeyEvent::with(
            Key::Left,
            Modifiers {
                alt: true,
                ..Modifiers::NONE
            },
        ));
        assert!(apps(&c.event(alt_left.clone(), &m).effects).is_empty());
        c.set_nav_state(true, false, false);
        assert_eq!(apps(&c.event(alt_left, &m).effects), vec![AppRequest::Back]);
        let o = c.event(ctrl(Key::Tab), &m);
        assert_eq!(apps(&o.effects), vec![AppRequest::ActivateTab(TabId(2))]);
    }

    #[test]
    fn escape_stops_a_load_only_when_not_editing() {
        let mut c = chrome();
        let m = Mono;
        let esc = Event::Key(KeyEvent::new(Key::Escape));
        assert!(!c.event(esc.clone(), &m).consumed, "idle: the page's");
        c.set_nav_state(false, false, true);
        assert_eq!(
            apps(&c.event(esc.clone(), &m).effects),
            vec![AppRequest::Stop]
        );
        c.event(ctrl(Key::Char('l')), &m);
        assert!(
            apps(&c.event(esc, &m).effects).is_empty(),
            "leaves the field instead"
        );
        assert!(!c.address().is_editing());
    }

    #[test]
    fn clicks_in_the_page_are_the_pages_and_blur_the_field() {
        let mut c = chrome();
        let m = Mono;
        c.event(ctrl(Key::Char('l')), &m);
        let o = c.event(ptr(400, 300, PointerKind::Down(Button::Primary)), &m);
        assert!(!o.consumed, "the page gets the click");
        assert!(!c.address().is_editing(), "but the field let go first");
        assert!(
            o.effects
                .contains(&Effect::Platform(PlatformEffect::SetImeAllowed(false)))
        );
        assert!(c.is_page_point(Point { x: 400, y: 300 }));
        assert!(!c.is_page_point(Point { x: 400, y: 50 }));
    }

    #[test]
    fn first_click_in_the_field_selects_all_the_next_places_the_caret() {
        let mut c = chrome();
        let m = Mono;
        let l = c.layout();
        let f = l.field.center();
        c.event(ptr(f.x, f.y, PointerKind::Down(Button::Primary)), &m);
        assert!(c.address().is_editing());
        assert!(c.address().selection().is_some());
        c.event(ptr(f.x, f.y, PointerKind::Up(Button::Primary)), &m);
        // Second press: Mono is 8.4 px/char at 14 px; click at the 3rd boundary.
        let x = l.text_x + 25;
        c.event(ptr(x, f.y, PointerKind::Down(Button::Primary)), &m);
        assert_eq!(c.address().selection(), None);
        assert_eq!(c.address().cursor(), 3);
        // Drag to select to the right.
        c.event(ptr(x + 17, f.y, PointerKind::Move), &m);
        assert_eq!(c.address().selection(), Some((3, 5)));
        c.event(ptr(x + 17, f.y, PointerKind::Up(Button::Primary)), &m);
        // A move after release no longer extends.
        c.event(ptr(x + 60, f.y, PointerKind::Move), &m);
        assert_eq!(c.address().selection(), Some((3, 5)));
    }

    #[test]
    fn tab_and_toolbar_clicks_flow_through_to_requests() {
        let mut c = chrome();
        c.set_nav_state(true, false, false);
        let l = c.layout();
        assert_eq!(
            apps(&click(&mut c, l.tabs[1].rect.center())),
            vec![AppRequest::ActivateTab(TabId(2))]
        );
        let l = c.layout();
        assert_eq!(
            apps(&click(&mut c, l.back.center())),
            vec![AppRequest::Back]
        );
        assert_eq!(
            apps(&click(&mut c, l.new_tab.center())),
            vec![AppRequest::NewTab]
        );
        let close = l.tabs[1].close.center();
        assert_eq!(
            apps(&click(&mut c, close)),
            vec![AppRequest::CloseTab(TabId(2))]
        );
    }

    #[test]
    fn window_controls_emit_platform_actions() {
        let mut c = chrome();
        let l = c.layout();
        let e = click(&mut c, l.controls[0].center());
        assert!(e.contains(&Effect::Platform(PlatformEffect::Window(
            WindowAction::Close
        ))));
        let e = click(&mut c, l.controls[2].center());
        assert!(e.contains(&Effect::Platform(PlatformEffect::Window(
            WindowAction::Maximize
        ))));
    }

    #[test]
    fn cursor_changes_are_reported_once() {
        let mut c = chrome();
        let m = Mono;
        let f = c.layout().field.center();
        let o = c.event(ptr(f.x, f.y, PointerKind::Move), &m);
        assert!(
            o.effects
                .contains(&Effect::Platform(PlatformEffect::SetCursor(
                    CursorShape::Text
                )))
        );
        let o = c.event(ptr(f.x + 1, f.y, PointerKind::Move), &m);
        assert!(
            !o.effects
                .iter()
                .any(|e| matches!(e, Effect::Platform(PlatformEffect::SetCursor(_))))
        );
    }

    #[test]
    fn resize_relayouts_and_a_move_over_the_page_is_not_consumed() {
        let mut c = chrome();
        let m = Mono;
        c.event(Event::Resize(Size::new(640, 400)), &m);
        assert_eq!(c.layout().page, Rect::new(0, 92, 640, 308));
        assert!(!c.event(ptr(300, 200, PointerKind::Move), &m).consumed);
    }

    #[test]
    fn ime_area_follows_the_caret_while_editing() {
        let mut c = chrome();
        let m = Mono;
        let o = c.event(ctrl(Key::Char('l')), &m);
        assert!(
            o.effects
                .iter()
                .any(|e| matches!(e, Effect::Platform(PlatformEffect::SetImeCursorArea(_))))
        );
        c.event(Event::Text("abcd".into()), &m);
        let r = c.caret_rect(&m);
        assert_eq!(r.x, c.layout().text_x + 33); // 4 x 8.4 px
        assert!(r.h > 0);
    }

    #[test]
    fn window_focus_loss_cancels_a_press() {
        let mut c = chrome();
        let m = Mono;
        let l = c.layout();
        let p = l.controls[0].center();
        c.event(ptr(p.x, p.y, PointerKind::Down(Button::Primary)), &m);
        c.event(Event::WindowFocus(false), &m);
        let o = c.event(ptr(p.x, p.y, PointerKind::Up(Button::Primary)), &m);
        assert!(
            !o.effects
                .iter()
                .any(|e| matches!(e, Effect::Platform(PlatformEffect::Window(_))))
        );
    }
}
