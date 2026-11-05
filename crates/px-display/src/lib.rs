#![deny(unsafe_op_in_unsafe_fn)]

//! The window layer (ADR 044, accepted 2026-11-05): hand-written Win32 and X11
//! behind `px_ui::platform::PlatformWindow`.
//!
//! - [`x11`] (Unix): the X11 core protocol over a `UnixStream`, in safe Rust.
//!   No dependency, no `unsafe`, no C library. Pure-Wayland sessions run
//!   through XWayland; there is no native Wayland.
//! - `win32` (Windows only): `windows-sys` with a `// SAFETY:` comment on every
//!   unsafe block. This is the only place in the crate with `unsafe`.
//!
//! Both backends implement [`Display`]: the chrome's `PlatformWindow` plus a way
//! to wait for [`DisplayEvent`]s. [`run`] is the loop every embedder shares.
//!
//! Who may link this: the browser binary only. A content, GPU or network
//! process never holds a display connection (invariant 1); `cargo tree -p
//! px-content` must never list this crate.

use std::fmt;
use std::time::Duration;

use px_ui::geom::Rect;
use px_ui::input::Event;
use px_ui::platform::PlatformWindow;

pub mod shape;
pub use shape::{Mask, WindowKind, WindowShape};

#[cfg(unix)]
pub mod x11;

#[cfg(windows)]
pub mod win32;

/// Why the window layer stopped working. Every variant is terminal for the
/// window: the caller closes it (fail closed).
#[derive(Debug)]
pub enum DisplayError {
    /// No usable display: bad `DISPLAY`, refused authorisation, unsupported
    /// visual. The message says why.
    Connect(String),
    /// The server sent bytes the decoder will not accept.
    Protocol(String),
    /// The connection closed.
    Closed,
    Io(std::io::Error),
}

impl fmt::Display for DisplayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplayError::Connect(m) => write!(f, "cannot open a display: {m}"),
            DisplayError::Protocol(m) => write!(f, "display protocol error: {m}"),
            DisplayError::Closed => f.write_str("the display connection closed"),
            DisplayError::Io(e) => write!(f, "display i/o error: {e}"),
        }
    }
}

impl std::error::Error for DisplayError {}

impl From<std::io::Error> for DisplayError {
    fn from(e: std::io::Error) -> DisplayError {
        DisplayError::Io(e)
    }
}

/// What a window reports besides chrome input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DisplayEvent {
    /// Input for `px_ui::platform::pump`.
    Ui(Event),
    /// The OS wants this region (window coordinates) painted again.
    Redraw(Rect),
    /// The user asked to close the window.
    Close,
}

/// Window options. Sizes are logical pixels.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub title: String,
    pub width: u32,
    pub height: u32,
    /// Ask the window manager for no decorations. The chrome draws its own
    /// controls, but the seam has no "begin move/resize" request yet, so a
    /// frameless window cannot be dragged (ADR 044 follow-up). A shaped window
    /// is always frameless, whatever this says.
    pub frameless: bool,
    /// What the window is for (main, dialog, popup, menu, tooltip).
    pub kind: WindowKind,
    /// The window outline. `Some` for every window by default: all top-level
    /// windows are squircles (ui-direction.md). `None` only for tests that read
    /// raw pixels and for a platform that cannot shape.
    pub shape: Option<WindowShape>,
    /// Screen position of the window's top-left corner, for popups, menus and
    /// tooltips; `None` lets the window manager place it.
    pub position: Option<(i32, i32)>,
}

impl WindowOptions {
    /// A squircle window of the main kind.
    pub fn new(title: &str, width: u32, height: u32) -> WindowOptions {
        WindowOptions::of_kind(WindowKind::Main, title, width, height)
    }

    /// A window of `kind` with that kind's squircle radius.
    pub fn of_kind(kind: WindowKind, title: &str, width: u32, height: u32) -> WindowOptions {
        WindowOptions {
            title: title.to_string(),
            width,
            height,
            frameless: false,
            kind,
            shape: Some(WindowShape::for_kind(kind)),
            position: None,
        }
    }

    /// A popup, menu or tooltip at screen position `(x, y)`.
    pub fn transient(kind: WindowKind, width: u32, height: u32, x: i32, y: i32) -> WindowOptions {
        let mut o = WindowOptions::of_kind(kind, "", width, height);
        o.frameless = true;
        o.position = Some((x, y));
        o
    }

    /// A plain rectangular window (no shape). Tests and unsupported platforms.
    pub fn square(mut self) -> WindowOptions {
        self.shape = None;
        self
    }
}

/// A window the chrome can draw into and an embedder can wait on.
pub trait Display: PlatformWindow {
    /// Wait up to `timeout` (forever for `None`) for events. Returns early with
    /// whatever has arrived; an empty vector means the wait timed out.
    fn wait_events(&mut self, timeout: Option<Duration>)
    -> Result<Vec<DisplayEvent>, DisplayError>;
}

/// The shared loop. `frame` is called whenever the window needs pixels and
/// returns the new frame; `input` is called for each chrome event. Returns when
/// the window is closed, or at `stop_at` if given (tools and tests).
pub fn run<D: Display>(
    window: &mut D,
    stop_at: Option<std::time::Instant>,
    mut input: impl FnMut(&mut D, Event),
    mut frame: impl FnMut(&mut D) -> Option<px_paint::raster::Canvas>,
) -> Result<(), DisplayError> {
    let mut dirty = Some(full(window));
    loop {
        if let Some(d) = dirty.take()
            && let Some(canvas) = frame(window)
        {
            window
                .present(&canvas, d)
                .map_err(|e| DisplayError::Protocol(format!("{e:?}")))?;
        }
        let until_stop = stop_at.map(|t| t.saturating_duration_since(std::time::Instant::now()));
        if until_stop.is_some_and(|d| d.is_zero()) {
            return Ok(());
        }
        let timeout = if dirty.is_some() {
            Some(Duration::ZERO)
        } else {
            until_stop
        };
        for ev in window.wait_events(timeout)? {
            match ev {
                DisplayEvent::Close => return Ok(()),
                DisplayEvent::Redraw(r) => dirty = Some(union(dirty, r)),
                DisplayEvent::Ui(e) => {
                    if let Event::Resize(_) = e {
                        dirty = Some(full(window));
                    }
                    // The chrome asks for a repaint itself (`PlatformEffect::Redraw`).
                    input(window, e);
                }
            }
        }
    }
}

fn full(w: &impl PlatformWindow) -> Rect {
    let s = w.size();
    Rect::new(0, 0, s.w, s.h)
}

fn union(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => {
            let x = a.x.min(b.x);
            let y = a.y.min(b.y);
            let r = a.right().max(b.right());
            let bt = a.bottom().max(b.bottom());
            Rect::new(x, y, r - x, bt - y)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_covers_both() {
        let r = union(Some(Rect::new(0, 0, 10, 10)), Rect::new(20, 5, 5, 5));
        assert_eq!(r, Rect::new(0, 0, 25, 10));
        assert_eq!(union(None, Rect::new(1, 2, 3, 4)), Rect::new(1, 2, 3, 4));
    }
}
