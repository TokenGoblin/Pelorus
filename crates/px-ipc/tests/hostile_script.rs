//! Hostile-input tests for the script vocabulary, in the style of
//! `hostile_gpu.rs`.
//!
//! The content process decodes `ScriptRequest` from a broker that relays page
//! text, and the broker decodes `ScriptResponse` from a process that runs
//! arbitrary script. Both decoders are driven the way an attacker-controlled
//! peer would drive them: by hand-built bytes. Expected encodings are derived
//! from postcard's rules (varint variant index, varint length, raw bytes), not
//! produced by the encoder under test.

use px_ipc::script::{
    MAX_SCRIPT_ERROR_BYTES, MAX_SCRIPT_RESULT_BYTES, MAX_SCRIPT_SOURCE_BYTES, ScriptRequest,
    ScriptResponse, ScriptResult, ScriptSource, TerminateReason,
};
use px_ipc::{IpcError, Message, decode_frame, encode_frame};

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

/// `ScriptRequest::RunScript` (variant 1) claiming `count` bytes of source,
/// followed by `body`.
fn run_script_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(1);
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(ScriptRequest::TAG, &bytes)
}

/// `ScriptResponse::Result(ScriptResult::Value)` claiming `count` bytes.
fn value_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(0); // Result
    bytes.extend(varint(0)); // Value
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(ScriptResponse::TAG, &bytes)
}

/// `ScriptResponse::Result(ScriptResult::Error)` claiming `count` bytes.
fn error_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(0);
    bytes.extend(varint(1));
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(ScriptResponse::TAG, &bytes)
}

fn rejected<T: Message + std::fmt::Debug>(frame: &[u8]) -> bool {
    matches!(
        decode_frame::<T>(frame),
        Err(IpcError::Malformed | IpcError::MessageTooLarge { .. } | IpcError::PeerClosed)
    )
}

#[test]
fn script_hostile_the_seed_has_the_encoding_derived_by_hand() {
    // Variant 0, then 300 as a varint: 300 = 0b1_0010_1100 -> 0xAC 0x02.
    let frame = framed(ScriptRequest::TAG, &[0, 0xAC, 0x02]);
    assert_eq!(
        decode_frame::<ScriptRequest>(&frame).expect("decode"),
        ScriptRequest::Init { seed: 300 }
    );
    assert_eq!(
        encode_frame(&ScriptRequest::Init { seed: 300 }).expect("encode"),
        frame
    );
}

#[test]
fn script_hostile_a_source_at_the_cap_is_accepted() {
    let body = vec![b'a'; MAX_SCRIPT_SOURCE_BYTES];
    let frame = run_script_claiming(MAX_SCRIPT_SOURCE_BYTES as u64, &body);
    match decode_frame::<ScriptRequest>(&frame) {
        Ok(ScriptRequest::RunScript { source }) => {
            assert_eq!(source.as_str().len(), MAX_SCRIPT_SOURCE_BYTES);
        }
        other => panic!("expected the full source, got {other:?}"),
    }
}

#[test]
fn script_hostile_a_source_one_byte_over_the_cap_is_refused_with_the_bytes_present() {
    let n = MAX_SCRIPT_SOURCE_BYTES + 1;
    assert!(rejected::<ScriptRequest>(&run_script_claiming(
        n as u64,
        &vec![b'a'; n]
    )));
}

#[test]
fn script_hostile_an_oversized_source_cannot_be_built_to_send() {
    let too_big = "a".repeat(MAX_SCRIPT_SOURCE_BYTES + 1);
    assert!(ScriptSource::new(too_big).is_err());
}

#[test]
fn script_hostile_an_enormous_claimed_length_with_no_body_costs_nothing() {
    for count in [u64::from(u32::MAX), u64::MAX, 1 << 40, 1 << 20, 1 << 18] {
        assert!(rejected::<ScriptRequest>(&run_script_claiming(count, &[])));
        assert!(rejected::<ScriptResponse>(&value_claiming(count, &[])));
        assert!(rejected::<ScriptResponse>(&error_claiming(count, &[])));
    }
}

#[test]
fn script_hostile_a_megabyte_of_source_is_refused_by_the_cap_not_only_the_frame() {
    // Just under the frame ceiling: the framing layer would accept it, the
    // field cap must not.
    let n = 1024 * 1024 - 16;
    assert!(rejected::<ScriptRequest>(&run_script_claiming(
        n as u64,
        &vec![b'x'; n]
    )));
}

#[test]
fn script_hostile_a_short_body_is_refused() {
    assert!(rejected::<ScriptRequest>(&run_script_claiming(5, b"1+1")));
}

#[test]
fn script_hostile_invalid_utf8_source_is_refused() {
    // 0xFF never appears in UTF-8; a lone continuation byte likewise.
    assert!(rejected::<ScriptRequest>(&run_script_claiming(
        2,
        &[0xFF, 0x80]
    )));
}

#[test]
fn script_hostile_a_result_over_its_cap_is_refused_and_one_at_it_is_not() {
    let n = MAX_SCRIPT_RESULT_BYTES + 1;
    assert!(rejected::<ScriptResponse>(&value_claiming(
        n as u64,
        &vec![b'a'; n]
    )));
    let n = MAX_SCRIPT_RESULT_BYTES;
    assert!(matches!(
        decode_frame::<ScriptResponse>(&value_claiming(n as u64, &vec![b'a'; n])),
        Ok(ScriptResponse::Result(ScriptResult::Value { .. }))
    ));
}

#[test]
fn script_hostile_an_error_over_its_cap_is_refused_and_one_at_it_is_not() {
    let n = MAX_SCRIPT_ERROR_BYTES + 1;
    assert!(rejected::<ScriptResponse>(&error_claiming(
        n as u64,
        &vec![b'a'; n]
    )));
    let n = MAX_SCRIPT_ERROR_BYTES;
    assert!(matches!(
        decode_frame::<ScriptResponse>(&error_claiming(n as u64, &vec![b'a'; n])),
        Ok(ScriptResponse::Result(ScriptResult::Error { .. }))
    ));
}

#[test]
fn script_hostile_a_non_canonical_length_varint_is_refused() {
    // The length 1 written as 0x81 0x00 (padded). postcard decodes it; the
    // codec's re-encode comparison must refuse it, or two byte strings would
    // name one message.
    let mut bytes = varint(1);
    bytes.extend([0x81, 0x00]);
    bytes.push(b'a');
    assert!(rejected::<ScriptRequest>(&framed(
        ScriptRequest::TAG,
        &bytes
    )));
}

#[test]
fn script_hostile_trailing_bytes_after_a_valid_message_are_refused() {
    let frame = framed(ScriptRequest::TAG, &[0, 1, 0xEE]);
    assert!(rejected::<ScriptRequest>(&frame));
}

#[test]
fn script_hostile_the_other_directions_bytes_do_not_decode() {
    // A valid Init, tagged as a response, and a valid response, tagged as a
    // request: a peer that reflects what it was sent produces neither.
    assert!(rejected::<ScriptResponse>(&framed(
        ScriptResponse::TAG ^ 0x03,
        &[0, 1]
    )));
    let reflected = framed(ScriptRequest::TAG, &[0, 1]);
    assert!(rejected::<ScriptResponse>(&reflected));
    let reply = encode_frame(&ScriptResponse::Result(ScriptResult::Terminated {
        reason: TerminateReason::TimerOverflow,
    }))
    .expect("encode");
    assert!(rejected::<ScriptRequest>(&reply));
    // Nor does a GPU message pass as a script message.
    assert!(rejected::<ScriptRequest>(&framed(0xA3, &[0, 1])));
}

#[test]
fn script_hostile_unknown_variants_and_reasons_are_refused() {
    for variant in [4u64, 5, 127, 128, 300, u64::from(u32::MAX)] {
        assert!(rejected::<ScriptRequest>(&framed(
            ScriptRequest::TAG,
            &varint(variant)
        )));
    }
    // ScriptResponse::Result(Terminated) with reason 4 (there are four: 0..=3).
    let mut bytes = varint(0);
    bytes.extend(varint(2));
    bytes.extend(varint(4));
    assert!(rejected::<ScriptResponse>(&framed(
        ScriptResponse::TAG,
        &bytes
    )));
    // And reason 3 is the last valid one: MicrotaskBudget.
    let mut bytes = varint(0);
    bytes.extend(varint(2));
    bytes.extend(varint(3));
    assert_eq!(
        decode_frame::<ScriptResponse>(&framed(ScriptResponse::TAG, &bytes)).expect("decode"),
        ScriptResponse::Result(ScriptResult::Terminated {
            reason: TerminateReason::MicrotaskBudget
        })
    );
}

/// Variants 2 and 3 of `ScriptRequest` and 1 and 2 of `ScriptResponse` are the
/// testing vocabulary. A release build must not decode them: they would be a
/// way for a peer to ask the content process for its internals.
#[cfg(not(feature = "testing"))]
#[test]
fn script_hostile_the_testing_variants_do_not_exist_in_a_shipping_build() {
    for variant in [2u64, 3] {
        assert!(rejected::<ScriptRequest>(&framed(
            ScriptRequest::TAG,
            &varint(variant)
        )));
    }
    for variant in [1u64, 2] {
        assert!(rejected::<ScriptResponse>(&framed(
            ScriptResponse::TAG,
            &varint(variant)
        )));
    }
}

#[cfg(feature = "testing")]
#[test]
fn script_hostile_the_testing_variants_exist_only_with_the_feature() {
    assert_eq!(
        decode_frame::<ScriptRequest>(&framed(ScriptRequest::TAG, &varint(2))).expect("decode"),
        ScriptRequest::Counters
    );
    assert_eq!(
        decode_frame::<ScriptRequest>(&framed(ScriptRequest::TAG, &varint(3))).expect("decode"),
        ScriptRequest::ClockLog
    );
}

#[test]
fn script_hostile_every_truncation_of_a_valid_run_script_is_refused() {
    let frame = encode_frame(&ScriptRequest::RunScript {
        source: ScriptSource::new("throw 1".into()).expect("small"),
    })
    .expect("encode");
    for cut in 0..frame.len() {
        assert!(
            rejected::<ScriptRequest>(&frame[..cut]),
            "a frame cut at {cut} decoded"
        );
    }
    assert!(decode_frame::<ScriptRequest>(&frame).is_ok());
}
