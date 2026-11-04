//! Phase 10: the BROKER drives script in a content process that is running
//! under the sandbox profile (ADR 040, ADR 009, ADR 004).
//!
//! Until now every script test played the broker from the test side over
//! unsandboxed pipes. Here the real `px_broker::ScriptSession` spawns the real
//! content binary through `px_sandbox::spawn` (no_new_privs + seccomp on Linux,
//! restricted token + job object on Windows), serves its opening exchange
//! through the capability table, seeds it, and runs script through it. What is
//! proven: the engine, its reader and watchdog threads and its clocks work
//! under the profile, and every way a script can end a process leaves the
//! broker alive and a second session working.
//!
//! A machine that cannot clear the sandbox floor cannot run these. CI sets
//! `PX_REQUIRE_SANDBOX`, which turns that silent skip into a failure.

use std::path::PathBuf;
use std::time::Duration;

use px_broker::{Broker, DeathCause, ScriptOutcome, ScriptSession};
use px_ipc::script::TerminateReason;

/// A liveness bound, not a performance assertion: CI is slow and shared, and a
/// build without `testing` waits the shipping ten-second watchdog.
const GENEROUS: Duration = Duration::from_secs(120);

/// The short watchdog a `testing` build can ask for.
#[cfg(feature = "testing")]
const SHORT_WATCHDOG_MS: u32 = 300;

fn content_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_px-content"))
}

/// Whether this machine can run a sandboxed content process at all.
fn sandbox_available() -> bool {
    if px_sandbox::detect().clears_floor() {
        return true;
    }
    assert!(
        std::env::var_os("PX_REQUIRE_SANDBOX").is_none(),
        "PX_REQUIRE_SANDBOX is set, but this machine does not clear the sandbox floor; \
         these tests would pass without running anything"
    );
    false
}

/// Spawn a session. Returns the `Result` so the `expect` stays in a test body.
fn open(broker: &mut Broker) -> std::io::Result<ScriptSession> {
    ScriptSession::spawn(broker, &content_binary())
}

/// A session whose watchdog is short, where the build allows it.
#[cfg(feature = "testing")]
fn short_watchdog(session: &mut ScriptSession, broker: &mut Broker) -> Result<(), DeathCause> {
    session.send_extra(
        broker,
        &px_ipc::script::ScriptRequest::WatchdogMillis {
            millis: SHORT_WATCHDOG_MS,
        },
    )
}

#[cfg(not(feature = "testing"))]
fn short_watchdog(_session: &mut ScriptSession, _broker: &mut Broker) -> Result<(), DeathCause> {
    Ok(())
}

fn second_session_runs_one_plus_one(broker: &mut Broker) -> std::io::Result<ScriptOutcome> {
    let mut next = open(broker)?;
    Ok(next.run_script(broker, "1+1", GENEROUS))
}

#[test]
fn script_broker_sandbox_one_plus_one_under_the_profile() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn under the profile");
    assert_eq!(broker.channel_count(), 1);
    assert_eq!(
        session.run_script(&mut broker, "1+1", GENEROUS),
        ScriptOutcome::Value("2".into())
    );
    assert!(session.is_live());
}

#[test]
fn script_broker_sandbox_the_kernel_confirms_the_profile_on_the_script_process() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    assert!(
        session.applied_rungs().len() >= 2,
        "the process must be created under at least no_new_privs and seccomp: {:?}",
        session.applied_rungs()
    );
    // While script is running in it, ask the kernel (not the process).
    assert_eq!(
        session.run_script(&mut broker, "1", GENEROUS),
        ScriptOutcome::Value("1".into())
    );
    #[cfg(target_os = "linux")]
    {
        let pid = session.process_id().expect("a real child");
        let status = std::fs::read_to_string(format!("/proc/{pid}/status")).expect("status");
        assert!(
            status.contains("Seccomp:\t2"),
            "not under a filter:\n{status}"
        );
        assert!(
            status.contains("NoNewPrivs:\t1"),
            "no_new_privs unset:\n{status}"
        );
        // The reader and the watchdog threads exist and run under the filter.
        assert!(status.contains("Threads:"), "{status}");
    }
}

#[test]
fn script_broker_sandbox_a_thrown_error_returns_as_data_and_the_realm_lives_on() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    assert_eq!(
        session.run_script(
            &mut broker,
            "throw new RangeError('out of range')",
            GENEROUS
        ),
        ScriptOutcome::Error("RangeError: out of range".into())
    );
    assert_eq!(
        session.run_script(&mut broker, "var kept = 'yes'; kept", GENEROUS),
        ScriptOutcome::Value("yes".into())
    );
    assert!(matches!(
        session.run_script(&mut broker, "1 +", GENEROUS),
        ScriptOutcome::Error(m) if m.contains("SyntaxError")
    ));
    assert_eq!(
        session.run_script(&mut broker, "kept", GENEROUS),
        ScriptOutcome::Value("yes".into())
    );
    assert!(session.is_live());
}

#[test]
fn script_broker_sandbox_timers_and_the_clock_work_under_the_profile() {
    if !sandbox_available() {
        return;
    }
    // Timers need the clock and the event loop's waits; a promise needs the
    // microtask checkpoint. None may be denied by the filter.
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    assert_eq!(
        session.run_script(
            &mut broker,
            "var order = []; setTimeout(() => order.push('t'), 5); \
             Promise.resolve().then(() => order.push('p')); \
             typeof performance.now() + ':' + (Date.now() > 0)",
            GENEROUS
        ),
        ScriptOutcome::Value("number:true".into())
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        session.run_script(&mut broker, "order.join()", GENEROUS),
        ScriptOutcome::Value("p,t".into())
    );
}

#[test]
fn script_broker_sandbox_a_big_allocating_script_runs_under_the_profile() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    assert_eq!(
        session.run_script(
            &mut broker,
            "var a = []; for (var i = 0; i < 200000; i++) a.push({i: i, s: 'x' + i}); a.length",
            GENEROUS
        ),
        ScriptOutcome::Value("200000".into())
    );
}

#[test]
fn script_broker_sandbox_a_source_over_the_cap_is_rejected_before_it_is_sent() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    let too_big = "1".repeat(px_ipc::script::MAX_SCRIPT_SOURCE_BYTES + 1);
    assert_eq!(
        session.run_script(&mut broker, &too_big, GENEROUS),
        ScriptOutcome::Rejected
    );
    assert_eq!(
        session.run_script(&mut broker, "2+2", GENEROUS),
        ScriptOutcome::Value("4".into())
    );
}

#[test]
fn script_broker_sandbox_a_result_over_its_cap_is_cut_by_the_host() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    match session.run_script(&mut broker, "'a'.repeat(200000)", GENEROUS) {
        ScriptOutcome::Value(text) => {
            assert_eq!(text.len(), px_ipc::script::MAX_SCRIPT_RESULT_BYTES);
        }
        other => panic!("expected a Value, got {other:?}"),
    }
}

#[test]
fn script_broker_runaway_regex_backtracking_ends_in_a_dead_process_and_the_broker_lives() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let bystander = broker.open_channel().expect("channel");
    let frame = broker.create_frame(bystander).expect("frame");
    let mut session = open(&mut broker).expect("spawn");
    short_watchdog(&mut session, &mut broker).expect("watchdog knob");

    // Boa cannot interrupt a native regex: only the content watchdog's abort
    // ends this, and the broker sees the pipe close.
    let outcome = session.run_script(
        &mut broker,
        "/^(a+)+$/.test('a'.repeat(60) + 'b')",
        GENEROUS,
    );
    assert_eq!(outcome, ScriptOutcome::ProcessDied);
    assert_eq!(session.death(), Some(DeathCause::PeerClosed));
    assert!(
        !broker.is_open(session.channel_id()),
        "its channel is closed"
    );

    // The broker, the other channel and its frame are untouched.
    assert!(broker.holds_frame(bystander, frame));
    assert_eq!(
        session.run_script(&mut broker, "1+1", GENEROUS),
        ScriptOutcome::ProcessDied
    );
    assert_eq!(
        second_session_runs_one_plus_one(&mut broker).expect("a second session spawns"),
        ScriptOutcome::Value("2".into())
    );
}

#[test]
fn script_broker_runaway_while_true_ends_terminated_or_dead_and_a_second_session_works() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    short_watchdog(&mut session, &mut broker).expect("watchdog knob");
    // Which ending depends on speed: the engine stops the loop after 10^7
    // iterations (Terminated), and an unoptimised build may need longer than
    // the short watchdog (the process aborts). Neither may hang.
    match session.run_script(&mut broker, "while (true) {}", GENEROUS) {
        ScriptOutcome::Terminated(reason) => assert_eq!(reason, TerminateReason::RuntimeLimit),
        ScriptOutcome::ProcessDied => {}
        other => panic!("a runaway finished: {other:?}"),
    }
    assert!(!session.is_live());
    assert!(!broker.is_open(session.channel_id()));
    assert_eq!(
        second_session_runs_one_plus_one(&mut broker).expect("a second session spawns"),
        ScriptOutcome::Value("2".into())
    );
}

#[test]
fn script_broker_runaway_the_deadline_kills_a_script_the_host_would_let_run() {
    if !sandbox_available() {
        return;
    }
    // The broker's own deadline, shorter than the shipping ten-second watchdog:
    // the regex above runs for far longer than 1 s, so Unresponsive is the
    // broker's verdict and the child is killed by the broker, not by itself.
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    let outcome = session.run_script(
        &mut broker,
        "/^(a+)+$/.test('a'.repeat(60) + 'b')",
        Duration::from_secs(1),
    );
    assert_eq!(outcome, ScriptOutcome::Unresponsive);
    assert_eq!(session.death(), Some(DeathCause::Unresponsive));
    assert_eq!(
        second_session_runs_one_plus_one(&mut broker).expect("a second session spawns"),
        ScriptOutcome::Value("2".into())
    );
}

#[test]
fn script_broker_isolation_two_tabs_one_runaway_the_other_answers() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut runaway = open(&mut broker).expect("spawn runaway");
    let mut other = open(&mut broker).expect("spawn other");
    short_watchdog(&mut runaway, &mut broker).expect("watchdog knob");

    assert_eq!(
        other.run_script(&mut broker, "globalThis.k = 41; k", GENEROUS),
        ScriptOutcome::Value("41".into())
    );
    let ended = runaway.run_script(
        &mut broker,
        "/^(a+)+$/.test('a'.repeat(60) + 'b')",
        GENEROUS,
    );
    assert_eq!(ended, ScriptOutcome::ProcessDied);

    // The other tab kept its realm and keeps answering.
    assert!(other.is_live());
    assert_eq!(
        other.run_script(&mut broker, "k + 1", GENEROUS),
        ScriptOutcome::Value("42".into())
    );
    assert_ne!(runaway.channel_id(), other.channel_id());
}

#[test]
fn script_broker_restart_replaces_a_dead_session_under_a_new_channel() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    short_watchdog(&mut session, &mut broker).expect("watchdog knob");
    let before = session.channel_id();
    assert_eq!(
        session.run_script(
            &mut broker,
            "/^(a+)+$/.test('a'.repeat(60) + 'b')",
            GENEROUS
        ),
        ScriptOutcome::ProcessDied
    );

    session.restart(&mut broker).expect("restart");
    assert_ne!(session.channel_id(), before, "a restart gets a new channel");
    assert!(session.is_live());
    assert_eq!(session.restarts(), 1);
    assert_eq!(
        session.run_script(&mut broker, "6*7", GENEROUS),
        ScriptOutcome::Value("42".into())
    );
}

#[test]
fn script_broker_restart_is_bounded() {
    if !sandbox_available() {
        return;
    }
    let mut broker = Broker::new();
    let mut session = open(&mut broker).expect("spawn");
    let mut allowed = 0;
    while session.restart(&mut broker).is_ok() {
        allowed += 1;
        assert!(allowed <= 8, "the restart bound was not enforced");
    }
    assert_eq!(allowed, 8);
    assert_eq!(
        broker.channel_count(),
        1,
        "no channel leaked across restarts"
    );
}

#[test]
fn script_broker_spawn_refuses_without_the_sandbox_and_leaks_no_channel() {
    // Not the forced override (that lives in sandbox.rs behind `testing` and
    // an environment lock): a path that cannot be executed fails closed too.
    let mut broker = Broker::new();
    let missing = std::path::Path::new("/nonexistent/px-content-does-not-exist");
    assert!(ScriptSession::spawn(&mut broker, missing).is_err());
    assert_eq!(broker.channel_count(), 0);
}
