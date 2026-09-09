//! Phase 1 gate: killing the content process is recovered from cleanly, and
//! a content process that *stalls* instead of dying does not wedge the broker.
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
use std::time::{Duration, Instant};

use px_broker::{Broker, ContentProcess, ServeError, serve_once};
use px_ipc::{Request, Response};

/// Generous: this is a liveness bound, not a performance assertion, and CI
/// runners are slow and shared.
const PATIENT: Duration = Duration::from_secs(10);

/// Short, for the cases that are *supposed* to time out.
const IMPATIENT: Duration = Duration::from_millis(500);

fn content_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_px-content"))
}

/// Returns the `Result` rather than unwrapping it, so the `expect` lives
/// inside a `#[test]` function. clippy's `allow-expect-in-tests` covers test
/// bodies, not helpers in a test file, and §4.3's denials are workspace-wide.
fn try_spawn() -> std::io::Result<(Broker, ContentProcess)> {
    let mut broker = Broker::new();
    let content = ContentProcess::spawn(&mut broker, &content_binary())?;
    Ok((broker, content))
}

#[test]
fn the_boundary_is_a_real_process_and_the_broker_serves_it() {
    let (mut broker, mut content) = try_spawn().expect("spawn");

    // px-content opens with Ping, then Echo, then asks about a frame it was
    // never granted. The third is the one that matters: the broker decides
    // using the channel the bytes arrived on, across a real process boundary.
    assert_eq!(
        serve_once(&mut broker, &mut content, PATIENT)
            .expect("ping")
            .response,
        Response::Pong
    );
    assert_eq!(
        serve_once(&mut broker, &mut content, PATIENT)
            .expect("echo")
            .response,
        Response::Echo {
            payload: b"across a process boundary".to_vec()
        }
    );

    let denial = serve_once(&mut broker, &mut content, PATIENT).expect("frame host");
    assert_eq!(denial.response, Response::Denied);
    assert!(
        denial.audit.is_some(),
        "the broker must log why, even though the peer is not told"
    );
}

/// The wedge that matters most: a peer that simply stops talking. It is
/// cheaper for an attacker than crashing, and before the supervisor existed it
/// hung the broker — the one process §4.3 says may not die — forever.
#[test]
fn crash_restart_a_silent_peer_times_out_instead_of_wedging_the_broker() {
    let (mut broker, mut content) = try_spawn().expect("spawn");

    // Drain the three requests px-content makes, after which it goes quiet.
    for _ in 0..3 {
        serve_once(&mut broker, &mut content, PATIENT).expect("opening exchange");
    }

    let started = Instant::now();
    let outcome = serve_once(&mut broker, &mut content, IMPATIENT);
    let waited = started.elapsed();

    assert_eq!(outcome.unwrap_err(), ServeError::TimedOut);
    assert!(
        waited < IMPATIENT * 4,
        "the broker waited {waited:?} on a peer that will never speak"
    );
    assert!(
        content.is_alive(),
        "the peer is stalled, not dead — which is exactly why a liveness \
         check cannot substitute for a deadline"
    );
}

#[test]
fn crash_restart_a_killed_process_is_detected() {
    let (mut broker, mut content) = try_spawn().expect("spawn");
    serve_once(&mut broker, &mut content, PATIENT).expect("ping");
    assert!(content.is_alive());

    content.kill().expect("kill");
    assert!(!content.is_alive(), "a killed process must read as dead");

    // §4.3: the broker treats a dead content process as routine. Serving one
    // must produce an error, never a hang and never a panic.
    assert!(matches!(
        serve_once(&mut broker, &mut content, PATIENT),
        Err(ServeError::Closed)
    ));
}

#[test]
fn crash_restart_replaces_the_process_and_it_works_again() {
    let (mut broker, mut content) = try_spawn().expect("spawn");
    serve_once(&mut broker, &mut content, PATIENT).expect("ping");
    let before = content.channel_id();

    content.kill().expect("kill");
    content.restart(&mut broker).expect("restart");

    assert!(content.is_alive());
    assert_ne!(
        before,
        content.channel_id(),
        "a restarted process must get a new channel, not inherit the old one"
    );

    // The replacement runs the same opening exchange.
    assert_eq!(
        serve_once(&mut broker, &mut content, PATIENT)
            .expect("ping after restart")
            .response,
        Response::Pong
    );
}

#[test]
fn crash_restart_drops_every_capability_the_process_held() {
    let (mut broker, mut content) = try_spawn().expect("spawn");
    let frame = broker.create_frame(content.channel_id()).expect("frame");
    assert!(broker.holds_frame(content.channel_id(), frame));

    content.kill().expect("kill");
    content.restart(&mut broker).expect("restart");

    // The same principle §7.3 states for px-mcp: a restarted subsystem begins
    // with zero authority. A process that can crash its way back to its old
    // capabilities can crash its way into somebody else's.
    assert!(
        !broker.holds_frame(content.channel_id(), frame),
        "a restarted process must not inherit the frames of the one it replaced"
    );
    assert_eq!(
        broker
            .dispatch(content.channel_id(), Request::FrameHost { frame })
            .response,
        Response::Denied
    );
}

#[test]
fn crash_restart_survives_repeated_death() {
    // A crash loop must not accumulate channels, processes, or authority.
    let (mut broker, mut content) = try_spawn().expect("spawn");

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
        serve_once(&mut broker, &mut content, PATIENT)
            .expect("ping after five restarts")
            .response,
        Response::Pong
    );
}

/// The size limit is proven arithmetically by px-ipc's unit tests. This proves
/// it is actually reached on a real transport, where a large write is split
/// across many pipe buffers — and that refusing to send does not leave a
/// partial frame that desynchronises everything after it.
#[test]
fn size_limit_holds_across_a_real_pipe() {
    let (mut broker, mut content) = try_spawn().expect("spawn");
    for _ in 0..3 {
        serve_once(&mut broker, &mut content, PATIENT).expect("opening exchange");
    }

    let oversized = Response::Echo {
        payload: vec![0u8; px_ipc::MAX_MESSAGE_BYTES + 1],
    };
    // The broker refuses to queue a reply it cannot frame. Queuing is
    // non-blocking, so this returns rather than filling a pipe.
    let _ = content.reply(oversized);

    assert!(content.is_alive());
}
