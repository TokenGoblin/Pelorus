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
pub mod emit;
pub mod ir;
pub mod lexer;
pub mod parser;
pub mod resolve;
pub mod span;

use resolve::{BuildOptions, Model};

/// Parse, resolve, lower and run `emitters` over `sources`
/// (`(file name, text)` pairs). Errors are rendered `file:line:col: ...`.
pub fn generate(
    sources: &[(String, String)],
    opts: BuildOptions,
    emitters: &[&dyn emit::Emitter],
) -> Result<Vec<emit::OutputFile>, Vec<String>> {
    let model = Model::from_sources(sources, opts)?;
    let bindings = ir::lower(&model).map_err(|e| vec![e.to_string()])?;
    let mut out = Vec::new();
    for e in emitters {
        out.extend(e.emit(&bindings).map_err(|x| vec![x.to_string()])?);
    }
    Ok(out)
}
