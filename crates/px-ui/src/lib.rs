#![forbid(unsafe_code)]

//! Browser chrome, the part that does not depend on a window (Phase 18).
//!
//! Tab strip, toolbar and address bar as pure state machines, a layout in
//! logical pixels, and a painter that turns the lot into a `px-paint` display
//! list for the deterministic software rasterizer. Nothing here opens a window,
//! reads a clock, touches the filesystem or the network, or uses `unsafe`: the
//! platform (ADR 044, implemented by `px-display`) feeds [`input::Event`]s in, carries
//! [`input::Effect`]s out, and shows the `Canvas`. [`platform`] is that seam.
//!
//! Invariants from this crate's CLAUDE.md that the code enforces: strings come
//! from `px-brand`; no page-controlled pixel (a favicon is never drawn, titles
//! are sanitised text); the origin is shown with the registrable domain
//! emphasised; no padlock.

pub mod address;
pub mod chrome;
pub mod geom;
pub mod input;
pub mod layout;
pub mod paint;
pub mod platform;
pub mod shape;
pub mod shortcuts;
pub mod tabs;
pub mod text;
pub mod theme;
pub mod toolbar;

pub use chrome::{Chrome, ControlDots, PlatformConfig, Stop};
pub use input::{Effect, Event, Outcome};
