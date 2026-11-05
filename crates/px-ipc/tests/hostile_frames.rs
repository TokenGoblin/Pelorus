//! Hostile-input tests for the frame vocabulary (Phase 14), in the style of
//! `hostile_script.rs`. Expected encodings are derived from postcard's rules,
//! not produced by the encoder under test.

use px_ipc::frames::{
    FrameEvent, FrameRequest, FrameResponse, MAX_NAVIGATE_URL_BYTES, NavigateUrl, NavigationKind,
};
use px_ipc::{FrameId, IpcError, Message, decode_frame, encode_frame};

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

/// `Navigate` (variant 1) on frame (1, 0) claiming `count` URL bytes.
fn navigate_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(1);
    bytes.extend(varint(1)); // frame.index
    bytes.extend(varint(0)); // frame.generation
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    bytes.push(0); // kind Push
    framed(FrameRequest::TAG, &bytes)
}

#[test]
fn frame_wire_url_at_the_cap_decodes_and_one_over_does_not() {
    let at_cap = "a".repeat(MAX_NAVIGATE_URL_BYTES);
    let ok = navigate_claiming(MAX_NAVIGATE_URL_BYTES as u64, at_cap.as_bytes());
    assert!(matches!(
        decode_frame::<FrameRequest>(&ok),
        Ok(FrameRequest::Navigate { .. })
    ));

    let over = "a".repeat(MAX_NAVIGATE_URL_BYTES + 1);
    let bad = navigate_claiming(MAX_NAVIGATE_URL_BYTES as u64 + 1, over.as_bytes());
    assert!(matches!(
        decode_frame::<FrameRequest>(&bad),
        Err(IpcError::Malformed)
    ));
}

#[test]
fn frame_wire_a_huge_claimed_url_with_no_bytes_is_malformed() {
    let bad = navigate_claiming(u64::from(u32::MAX), b"");
    assert!(matches!(
        decode_frame::<FrameRequest>(&bad),
        Err(IpcError::Malformed)
    ));
}

#[test]
fn frame_wire_the_encoder_refuses_an_overlong_url() {
    assert!(NavigateUrl::new("a".repeat(MAX_NAVIGATE_URL_BYTES + 1)).is_err());
}

#[test]
fn frame_wire_unknown_variants_and_kinds_are_refused() {
    for variant in 2..=200u64 {
        let frame = framed(FrameRequest::TAG, &varint(variant));
        assert!(
            matches!(
                decode_frame::<FrameRequest>(&frame),
                Err(IpcError::Malformed)
            ),
            "request variant {variant}"
        );
        let frame = framed(FrameEvent::TAG, &varint(variant));
        assert!(
            decode_frame::<FrameEvent>(&frame).is_err(),
            "event variant {variant}"
        );
    }
    // A NavigationKind this build does not know.
    let mut bytes = varint(1);
    bytes.extend([1, 0, 0, 3]); // frame (1,0), empty url, kind 3
    assert!(decode_frame::<FrameRequest>(&framed(FrameRequest::TAG, &bytes)).is_err());
}

#[test]
fn frame_wire_each_direction_refuses_the_others_bytes() {
    let frame = FrameId::new(1, 1);
    let request = encode_frame(&FrameRequest::CreateChildFrame {
        parent: frame,
        element_ordinal: 0,
    })
    .expect("encode");
    assert!(decode_frame::<FrameResponse>(&request).is_err());
    assert!(decode_frame::<FrameEvent>(&request).is_err());

    let event = encode_frame(&FrameEvent::FrameCrashed { frame }).expect("encode");
    assert!(decode_frame::<FrameRequest>(&event).is_err());
    assert!(decode_frame::<FrameResponse>(&event).is_err());
}

#[test]
fn frame_wire_denied_is_the_same_bytes_whatever_was_refused() {
    let a = encode_frame(&FrameResponse::Denied).expect("encode");
    let b = encode_frame(&FrameResponse::Denied).expect("encode");
    assert_eq!(a, b);
    assert_eq!(a.len(), 4 + 1 + 1, "prefix, tag, one discriminant byte");
}

#[test]
fn frame_wire_viewport_dimensions_beyond_u16_do_not_decode() {
    let mut bytes = varint(2); // SetViewport
    bytes.extend([1, 0]); // frame
    bytes.extend(varint(70_000)); // width > u16::MAX
    bytes.extend(varint(1));
    assert!(decode_frame::<FrameEvent>(&framed(FrameEvent::TAG, &bytes)).is_err());
    let _ = NavigationKind::Push;
}
