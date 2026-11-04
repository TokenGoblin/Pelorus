//! Hostile-input tests for the GPU vocabulary, in the style of `hostile.rs`.
//!
//! The broker decodes `GpuResponse` from a process that is sandboxed because it
//! may be compromised, and the GPU process decodes `GpuRequest` from a broker
//! that relays page-derived data. Both decoders are driven here the way an
//! attacker-controlled peer would drive them: by hand-built bytes.

use px_ipc::gpu::{
    GpuFailure, GpuRequest, GpuResponse, ItemChunk, MAX_ITEMS_PER_MESSAGE, MAX_STRIP_BYTES,
    StripPixels, WireItem,
};
use px_ipc::{IpcError, MAX_MESSAGE_BYTES, Message, decode_frame, encode_frame};

fn framed(tag: u8, body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(body.len() + 1).unwrap_or(u32::MAX);
    let mut frame = length.to_le_bytes().to_vec();
    frame.push(tag);
    frame.extend_from_slice(body);
    frame
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// `GpuRequest::Items` (variant 1) claiming `count` items, followed by `body`.
fn items_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(1);
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(GpuRequest::TAG, &bytes)
}

/// `GpuResponse::Strip` (variant 2) claiming `count` pixel bytes.
fn strip_claiming(rows: u64, count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(2);
    bytes.extend(varint(0)); // first_row
    bytes.extend(varint(rows));
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(GpuResponse::TAG, &bytes)
}

fn rejected<T: Message + std::fmt::Debug>(frame: &[u8]) -> bool {
    matches!(
        decode_frame::<T>(frame),
        Err(IpcError::Malformed | IpcError::MessageTooLarge { .. } | IpcError::PeerClosed)
    )
}

#[test]
fn gpu_hostile_an_item_count_over_the_cap_is_refused_before_any_item_is_read() {
    // 1025 PopClips (variant 4, one byte each): a perfectly well-formed
    // sequence one element over the cap, as a hostile broker would send.
    let body = vec![4u8; MAX_ITEMS_PER_MESSAGE + 1];
    let frame = items_claiming(MAX_ITEMS_PER_MESSAGE as u64 + 1, &body);
    assert!(rejected::<GpuRequest>(&frame));
}

#[test]
fn gpu_hostile_exactly_the_cap_is_accepted() {
    let body = vec![4u8; MAX_ITEMS_PER_MESSAGE];
    let frame = items_claiming(MAX_ITEMS_PER_MESSAGE as u64, &body);
    match decode_frame::<GpuRequest>(&frame) {
        Ok(GpuRequest::Items { items }) => assert_eq!(items.len(), MAX_ITEMS_PER_MESSAGE),
        other => panic!("expected the full chunk, got {other:?}"),
    }
}

/// The amplification the cap exists for: a megabyte of one-byte items, which
/// serde's plain `Vec` would turn into a million enum values.
#[test]
fn gpu_hostile_a_megabyte_of_one_byte_items_does_not_become_a_million_items() {
    let count = (MAX_MESSAGE_BYTES - 16) as u64;
    let body = vec![4u8; MAX_MESSAGE_BYTES - 16];
    assert!(rejected::<GpuRequest>(&items_claiming(count, &body)));
}

#[test]
fn gpu_hostile_an_enormous_claimed_count_with_no_body_costs_nothing() {
    for count in [u64::from(u32::MAX), u64::MAX, 1 << 40, 1 << 20] {
        assert!(rejected::<GpuRequest>(&items_claiming(count, &[])));
        assert!(rejected::<GpuResponse>(&strip_claiming(1, count, &[])));
    }
}

#[test]
fn gpu_hostile_a_strip_over_the_cap_is_refused_even_when_the_bytes_are_present() {
    let count = MAX_STRIP_BYTES as u64 + 1;
    let body = vec![0u8; MAX_STRIP_BYTES + 1];
    assert!(rejected::<GpuResponse>(&strip_claiming(1, count, &body)));
}

#[test]
fn gpu_hostile_a_strip_at_the_cap_is_accepted() {
    let pixels = StripPixels::new(vec![7; MAX_STRIP_BYTES]).expect("at the cap");
    let response = GpuResponse::Strip {
        first_row: 0,
        rows: 64,
        pixels,
    };
    let frame = encode_frame(&response).expect("a strip at the cap fits in a message");
    assert!(frame.len() < MAX_MESSAGE_BYTES);
    assert_eq!(
        decode_frame::<GpuResponse>(&frame).expect("decode"),
        response
    );
}

#[test]
fn gpu_hostile_a_short_body_is_refused() {
    // Claims three items, supplies two.
    let frame = items_claiming(3, &[4, 4]);
    assert!(rejected::<GpuRequest>(&frame));
}

#[test]
fn gpu_hostile_trailing_bytes_after_a_valid_message_are_refused() {
    let mut frame = encode_frame(&GpuRequest::Draw).expect("encode");
    frame.push(0);
    let length = u32::try_from(frame.len() - 4).expect("fits");
    frame.splice(0..4, length.to_le_bytes());
    assert!(rejected::<GpuRequest>(&frame));
}

#[test]
fn gpu_hostile_a_padded_varint_is_not_a_second_spelling_of_a_message() {
    // Draw is variant 2. 0x82 0x00 is a non-canonical spelling of 2.
    let frame = framed(GpuRequest::TAG, &[0x82, 0x00]);
    assert!(rejected::<GpuRequest>(&frame));
    // And the same trick on a count.
    let mut body = varint(1);
    body.extend([0x81, 0x00]); // a padded 1
    body.push(4);
    assert!(rejected::<GpuRequest>(&framed(GpuRequest::TAG, &body)));
}

#[test]
fn gpu_hostile_an_unknown_variant_or_enum_value_is_refused() {
    assert!(rejected::<GpuRequest>(&framed(
        GpuRequest::TAG,
        &varint(200)
    )));
    assert!(rejected::<GpuResponse>(&framed(
        GpuResponse::TAG,
        &varint(200)
    )));
    // Failed { why } with a discriminant that does not exist.
    let mut body = varint(3);
    body.extend(varint(99));
    assert!(rejected::<GpuResponse>(&framed(GpuResponse::TAG, &body)));
    // And one that does.
    let mut body = varint(3);
    body.extend(varint(1));
    assert_eq!(
        decode_frame::<GpuResponse>(&framed(GpuResponse::TAG, &body)).expect("decode"),
        GpuResponse::Failed {
            why: GpuFailure::BadItemCount
        }
    );
}

#[test]
fn gpu_hostile_a_border_with_a_bad_style_is_refused() {
    // Items { [Border { rect, sides: 4 x { width, style: 7, colour } }] }.
    let mut body = varint(1); // one item
    body.extend(varint(1)); // Border
    body.extend([0, 0, 0, 0]); // rect
    body.extend([0, 7, 0, 0, 0, 0]); // side: width 0, style 7 (invalid)
    body.extend([0, 0, 0, 0, 0, 0].repeat(3));
    assert!(rejected::<GpuRequest>(&items_claiming_raw(&body)));
}

fn items_claiming_raw(after_variant: &[u8]) -> Vec<u8> {
    let mut bytes = varint(1);
    bytes.extend_from_slice(after_variant);
    framed(GpuRequest::TAG, &bytes)
}

#[test]
fn gpu_hostile_each_direction_refuses_the_others_bytes() {
    let request = encode_frame(&GpuRequest::Draw).expect("encode");
    let response = encode_frame(&GpuResponse::Ready { landlock_abi: 0 }).expect("encode");
    assert!(rejected::<GpuResponse>(&request));
    assert!(rejected::<GpuRequest>(&response));
    // And not the content process's vocabulary either.
    let ping = encode_frame(&px_ipc::Request::Ping).expect("encode");
    assert!(rejected::<GpuRequest>(&ping));
    assert!(rejected::<GpuResponse>(&ping));
}

#[test]
fn gpu_hostile_a_chunk_over_the_cap_cannot_be_built_or_sent() {
    let too_many = vec![WireItem::PopClip; MAX_ITEMS_PER_MESSAGE + 1];
    assert!(ItemChunk::new(too_many).is_err());
    assert!(StripPixels::new(vec![0; MAX_STRIP_BYTES + 1]).is_err());
}

/// Generative: random bytes under both tags never panic and are never accepted
/// as anything but a canonical message. Fixed seed, no dependency.
#[test]
fn gpu_hostile_random_bodies_never_panic() {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for _ in 0..4000 {
        let len = (next() % 96) as usize;
        let body: Vec<u8> = (0..len).map(|_| (next() >> 56) as u8).collect();
        for tag in [GpuRequest::TAG, GpuResponse::TAG] {
            let frame = framed(tag, &body);
            if let Ok(message) = decode_frame::<GpuRequest>(&frame) {
                // Anything accepted must re-encode to exactly these bytes.
                assert_eq!(encode_frame(&message).expect("re-encode"), frame);
            }
            if let Ok(message) = decode_frame::<GpuResponse>(&frame) {
                assert_eq!(encode_frame(&message).expect("re-encode"), frame);
            }
        }
    }
}

#[cfg(feature = "testing")]
mod testing_vocabulary {
    use super::*;
    use px_ipc::gpu::{GpuFault, MAX_PROBE_ATTEMPTS, ProbeAttempt, ProbeOp};

    #[test]
    fn gpu_hostile_the_probe_vocabulary_roundtrips_under_testing() {
        let request = GpuRequest::Arm {
            fault: GpuFault::Flood,
        };
        let frame = encode_frame(&request).expect("encode");
        assert_eq!(decode_frame::<GpuRequest>(&frame).expect("decode"), request);
    }

    #[test]
    fn gpu_hostile_probe_results_are_bounded() {
        let attempt = ProbeAttempt {
            op: ProbeOp::Socket,
            succeeded: false,
            errno: 1,
        };
        assert!(
            px_ipc::gpu::Bounded::<ProbeAttempt, MAX_PROBE_ATTEMPTS>::new(vec![
                attempt;
                MAX_PROBE_ATTEMPTS
                    + 1
            ])
            .is_err()
        );
    }
}
