#![forbid(unsafe_code)]

//! Content blocking and tracker mitigations (Phase 19).
//!
//! See the module docs; the crate-level documentation is completed with the
//! matcher.

pub mod filter;
pub mod glob;
pub mod matcher;
