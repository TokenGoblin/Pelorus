#![no_main]

//! §4.4: "Fuzz corpus includes 100,000-level nesting for each parser."
//!
//! `dom_stale_handle` and `dom_mutation` drive the arena's API with operation
//! bytes. Neither sends a byte of HTML through html5ever and the `TreeSink` --
//! which is the surface an attacker actually reaches, because a page is bytes
//! rather than a sequence of `append_child` calls.
//!
//! The body lives in `px_dom::harness` so `cargo test` runs it too. See
//! `dom_stale_handle.rs` for why.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    px_dom::harness::parse_html(data);
});
