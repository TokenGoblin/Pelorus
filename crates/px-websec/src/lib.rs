#![forbid(unsafe_code)]

//! SOP, CORS, CSP, mixed content, referrer, COOP/COEP.
//!
//! Phase 12. Each policy is a separate module of pure decision functions; the
//! integration (px-net, px-content) supplies facts and acts on the verdicts.
//! Every module lists its `ENFORCEMENT_POINTS`; see `origin::trace`.
//! See docs/build-spec.md §9 and this crate's CLAUDE.md.

pub mod origin;
pub mod cors;
pub mod mixed;
pub mod referrer;
