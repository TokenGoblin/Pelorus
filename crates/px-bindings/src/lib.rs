#![forbid(unsafe_code)]

//! WebIDL parser, semantic model and code-generator front end (Phase 11).
//!
//! Pipeline: `parser::parse` (text to `ast`), `resolve::Model::build`
//! (merge, check, resolve), `ir::lower` (engine-neutral glue description),
//! `emit::Emitter` implementations (Rust source text).
//!
//! Std only, no unsafe, no panics on hostile input: every limit is a
//! constant in the module that enforces it.

pub mod ast;
pub mod lexer;
pub mod parser;
pub mod span;
