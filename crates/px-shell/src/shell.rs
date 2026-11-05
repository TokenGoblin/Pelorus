//! The browser state behind the window: tabs with history, the page in each, scroll,
//! link clicks, and the compositing of the page under the chrome.
//!
//! Window-independent on purpose: events go in through [`Shell::handle`] with any
//! `PlatformWindow`, frames come out of [`Shell::frame`], so the tests drive it with
//! a mock window and no display.

use px_paint::raster::Canvas;
use px_ui::geom::{Point, Rect, Size};
use px_ui::input::{AppRequest, Button, CursorShape, Event, Key, PointerKind};
use px_ui::paint;
use px_ui::platform::{PlatformWindow, pump};
use px_ui::tabs::{Tab, TabId};
use px_ui::theme::Theme;
use px_ui::{Chrome, PlatformConfig};

use crate::fonts::{ChromeFont, LayoutFonts};
use crate::nav::{self, Location};
use crate::page::{self, Laid};

/// Pixels per wheel detent, and per arrow-key press.
const WHEEL_STEP: i32 = 48;
const KEY_STEP: i32 = 40;

struct Entry {
    loc: Location,
    scroll: i32,
}

struct Doc {
    source: String,
    file: Option<std::path::PathBuf>,
    laid: Option<Laid>,
    title: String,
}

struct TabState {
    id: TabId,
    history: Vec<Entry>,
    pos: usize,
    doc: Doc,
}

impl TabState {
    fn entry(&mut self) -> Option<&mut Entry> {
        self.history.get_mut(self.pos)
    }
    fn loc(&self) -> Option<&Location> {
        self.history.get(self.pos).map(|e| &e.loc)
    }
    fn scroll(&self) -> i32 {
        self.history.get(self.pos).map_or(0, |e| e.scroll)
    }
}

fn blank_doc() -> Doc {
    Doc {
        source: String::new(),
        file: None,
        laid: None,
        title: String::new(),
    }
}

pub struct Shell {
    chrome: Chrome,
    font: ChromeFont,
    theme: Theme,
    tabs: Vec<TabState>,
    active: usize,
    next_id: u64,
    /// The link under the pointer, as a document-space href.
    hover: Option<String>,
    /// The href a primary press started on; a release on the same one follows it.
    pressed: Option<String>,
}

impl Shell {
    pub fn new(size: Size, config: PlatformConfig, theme: Theme, start: &[Location]) -> Shell {
        let mut s = Shell {
            chrome: Chrome::new(config, size),
            font: ChromeFont::new(),
            theme,
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            hover: None,
            pressed: None,
        };
        for loc in start {
            s.open_tab(loc.clone());
        }
        if s.tabs.is_empty() {
            s.open_tab(Location::welcome());
        }
        s.active = 0;
        s.sync();
        s
    }

    // -- inspection (tests and the binary's flags) --------------------------------

    pub fn tab_titles(&self) -> Vec<String> {
        self.tabs.iter().map(|t| self.title_of(t)).collect()
    }

    pub fn active_location(&self) -> Option<&Location> {
        self.tabs.get(self.active).and_then(TabState::loc)
    }

    pub fn scroll(&self) -> i32 {
        self.tabs.get(self.active).map_or(0, TabState::scroll)
    }

    pub fn address_text(&self) -> String {
        self.chrome
            .address()
            .display()
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect()
    }

    pub fn set_scroll(&mut self, y: i32) {
        self.scroll_to(y);
    }

    // -- state --------------------------------------------------------------------

    fn title_of(&self, t: &TabState) -> String {
        if t.doc.title.is_empty() {
            t.loc().map(Location::fallback_title).unwrap_or_default()
        } else {
            t.doc.title.clone()
        }
    }

    fn sync(&mut self) {
        let tabs: Vec<Tab> = self
            .tabs
            .iter()
            .map(|t| Tab::new(t.id, &self.title_of(t), false))
            .collect();
        let active = self.tabs.get(self.active).map(|t| t.id);
        self.chrome.set_tabs(tabs, active);
        if let Some(t) = self.tabs.get(self.active) {
            self.chrome.set_url(
                t.loc()
                    .map(Location::display)
                    .unwrap_or_else(|| Location::welcome().display()),
            );
            self.chrome
                .set_nav_state(t.pos > 0, t.pos + 1 < t.history.len(), false);
        }
    }

    fn open_tab(&mut self, loc: Location) {
        let id = TabId(self.next_id);
        self.next_id += 1;
        let mut t = TabState {
            id,
            history: vec![Entry {
                loc: loc.clone(),
                scroll: 0,
            }],
            pos: 0,
            doc: blank_doc(),
        };
        load_into(&mut t.doc, &loc);
        self.tabs.push(t);
        self.active = self.tabs.len() - 1;
    }

    fn navigate(&mut self, loc: Location) {
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        t.history.truncate(t.pos + 1);
        t.history.push(Entry {
            loc: loc.clone(),
            scroll: 0,
        });
        t.pos = t.history.len() - 1;
        load_into(&mut t.doc, &loc);
        self.hover = None;
        self.pressed = None;
        self.sync();
    }

    fn go_history(&mut self, delta: i32) {
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        let to = t.pos as i64 + i64::from(delta);
        if to < 0 || to as usize >= t.history.len() {
            return;
        }
        t.pos = to as usize;
        if let Some(loc) = t.loc().cloned() {
            load_into(&mut t.doc, &loc);
        }
        self.hover = None;
        self.sync();
    }

    fn apply(&mut self, r: AppRequest) {
        match r {
            AppRequest::NewTab => {
                self.open_tab(Location::welcome());
            }
            AppRequest::CloseTab(id) => {
                self.tabs.retain(|t| t.id != id);
                if self.tabs.is_empty() {
                    self.open_tab(Location::welcome());
                }
                self.active = self.active.min(self.tabs.len() - 1);
            }
            AppRequest::ActivateTab(id) => {
                if let Some(i) = self.tabs.iter().position(|t| t.id == id) {
                    self.active = i;
                }
            }
            AppRequest::MoveTab { tab, to } => {
                if let Some(from) = self.tabs.iter().position(|t| t.id == tab) {
                    let active_id = self.tabs.get(self.active).map(|t| t.id);
                    let t = self.tabs.remove(from);
                    self.tabs.insert(to.min(self.tabs.len()), t);
                    self.active = self
                        .tabs
                        .iter()
                        .position(|t| Some(t.id) == active_id)
                        .unwrap_or(0);
                }
            }
            AppRequest::Navigate(text) => match Location::parse(&text) {
                Ok(loc) => self.navigate(loc),
                Err(msg) => {
                    // Show the reason as a page in this tab's history.
                    if let Some(t) = self.tabs.get_mut(self.active) {
                        t.doc.source = nav::error_page(&msg);
                        t.doc.file = None;
                        t.doc.laid = None;
                        t.doc.title = String::new();
                    }
                }
            },
            AppRequest::Back => self.go_history(-1),
            AppRequest::Forward => self.go_history(1),
            AppRequest::Reload => {
                if let Some(t) = self.tabs.get_mut(self.active)
                    && let Some(loc) = t.loc().cloned()
                {
                    load_into(&mut t.doc, &loc);
                }
            }
            AppRequest::Stop | AppRequest::OpenMenu(_) => {}
        }
        self.sync();
    }

    // -- scrolling ----------------------------------------------------------------

    fn view(&self) -> Rect {
        let r = self.chrome.page_rect();
        Rect::new(r.x, r.y, r.w.max(1), r.h.max(1))
    }

    fn max_scroll(&self) -> i32 {
        let v = self.view();
        self.tabs
            .get(self.active)
            .and_then(|t| t.doc.laid.as_ref())
            .map_or(0, |l| l.max_scroll(v.h))
    }

    fn scroll_to(&mut self, y: i32) {
        self.ensure_layout();
        let max = self.max_scroll();
        if let Some(e) = self.tabs.get_mut(self.active).and_then(TabState::entry) {
            e.scroll = y.clamp(0, max.max(0));
        }
    }

    fn scroll_by(&mut self, dy: i32) {
        let now = self.scroll();
        self.scroll_to(now.saturating_add(dy));
    }

    // -- input --------------------------------------------------------------------

    pub fn handle(&mut self, window: &mut dyn PlatformWindow, ev: Event) {
        let before = (self.scroll(), self.active, self.hover.clone());
        let (reqs, page_owns) = pump(window, &mut self.chrome, &self.font, ev.clone());
        for r in reqs {
            self.apply(r);
        }
        if page_owns {
            self.page_event(window, &ev);
        }
        if matches!(ev, Event::Resize(_)) {
            let s = self.scroll();
            self.scroll_to(s);
        }
        if before != (self.scroll(), self.active, self.hover.clone()) {
            window.request_redraw();
        }
        // Navigation changes the document under an unchanged chrome.
        if matches!(ev, Event::Pointer(_) | Event::Key(_)) {
            window.request_redraw();
        }
    }

    fn page_event(&mut self, window: &mut dyn PlatformWindow, ev: &Event) {
        let page = self.view();
        let step = (page.h * 9 / 10).max(KEY_STEP);
        match ev {
            Event::Wheel { notches } => self.scroll_by(notches.saturating_mul(WHEEL_STEP)),
            Event::Key(k) if !k.mods.ctrl && !k.mods.alt && !k.mods.meta => match k.key {
                Key::Down => self.scroll_by(KEY_STEP),
                Key::Up => self.scroll_by(-KEY_STEP),
                Key::PageDown => self.scroll_by(step),
                Key::PageUp => self.scroll_by(-step),
                Key::Home => self.scroll_to(0),
                Key::End => self.scroll_to(i32::MAX),
                Key::Char(' ') => self.scroll_by(if k.mods.shift { -step } else { step }),
                _ => {}
            },
            Event::Pointer(p) => {
                let over = if self.chrome.is_page_point(p.pos) {
                    self.link_at(p.pos)
                } else {
                    None
                };
                let shape = if over.is_some() {
                    CursorShape::Pointer
                } else {
                    CursorShape::Default
                };
                if over != self.hover {
                    window.set_cursor(shape);
                    self.hover = over;
                }
                match p.kind {
                    PointerKind::Down(Button::Primary) => self.pressed = self.hover.clone(),
                    PointerKind::Up(Button::Primary) => {
                        let pressed = self.pressed.take();
                        if let (Some(href), Some(down)) = (self.hover.clone(), pressed)
                            && href == down
                            && let Some(loc) = self.active_location().and_then(|l| l.resolve(&href))
                        {
                            self.navigate(loc);
                            self.hover = None;
                            window.set_cursor(CursorShape::Default);
                        }
                    }
                    PointerKind::Leave => {
                        self.hover = None;
                        self.pressed = None;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    /// The href under a window point, if it is on a link in the page.
    fn link_at(&mut self, p: Point) -> Option<String> {
        self.ensure_layout();
        let v = self.view();
        let scroll = self.scroll();
        self.tabs
            .get(self.active)?
            .doc
            .laid
            .as_ref()?
            .link_at(p.x - v.x, p.y - v.y + scroll)
            .map(str::to_string)
    }

    // -- frames -------------------------------------------------------------------

    /// Lay the active page out for the current page area if it is not already.
    fn ensure_layout(&mut self) {
        let v = self.view();
        let (w, h) = (v.w as u32, v.h as u32);
        let Some(t) = self.tabs.get_mut(self.active) else {
            return;
        };
        if t.doc.laid.as_ref().is_some_and(|l| l.width == w) {
            return;
        }
        let laid = match page::layout(&t.doc.source, t.doc.file.as_deref(), w, h) {
            Ok(l) => l,
            Err(e) => {
                t.doc.source = nav::error_page(&e);
                t.doc.file = None;
                match page::layout(&t.doc.source, None, w, h) {
                    Ok(l) => l,
                    Err(_) => return,
                }
            }
        };
        t.doc.title = laid.title.clone();
        t.doc.laid = Some(laid);
        let s = t.scroll();
        self.scroll_to(s);
        self.sync();
    }

    /// The whole window: chrome, then the page blitted into its area.
    pub fn frame(&mut self) -> Option<Canvas> {
        self.ensure_layout();
        let s = self.chrome.size();
        let (w, h) = (
            u32::try_from(s.w).ok()?.max(1),
            u32::try_from(s.h).ok()?.max(1),
        );
        let mut list = px_paint::DisplayList::new();
        paint::paint(
            &self.chrome,
            &self.font,
            self.theme,
            Point::default(),
            &mut list,
        );
        let mut cv = px_paint::raster::rasterize_with(&list, w, h, &LayoutFonts).ok()?;
        let v = self.view();
        let laid = self.tabs.get(self.active)?.doc.laid.as_ref()?;
        if let Ok(pg) = laid.render(self.scroll(), v.w as u32, v.h as u32) {
            blit(&mut cv, &pg, v.x, v.y, self.chrome.config().window_radius);
        }
        Some(cv)
    }
}

fn load_into(doc: &mut Doc, loc: &Location) {
    let l = loc.load();
    doc.source = l.source;
    doc.file = l.file;
    doc.laid = None;
    doc.title = String::new();
}

/// Copy `src` into `dst` at (`x`, `y`), clipped to `dst`, leaving the chrome's own
/// pixels in the bottom corners of a rounded window (`radius` px, 0 for square).
fn blit(dst: &mut Canvas, src: &Canvas, x: i32, y: i32, radius: i32) {
    for row in 0..src.height as i32 {
        let dy = y + row;
        if dy < 0 || dy >= dst.height as i32 {
            continue;
        }
        let x0 = x.max(0);
        let x1 = (x + src.width as i32).min(dst.width as i32);
        if x0 >= x1 {
            continue;
        }
        let s = (row as usize * src.width as usize + (x0 - x) as usize) * 4;
        let d = (dy as usize * dst.width as usize + x0 as usize) * 4;
        let n = (x1 - x0) as usize * 4;
        let Some(from) = src.pixels.get(s..s + n) else {
            continue;
        };
        // Rows within `radius` of the window's bottom edge lose their corner pixels.
        let up = dst.height as i32 - 1 - dy;
        let inset = if radius > 0 && up < radius {
            let dyc = radius - 1 - up;
            let half =
                (i64::from(radius) * i64::from(radius) - i64::from(dyc) * i64::from(dyc)).max(0);
            radius - (half as f64).sqrt() as i32
        } else {
            0
        };
        let (a, b) = (x0.max(inset), x1.min(dst.width as i32 - inset));
        if a >= b {
            continue;
        }
        let (fa, ta) = (((a - x0) * 4) as usize, ((a - x0) * 4) as usize);
        let len = ((b - a) * 4) as usize;
        if let (Some(f), Some(t)) = (
            from.get(fa..fa + len),
            dst.pixels.get_mut(d + ta..d + ta + len),
        ) {
            t.copy_from_slice(f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_ui::input::{KeyEvent, PointerEvent};
    use px_ui::platform::PlatformError;

    #[derive(Default)]
    struct Mock {
        redraws: usize,
        cursor: Option<CursorShape>,
    }

    impl PlatformWindow for Mock {
        fn config(&self) -> PlatformConfig {
            PlatformConfig::desktop()
        }
        fn size(&self) -> Size {
            Size::new(900, 600)
        }
        fn present(&mut self, _: &Canvas, _: Rect) -> Result<(), PlatformError> {
            Ok(())
        }
        fn set_cursor(&mut self, s: CursorShape) {
            self.cursor = Some(s);
        }
        fn set_ime_allowed(&mut self, _: bool) {}
        fn set_ime_cursor_area(&mut self, _: Rect) {}
        fn clipboard_write(&mut self, _: &str) {}
        fn clipboard_request_read(&mut self) {}
        fn window_action(&mut self, _: px_ui::input::WindowAction) {}
        fn request_redraw(&mut self) {
            self.redraws += 1;
        }
    }

    fn shell(start: &[Location]) -> Shell {
        let mut s = Shell::new(
            Size::new(900, 600),
            PlatformConfig::desktop(),
            Theme::Light,
            start,
        );
        s.frame();
        s
    }

    fn click(s: &mut Shell, w: &mut Mock, x: i32, y: i32) {
        for kind in [
            PointerKind::Move,
            PointerKind::Down(Button::Primary),
            PointerKind::Up(Button::Primary),
        ] {
            s.handle(w, Event::Pointer(PointerEvent::new(x, y, kind)));
        }
    }

    fn tmp(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("px-shell-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn wheel_and_keys_scroll_and_clamp() {
        let mut s = shell(&[Location::About("long".into())]);
        let mut w = Mock::default();
        assert_eq!(s.scroll(), 0);
        s.handle(&mut w, Event::Wheel { notches: 2 });
        assert_eq!(s.scroll(), 96);
        s.handle(&mut w, Event::Wheel { notches: -10 });
        assert_eq!(s.scroll(), 0);
        s.handle(&mut w, Event::Key(KeyEvent::new(Key::End)));
        let max = s.scroll();
        assert!(max > 500);
        s.handle(&mut w, Event::Key(KeyEvent::new(Key::PageDown)));
        assert_eq!(s.scroll(), max);
        s.handle(&mut w, Event::Key(KeyEvent::new(Key::Home)));
        assert_eq!(s.scroll(), 0);
        s.handle(&mut w, Event::Key(KeyEvent::new(Key::Char(' '))));
        assert!(s.scroll() > 100);
        assert!(w.redraws > 0);
    }

    #[test]
    fn clicking_a_link_navigates_and_updates_title_and_address() {
        let b = tmp("b.html", "<!doctype html><title>Page B</title><p>b</p>");
        let a = tmp(
            "a.html",
            "<!doctype html><title>Page A</title><body style='margin:0'>\
             <p style='margin:0'><a href='b.html'>go to b go to b go to b</a></p>",
        );
        let mut s = shell(&[Location::File(a.clone())]);
        assert_eq!(s.tab_titles(), vec!["Page A".to_string()]);
        assert!(s.address_text().ends_with("a.html"));
        let mut w = Mock::default();
        let v = s.view();
        // A move over the link shows the pointer cursor.
        s.handle(
            &mut w,
            Event::Pointer(PointerEvent::new(v.x + 10, v.y + 8, PointerKind::Move)),
        );
        assert_eq!(w.cursor, Some(CursorShape::Pointer));
        click(&mut s, &mut w, v.x + 10, v.y + 8);
        s.frame();
        assert_eq!(s.active_location(), Some(&Location::File(b.clone())));
        assert_eq!(s.tab_titles(), vec!["Page B".to_string()]);
        assert!(s.address_text().ends_with("b.html"));
        // Back returns, with history.
        s.apply(AppRequest::Back);
        s.frame();
        assert_eq!(s.tab_titles(), vec!["Page A".to_string()]);
        // A click on empty space does nothing.
        click(&mut s, &mut w, v.x + 600, v.y + 400);
        assert_eq!(s.active_location(), Some(&Location::File(a)));
    }

    #[test]
    fn resize_relayouts_and_reclamps() {
        let mut s = shell(&[Location::About("long".into())]);
        let mut w = Mock::default();
        s.handle(&mut w, Event::Key(KeyEvent::new(Key::End)));
        let before = s.scroll();
        s.handle(&mut w, Event::Resize(Size::new(500, 900)));
        let cv = s.frame().unwrap();
        assert_eq!((cv.width, cv.height), (500, 900));
        assert!(s.scroll() <= s.max_scroll());
        assert_ne!(before, s.scroll());
    }

    #[test]
    fn a_frame_has_page_pixels_under_the_chrome() {
        let mut s = shell(&[Location::About("welcome".into())]);
        let cv = s.frame().unwrap();
        let v = s.view();
        // Somewhere in the page area there is ink that is not the chrome's.
        let mut inks = std::collections::HashSet::new();
        for y in (v.y..v.y + v.h).step_by(7) {
            for x in (v.x..v.x + v.w).step_by(7) {
                inks.insert(cv.pixel(x as u32, y as u32));
            }
        }
        assert!(inks.len() > 3);
    }

    #[test]
    fn network_addresses_are_refused_with_a_page() {
        let mut s = shell(&[Location::welcome()]);
        s.apply(AppRequest::Navigate("https://example.com/".into()));
        s.frame();
        assert_eq!(s.active_location(), Some(&Location::welcome()));
        assert!(
            s.tabs
                .first()
                .unwrap()
                .doc
                .source
                .contains("local files only")
        );
    }

    #[test]
    fn tabs_open_close_and_switch() {
        let mut s = shell(&[Location::welcome(), Location::About("flex".into())]);
        assert_eq!(s.tab_titles().len(), 2);
        s.apply(AppRequest::NewTab);
        assert_eq!(s.tab_titles().len(), 3);
        let id = s.tabs.first().unwrap().id;
        s.apply(AppRequest::ActivateTab(id));
        assert_eq!(s.active, 0);
        s.apply(AppRequest::CloseTab(id));
        assert_eq!(s.tab_titles().len(), 2);
    }
}
