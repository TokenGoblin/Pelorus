#![no_main]

//! The other direction (§4.5).
//!
//! A content process decoding a `Response` matters less than the broker
//! decoding a `Request` — the broker is the privileged side — but it is not
//! nothing: a compromised broker is game over anyway, while a *buggy* one that
//! emits a malformed response must not be able to crash a content process in a
//! way an attacker can steer.

use libfuzzer_sys::fuzz_target;
use px_ipc::Response;

fuzz_target!(|data: &[u8]| {
    let _ = px_ipc::decode_frame::<Response>(data);
});
