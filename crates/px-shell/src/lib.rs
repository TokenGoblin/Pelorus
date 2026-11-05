#![forbid(unsafe_code)]

//! The interim browser shell (Phase 18): the `px-ui` chrome in a `px-display`
//! window with a real page, rendered by our own engine, in the content area.
//!
//! Everything runs in one process. That is a stand-in: the process split (content
//! process, broker-owned window and files, frames over IPC) is Phase 19
//! integration. See this crate's CLAUDE.md.

pub mod decode;
pub mod fonts;
pub mod images;
pub mod nav;
pub mod page;
pub mod shell;
pub mod web;

pub use nav::Location;
pub use shell::Shell;
