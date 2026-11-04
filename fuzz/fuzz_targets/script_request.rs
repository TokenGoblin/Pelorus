#![no_main]

//! The content process decoding what the broker relayed (Phase 10, §4.5).
//!
//! The source text is derived from a page, and this decoder is the first thing
//! in the content process that touches it. Anything that decodes must re-encode
//! to the same bytes; nothing may panic or allocate past the string cap.

use libfuzzer_sys::fuzz_target;
use px_ipc::script::ScriptRequest;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = px_ipc::decode_frame::<ScriptRequest>(data) {
        let again = px_ipc::encode_frame(&message).expect("a decoded message re-encodes");
        assert_eq!(again, data, "decoding is not injective");
    }
});
