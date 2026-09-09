//! Phase 1 gate: killing the content process is recovered from cleanly.
//!
//! These are the only tests in the phase that cross a real process boundary.
//! Everything else drives the codec and the broker in memory, which is right
//! for fuzzing and for speed — but a process boundary that has only ever been
//! simulated is a process boundary nobody has tested. Spawning is also where
//! Windows and Linux differ most, so this suite runs on both in CI.
//!
//! It lives in `px-content` rather than `px-broker` because
//! `CARGO_BIN_EXE_px-content` is only set for this package's own tests.

use std::path::PathBuf;

use px_broker::{Broker, ContentProcess};
use px_ipc::{DenyReason, FrameHost, Request, Response};

fn content_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_px-content"))
}

#[test]
fn the_boundary_is_a_real_process_and_it_answers() {
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");

    assert!(content.is_alive());
    let response = content
        .round_trip(&Request::Echo {
            payload: b"across a process boundary".to_vec(),
        })
        .expect("round trip");
    assert_eq!(
        response,
        Response::Echo {
            payload: b"across a process boundary".to_vec()
        }
    );
}

#[test]
fn crash_restart_a_killed_process_is_detected() {
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");
    assert!(content.is_alive());

    content.kill().expect("kill");
    assert!(!content.is_alive(), "a killed process must read as dead");

    // §4.3: the broker treats a dead content process as routine. Talking to
    // one must produce an error, never a hang and never a panic.
    let result = content.round_trip(&Request::Ping);
    assert!(
        result.is_err(),
        "a request to a dead process must fail, got {result:?}"
    );
}

#[test]
fn crash_restart_replaces_the_process_and_it_works_again() {
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");
    let before = content.channel_id();

    content.kill().expect("kill");
    content.restart(&mut broker).expect("restart");

    assert!(content.is_alive());
    assert_ne!(
        before,
        content.channel_id(),
        "a restarted process must get a new channel, not inherit the old one"
    );

    let response = content
        .round_trip(&Request::Echo {
            payload: b"after restart".to_vec(),
        })
        .expect("round trip after restart");
    assert_eq!(
        response,
        Response::Echo {
            payload: b"after restart".to_vec()
        }
    );
}

#[test]
fn crash_restart_drops_every_capability_the_process_held() {
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");

    let frame = broker.create_frame(content.channel_id()).expect("frame");
    assert_eq!(
        broker.dispatch(content.channel_id(), Request::FrameHost { frame }),
        Response::FrameHost {
            host: FrameHost::Local
        }
    );

    content.kill().expect("kill");
    content.restart(&mut broker).expect("restart");

    // The same principle §7.3 states for px-mcp: a restarted subsystem begins
    // with zero authority. A process that can crash its way back to its old
    // capabilities can crash its way into somebody else's.
    assert_eq!(
        broker.dispatch(content.channel_id(), Request::FrameHost { frame }),
        Response::Denied {
            reason: DenyReason::NoSuchFrame
        },
        "a restarted process must not inherit the frames of the one it replaced"
    );
}

#[test]
fn crash_restart_survives_repeated_death() {
    // A crash loop must not accumulate channels, processes, or authority.
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");

    for round in 0..5 {
        content.kill().expect("kill");
        content.restart(&mut broker).expect("restart");
        assert!(content.is_alive(), "round {round}");
        assert_eq!(
            broker.channel_count(),
            1,
            "round {round}: dead channels must not accumulate"
        );
    }

    assert_eq!(
        content.round_trip(&Request::Ping).expect("ping"),
        Response::Pong
    );
}

#[test]
fn size_limit_holds_across_a_real_pipe() {
    // The in-memory tests prove the decoder's arithmetic. This proves the
    // limit is actually reached on a real transport, where a large write is
    // split across many pipe buffers.
    let mut broker = Broker::new();
    let mut content = ContentProcess::spawn(&mut broker, &content_binary()).expect("spawn");

    let oversized = Request::Echo {
        payload: vec![0u8; px_ipc::MAX_MESSAGE_BYTES + 1],
    };
    let result = content.round_trip(&oversized);
    assert!(
        result.is_err(),
        "an oversized message must be refused before it is written, got {result:?}"
    );

    // And the channel is still usable: refusing to send must not have written
    // a partial frame that desynchronises the stream.
    assert!(content.is_alive());
    assert_eq!(
        content.round_trip(&Request::Ping).expect("ping"),
        Response::Pong
    );
}
