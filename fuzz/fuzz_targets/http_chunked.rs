#![no_main]

//! Fuzz the chunked body decoder (§4.5, §9 Phase 3).
//!
//! A separate target from `http_response` rather than a mode of it, because
//! the decoder is a loop with its own state — a size, an offset, an
//! accumulating buffer — and reaching it through a valid head would spend most
//! of the fuzzer's budget generating well-formed headers instead of attacking
//! the loop.
//!
//! What this is looking for, specifically:
//!
//! - a declared chunk size that outruns the buffer and is read anyway;
//! - a size whose arithmetic overflows on the way to an offset;
//! - a chain of chunks that individually pass their bound and collectively do
//!   not, which is the shape a body-size limit is most often missing;
//! - a trailer section that never terminates.
//!
//! As with the other targets the only assertion is that it must not panic,
//! hang, or allocate on a claim.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((body, consumed)) = px_net::http1::decode_chunked(data) {
        assert!(
            consumed <= data.len(),
            "decoder consumed {consumed} of {} bytes",
            data.len()
        );
        // The per-chunk bound is easy to get right and the cumulative one is
        // easy to forget, so the cumulative one is what is asserted.
        assert!(
            body.len() <= px_net::http1::MAX_BODY_BYTES,
            "accumulated {} bytes past the body limit",
            body.len()
        );
    }
});
