//! The X11 backend: the core protocol, hand-written, in safe Rust.
//!
//! What it does: one top-level window, `PutImage` presentation with damage (in
//! row bands, or whole with BIG-REQUESTS), keys and text from the server's
//! keyboard table, pointer, resize, close, font cursors, minimise/maximise via
//! EWMH, and the CLIPBOARD selection in both directions.
//!
//! What it leaves out, and why that is acceptable for Phase 18 (ADR 044):
//! MIT-SHM (an optimisation), XKB (groups, dead keys), RANDR/Xft.dpi scale (the
//! scale is 1; XWayland renders integer-scaled), INCR clipboard transfers
//! (payloads over the request limit are refused), drag and drop, input methods
//! (XIM or an IBus/Fcitx D-Bus client is its own ADR), and the wheel (the chrome
//! has no scroll event yet).
//!
//! X11 has no isolation between clients: anything on the display can send this
//! window input or read its pixels. Nothing here can change that.

pub mod conn;
pub mod discover;
pub mod keys;
pub mod proto;
mod window;

pub use window::{ShapeMode, X11Window};
