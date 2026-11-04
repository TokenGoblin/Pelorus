#![no_main]

//! Arbitrary IDL text through the whole generator front end (Phase 11).
//!
//! The IDL files are first-party, but the parser sits in the build chain, so
//! it is held to the same bar as a decoder: whatever the bytes, parse,
//! resolve, lower and emit must return `Ok` or `Err` and never panic or hang.
//! The input is split at the first NUL into two files, so cross-file name
//! resolution, partials and mixins are reachable from a flat byte string.

use libfuzzer_sys::fuzz_target;
use px_bindings::emit::{Emitter, RustHostEmitter};
use px_bindings::resolve::BuildOptions;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut parts = text.splitn(2, '\0');
    let first = parts.next().unwrap_or("");
    let mut sources = vec![("a.idl".to_owned(), first.to_owned())];
    if let Some(second) = parts.next() {
        sources.push(("b.idl".to_owned(), second.to_owned()));
    }
    let emitters: [&dyn Emitter; 1] = [&RustHostEmitter];
    for require_exposed in [false, true] {
        let _ = px_bindings::generate(&sources, BuildOptions { require_exposed }, &emitters);
    }
});
