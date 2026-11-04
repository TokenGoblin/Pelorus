//! Hostile-input tests for the glyph arena and font transfer vocabulary
//! (Phase 9 unit 4), in the style of `hostile_gpu.rs`. The sequencing rules
//! (out-of-order chunk, unknown font, duplicate id) are the assembler's and are
//! tested in `px_ipc::gpu_fonts`; these are about the bytes.

use px_ipc::gpu::{
    FontBytes, GlyphChunk, GpuRequest, GpuResponse, ItemChunk, MAX_FONT_CHUNK_BYTES,
    MAX_GLYPHS_PER_MESSAGE, WireColor, WireGlyph, WireItem,
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

fn rejected<T: Message + std::fmt::Debug>(frame: &[u8]) -> bool {
    matches!(
        decode_frame::<T>(frame),
        Err(IpcError::Malformed | IpcError::MessageTooLarge { .. } | IpcError::PeerClosed)
    )
}

// Variant numbers of the non-testing requests: Begin 0, Items 1, Draw 2,
// Glyphs 3, FontBegin 4, FontChunk 5.
const GLYPHS: u64 = 3;
const FONT_CHUNK: u64 = 5;

fn glyphs_claiming(count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(GLYPHS);
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(GpuRequest::TAG, &bytes)
}

fn chunk_claiming(font: u64, offset: u64, count: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = varint(FONT_CHUNK);
    bytes.extend(varint(font));
    bytes.extend(varint(offset));
    bytes.extend(varint(count));
    bytes.extend_from_slice(body);
    framed(GpuRequest::TAG, &bytes)
}

fn samples() -> Vec<GpuRequest> {
    vec![
        GpuRequest::Begin {
            serial: 1,
            width: 300,
            height: 200,
            item_count: 3,
            glyph_count: 2,
        },
        GpuRequest::Glyphs {
            glyphs: GlyphChunk::new(vec![
                WireGlyph {
                    glyph: 70000,
                    x: -5,
                    y: 1 << 20,
                },
                WireGlyph {
                    glyph: 0,
                    x: 0,
                    y: 0,
                },
            ])
            .expect("bounded"),
        },
        GpuRequest::FontBegin {
            font: 3,
            len: 1 << 20,
        },
        GpuRequest::FontChunk {
            font: 3,
            offset: 262_144,
            bytes: FontBytes::new(vec![1, 2, 3]).expect("bounded"),
        },
        GpuRequest::Items {
            items: ItemChunk::new(vec![WireItem::GlyphRun {
                font: 3,
                size: 960,
                color: WireColor {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 4,
                },
                start: 1,
                len: 2,
            }])
            .expect("bounded"),
        },
    ]
}

/// The fuzz seeds for the new shapes are valid and canonical, so the fuzzer
/// starts inside the interesting part of the decoder.
#[test]
fn gpu_hostile_the_fuzz_seeds_decode_and_reencode_identically() {
    let requests: [&[u8]; 5] = [
        include_bytes!("../../../fuzz/corpus/gpu_request/begin"),
        include_bytes!("../../../fuzz/corpus/gpu_request/glyphs-one"),
        include_bytes!("../../../fuzz/corpus/gpu_request/font-begin"),
        include_bytes!("../../../fuzz/corpus/gpu_request/font-chunk"),
        include_bytes!("../../../fuzz/corpus/gpu_request/items-one-glyphrun"),
    ];
    for seed in requests {
        let message = decode_frame::<GpuRequest>(seed).expect("seed decodes");
        assert_eq!(encode_frame(&message).expect("re-encode"), seed);
    }
    let seed: &[u8] = include_bytes!("../../../fuzz/corpus/gpu_response/failed-unknown-font");
    let message = decode_frame::<GpuResponse>(seed).expect("seed decodes");
    assert_eq!(encode_frame(&message).expect("re-encode"), seed);
}

#[test]
fn gpu_hostile_the_new_messages_roundtrip_canonically() {
    for request in samples() {
        let frame = encode_frame(&request).expect("encode");
        assert_eq!(decode_frame::<GpuRequest>(&frame).expect("decode"), request);
    }
}

#[test]
fn gpu_hostile_the_new_messages_are_refused_when_truncated_at_every_byte() {
    for request in samples() {
        let frame = encode_frame(&request).expect("encode");
        for cut in 0..frame.len() {
            // The transport view: the frame simply ends early.
            assert!(
                rejected::<GpuRequest>(&frame[..cut]),
                "{request:?} cut at {cut}"
            );
            // The codec view: a length prefix that agrees with the cut.
            if cut >= 5 {
                let body = &frame[5..cut];
                assert!(
                    rejected::<GpuRequest>(&framed(GpuRequest::TAG, body)),
                    "{request:?} body cut at {cut}"
                );
            }
        }
    }
}

#[test]
fn gpu_hostile_trailing_bytes_after_the_new_messages_are_refused() {
    for request in samples() {
        let mut body = postcard::to_allocvec(&request).expect("body");
        body.push(0);
        assert!(rejected::<GpuRequest>(&framed(GpuRequest::TAG, &body)));
    }
}

#[test]
fn gpu_hostile_a_glyph_count_over_the_cap_is_refused_before_any_glyph_is_read() {
    // The smallest glyph is three bytes: {0, 0, 0}.
    let over = MAX_GLYPHS_PER_MESSAGE + 1;
    let body = vec![0u8; 3 * over];
    assert!(rejected::<GpuRequest>(&glyphs_claiming(over as u64, &body)));
    let ok = vec![0u8; 3 * MAX_GLYPHS_PER_MESSAGE];
    match decode_frame::<GpuRequest>(&glyphs_claiming(MAX_GLYPHS_PER_MESSAGE as u64, &ok)) {
        Ok(GpuRequest::Glyphs { glyphs }) => assert_eq!(glyphs.len(), MAX_GLYPHS_PER_MESSAGE),
        other => panic!("expected the full chunk, got {other:?}"),
    }
    for count in [u64::from(u32::MAX), u64::MAX, 1 << 40] {
        assert!(rejected::<GpuRequest>(&glyphs_claiming(count, &[])));
    }
}

#[test]
fn gpu_hostile_a_font_chunk_over_the_cap_is_refused_even_when_the_bytes_are_present() {
    let count = MAX_FONT_CHUNK_BYTES as u64 + 1;
    let body = vec![0u8; MAX_FONT_CHUNK_BYTES + 1];
    assert!(rejected::<GpuRequest>(&chunk_claiming(1, 0, count, &body)));
    assert!(FontBytes::new(vec![0; MAX_FONT_CHUNK_BYTES + 1]).is_err());
}

#[test]
fn gpu_hostile_a_font_chunk_at_the_cap_is_accepted_and_fits_in_a_message() {
    let request = GpuRequest::FontChunk {
        font: u32::MAX,
        offset: u32::MAX,
        bytes: FontBytes::new(vec![9; MAX_FONT_CHUNK_BYTES]).expect("at the cap"),
    };
    let frame = encode_frame(&request).expect("a chunk at the cap fits");
    assert!(frame.len() < MAX_MESSAGE_BYTES);
    assert_eq!(decode_frame::<GpuRequest>(&frame).expect("decode"), request);
}

#[test]
fn gpu_hostile_an_enormous_claimed_font_chunk_costs_nothing() {
    for count in [u64::from(u32::MAX), u64::MAX, 1 << 40, 1 << 20] {
        assert!(rejected::<GpuRequest>(&chunk_claiming(1, 0, count, &[])));
    }
}

#[test]
fn gpu_hostile_a_padded_varint_in_a_font_message_is_refused() {
    // FontBegin (4) with font id 1 spelled 0x81 0x00.
    let mut body = varint(4);
    body.extend([0x81, 0x00]);
    body.extend(varint(10));
    assert!(rejected::<GpuRequest>(&framed(GpuRequest::TAG, &body)));
}

#[test]
fn gpu_hostile_the_new_failure_reasons_decode_and_a_bogus_one_does_not() {
    // GpuFailure has ten values (0..=9).
    for why in 0u64..=9 {
        let mut body = varint(3);
        body.extend(varint(why));
        assert!(
            decode_frame::<GpuResponse>(&framed(GpuResponse::TAG, &body)).is_ok(),
            "{why}"
        );
    }
    let mut body = varint(3);
    body.extend(varint(10));
    assert!(rejected::<GpuResponse>(&framed(GpuResponse::TAG, &body)));
}

#[test]
fn gpu_hostile_random_bodies_under_the_new_variants_never_panic() {
    let mut state = 0xD1B5_4A32_D192_ED03u64;
    let mut next = || {
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for _ in 0..4000 {
        let len = (next() % 64) as usize;
        let mut body = varint(3 + next() % 3);
        body.extend((0..len).map(|_| (next() >> 56) as u8));
        let frame = framed(GpuRequest::TAG, &body);
        if let Ok(message) = decode_frame::<GpuRequest>(&frame) {
            assert_eq!(encode_frame(&message).expect("re-encode"), frame);
        }
    }
}
