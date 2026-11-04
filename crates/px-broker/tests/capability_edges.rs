//! The broker's capability table from outside the crate: the audit trail, the
//! forged-handle space, visibility versus ownership, and bounds.
//!
//! The in-crate tests cover the headline confused-deputy cases. These pin the
//! edges around them, using only the public API a caller of the broker has.

use px_broker::{Broker, DenyReason, FrameTree};
use px_ipc::{FrameHost, FrameId, Request, Response};

fn host_of(broker: &mut Broker, channel: px_broker::ChannelId, frame: FrameId) -> Response {
    broker
        .dispatch(channel, Request::FrameHost { frame })
        .response
}

#[test]
fn broker_allowed_requests_leave_no_audit_entry() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    let frame = broker.create_frame(a).expect("frame");
    assert_eq!(
        broker.dispatch_guarded(a, Request::Ping).response,
        Response::Pong
    );
    assert_eq!(
        broker
            .dispatch_guarded(a, Request::FrameHost { frame })
            .response,
        Response::FrameHost {
            host: FrameHost::Local
        }
    );
    assert_eq!(broker.audit_log().count(), 0);
}

/// The wire answer is a bare Denied for every refusal; the log, and only the
/// log, says which kind it was.
#[test]
fn broker_audit_records_the_reason_and_the_channel_in_order() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("a");
    let b = broker.open_channel().expect("b");
    let a_frame = broker.create_frame(a).expect("frame");
    let forged = FrameId::new(9_999, 0);

    let first = broker.dispatch_guarded(b, Request::FrameHost { frame: a_frame });
    let second = broker.dispatch_guarded(b, Request::FrameHost { frame: forged });
    broker.close_channel(a);
    let third = broker.dispatch_guarded(a, Request::Ping);

    for decision in [&first, &second, &third] {
        assert_eq!(decision.response, Response::Denied);
    }
    // After `a` closed, its frame no longer exists at all, so b probing it
    // is NoSuchFrame, not NotYourFrame.
    let fourth = broker.dispatch_guarded(b, Request::FrameHost { frame: a_frame });
    assert_eq!(fourth.audit, Some(DenyReason::NoSuchFrame));

    let log: Vec<_> = broker.audit_log().collect();
    assert_eq!(
        log,
        vec![
            (b, DenyReason::NotYourFrame),
            (b, DenyReason::NoSuchFrame),
            (a, DenyReason::UnknownChannel),
            (b, DenyReason::NoSuchFrame),
        ]
    );
}

#[test]
fn broker_extreme_frame_handles_are_denied_without_panicking() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    let _real = broker.create_frame(a).expect("frame");
    for (index, generation) in [
        (u32::MAX, 0),
        (0, u32::MAX),
        (u32::MAX, u32::MAX),
        (1, 0),
        (0, 1),
    ] {
        let frame = FrameId::new(index, generation);
        assert_eq!(
            host_of(&mut broker, a, frame),
            Response::Denied,
            "({index}, {generation}) is not a live handle"
        );
        assert!(!broker.can_see(a, frame));
        assert!(!broker.holds_frame(a, frame));
        assert!(!broker.grant_visibility(a, frame));
    }
}

#[test]
fn broker_visibility_grant_lets_a_viewer_ask_but_never_own() {
    let mut broker = Broker::new();
    let owner = broker.open_channel().expect("owner");
    let viewer = broker.open_channel().expect("viewer");
    let frame = broker.create_frame(owner).expect("frame");

    assert!(!broker.can_see(viewer, frame));
    assert!(broker.grant_visibility(viewer, frame));
    assert!(broker.can_see(viewer, frame));
    assert!(!broker.holds_frame(viewer, frame));
    assert_eq!(
        host_of(&mut broker, viewer, frame),
        Response::FrameHost {
            host: FrameHost::Remote
        }
    );
    assert_eq!(
        host_of(&mut broker, owner, frame),
        Response::FrameHost {
            host: FrameHost::Local
        }
    );
}

#[test]
fn broker_visibility_cannot_be_granted_to_a_closed_channel() {
    let mut broker = Broker::new();
    let owner = broker.open_channel().expect("owner");
    let viewer = broker.open_channel().expect("viewer");
    let frame = broker.create_frame(owner).expect("frame");
    broker.close_channel(viewer);
    assert!(!broker.grant_visibility(viewer, frame));
    assert!(!broker.can_see(viewer, frame));
}

#[test]
fn broker_closing_the_owner_revokes_every_viewers_access() {
    let mut broker = Broker::new();
    let owner = broker.open_channel().expect("owner");
    let viewer = broker.open_channel().expect("viewer");
    let frame = broker.create_frame(owner).expect("frame");
    assert!(broker.grant_visibility(viewer, frame));

    broker.close_channel(owner);
    assert!(!broker.can_see(viewer, frame));
    assert_eq!(host_of(&mut broker, viewer, frame), Response::Denied);
}

#[test]
fn broker_closing_a_viewer_does_not_disturb_the_owner() {
    let mut broker = Broker::new();
    let owner = broker.open_channel().expect("owner");
    let viewer = broker.open_channel().expect("viewer");
    let frame = broker.create_frame(owner).expect("frame");
    assert!(broker.grant_visibility(viewer, frame));
    broker.close_channel(viewer);
    assert!(broker.holds_frame(owner, frame));
    assert_eq!(broker.channel_count(), 1);
}

#[test]
fn broker_close_is_idempotent_and_closing_twice_cannot_revive_anything() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    let frame = broker.create_frame(a).expect("frame");
    broker.close_channel(a);
    broker.close_channel(a);
    assert!(!broker.is_open(a));
    assert_eq!(broker.channel_count(), 0);
    assert!(
        broker.create_frame(a).is_none(),
        "no frames for a closed channel"
    );
    assert_eq!(host_of(&mut broker, a, frame), Response::Denied);
}

#[test]
fn broker_frame_ids_are_unique_across_channels_while_alive() {
    let mut broker = Broker::new();
    let mut seen = Vec::new();
    for _ in 0..8 {
        let channel = broker.open_channel().expect("channel");
        for _ in 0..8 {
            let frame = broker.create_frame(channel).expect("frame");
            assert!(!seen.contains(&frame), "{frame:?} handed out twice");
            seen.push(frame);
        }
    }
    assert_eq!(seen.len(), 64);
}

#[test]
fn broker_a_slot_reused_after_close_gets_a_new_generation() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("a");
    let old = broker.create_frame(a).expect("frame");
    broker.close_channel(a);
    let b = broker.open_channel().expect("b");
    let new = broker.create_frame(b).expect("frame");
    assert_eq!(new.index(), old.index(), "the free slot is reused");
    assert_ne!(new.generation(), old.generation());
    assert!(!broker.can_see(b, old), "the stale handle must not resolve");
    assert_eq!(host_of(&mut broker, b, old), Response::Denied);
}

#[test]
fn broker_echo_returns_the_payload_unchanged_at_the_message_size_limit() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("channel");
    for payload in [
        Vec::new(),
        vec![0u8],
        (0..=255u8).collect::<Vec<_>>(),
        vec![0xa5; px_ipc::MAX_MESSAGE_BYTES / 2],
    ] {
        let decision = broker.dispatch(
            a,
            Request::Echo {
                payload: payload.clone(),
            },
        );
        assert_eq!(decision.response, Response::Echo { payload });
        assert_eq!(decision.audit, None);
    }
}

#[test]
fn broker_audit_log_never_exceeds_its_capacity_and_drops_oldest() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("a");
    let b = broker.open_channel().expect("b");
    // One distinguishable early entry, then a flood.
    broker.close_channel(b);
    broker.dispatch_guarded(b, Request::Ping);
    for i in 0..1000u32 {
        broker.dispatch_guarded(
            a,
            Request::FrameHost {
                frame: FrameId::new(100 + i, 0),
            },
        );
    }
    let log: Vec<_> = broker.audit_log().collect();
    assert_eq!(log.len(), 256);
    assert!(
        log.iter().all(|(channel, _)| *channel == a),
        "the early UnknownChannel entry was evicted, oldest first"
    );
}

#[test]
fn broker_frame_tree_destroy_is_owner_only_and_single_shot() {
    let mut broker = Broker::new();
    let owner = broker.open_channel().expect("owner");
    let other = broker.open_channel().expect("other");

    let mut tree = FrameTree::default();
    let frame = tree.create(owner).expect("frame");
    assert_eq!(tree.owner(frame), Some(owner));
    assert!(!tree.destroy(other, frame), "not the owner");
    assert_eq!(tree.owner(frame), Some(owner));
    assert!(!tree.destroy(owner, FrameId::new(frame.index(), frame.generation() + 1)));
    assert!(tree.destroy(owner, frame));
    assert!(!tree.destroy(owner, frame), "already gone");
    assert_eq!(tree.owner(frame), None);
}

#[test]
fn broker_frame_tree_release_all_frees_only_that_channels_frames() {
    let mut broker = Broker::new();
    let a = broker.open_channel().expect("a");
    let b = broker.open_channel().expect("b");
    let mut tree = FrameTree::default();
    let fa = tree.create(a).expect("fa");
    let fb = tree.create(b).expect("fb");
    tree.release_all(a);
    assert_eq!(tree.owner(fa), None);
    assert_eq!(tree.owner(fb), Some(b));
}

#[test]
fn broker_channel_ids_are_distinct_and_increasing() {
    let mut broker = Broker::new();
    let ids: Vec<u64> = (0..16)
        .map(|_| broker.open_channel().expect("channel").as_u64())
        .collect();
    for pair in ids.windows(2) {
        if let [x, y] = pair {
            assert!(x < y);
        }
    }
}
