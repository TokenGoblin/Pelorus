#![no_main]

//! The cross-process frame vocabulary (Phase 14, §4.5): anything that decodes
//! must re-encode to the same bytes; nothing may panic.

use libfuzzer_sys::fuzz_target;
use px_ipc::frames::FrameEvent;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = px_ipc::decode_frame::<FrameEvent>(data) {
        let again = px_ipc::encode_frame(&message).expect("a decoded message re-encodes");
        assert_eq!(again, data, "decoding is not injective");
    }
});
