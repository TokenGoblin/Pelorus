//! The seam for the window layer (ADR 044, proposed; nothing here depends on it).
//!
//! Whichever stack the user picks (hand-written Win32 + X11, or winit) implements
//! [`PlatformWindow`] and runs the loop in [`pump`]. The chrome never learns what
//! is behind the trait. The surface follows the list in
//! `docs/research/phase-18-window-stack.md` section 6; adding a method later
//! changes every backend, so the ones the research says to include from the start
//! (IME cursor area, clipboard round trip, damage on present) are here already.

use px_paint::Canvas;

use crate::chrome::{Chrome, PlatformConfig};
use crate::geom::{Rect, Size};
use crate::input::{AppRequest, CursorShape, Effect, Event, Outcome, PlatformEffect, WindowAction};
use crate::paint;
use crate::text::TextShaper;
use crate::theme::Theme;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// The display connection is gone or refused the frame.
    Present(String),
}

/// What the chrome needs from the OS window. All calls are from one thread.
pub trait PlatformWindow {
    /// Shortcut modifier, controls placement, corner radius.
    fn config(&self) -> PlatformConfig;
    /// Client-area size in logical pixels.
    fn size(&self) -> Size;
    /// Show `frame` (window-sized RGBA) with `damage` as the changed region.
    fn present(&mut self, frame: &Canvas, damage: Rect) -> Result<(), PlatformError>;
    fn set_cursor(&mut self, shape: CursorShape);
    fn set_ime_allowed(&mut self, allowed: bool);
    /// Caret rectangle in window coordinates.
    fn set_ime_cursor_area(&mut self, area: Rect);
    fn clipboard_write(&mut self, text: &str);
    /// Asks for the clipboard; the answer comes back as [`Event::Clipboard`].
    fn clipboard_request_read(&mut self);
    fn window_action(&mut self, action: WindowAction);
    fn request_redraw(&mut self);
}

/// Carry out the platform half of `effects`; return the app half for the browser.
pub fn apply(window: &mut dyn PlatformWindow, effects: Vec<Effect>) -> Vec<AppRequest> {
    let mut app = Vec::new();
    for e in effects {
        match e {
            Effect::App(r) => app.push(r),
            Effect::Platform(p) => match p {
                PlatformEffect::Redraw => window.request_redraw(),
                PlatformEffect::SetCursor(s) => window.set_cursor(s),
                PlatformEffect::SetImeAllowed(b) => window.set_ime_allowed(b),
                PlatformEffect::SetImeCursorArea(r) => window.set_ime_cursor_area(r),
                PlatformEffect::ClipboardWrite(t) => window.clipboard_write(&t),
                PlatformEffect::ClipboardRead => window.clipboard_request_read(),
                PlatformEffect::Window(a) => window.window_action(a),
            },
        }
    }
    app
}

/// One input event through the chrome and out to the window. Returns the
/// browser's share and whether the event is the page's.
pub fn pump(
    window: &mut dyn PlatformWindow,
    chrome: &mut Chrome,
    shaper: &dyn TextShaper,
    event: Event,
) -> (Vec<AppRequest>, bool) {
    let Outcome { consumed, effects } = chrome.event(event, shaper);
    (apply(window, effects), !consumed)
}

/// Rasterize the chrome for `present`. The page surface is the compositor's
/// business and goes into [`Chrome::page_rect`] afterwards, clipped to it.
pub fn render_frame(
    chrome: &Chrome,
    shaper: &dyn TextShaper,
    theme: Theme,
) -> Result<Canvas, px_paint::raster::RasterError> {
    let mut list = px_paint::DisplayList::new();
    paint::paint(
        chrome,
        shaper,
        theme,
        crate::geom::Point::default(),
        &mut list,
    );
    let s = chrome.size();
    px_paint::raster::rasterize_with(&list, s.w.max(0) as u32, s.h.max(0) as u32, &NoGlyphs)
}

/// Glyph outlines are the embedder's too; `render_frame` without fonts draws
/// text as nothing. Embedders call `paint::paint` and rasterize with their own
/// `GlyphSource` instead.
struct NoGlyphs;

impl px_paint::raster::GlyphSource for NoGlyphs {
    fn units_per_em(&self, _: px_paint::FontId) -> Option<u32> {
        None
    }
    fn outline(&self, _: px_paint::FontId, _: u32) -> Option<Vec<px_paint::glyph::PathCmd>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{Key, KeyEvent, Modifiers};
    use crate::text::testing::Mono;

    #[derive(Default)]
    struct Recording {
        calls: Vec<String>,
    }

    impl PlatformWindow for Recording {
        fn config(&self) -> PlatformConfig {
            PlatformConfig::desktop()
        }
        fn size(&self) -> Size {
            Size::new(920, 568)
        }
        fn present(&mut self, _: &Canvas, d: Rect) -> Result<(), PlatformError> {
            self.calls.push(format!("present {d:?}"));
            Ok(())
        }
        fn set_cursor(&mut self, s: CursorShape) {
            self.calls.push(format!("cursor {s:?}"));
        }
        fn set_ime_allowed(&mut self, b: bool) {
            self.calls.push(format!("ime {b}"));
        }
        fn set_ime_cursor_area(&mut self, _: Rect) {
            self.calls.push("ime-area".into());
        }
        fn clipboard_write(&mut self, t: &str) {
            self.calls.push(format!("clip-write {t}"));
        }
        fn clipboard_request_read(&mut self) {
            self.calls.push("clip-read".into());
        }
        fn window_action(&mut self, a: WindowAction) {
            self.calls.push(format!("window {a:?}"));
        }
        fn request_redraw(&mut self) {
            self.calls.push("redraw".into());
        }
    }

    #[test]
    fn a_mock_window_drives_the_whole_loop() {
        let mut w = Recording::default();
        let mut chrome = Chrome::new(w.config(), w.size());
        let m = Mono;
        let ctrl = |k| {
            Event::Key(KeyEvent::with(
                k,
                Modifiers {
                    ctrl: true,
                    ..Modifiers::NONE
                },
            ))
        };
        let (app, page) = pump(&mut w, &mut chrome, &m, ctrl(Key::Char('l')));
        assert!(app.is_empty() && !page);
        assert!(w.calls.contains(&"ime true".to_string()));
        assert!(w.calls.contains(&"redraw".to_string()));
        pump(&mut w, &mut chrome, &m, Event::Text("a b".into()));
        pump(&mut w, &mut chrome, &m, ctrl(Key::Char('a')));
        pump(&mut w, &mut chrome, &m, ctrl(Key::Char('c')));
        assert!(w.calls.contains(&"clip-write a b".to_string()));
        pump(&mut w, &mut chrome, &m, ctrl(Key::Char('v')));
        assert!(w.calls.contains(&"clip-read".to_string()));
        pump(&mut w, &mut chrome, &m, Event::Clipboard("x\ny".into()));
        let (app, _) = pump(
            &mut w,
            &mut chrome,
            &m,
            Event::Key(KeyEvent::new(Key::Enter)),
        );
        assert_eq!(app, vec![AppRequest::Navigate("xy".into())]);
        // An unconsumed key is reported as the page's.
        let (_, page) = pump(
            &mut w,
            &mut chrome,
            &m,
            Event::Key(KeyEvent::new(Key::Char('z'))),
        );
        assert!(page);
    }

    #[test]
    fn render_frame_without_fonts_is_the_window_size_and_deterministic() {
        let chrome = Chrome::new(PlatformConfig::desktop(), Size::new(320, 200));
        let a = render_frame(&chrome, &Mono, Theme::Dark).unwrap();
        let b = render_frame(&chrome, &Mono, Theme::Dark).unwrap();
        assert_eq!((a.width, a.height), (320, 200));
        assert_eq!(a, b);
        assert!(
            render_frame(
                &Chrome::new(PlatformConfig::desktop(), Size::new(0, 0)),
                &Mono,
                Theme::Dark
            )
            .is_err()
        );
    }
}
