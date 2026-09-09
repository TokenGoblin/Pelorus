#![deny(unsafe_op_in_unsafe_fn)]

//! Process sandboxing; the only crate permitted unsafe.
//!
//! The one crate permitted to use unsafe. Every block carries a
//! SAFETY comment justified against the OS documentation.
//!
//! Phase 0 skeleton: no implementation. Phase 2 fills this in;
//! see docs/build-spec.md §9 and this crate's CLAUDE.md.
