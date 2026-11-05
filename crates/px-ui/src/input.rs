//! What the platform tells the chrome, and what the chrome asks for back.
//!
//! Everything here is plain data. The chrome never calls the platform; it returns
//! [`Effect`]s and the platform layer (ADR 044, undecided) carries them out. That
//! is the whole seam: events in, effects out, a `Canvas` for the window to show.

use crate::geom::{Point, Rect, Size};
pub use crate::tabs::TabId;

/// A logical key. The platform maps layout-dependent keys to this; the physical
/// scancode is not the chrome's business (the page gets that).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// A character key, lower-cased by the platform (shift is in [`Modifiers`]).
    Char(char),
    Enter,
    Escape,
    Backspace,
    Delete,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// `F(5)` is F5.
    F(u8),
}

/// Held modifiers, as physical keys. Which one is the "primary" shortcut
/// modifier differs per OS; see [`PrimaryModifier`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
}

/// Which modifier carries shortcuts such as "new tab": Control on Windows and
/// Linux, Command (meta) on macOS. Supplied by the platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrimaryModifier {
    Ctrl,
    Meta,
}

impl Modifiers {
    pub const NONE: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: false,
        meta: false,
    };

    pub fn primary(&self, which: PrimaryModifier) -> bool {
        match which {
            PrimaryModifier::Ctrl => self.ctrl,
            PrimaryModifier::Meta => self.meta,
        }
    }
}

/// A key press or auto-repeat. Releases are not delivered to the chrome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: Key,
    pub mods: Modifiers,
}

impl KeyEvent {
    pub fn new(key: Key) -> KeyEvent {
        KeyEvent {
            key,
            mods: Modifiers::NONE,
        }
    }

    pub fn with(key: Key, mods: Modifiers) -> KeyEvent {
        KeyEvent { key, mods }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Primary,
    Secondary,
    Middle,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerKind {
    Move,
    Down(Button),
    Up(Button),
    /// The pointer left the window.
    Leave,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PointerEvent {
    pub pos: Point,
    pub kind: PointerKind,
    pub mods: Modifiers,
}

impl PointerEvent {
    pub fn new(x: i32, y: i32, kind: PointerKind) -> PointerEvent {
        PointerEvent {
            pos: Point { x, y },
            kind,
            mods: Modifiers::NONE,
        }
    }
}

/// An input-method composition in progress (not yet committed text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Preedit {
    pub text: String,
}

/// Everything the platform can tell the chrome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    Key(KeyEvent),
    /// Committed text (typed characters, or an IME commit), UTF-8.
    Text(String),
    /// The IME's composition changed; `None` ends it without committing.
    Preedit(Option<Preedit>),
    Pointer(PointerEvent),
    /// The answer to [`PlatformEffect::ClipboardRead`].
    Clipboard(String),
    /// Window gained (`true`) or lost (`false`) keyboard focus.
    WindowFocus(bool),
    /// Client area resized, logical pixels.
    Resize(Size),
}

/// What the chrome did with an event. An unconsumed event belongs to the page
/// (the broker decides which frame, never the page: invariant 1).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub consumed: bool,
    pub effects: Vec<Effect>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Default,
    Text,
    Pointer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowAction {
    Close,
    Minimize,
    Maximize,
}

/// Requests for the platform layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlatformEffect {
    /// Something changed; draw a new frame.
    Redraw,
    SetCursor(CursorShape),
    SetImeAllowed(bool),
    /// Caret rectangle, window coordinates, for IME candidate placement.
    SetImeCursorArea(Rect),
    ClipboardWrite(String),
    /// Asynchronous: the answer arrives as [`Event::Clipboard`].
    ClipboardRead,
    Window(WindowAction),
}

/// Requests for the browser: the chrome states what the user asked for; it does
/// not navigate, close or open anything itself, and it never sends an identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppRequest {
    /// Raw text the user submitted in the address field. The browser decides
    /// whether it is a URL or a search; the chrome does not guess.
    Navigate(String),
    Back,
    Forward,
    Reload,
    Stop,
    NewTab,
    CloseTab(TabId),
    ActivateTab(TabId),
    /// The strip already shows the new order; the browser should record it.
    MoveTab { tab: TabId, to: usize },
    /// Open the overflow menu, anchored at this rectangle.
    OpenMenu(Rect),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Platform(PlatformEffect),
    App(AppRequest),
}
