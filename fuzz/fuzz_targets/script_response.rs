#![no_main]

//! The broker decoding what a content process wrote (Phase 10, §4.5).
//!
//! The content process runs arbitrary script and is expected to be compromised
//! some day, and the result string is produced by that script, so this is the
//! privileged side decoding attacker-influenced bytes. Anything that decodes
//! must re-encode to the same bytes; nothing may panic or allocate past the
//! per-field caps.

use libfuzzer_sys::fuzz_target;
use px_ipc::script::ScriptResponse;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = px_ipc::decode_frame::<ScriptResponse>(data) {
        let again = px_ipc::encode_frame(&message).expect("a decoded message re-encodes");
        assert_eq!(again, data, "decoding is not injective");
    }
});
