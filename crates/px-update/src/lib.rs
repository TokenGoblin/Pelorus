#![forbid(unsafe_code)]

//! Signed manifest fetch and data-file updates (Phase 20).
//!
//! std only, no I/O in this crate yet: the network is reached through the
//! `Transport` trait that px-net will implement. See docs/adr/041 and this
//! crate's CLAUDE.md.

pub mod ed25519;
pub mod hex;
pub mod manifest;
pub mod sha2;
pub mod version;
