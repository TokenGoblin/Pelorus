#![no_main]

//! §4.1's fuzz target, named there explicitly: *"hold stale IDs across
//! mutations, assert every lookup is `None`."*
//!
//! The body lives in `px_dom::harness` so that `cargo test` runs it too, on
//! both operating systems, on every push. Phase 1 shipped a 24-hour campaign
//! that reported clean while never reaching the code it was built for; a
//! harness only ever executed by the campaign is one nobody watches fail.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    px_dom::harness::stale_handles(data);
});
