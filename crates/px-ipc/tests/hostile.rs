//! Hostile-input tests for the px-ipc codec.
//!
//! Everything here drives the public surface (`decode_frame`, `encode_frame`,
//! `Channel`) the way a fully attacker-controlled peer would. Generative
//! tests use a hand-written xorshift PRNG with fixed seeds, so a failure is
//! reproducible from the seed and no dependency is added.
//!
//! Helpers avoid unwrap/expect/panic/indexing: clippy's test allowances cover
//! `#[test]` bodies, not helper functions.

use std::io::{Cursor, Read};

use px_ipc::{
    Channel, FrameHost, FrameId, IpcError, MAX_MESSAGE_BYTES, Message, Request, Response,
    decode_frame, encode_frame,
};

// ---------------------------------------------------------------- helpers

/// xorshift64*: tiny, deterministic, good enough to explore shapes.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
    fn byte(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.byte()).collect()
    }
}

fn random_request(rng: &mut Rng) -> Request {
    match rng.below(3) {
        0 => Request::Ping,
        1 => {
            // Mostly small, occasionally large enough for a multi-byte varint.
            let len = if rng.below(20) == 0 {
                rng.below(5000)
            } else {
                rng.below(64)
            };
            Request::Echo {
                payload: rng.bytes(len),
            }
        }
        _ => Request::FrameHost {
            frame: FrameId::new(rng.next_u32(), rng.next_u32()),
        },
    }
}

fn random_response(rng: &mut Rng) -> Response {
    match rng.below(4) {
        0 => Response::Pong,
        1 => {
            let len = if rng.below(20) == 0 {
                rng.below(5000)
            } else {
                rng.below(64)
            };
            Response::Echo {
                payload: rng.bytes(len),
            }
        }
        2 => Response::FrameHost {
            host: if rng.below(2) == 0 {
                FrameHost::Local
            } else {
                FrameHost::Remote
            },
        },
        _ => Response::Denied,
    }
}

/// Build a frame by hand: LE length prefix, then `body` verbatim.
fn frame_with_prefix(prefix: u32, body: &[u8]) -> Vec<u8> {
    let mut f = prefix.to_le_bytes().to_vec();
    f.extend_from_slice(body);
    f
}

/// Build a frame whose prefix honestly describes `body`.
fn honest_frame(body: &[u8]) -> Vec<u8> {
    frame_with_prefix(u32::try_from(body.len()).unwrap_or(u32::MAX), body)
}

/// `tag || postcard-bytes`, honestly framed.
fn tagged<T: Message>(postcard_bytes: &[u8]) -> Vec<u8> {
    let mut body = vec![T::TAG];
    body.extend_from_slice(postcard_bytes);
    honest_frame(&body)
}

/// Re-frame an existing valid frame's body after a transformation, fixing the
/// prefix so the framing layer is not what rejects it.
fn body_of(frame: &[u8]) -> Vec<u8> {
    frame.get(4..).map(<[u8]>::to_vec).unwrap_or_default()
}

fn all_valid_requests() -> Vec<Request> {
    vec![
        Request::Ping,
        Request::Echo { payload: vec![] },
        Request::Echo {
            payload: vec![1, 2, 3],
        },
        Request::Echo {
            payload: vec![0xAB; 300],
        },
        Request::FrameHost {
            frame: FrameId::new(0, 0),
        },
        Request::FrameHost {
            frame: FrameId::new(u32::MAX, u32::MAX),
        },
        Request::FrameHost {
            frame: FrameId::new(127, 128),
        },
    ]
}

fn all_valid_responses() -> Vec<Response> {
    vec![
        Response::Pong,
        Response::Echo { payload: vec![] },
        Response::Echo {
            payload: vec![9; 200],
        },
        Response::FrameHost {
            host: FrameHost::Local,
        },
        Response::FrameHost {
            host: FrameHost::Remote,
        },
        Response::Denied,
    ]
}

/// A reader that hands out at most `step` bytes per `read` call.
struct Dribble<'a> {
    data: &'a [u8],
    step: usize,
}
impl Read for Dribble<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.step.min(buf.len()).min(self.data.len());
        let (head, tail) = self.data.split_at(n);
        if let Some(dst) = buf.get_mut(..n) {
            dst.copy_from_slice(head);
        }
        self.data = tail;
        Ok(n)
    }
}

/// A reader that yields its bytes then fails with a non-EOF error.
struct ThenError<'a> {
    data: &'a [u8],
}
impl Read for ThenError<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.data.is_empty() {
            return Err(std::io::Error::other("boom"));
        }
        let n = self.data.len().min(buf.len());
        let (head, tail) = self.data.split_at(n);
        if let Some(dst) = buf.get_mut(..n) {
            dst.copy_from_slice(head);
        }
        self.data = tail;
        Ok(n)
    }
}

// ---------------------------------------------------------- length prefixes

#[test]
fn prefix_zero_is_malformed_and_poisons() {
    let frame = frame_with_prefix(0, &[]);
    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(IpcError::Malformed)
    ));

    let mut cur = Cursor::new(frame_with_prefix(0, &[]));
    let mut ch = Channel::new(&mut cur, std::io::sink());
    assert!(matches!(ch.recv::<Request>(), Err(IpcError::Malformed)));
    assert!(ch.is_poisoned());
}

#[test]
fn prefix_one_with_only_a_tag_is_malformed_for_both_directions() {
    assert!(matches!(
        decode_frame::<Request>(&tagged::<Request>(&[])),
        Err(IpcError::Malformed)
    ));
    assert!(matches!(
        decode_frame::<Response>(&tagged::<Response>(&[])),
        Err(IpcError::Malformed)
    ));
}

#[test]
fn prefix_exactly_max_with_garbage_body_is_malformed_not_too_large() {
    let mut body = vec![Request::TAG];
    body.resize(MAX_MESSAGE_BYTES, 0);
    let frame = honest_frame(&body);
    // Variant 0 (Ping) followed by a megabyte of trailing zeros.
    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(IpcError::Malformed)
    ));
}

#[test]
fn prefix_max_and_body_short_is_a_closed_peer() {
    let max = u32::try_from(MAX_MESSAGE_BYTES).expect("fits");
    let frame = frame_with_prefix(max, &[Request::TAG, 0]);
    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(IpcError::PeerClosed)
    ));
}

#[test]
fn prefix_max_plus_one_is_too_large_even_with_the_whole_body_present() {
    let over = MAX_MESSAGE_BYTES + 1;
    let mut body = vec![Request::TAG];
    body.resize(over, 0);
    let frame = frame_with_prefix(u32::try_from(over).expect("fits"), &body);
    match decode_frame::<Request>(&frame) {
        Err(IpcError::MessageTooLarge { claimed }) => assert_eq!(claimed, over as u64),
        other => panic!("expected MessageTooLarge, got {other:?}"),
    }
}

#[test]
fn overflow_shaped_prefixes_are_all_too_large() {
    let shapes: [u32; 12] = [
        u32::MAX,
        u32::MAX - 1,
        0x8000_0000,
        0x7FFF_FFFF,
        0x0100_0000,
        0x0010_0001,
        0xFFFF_0000,
        1 << 24,
        0xDEAD_BEEF,
        0x4141_4141,                            // "AAAA" interpreted as a length
        u32::from_be_bytes(7u32.to_le_bytes()), // a small length sent big-endian
        u32::from_le_bytes(*b"GET "),
    ];
    for prefix in shapes {
        assert!(
            prefix as usize > MAX_MESSAGE_BYTES,
            "test shape {prefix:#x}"
        );
        let frame = frame_with_prefix(prefix, &[Request::TAG, 0]);
        match decode_frame::<Request>(&frame) {
            Err(IpcError::MessageTooLarge { claimed }) => {
                assert_eq!(claimed, u64::from(prefix));
            }
            other => panic!("prefix {prefix:#x}: expected MessageTooLarge, got {other:?}"),
        }
    }
}

#[test]
fn a_big_endian_valid_frame_is_rejected() {
    // A peer that writes the length big-endian: 00 00 00 02 A1 00.
    let mut frame = 2u32.to_be_bytes().to_vec();
    frame.extend_from_slice(&[Request::TAG, 0]);
    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(IpcError::MessageTooLarge { .. })
    ));
}

#[test]
fn prefix_larger_than_body_is_a_closed_peer_for_every_shortfall() {
    let valid = encode_frame(&Request::Echo {
        payload: vec![5; 40],
    })
    .expect("encode");
    let body = body_of(&valid);
    for extra in 1..=64u32 {
        let claimed = u32::try_from(body.len()).expect("fits") + extra;
        let frame = frame_with_prefix(claimed, &body);
        assert!(
            matches!(decode_frame::<Request>(&frame), Err(IpcError::PeerClosed)),
            "claimed {extra} bytes more than were sent"
        );
    }
}

#[test]
fn prefix_smaller_than_body_never_decodes_as_the_full_message() {
    let msg = Request::Echo {
        payload: vec![5; 40],
    };
    let valid = encode_frame(&msg).expect("encode");
    let body = body_of(&valid);
    for short in 1..=body.len() {
        let claimed = u32::try_from(body.len() - short).expect("fits");
        let frame = frame_with_prefix(claimed, &body);
        // The declared window is a strict prefix of the encoding, so it can
        // never decode as the original message.
        match decode_frame::<Request>(&frame) {
            Ok(decoded) => assert_ne!(decoded, msg, "short by {short}"),
            Err(IpcError::Malformed) => {}
            Err(other) => panic!("short by {short}: unexpected {other:?}"),
        }
    }
}

#[test]
fn bytes_after_a_frame_are_the_next_frame_not_part_of_this_one() {
    // Prefix says 2 (tag + Ping); eight more junk bytes follow on the stream.
    let mut stream = frame_with_prefix(2, &[Request::TAG, 0]);
    stream.extend_from_slice(&[0xFF; 8]);
    let mut cur = Cursor::new(stream);
    let mut ch = Channel::new(&mut cur, std::io::sink());
    assert_eq!(ch.recv::<Request>().expect("first"), Request::Ping);
    assert!(!ch.is_poisoned());
    // 0xFFFFFFFF as a prefix: too large, and now the channel is poisoned.
    assert!(matches!(
        ch.recv::<Request>(),
        Err(IpcError::MessageTooLarge { .. })
    ));
    assert!(ch.is_poisoned());
}

// -------------------------------------------------------------- truncation

#[test]
fn truncation_at_every_byte_of_a_valid_frame_is_a_closed_peer() {
    for request in all_valid_requests() {
        let frame = encode_frame(&request).expect("encode");
        for cut in 0..frame.len() {
            let head = frame.get(..cut).expect("in range");
            assert!(
                matches!(decode_frame::<Request>(head), Err(IpcError::PeerClosed)),
                "{request:?} cut at {cut}/{}",
                frame.len()
            );
        }
        assert_eq!(decode_frame::<Request>(&frame).expect("full"), request);
    }
    for response in all_valid_responses() {
        let frame = encode_frame(&response).expect("encode");
        for cut in 0..frame.len() {
            let head = frame.get(..cut).expect("in range");
            assert!(
                matches!(decode_frame::<Response>(head), Err(IpcError::PeerClosed)),
                "{response:?} cut at {cut}/{}",
                frame.len()
            );
        }
    }
}

#[test]
fn truncated_payload_with_an_honest_prefix_is_malformed_at_every_byte() {
    for request in all_valid_requests() {
        let frame = encode_frame(&request).expect("encode");
        let body = body_of(&frame);
        for cut in 0..body.len() {
            let head = body.get(..cut).expect("in range");
            let reframed = honest_frame(head);
            assert!(
                matches!(decode_frame::<Request>(&reframed), Err(IpcError::Malformed)),
                "{request:?} body cut to {cut}/{}",
                body.len()
            );
        }
    }
    for response in all_valid_responses() {
        let frame = encode_frame(&response).expect("encode");
        let body = body_of(&frame);
        for cut in 0..body.len() {
            let head = body.get(..cut).expect("in range");
            let reframed = honest_frame(head);
            assert!(
                matches!(
                    decode_frame::<Response>(&reframed),
                    Err(IpcError::Malformed)
                ),
                "{response:?} body cut to {cut}/{}",
                body.len()
            );
        }
    }
}

#[test]
fn a_prefix_cut_short_is_a_closed_peer_and_does_not_poison() {
    for cut in 0..4usize {
        let frame = encode_frame(&Request::Ping).expect("encode");
        let head = frame.get(..cut).expect("in range").to_vec();
        let mut cur = Cursor::new(head);
        let mut ch = Channel::new(&mut cur, std::io::sink());
        assert!(matches!(ch.recv::<Request>(), Err(IpcError::PeerClosed)));
        assert!(!ch.is_poisoned(), "EOF at a clean boundary is routine");
        // And it stays that way, repeatably.
        assert!(matches!(ch.recv::<Request>(), Err(IpcError::PeerClosed)));
    }
}

// ----------------------------------------------------------- trailing bytes

#[test]
fn trailing_garbage_inside_the_frame_is_malformed_for_every_length() {
    for request in all_valid_requests() {
        let frame = encode_frame(&request).expect("encode");
        let body = body_of(&frame);
        for extra in 1..=40usize {
            for fill in [0x00u8, 0x01, 0xFF, 0xA1] {
                let mut longer = body.clone();
                longer.extend(std::iter::repeat_n(fill, extra));
                assert!(
                    matches!(
                        decode_frame::<Request>(&honest_frame(&longer)),
                        Err(IpcError::Malformed)
                    ),
                    "{request:?} + {extra} x {fill:#x}"
                );
            }
        }
    }
}

#[test]
fn trailing_garbage_on_a_response_is_malformed() {
    for response in all_valid_responses() {
        let mut body = body_of(&encode_frame(&response).expect("encode"));
        body.push(0);
        assert!(matches!(
            decode_frame::<Response>(&honest_frame(&body)),
            Err(IpcError::Malformed)
        ));
    }
}

// ------------------------------------------------------------- discriminants

fn varint_encodings(value: u32) -> Vec<Vec<u8>> {
    // Canonical LEB128 of `value`.
    let mut out = Vec::new();
    let mut v = value;
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    vec![out]
}

#[test]
fn every_out_of_range_request_discriminant_is_malformed() {
    // Valid: 0..=2.
    for d in 3u32..=2048 {
        for bytes in varint_encodings(d) {
            // Give it generous trailing material so only the discriminant can
            // be the reason.
            let mut pc = bytes.clone();
            pc.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            assert!(
                matches!(
                    decode_frame::<Request>(&tagged::<Request>(&pc)),
                    Err(IpcError::Malformed)
                ),
                "request discriminant {d}"
            );
        }
    }
}

#[test]
fn every_out_of_range_response_discriminant_is_malformed() {
    // Valid: 0..=3.
    for d in 4u32..=2048 {
        for bytes in varint_encodings(d) {
            let mut pc = bytes.clone();
            pc.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            assert!(
                matches!(
                    decode_frame::<Response>(&tagged::<Response>(&pc)),
                    Err(IpcError::Malformed)
                ),
                "response discriminant {d}"
            );
        }
    }
}

#[test]
fn huge_discriminants_are_malformed() {
    for pc in [
        vec![0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
        vec![0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01],
        vec![
            0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80,
        ],
    ] {
        assert!(matches!(
            decode_frame::<Request>(&tagged::<Request>(&pc)),
            Err(IpcError::Malformed)
        ));
        assert!(matches!(
            decode_frame::<Response>(&tagged::<Response>(&pc)),
            Err(IpcError::Malformed)
        ));
    }
}

#[test]
fn every_out_of_range_frame_host_discriminant_is_malformed() {
    // Response::FrameHost = variant 2, then FrameHost enum: valid 0..=1.
    for d in 2u8..=255 {
        let pc = [2u8, d];
        // d >= 128 is a continuation byte; add a terminator so the varint is
        // complete and the value is genuinely out of range.
        let mut v = pc.to_vec();
        if d >= 0x80 {
            v.push(0x01);
        }
        assert!(
            matches!(
                decode_frame::<Response>(&tagged::<Response>(&v)),
                Err(IpcError::Malformed)
            ),
            "host discriminant {d}"
        );
    }
}

#[test]
fn a_wrong_tag_is_malformed_for_every_tag_value() {
    let pc = body_of(&encode_frame(&Request::Ping).expect("encode"));
    // pc is [tag, 0]; swap the tag through all 256 values.
    for tag in 0..=255u8 {
        let body = [tag, 0];
        let frame = honest_frame(&body);
        let result = decode_frame::<Request>(&frame);
        if tag == Request::TAG {
            assert_eq!(result.expect("valid"), Request::Ping);
        } else {
            assert!(matches!(result, Err(IpcError::Malformed)), "tag {tag:#x}");
        }
    }
    drop(pc);
}

#[test]
fn every_response_frame_is_rejected_as_a_request_and_vice_versa() {
    for response in all_valid_responses() {
        let frame = encode_frame(&response).expect("encode");
        assert!(matches!(
            decode_frame::<Request>(&frame),
            Err(IpcError::Malformed)
        ));
    }
    for request in all_valid_requests() {
        let frame = encode_frame(&request).expect("encode");
        assert!(matches!(
            decode_frame::<Response>(&frame),
            Err(IpcError::Malformed)
        ));
    }
}

// ---------------------------------------------------- nesting and big claims

#[test]
fn deeply_nested_looking_payloads_are_malformed_not_a_stack_overflow() {
    // The wire types are not recursive, but a hostile peer does not know or
    // care: a megabyte of bytes that look like "enter another level" must not
    // recurse or loop.
    for fill in [0x01u8, 0x02, 0x80, 0xFF, 0x00] {
        let mut body = vec![Request::TAG];
        body.resize(MAX_MESSAGE_BYTES, fill);
        let frame = honest_frame(&body);
        assert!(
            decode_frame::<Request>(&frame).is_err(),
            "fill {fill:#x} must not decode"
        );
        let mut rbody = vec![Response::TAG];
        rbody.resize(MAX_MESSAGE_BYTES, fill);
        assert!(decode_frame::<Response>(&honest_frame(&rbody)).is_err());
    }
}

#[test]
fn a_claimed_huge_vec_length_is_rejected_without_allocating_the_claim() {
    // Echo = variant 1, then a varint element count, then (almost) no data.
    // If the decoder reserved `count` bytes up front, these would abort the
    // process with an allocation failure rather than return.
    let claims: [&[u8]; 5] = [
        // u32::MAX
        &[1, 0xFF, 0xFF, 0xFF, 0xFF, 0x0F],
        // 2^35
        &[1, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01],
        // 2^63
        &[
            1, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
        ],
        // u64::MAX
        &[
            1, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x01,
        ],
        // 1 GiB
        &[1, 0x80, 0x80, 0x80, 0x80, 0x04],
    ];
    for pc in claims {
        for with_tail in [0usize, 1, 100, 4096] {
            let mut v = pc.to_vec();
            v.extend(std::iter::repeat_n(0x55u8, with_tail));
            assert!(
                matches!(
                    decode_frame::<Request>(&tagged::<Request>(&v)),
                    Err(IpcError::Malformed)
                ),
                "claim {pc:?} tail {with_tail}"
            );
        }
    }
    // Same shape in the other direction.
    for pc in claims {
        let mut v = pc.to_vec();
        if let Some(first) = v.first_mut() {
            *first = 1; // Response::Echo is also variant 1.
        }
        assert!(matches!(
            decode_frame::<Response>(&tagged::<Response>(&v)),
            Err(IpcError::Malformed)
        ));
    }
}

#[test]
fn a_vec_length_larger_than_the_frame_by_one_is_malformed() {
    // Payload of 10 declared, 9 present.
    let mut pc = vec![1u8, 10];
    pc.extend_from_slice(&[7; 9]);
    assert!(matches!(
        decode_frame::<Request>(&tagged::<Request>(&pc)),
        Err(IpcError::Malformed)
    ));
    // Exactly right decodes.
    pc.push(7);
    assert!(decode_frame::<Request>(&tagged::<Request>(&pc)).is_ok());
}

// ---------------------------------------------------------- size boundaries

/// Largest Echo payload whose frame body (tag + postcard) fits the limit.
fn largest_echo_payload() -> usize {
    // tag(1) + variant(1) + 3-byte varint length for ~1 MiB.
    MAX_MESSAGE_BYTES - 5
}

#[test]
fn echo_request_at_exactly_the_limit_roundtrips() {
    let n = largest_echo_payload();
    let msg = Request::Echo {
        payload: vec![0x5A; n],
    };
    let frame = encode_frame(&msg).expect("exactly at the limit must send");
    let prefix = u32::from_le_bytes(frame.get(..4).and_then(|s| s.try_into().ok()).expect("4"));
    assert_eq!(prefix as usize, MAX_MESSAGE_BYTES);
    assert_eq!(decode_frame::<Request>(&frame).expect("decode"), msg);
}

#[test]
fn echo_request_one_over_the_limit_is_refused_on_send() {
    let n = largest_echo_payload() + 1;
    match encode_frame(&Request::Echo {
        payload: vec![0; n],
    }) {
        Err(IpcError::MessageTooLarge { claimed }) => {
            assert_eq!(claimed, (MAX_MESSAGE_BYTES + 1) as u64);
        }
        other => panic!("expected MessageTooLarge, got {other:?}"),
    }
}

#[test]
fn echo_response_at_and_over_the_limit() {
    let n = largest_echo_payload();
    let msg = Response::Echo {
        payload: vec![1; n],
    };
    let frame = encode_frame(&msg).expect("at limit");
    assert_eq!(decode_frame::<Response>(&frame).expect("decode"), msg);
    assert!(matches!(
        encode_frame(&Response::Echo {
            payload: vec![1; n + 1]
        }),
        Err(IpcError::MessageTooLarge { .. })
    ));
}

#[test]
fn a_refused_oversized_send_leaves_the_channel_usable() {
    let mut wire = Vec::new();
    let mut ch = Channel::new(std::io::empty(), &mut wire);
    assert!(
        ch.send(&Request::Echo {
            payload: vec![0; MAX_MESSAGE_BYTES]
        })
        .is_err()
    );
    assert!(!ch.is_poisoned());
    ch.send(&Request::Ping).expect("still usable");
    drop(ch);
    assert_eq!(decode_frame::<Request>(&wire).expect("ping"), Request::Ping);
}

#[test]
fn a_hand_framed_one_over_prefix_is_rejected_even_if_the_body_is_a_valid_echo() {
    // Peer-side forging of a frame the honest sender would have refused.
    let n = largest_echo_payload() + 1;
    let mut pc = vec![1u8];
    // varint length of n
    let mut v = n;
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            pc.push(b);
            break;
        }
        pc.push(b | 0x80);
    }
    pc.resize(pc.len() + n, 9);
    let frame = tagged::<Request>(&pc);
    assert!(matches!(
        decode_frame::<Request>(&frame),
        Err(IpcError::MessageTooLarge { .. })
    ));
}

// -------------------------------------------------- channel state semantics

#[test]
fn a_malformed_frame_poisons_and_a_following_valid_frame_is_never_served() {
    let mut stream = tagged::<Request>(&[0x77]); // bad discriminant
    stream.extend_from_slice(&encode_frame(&Request::Ping).expect("encode"));
    let mut cur = Cursor::new(stream);
    let mut ch = Channel::new(&mut cur, std::io::sink());

    assert!(matches!(ch.recv::<Request>(), Err(IpcError::Malformed)));
    assert!(ch.is_poisoned());
    // Repeated receives are permanently, identically Poisoned; the valid
    // frame behind the bad one is never delivered.
    for _ in 0..10 {
        assert!(matches!(ch.recv::<Request>(), Err(IpcError::Poisoned)));
        assert!(ch.is_poisoned());
    }
    // Poison also covers sending and the other direction's receive.
    assert!(matches!(ch.send(&Request::Ping), Err(IpcError::Poisoned)));
    assert!(matches!(ch.recv::<Response>(), Err(IpcError::Poisoned)));
}

#[test]
fn every_decode_failure_after_the_prefix_poisons() {
    let cases: Vec<Vec<u8>> = vec![
        frame_with_prefix(0, &[]),              // empty body
        tagged::<Request>(&[0xEE]),             // bad discriminant
        tagged::<Response>(&[0]),               // wrong direction
        tagged::<Request>(&[0, 0]),             // trailing byte
        frame_with_prefix(10, &[Request::TAG]), // short body
        frame_with_prefix(u32::MAX, &[]),       // too large
    ];
    for bytes in cases {
        let mut cur = Cursor::new(bytes.clone());
        let mut ch = Channel::new(&mut cur, std::io::sink());
        assert!(ch.recv::<Request>().is_err(), "{bytes:?}");
        assert!(ch.is_poisoned(), "{bytes:?} left the channel healthy");
        assert!(matches!(ch.recv::<Request>(), Err(IpcError::Poisoned)));
    }
}

#[test]
fn a_non_eof_io_error_poisons_whether_in_prefix_or_payload() {
    // Error while reading the prefix.
    let mut ch = Channel::new(ThenError { data: &[1, 0] }, std::io::sink());
    assert!(matches!(ch.recv::<Request>(), Err(IpcError::Io(_))));
    assert!(ch.is_poisoned());
    assert!(matches!(ch.recv::<Request>(), Err(IpcError::Poisoned)));

    // Error while reading the payload.
    let head = frame_with_prefix(50, &[Request::TAG, 1]);
    let mut ch = Channel::new(ThenError { data: &head }, std::io::sink());
    assert!(matches!(ch.recv::<Request>(), Err(IpcError::Io(_))));
    assert!(ch.is_poisoned());
}

#[test]
fn eof_mid_payload_poisons_but_eof_between_frames_does_not() {
    let good = encode_frame(&Request::Ping).expect("encode");
    let mut stream = good.clone();
    stream.extend_from_slice(&frame_with_prefix(9, &[Request::TAG]));
    let mut cur = Cursor::new(stream);
    let mut ch = Channel::new(&mut cur, std::io::sink());
    assert_eq!(ch.recv::<Request>().expect("first"), Request::Ping);
    assert!(!ch.is_poisoned());
    assert!(matches!(ch.recv::<Request>(), Err(IpcError::PeerClosed)));
    assert!(ch.is_poisoned());
}

#[test]
fn a_dribbling_reader_decodes_the_same_stream() {
    let mut rng = Rng::new(0xD1B1);
    let msgs: Vec<Request> = (0..200).map(|_| random_request(&mut rng)).collect();
    let mut wire = Vec::new();
    for m in &msgs {
        wire.extend_from_slice(&encode_frame(m).expect("encode"));
    }
    for step in [1usize, 2, 3, 5, 7, 4096, 9000] {
        let mut ch = Channel::new(Dribble { data: &wire, step }, std::io::sink());
        for m in &msgs {
            assert_eq!(&ch.recv::<Request>().expect("recv"), m, "step {step}");
        }
        assert!(matches!(ch.recv::<Request>(), Err(IpcError::PeerClosed)));
    }
}

#[test]
fn large_then_small_messages_keep_decoding_on_one_channel() {
    // Exercises the scratch shrink path between messages.
    let big = Request::Echo {
        payload: vec![3; largest_echo_payload()],
    };
    let mut wire = Vec::new();
    for m in [&big, &Request::Ping, &big, &Request::Ping] {
        wire.extend_from_slice(&encode_frame(m).expect("encode"));
    }
    let mut cur = Cursor::new(wire);
    let mut ch = Channel::new(&mut cur, std::io::sink());
    assert_eq!(ch.recv::<Request>().expect("1"), big);
    assert_eq!(ch.recv::<Request>().expect("2"), Request::Ping);
    assert_eq!(ch.recv::<Request>().expect("3"), big);
    assert_eq!(ch.recv::<Request>().expect("4"), Request::Ping);
}

// ------------------------------------------------------------- generative

#[test]
fn generative_requests_roundtrip() {
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF1);
    let mut seen = [false; 3];
    for i in 0..5000 {
        let m = random_request(&mut rng);
        match &m {
            Request::Ping => seen[0] = true,
            Request::Echo { .. } => seen[1] = true,
            Request::FrameHost { .. } => seen[2] = true,
        }
        let frame = encode_frame(&m).expect("encode");
        assert_eq!(decode_frame::<Request>(&frame).expect("decode"), m, "#{i}");
    }
    assert!(
        seen.iter().all(|s| *s),
        "generator must cover every variant"
    );
}

#[test]
fn generative_responses_roundtrip() {
    let mut rng = Rng::new(0xFEED_FACE_CAFE_BEEF);
    let mut seen = [false; 4];
    for i in 0..5000 {
        let m = random_response(&mut rng);
        match &m {
            Response::Pong => seen[0] = true,
            Response::Echo { .. } => seen[1] = true,
            Response::FrameHost { .. } => seen[2] = true,
            Response::Denied => seen[3] = true,
        }
        let frame = encode_frame(&m).expect("encode");
        assert_eq!(decode_frame::<Response>(&frame).expect("decode"), m, "#{i}");
    }
    assert!(seen.iter().all(|s| *s));
}

#[test]
fn generative_streams_roundtrip_through_a_channel() {
    let mut rng = Rng::new(42);
    let msgs: Vec<Response> = (0..2000).map(|_| random_response(&mut rng)).collect();
    let mut wire = Vec::new();
    {
        let mut tx = Channel::new(std::io::empty(), &mut wire);
        for m in &msgs {
            tx.send(m).expect("send");
        }
    }
    let mut cur = Cursor::new(wire);
    let mut rx = Channel::new(&mut cur, std::io::sink());
    for m in &msgs {
        assert_eq!(&rx.recv::<Response>().expect("recv"), m);
    }
    assert!(matches!(rx.recv::<Response>(), Err(IpcError::PeerClosed)));
}

#[test]
fn generative_byte_mutation_never_panics_and_failures_poison() {
    let mut rng = Rng::new(0xBAD5_EED5);
    let mut ok = 0usize;
    let mut err = 0usize;
    for _ in 0..20_000 {
        let mut frame = if rng.below(2) == 0 {
            encode_frame(&random_request(&mut rng)).expect("encode")
        } else {
            encode_frame(&random_response(&mut rng)).expect("encode")
        };
        // 1..=4 point mutations.
        for _ in 0..=rng.below(4) {
            let at = rng.below(frame.len());
            match rng.below(4) {
                0 => {
                    if let Some(b) = frame.get_mut(at) {
                        *b ^= 1 << rng.below(8);
                    }
                }
                1 => {
                    let v = rng.byte();
                    if let Some(b) = frame.get_mut(at) {
                        *b = v;
                    }
                }
                2 => frame.truncate(at),
                _ => frame.push(rng.byte()),
            }
        }
        for as_request in [true, false] {
            let mut cur = Cursor::new(frame.clone());
            let mut ch = Channel::new(&mut cur, std::io::sink());
            let failed = if as_request {
                ch.recv::<Request>().is_err()
            } else {
                ch.recv::<Response>().is_err()
            };
            if failed {
                err += 1;
                // Everything but a clean EOF at the frame boundary poisons.
                if !frame.is_empty() && frame.len() >= 4 {
                    assert!(ch.is_poisoned(), "failure left channel healthy");
                }
            } else {
                ok += 1;
            }
        }
    }
    // Sanity: the mutator must produce both outcomes, or it tests nothing.
    assert!(ok > 0 && err > 0, "ok={ok} err={err}");
}

#[test]
fn generative_pure_noise_never_panics() {
    let mut rng = Rng::new(0x0123_4567);
    for _ in 0..20_000 {
        let len = rng.below(64);
        let mut noise = rng.bytes(len);
        // Half the time, make the prefix plausible so the payload decoder
        // actually runs.
        if rng.below(2) == 0 && noise.len() >= 5 {
            let body_len = u32::try_from(noise.len() - 4).unwrap_or(0);
            for (dst, src) in noise.iter_mut().zip(body_len.to_le_bytes()) {
                *dst = src;
            }
            if let Some(tag) = noise.get_mut(4) {
                *tag = if rng.below(2) == 0 {
                    Request::TAG
                } else {
                    Response::TAG
                };
            }
        }
        let _ = decode_frame::<Request>(&noise);
        let _ = decode_frame::<Response>(&noise);
    }
}

/// Anything that decodes must have exactly one encoding of each frame body
/// the peer can produce, or the frame-to-message mapping is not injective
/// (CLAUDE.md for px-ipc: "trailing bytes are rejected" exists for this).
#[test]
fn generative_accepted_mutants_reencode_to_the_same_bytes() {
    let mut rng = Rng::new(0xC0FF_EE00);
    let mut accepted_mutants = 0usize;
    for _ in 0..20_000 {
        let original = encode_frame(&random_request(&mut rng)).expect("encode");
        let mut frame = original.clone();
        let at = rng.below(frame.len());
        if let Some(b) = frame.get_mut(at) {
            *b = rng.byte();
        }
        if frame == original {
            continue;
        }
        if let Ok(m) = decode_frame::<Request>(&frame) {
            accepted_mutants += 1;
            let again = encode_frame(&m).expect("re-encode");
            assert_eq!(
                again, frame,
                "two distinct frames decode to {m:?}: the mapping is not injective"
            );
        }
    }
    // Mutating payload bytes of an Echo/FrameHost legitimately yields other
    // valid messages, so this must have exercised the accepted branch.
    assert!(accepted_mutants > 0);
}

/// Regression for the bug the test above found: postcard decodes a varint
/// padded with continuation bytes, so `FrameHost` index 0 had many encodings.
#[test]
fn non_canonical_varints_are_malformed() {
    // Request::FrameHost (2), index 0 padded as 0x80 0x00, generation 0.
    assert!(matches!(
        decode_frame::<Request>(&tagged::<Request>(&[2, 0x80, 0x00, 0])),
        Err(IpcError::Malformed)
    ));
    // Canonical form still decodes.
    assert!(decode_frame::<Request>(&tagged::<Request>(&[2, 0, 0])).is_ok());
    // Padded discriminant: Ping as 0x80 0x00.
    assert!(matches!(
        decode_frame::<Request>(&tagged::<Request>(&[0x80, 0x00])),
        Err(IpcError::Malformed)
    ));
    // Padded Vec length.
    assert!(matches!(
        decode_frame::<Request>(&tagged::<Request>(&[1, 0x81, 0x00, 7])),
        Err(IpcError::Malformed)
    ));
}
