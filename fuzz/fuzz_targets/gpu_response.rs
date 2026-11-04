#![no_main]

//! The broker decoding what a GPU process wrote (Phase 8, §4.5).
//!
//! The GPU process is sandboxed because it is expected to be compromised, so
//! this is the privileged side decoding attacker-influenced bytes, the same
//! shape as `frame_request`. Anything that decodes must re-encode to the same
//! bytes; nothing may panic or allocate past the per-field bounds.

use libfuzzer_sys::fuzz_target;
use px_ipc::gpu::GpuResponse;

fuzz_target!(|data: &[u8]| {
    if let Ok(message) = px_ipc::decode_frame::<GpuResponse>(data) {
        let again = px_ipc::encode_frame(&message).expect("a decoded message re-encodes");
        assert_eq!(again, data, "decoding is not injective");
    }
});
