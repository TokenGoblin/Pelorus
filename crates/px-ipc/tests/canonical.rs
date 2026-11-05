//! One property, checked for every message type the crate names: the mapping
//! from frame bytes to messages is injective.
//!
//! Two things break it. Bytes after the frame the prefix declares (a decoder
//! that reads one frame and ignores the rest accepts any tail), and a
//! non-canonical varint inside the frame (postcard decodes `0x80 0x00` as 0).
//! The fuzz targets assert `encode(decode(bytes)) == bytes`; this is the same
//! assertion with hand-built inputs, over a list that a new vocabulary must be
//! added to.

use px_ipc::gpu::{GpuFailure, GpuRequest, GpuResponse};
use px_ipc::script::{ScriptRequest, ScriptResponse, ScriptResult, TerminateReason};
use px_ipc::{FrameHost, IpcError, Message, Request, Response, decode_frame, encode_frame};

fn reframe(tag_and_body: &[u8]) -> Vec<u8> {
    let length = u32::try_from(tag_and_body.len()).unwrap_or(u32::MAX);
    let mut frame = length.to_le_bytes().to_vec();
    frame.extend_from_slice(tag_and_body);
    frame
}

fn is_malformed<T: Message + std::fmt::Debug>(frame: &[u8]) -> bool {
    matches!(decode_frame::<T>(frame), Err(IpcError::Malformed))
}

/// `sample` must round-trip, and no other byte string near it may decode to
/// a message whose encoding differs from the input.
fn assert_injective<T: Message + std::fmt::Debug + PartialEq>(sample: T) -> Option<()> {
    let frame = encode_frame(&sample).ok()?;
    assert_eq!(decode_frame::<T>(&frame).ok()?, sample);

    // A tail after the declared frame.
    for tail in [&[0u8][..], &[0xFF], &[0, 0, 0, 0], &frame] {
        let mut longer = frame.clone();
        longer.extend_from_slice(tail);
        assert!(is_malformed::<T>(&longer), "{sample:?} + tail {tail:?}");
    }

    // The discriminant (the first body byte, below 0x80 for every type here)
    // padded into a two-byte varint.
    let body = frame.get(4..)?;
    let (tag, rest) = body.split_first()?;
    let (disc, rest) = rest.split_first()?;
    assert!(*disc < 0x80);
    let mut padded = vec![*tag, disc | 0x80, 0x00];
    padded.extend_from_slice(rest);
    assert!(is_malformed::<T>(&reframe(&padded)), "{sample:?} padded");

    // Every later byte below 0x80, padded the same way: whatever decodes must
    // re-encode to exactly the input. (Raw payload bytes may legitimately
    // form another valid message, so this asserts injectivity, not refusal.)
    for at in 2..body.len() {
        let byte = *body.get(at)?;
        if byte >= 0x80 {
            continue;
        }
        let mut changed = Vec::new();
        changed.extend_from_slice(body.get(..at)?);
        changed.push(byte | 0x80);
        changed.push(0x00);
        changed.extend_from_slice(body.get(at + 1..)?);
        let input = reframe(&changed);
        if let Ok(other) = decode_frame::<T>(&input) {
            assert_eq!(
                encode_frame(&other).ok()?,
                input,
                "{sample:?} padded at {at}"
            );
        }
    }
    Some(())
}

#[test]
fn every_message_type_decodes_injectively() {
    assert!(assert_injective(Request::Ping).is_some());
    assert!(
        assert_injective(Request::Echo {
            payload: vec![1, 2, 3],
        })
        .is_some()
    );
    assert!(assert_injective(Response::Pong).is_some());
    assert!(assert_injective(Response::Denied).is_some());
    assert!(
        assert_injective(Response::Echo {
            payload: vec![1, 2, 3],
        })
        .is_some()
    );
    assert!(
        assert_injective(Response::FrameHost {
            host: FrameHost::Local,
        })
        .is_some()
    );

    assert!(assert_injective(GpuRequest::Draw).is_some());
    assert!(
        assert_injective(GpuRequest::Begin {
            serial: 1,
            width: 2,
            height: 3,
            item_count: 4,
            glyph_count: 5,
        })
        .is_some()
    );
    assert!(assert_injective(GpuRequest::FontBegin { font: 7, len: 9 }).is_some());
    assert!(assert_injective(GpuResponse::Ready { landlock_abi: 3 }).is_some());
    assert!(
        assert_injective(GpuResponse::FrameHeader {
            serial: 1,
            width: 2,
            height: 3,
        })
        .is_some()
    );
    assert!(
        assert_injective(GpuResponse::Failed {
            why: GpuFailure::BadViewport,
        })
        .is_some()
    );

    assert!(assert_injective(ScriptRequest::Init { seed: 5 }).is_some());
    assert!(
        assert_injective(ScriptResponse::Result(ScriptResult::Terminated {
            reason: TerminateReason::TimerOverflow,
        }))
        .is_some()
    );
}
