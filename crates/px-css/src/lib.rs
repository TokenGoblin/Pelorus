//! Stylo integration, cascade, computed values.
//!
//! # Why this crate has no `#![forbid(unsafe_code)]`
//!
//! ADR 024. `stylo`'s `TElement` declares five required `unsafe fn` methods, and
//! `forbid(unsafe_code)` rejects *implementing* an unsafe method — not merely
//! writing an unsafe block. The ADR predicted this from a compiled probe; the
//! real impl produced exactly the predicted diagnostic, five times:
//!
//! ```text
//! error: implementation of an `unsafe` method
//! note: the lint level is defined here
//! 1 | #![forbid(unsafe_code)]
//! ```
//!
//! The rule that replaces the lint is **narrower**, not weaker:
//!
//! > `px-css` may declare `unsafe fn` where a `stylo` trait signature requires
//! > it. It contains **zero** `unsafe` blocks and **zero** `unsafe impl`.
//!
//! That holds, and holds comfortably. The body of an `unsafe fn` needs no
//! `unsafe` block, and every one of those five bodies is a `Cell` write through
//! [`data::StyleData`]. `Send` and `Sync` are never asserted. The one place a
//! transmute would have been needed — turning html5ever's `Atom` into stylo's
//! `GenericAtomIdent` — is covered by stylo's own safe `cast`, so the unsafe
//! stays in the crate that reviewed it.
//!
//! `ci/gate-unsafe-headers.sh` and `ci/gate-style.sh` both enforce the two
//! zero-counts, and both were verified to fail on a deliberately added unsafe
//! block before this line was removed.
//!
//! Phase 0 skeleton: no implementation. Phase 5 fills this in;
//! see docs/build-spec.md §9 and this crate's CLAUDE.md.

pub mod data;
pub mod dom;
pub mod element;
pub mod view;
