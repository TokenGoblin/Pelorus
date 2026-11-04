#![no_main]

//! The GPU process decoding what the broker relayed (Phase 8, §4.5).
//!
//! The less privileged side, but the display list it carries is derived from a
//! page, and the decoder is the first thing in the GPU process that touches it.

use libfuzzer_sys::fuzz_target;
use px_ipc::gpu::GpuRequest;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = px_ipc::decode_frame::<GpuRequest>(data) {
        let again = px_ipc::encode_frame(&message).expect("a decoded message re-encodes");
        assert_eq!(again, data, "decoding is not injective");
    }
});
