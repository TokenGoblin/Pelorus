#![forbid(unsafe_code)]

//! The GPU process boundary (Phase 8, ADR 033 and ADR 034).
//!
//! - [`rasterizer`]: the backend trait and the software backend.
//! - [`wire`]: the display list across the boundary, and a frame's strips back.
//! - [`server`]: the GPU process's request loop (the `px-gpu` binary wraps it).
//! - [`session`]: [`session::GpuSession`], the supervisor that owns the process,
//!   verifies its confinement, and falls back to software on any failure.
//!
//! The rasterizer inside the GPU process is the software one for now. Everything
//! that makes a separate process worth having (its own address space, a profile
//! that denies it files and sockets, a supervisor that survives its death) is
//! independent of which backend runs in it, and is what is built and tested here.

pub mod rasterizer;
pub mod server;
pub mod session;
pub mod wire;

#[cfg(feature = "testing")]
mod faults;

pub use rasterizer::{FailingRasterizer, Rasterizer, RenderError, SoftwareRasterizer};
pub use session::{Backend, Cause, GpuConfig, GpuEvent, GpuSession};
