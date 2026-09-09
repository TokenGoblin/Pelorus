#![no_main]

//! Fuzz the exact path a hostile content process drives: raw bytes in, a
//! decoded `Request` or an error out (§4.5).
//!
//! Framing and payload decoding are attacked together rather than separately,
//! because the interesting bugs live between them — a length prefix that
//! disagrees with the payload, a truncation at a type boundary, a prefix that
//! drives an allocation before it is checked.
//!
//! The only assertion is the one that matters: it must not panic, hang, or
//! allocate on a claim. Whether a given input decodes is not interesting.

use libfuzzer_sys::fuzz_target;
use px_ipc::Request;

fuzz_target!(|data: &[u8]| {
    let _ = px_ipc::decode_frame::<Request>(data);
});
