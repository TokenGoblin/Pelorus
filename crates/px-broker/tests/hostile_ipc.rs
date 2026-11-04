//! Hostile-peer tests for the broker's IPC paths: invariant 9 at the wire,
//! exact size limits through dispatch, and fail-closed handling of malformed
//! requests.
//!
//! Helpers avoid unwrap/expect/panic/indexing; assertions live in `#[test]`s.

use px_broker::{Broker, ChannelId, Decision, DenyReason};
use px_ipc::{
    FrameHost, FrameId, IpcError, MAX_MESSAGE_BYTES, Message, Request, Response, decode_frame,
    encode_frame,
};

/// What the supervised read path does with bytes from a channel: decode, and
/// only a successful decode reaches `dispatch_guarded`. A decode failure ends
/// the channel (the caller reaps the peer; here we close the broker side).
fn handle_wire(
    broker: &mut Broker,
    channel: ChannelId,
    bytes: &[u8],
) -> Result<Decision, IpcError> {
    match decode_frame::<Request>(bytes) {
        Ok(request) => Ok(broker.dispatch_guarded(channel, request)),
        Err(e) => {
            broker.close_channel(channel);
            Err(e)
        }
    }
}

/// A correctly framed, correctly tagged request around arbitrary postcard bytes.
fn tagged_request(postcard_bytes: &[u8]) -> Vec<u8> {
    let mut body = vec![Request::TAG];
    body.extend_from_slice(postcard_bytes);
    let mut frame = u32::try_from(body.len())
        .unwrap_or(u32::MAX)
        .to_le_bytes()
        .to_vec();
    frame.extend_from_slice(&body);
    frame
}

fn two_channels() -> Option<(Broker, ChannelId, ChannelId)> {
    let mut broker = Broker::new();
    let a = broker.open_channel()?;
    let b = broker.open_channel()?;
    Some((broker, a, b))
}

fn varint(mut v: usize) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let b = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return out;
        }
        out.push(b | 0x80);
    }
}

// ------------------------------------------------------ invariant 9, on wire

#[test]
fn identity_shaped_trailing_fields_are_rejected_for_every_shape() {
    let (mut broker, victim, _attacker) = two_channels().expect("channels");
    let frame = broker.create_frame(victim).expect("frame");
    let mut with_channel = vec![2u8, 0, 0];
    with_channel.extend_from_slice(&victim.as_u64().to_le_bytes());
    let shapes: Vec<Vec<u8>> = vec![
        vec![2, 0, 0, 1],             // FrameHost + trailing u8
        with_channel,                 // FrameHost + channel id
        vec![0, 5],                   // Ping + tab id
        vec![0, 3, b'a', b'b', b'c'], // Ping + origin string
        vec![1, 0, 0xFF],             // empty Echo + partition key
        vec![2, 0, 0, 0x0b, b'e', b'x', b'a', b'm', b'p', b'l', b'e'],
    ];
    for shape in shapes {
        let c = broker.open_channel().expect("channel");
        let result = handle_wire(&mut broker, c, &tagged_request(&shape));
        assert!(matches!(result, Err(IpcError::Malformed)), "{shape:?}");
        assert!(!broker.is_open(c), "{shape:?} must end the channel");
    }
    assert!(broker.holds_frame(victim, frame));
}

#[test]
fn identity_shaped_request_variants_do_not_exist() {
    // Any discriminant past the three real ones is where an "Identify",
    // "SetOrigin" or "AttachTab" request would live. None decodes.
    let mut broker = Broker::new();
    for d in 3u8..=127 {
        let c = broker.open_channel().expect("channel");
        let mut pc = vec![d];
        pc.extend_from_slice(&[0; 32]);
        let result = handle_wire(&mut broker, c, &tagged_request(&pc));
        assert!(matches!(result, Err(IpcError::Malformed)), "variant {d}");
        assert!(!broker.is_open(c));
    }
}

#[test]
fn echoed_bytes_that_look_like_requests_are_inert() {
    let (mut broker, victim, attacker) = two_channels().expect("channels");
    let frame = broker.create_frame(victim).expect("frame");

    // A complete, valid FrameHost frame for the victim's frame, smuggled as
    // an Echo payload. The broker must treat it as opaque bytes.
    let inner = encode_frame(&Request::FrameHost { frame }).expect("inner");
    let reply = broker.dispatch_guarded(
        attacker,
        Request::Echo {
            payload: inner.clone(),
        },
    );
    assert_eq!(reply.response, Response::Echo { payload: inner });
    assert_eq!(reply.audit, None);
    assert!(!broker.can_see(attacker, frame));
    assert!(!broker.holds_frame(attacker, frame));
}

#[test]
fn a_frame_id_built_from_channel_numbers_grants_nothing() {
    let (mut broker, victim, attacker) = two_channels().expect("channels");
    let frame = broker.create_frame(victim).expect("frame");
    let n = u32::try_from(victim.as_u64()).unwrap_or(0);
    let forged = [
        FrameId::new(n, 0),
        FrameId::new(0, n),
        FrameId::new(frame.index(), frame.generation().wrapping_add(1)),
        FrameId::new(u32::MAX, u32::MAX),
        FrameId::new(frame.index().wrapping_add(1), frame.generation()),
    ];
    for f in forged {
        let reply = broker.dispatch_guarded(attacker, Request::FrameHost { frame: f });
        assert_eq!(reply.response, Response::Denied, "{f:?}");
    }
    // Even the real handle is denied to a channel it was not given to.
    let reply = broker.dispatch_guarded(attacker, Request::FrameHost { frame });
    assert_eq!(reply.response, Response::Denied);
    assert_eq!(reply.audit, Some(DenyReason::NotYourFrame));
}

#[test]
fn the_same_bytes_get_different_answers_by_channel_alone() {
    let (mut broker, owner, other) = two_channels().expect("channels");
    let frame = broker.create_frame(owner).expect("frame");
    let bytes = encode_frame(&Request::FrameHost { frame }).expect("encode");

    let from_owner = handle_wire(&mut broker, owner, &bytes).expect("owner");
    let from_other = handle_wire(&mut broker, other, &bytes).expect("other");
    assert_eq!(
        from_owner.response,
        Response::FrameHost {
            host: FrameHost::Local
        }
    );
    assert_eq!(from_other.response, Response::Denied);
}

// --------------------------------------------------------------- size limits

/// Largest Echo payload that still fits in one frame (tag + variant + 3-byte
/// length varint).
const MAX_ECHO: usize = MAX_MESSAGE_BYTES - 5;

#[test]
fn echo_at_exactly_the_limit_is_served_end_to_end() {
    let (mut broker, a, _b) = two_channels().expect("channels");
    let request = Request::Echo {
        payload: vec![0xC3; MAX_ECHO],
    };
    let wire = encode_frame(&request).expect("request at the limit is sendable");
    let decision = handle_wire(&mut broker, a, &wire).expect("decode at the limit");
    assert_eq!(decision.audit, None);
    // The reply must itself be framable at the same limit, or the broker
    // would accept a request it cannot answer.
    let reply = encode_frame(&decision.response).expect("reply at the limit is sendable");
    assert_eq!(
        decode_frame::<Response>(&reply).expect("client decodes"),
        Response::Echo {
            payload: vec![0xC3; MAX_ECHO]
        }
    );
}

#[test]
fn echo_one_over_the_limit_never_reaches_dispatch() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");

    // The honest sender refuses.
    assert!(matches!(
        encode_frame(&Request::Echo {
            payload: vec![0; MAX_ECHO + 1]
        }),
        Err(IpcError::MessageTooLarge { .. })
    ));

    // A hostile sender frames it by hand: variant, varint length, payload.
    let n = MAX_ECHO + 1;
    let mut pc = vec![1u8];
    pc.extend_from_slice(&varint(n));
    pc.resize(pc.len() + n, 0);
    let result = handle_wire(&mut broker, a, &tagged_request(&pc));
    assert!(matches!(result, Err(IpcError::MessageTooLarge { .. })));
    assert!(!broker.is_open(a), "an oversized request ends the channel");
}

#[test]
fn echo_sizes_around_the_limit_are_exact() {
    for n in [MAX_ECHO - 1, MAX_ECHO, MAX_ECHO + 1, MAX_ECHO + 2] {
        let ok = encode_frame(&Request::Echo {
            payload: vec![1; n],
        })
        .is_ok();
        assert_eq!(ok, n <= MAX_ECHO, "payload {n}");
    }
}

#[test]
fn echo_dispatch_preserves_every_payload_byte_value() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    let payload: Vec<u8> = (0..=255u8).cycle().take(4096).collect();
    let reply = broker.dispatch_guarded(
        a,
        Request::Echo {
            payload: payload.clone(),
        },
    );
    assert_eq!(reply.response, Response::Echo { payload });
}

// ------------------------------------------------------------- fail closed

#[test]
fn malformed_requests_end_the_channel_and_serve_nothing_further() {
    let (mut broker, a, b) = two_channels().expect("channels");
    let frame_b = broker.create_frame(b).expect("frame");

    let bad: Vec<Vec<u8>> = vec![
        Vec::new(),
        vec![0, 0, 0],
        0u32.to_le_bytes().to_vec(),
        u32::MAX.to_le_bytes().to_vec(),
        tagged_request(&[0xEE]),
        tagged_request(&[0, 0]),
        encode_frame(&Response::Pong).expect("reflected response"),
    ];
    for bytes in bad {
        let c = broker.open_channel().expect("channel");
        assert!(handle_wire(&mut broker, c, &bytes).is_err(), "{bytes:?}");
        assert!(!broker.is_open(c));
        for req in [
            Request::Ping,
            Request::Echo { payload: vec![1] },
            Request::FrameHost { frame: frame_b },
        ] {
            let d = broker.dispatch_guarded(c, req);
            assert_eq!(d.response, Response::Denied);
            assert_eq!(d.audit, Some(DenyReason::UnknownChannel));
        }
    }
    // Other channels are unaffected by a neighbour's garbage.
    assert!(broker.is_open(a) && broker.is_open(b));
    assert_eq!(
        broker.dispatch_guarded(a, Request::Ping).response,
        Response::Pong
    );
    assert!(broker.holds_frame(b, frame_b));
}

#[test]
fn a_closed_channel_is_denied_for_every_request_shape() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    broker.close_channel(a);
    let shapes = [
        Request::Ping,
        Request::Echo { payload: vec![] },
        Request::Echo {
            payload: vec![0; 1000],
        },
        Request::FrameHost {
            frame: FrameId::new(0, 0),
        },
        Request::FrameHost {
            frame: FrameId::new(u32::MAX, u32::MAX),
        },
    ];
    for s in shapes {
        let d = broker.dispatch(a, s.clone());
        assert_eq!(d.response, Response::Denied, "{s:?}");
        assert_eq!(d.audit, Some(DenyReason::UnknownChannel));
    }
}

#[test]
fn a_flood_of_forged_frame_ids_is_denied_and_the_audit_log_stays_bounded() {
    let (mut broker, owner, attacker) = two_channels().expect("channels");
    let frame = broker.create_frame(owner).expect("frame");
    for i in 0..2000u32 {
        let d = broker.dispatch_guarded(
            attacker,
            Request::FrameHost {
                frame: FrameId::new(i, i.wrapping_mul(2_654_435_761)),
            },
        );
        assert_eq!(d.response, Response::Denied);
    }
    assert!(broker.audit_log().count() <= 256);
    assert!(broker.holds_frame(owner, frame));
    assert_eq!(
        broker
            .dispatch_guarded(owner, Request::FrameHost { frame })
            .response,
        Response::FrameHost {
            host: FrameHost::Local
        }
    );
}

#[test]
fn denials_for_forged_stale_and_foreign_handles_are_byte_identical_on_the_wire() {
    let (mut broker, owner, attacker) = two_channels().expect("channels");
    let live = broker.create_frame(owner).expect("frame");
    let doomed_owner = broker.open_channel().expect("channel");
    let stale = broker.create_frame(doomed_owner).expect("frame");
    broker.close_channel(doomed_owner);

    let probes = [
        live,                             // foreign
        stale,                            // owner gone
        FrameId::new(9999, 0),            // never existed
        FrameId::new(u32::MAX, u32::MAX), // forged extreme
    ];
    let mut wires = Vec::new();
    for p in probes {
        let d = broker.dispatch_guarded(attacker, Request::FrameHost { frame: p });
        wires.push(encode_frame(&d.response).expect("encode"));
    }
    let first = wires.first().expect("one").clone();
    for w in &wires {
        assert_eq!(w, &first);
    }
}
