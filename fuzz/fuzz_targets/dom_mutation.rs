#![no_main]

//! §9 Phase 4's "24h mutation fuzz clean".
//!
//! Checks that the tree is still a tree after any sequence of mutations --
//! acyclic, links agreeing in both directions, depth held, nothing leaked or
//! double-freed. "No crash" is much weaker: the arena is safe Rust and will
//! not crash, it will just silently hold a corrupt tree.
//!
//! The body lives in `px_dom::harness` for the reason given in
//! `dom_stale_handle.rs`.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    px_dom::harness::mutations(data);
});
