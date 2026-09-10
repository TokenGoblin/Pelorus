#![no_main]

//! Fuzz the response head parser: raw server bytes in, a head or an error out
//! (§4.5, §9 Phase 3).
//!
//! Every byte here was chosen by whoever served the page, so this is the same
//! shape of target as the IPC deserializers — a trust boundary driven directly
//! rather than through a helper that might sand the edges off.
//!
//! The status line, the header block and the framing decision are attacked
//! together, because the interesting bugs live between them: a `Content-Length`
//! that parses but disagrees with what an upstream read, a header whose value
//! carries the terminator that ends it, a length that sizes an allocation
//! before it is bounded.
//!
//! The only assertion is the one that matters: it must not panic, hang, or
//! allocate on a claim. Whether a given input parses is not interesting — a
//! parser that rejected everything would pass this, which is why the unit
//! tests assert the accepting direction and this target does not.

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok((head, consumed)) = px_net::http1::parse_response_head(data) {
        // The offset is what the caller uses to find the body. An offset past
        // the input would have it read bytes nobody sent.
        assert!(
            consumed <= data.len(),
            "parser consumed {consumed} of {} bytes",
            data.len()
        );

        // A declared length must already be bounded. This is the property that
        // stops four bytes on the wire buying an allocation, and it is checked
        // here rather than trusted because the check lives in one branch of a
        // function with several.
        if let px_net::http1::Framing::Length(length) = head.framing {
            assert!(
                length <= px_net::http1::MAX_BODY_BYTES,
                "accepted an unbounded content-length: {length}"
            );
        }
    }
});
