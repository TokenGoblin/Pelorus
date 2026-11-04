//! Phase 10 unit 4: runaway scripts against the real content binary, over its
//! real pipes (ADR 040, "Resource limits against runaway scripts").
//!
//! The test is the broker: it spawns the process, speaks the script protocol,
//! and judges only what a broker can observe. Every runaway ends one of two
//! ways, and the tests say which, because the difference is the point:
//!
//! * **Terminated.** An engine limit (layer 1) or a loop bound stopped script;
//!   the process says so and stays up. Boa can end these.
//! * **Dead.** The process aborted itself (layer 2, the watchdog, SIGABRT on
//!   Unix). Boa cannot interrupt a native builtin, so these are the shapes
//!   only the watchdog ends.
//!
//! In both cases this test process (the stand-in for the broker) is alive, the
//! next process runs `1+1`, and another process running at the same time never
//! stops answering. The broker's own wiring of `RunScript` is not built yet
//! (docs/backlog.md), so the "broker" here is the test peer; what is proven is
//! the property at the process boundary, which is the only place it can be.
//!
//! The watchdog is shortened with a `testing`-only request. A build without
//! the feature waits the shipping ten seconds instead, which the bounds allow.

// `cfg(test)` is how clippy knows helper code in an integration test is test
// code (clippy.toml allows expect in tests); it is always true here.
#[cfg(test)]
mod common;

use std::time::Duration;

use common::{Ended, GENEROUS, Peer, SHORT_WATCHDOG_MS, value, was_aborted};
use px_ipc::script::TerminateReason;

/// A process whose watchdog is short (under `testing`).
#[cfg(test)]
fn peer() -> Peer {
    let mut peer = Peer::spawn(1);
    peer.set_watchdog_ms(SHORT_WATCHDOG_MS);
    peer
}

/// What every ending must leave behind: a fresh process that works.
#[cfg(test)]
fn the_next_process_runs_one_plus_one() {
    let mut next = Peer::spawn(2);
    assert_eq!(next.run("1+1"), value("2"));
}

/// The process must be dead by its own `abort()`.
#[cfg(test)]
fn assert_aborted(ended: Ended, peer: &mut Peer) {
    match ended {
        Ended::Dead(status) => {
            assert!(was_aborted(status), "died, but not by abort: {status:?}");
            assert!(peer.recv().is_err(), "a dead process said something");
        }
        other => panic!("expected the watchdog to end the process, got {other:?}"),
    }
}

/// The process must have said script is stopped, and still be up.
#[cfg(test)]
fn assert_terminated(ended: Ended, peer: &mut Peer, reason: TerminateReason) {
    match ended {
        Ended::Terminated(got) => assert_eq!(got, reason),
        other => panic!("expected Terminated({reason:?}), got {other:?}"),
    }
    assert!(peer.child.try_wait().expect("try_wait").is_none());
    // And nothing runs afterwards.
    assert!(matches!(
        peer.run("1+1"),
        px_ipc::script::ScriptResult::Terminated { .. }
    ));
}

// ---- ended by the engine or the loop's own bounds: Terminated ---------------

#[test]
fn script_limits_runaway_deep_recursion_is_terminated_by_the_engine() {
    let mut peer = peer();
    let ended = peer.run_runaway("function f() { return f() + 1 } f()", GENEROUS);
    assert_terminated(ended, &mut peer, TerminateReason::RuntimeLimit);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_catch_and_continue_is_terminated_by_the_engine() {
    let mut peer = peer();
    // Boa's limit error is uncatchable, so the catch never runs and the outer
    // loop never gets a second turn.
    let ended = peer.run_runaway(
        "function f() { f() } for (;;) { try { f() } catch (e) {} }",
        GENEROUS,
    );
    assert_terminated(ended, &mut peer, TerminateReason::RuntimeLimit);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_an_endless_loop_ends_in_terminated_or_a_dead_process() {
    let mut peer = peer();
    // Which one depends on speed: the engine stops it after 10,000,000
    // iterations, and an unoptimised build may need longer than the short
    // watchdog to get there. Both are acceptable; neither may hang.
    match peer.run_runaway("while (true) {}", GENEROUS) {
        Ended::Terminated(reason) => assert_eq!(reason, TerminateReason::RuntimeLimit),
        Ended::Dead(status) => assert!(was_aborted(status), "{status:?}"),
        other => panic!("a runaway finished: {other:?}"),
    }
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_a_self_requeuing_microtask_is_terminated_by_the_budget() {
    let mut peer = peer();
    // Depth one forever, never yields: only the per-checkpoint budget catches
    // it. Every microtask is a heartbeat, so the watchdog never fires.
    let ended = peer.run_runaway("function f() { queueMicrotask(f) } f()", GENEROUS);
    assert_terminated(ended, &mut peer, TerminateReason::MicrotaskBudget);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_a_self_rearming_timer_never_starves_the_process() {
    let mut peer = peer();
    // Not a hang: every callback is a task boundary, so the process stays
    // responsive (and the watchdog stays quiet) while the page spins forever.
    // The nesting clamp (4 ms after five levels) is what keeps it cheap.
    assert_eq!(
        peer.run("var n = 0; function f() { n++; setTimeout(f, 0) } f(); 'armed'"),
        value("armed")
    );
    std::thread::sleep(Duration::from_millis(SHORT_WATCHDOG_MS as u64 * 3));
    assert!(peer.child.try_wait().expect("try_wait").is_none());
    assert_eq!(peer.run("n > 5"), value("true"));
    assert_eq!(peer.run("1+1"), value("2"));
    the_next_process_runs_one_plus_one();
}

// ---- only the watchdog can end these: Dead -----------------------------------

#[test]
fn script_limits_runaway_regex_catastrophic_backtracking_is_ended_by_the_watchdog() {
    let mut peer = peer();
    let ended = peer.run_runaway("/^(a+)+$/.test('a'.repeat(60) + 'b')", GENEROUS);
    assert_aborted(ended, &mut peer);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_a_calls_in_loops_tree_is_ended_by_the_watchdog() {
    let mut peer = peer();
    // The per-frame loop limit never sees this: each call starts a fresh count.
    // 3,000 iterations at four levels is 8e13.
    let ended = peer.run_runaway(
        "function a() { for (var i = 0; i < 3000; i++) b() } \
         function b() { for (var i = 0; i < 3000; i++) c() } \
         function c() { for (var i = 0; i < 3000; i++) d() } \
         function d() { for (var i = 0; i < 3000; i++) {} } a()",
        GENEROUS,
    );
    assert_aborted(ended, &mut peer);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_array_fill_of_four_billion_is_ended_by_the_watchdog() {
    let mut peer = peer();
    // `fill` is a native loop over 2**32-1 indices: no bytecode loop, so no
    // limit counts it, and no task boundary passes. Measured: the property
    // writes are slow enough that memory stays small for the first seconds
    // (about 20 MiB in 1.5 s, unoptimised); the time bound is what ends it.
    let ended = peer.run_runaway("new Array(2**32-1).fill(1).length", GENEROUS);
    assert_aborted(ended, &mut peer);
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_runaway_a_huge_string_repeat_ends_and_the_process_next_door_is_fine() {
    let mut peer = peer();
    // 2**28 characters. Measured on Boa 0.22.0: `repeat` is stopped by the
    // engine's own counted loop once the count passes the loop limit (2**24 and
    // up end as RuntimeLimit in about 300 ms), so this is a layer-1 ending, not
    // a watchdog one. `repeat(2**30)` is deliberately NOT run here: it asks the
    // allocator for 24 GiB up front (see the memory tests), which a test must
    // not do to a shared runner.
    match peer.run_runaway("'x'.repeat(2**28).length", GENEROUS) {
        Ended::Terminated(reason) => assert_eq!(reason, TerminateReason::RuntimeLimit),
        Ended::Dead(status) => assert!(was_aborted(status), "{status:?}"),
        other => panic!("a runaway finished: {other:?}"),
    }
    the_next_process_runs_one_plus_one();
}

// ---- isolation: a runaway in one process is nobody else's problem -----------

#[test]
fn script_limits_isolation_two_processes_one_runaway_the_other_keeps_answering() {
    let mut runaway = peer();
    let mut other = Peer::spawn(3);
    assert_eq!(other.run("globalThis.k = 41; k"), value("41"));

    runaway.send_script("/^(a+)+$/.test('a'.repeat(60) + 'b')");

    // While the first process is wedged inside a native call, the second one
    // answers every request, promptly, for as long as the first is alive.
    let started = std::time::Instant::now();
    let mut answered = 0u32;
    while runaway.child.try_wait().expect("try_wait").is_none() {
        assert_eq!(other.run("k + 1"), value("42"));
        answered += 1;
        assert!(
            started.elapsed() < GENEROUS,
            "the runaway was never ended by the watchdog"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(answered > 0, "the second process was never asked anything");

    // The wedged one is dead by its own abort, and said nothing.
    assert!(was_aborted(runaway.wait_exit()));
    assert!(runaway.recv().is_err());

    // The other one is untouched: same realm, same state, still serving.
    assert_eq!(other.run("k + 1"), value("42"));
    assert!(other.child.try_wait().expect("try_wait").is_none());
    the_next_process_runs_one_plus_one();
}

#[test]
fn script_limits_isolation_a_terminated_process_does_not_stop_its_neighbour() {
    let mut runaway = peer();
    let mut other = Peer::spawn(3);
    let ended = runaway.run_runaway("function f() { f() } f()", GENEROUS);
    assert_terminated(ended, &mut runaway, TerminateReason::RuntimeLimit);
    assert_eq!(other.run("6 * 7"), value("42"));
}

/// The real broker, a real sandboxed content process holding a frame, and a
/// runaway that dies next to them. The broker's own `RunScript` wiring is not
/// built yet (docs/backlog.md), so the runaway is the unsandboxed test peer;
/// what this proves is that an aborted script process leaves the broker alive
/// and a different process's frame tree and capabilities exactly as they were.
#[test]
fn script_limits_isolation_a_dead_script_process_leaves_the_broker_and_its_frames_intact() {
    use px_broker::{Broker, ContentProcess, serve_once};
    use px_ipc::{Request, Response};

    let mut broker = Broker::new();
    let mut neighbour = ContentProcess::spawn(
        &mut broker,
        std::path::Path::new(env!("CARGO_BIN_EXE_px-content")),
    )
    .expect("spawn the neighbour");
    let frame = broker
        .create_frame(neighbour.channel_id())
        .expect("a frame for the neighbour");
    assert!(broker.holds_frame(neighbour.channel_id(), frame));
    let channels = broker.channel_count();

    let mut runaway = peer();
    let ended = runaway.run_runaway("/^(a+)+$/.test('a'.repeat(60) + 'b')", GENEROUS);
    assert_aborted(ended, &mut runaway);

    // The broker is alive and answers, the frame is still the neighbour's, the
    // channel table did not change, and the neighbour is still being served.
    assert_eq!(broker.channel_count(), channels);
    assert!(broker.holds_frame(neighbour.channel_id(), frame));
    assert_eq!(
        broker
            .dispatch(neighbour.channel_id(), Request::FrameHost { frame })
            .response,
        Response::FrameHost {
            host: px_ipc::FrameHost::Local
        }
    );
    assert!(neighbour.is_alive());
    assert_eq!(
        serve_once(&mut broker, &mut neighbour, Duration::from_secs(30))
            .expect("the neighbour's opening ping")
            .response,
        Response::Pong
    );
}

// ---- memory: documented, not contained ---------------------------------------

#[test]
fn script_limits_memory_a_buffer_over_the_engine_cap_is_refused_and_the_process_lives() {
    let mut peer = peer();
    // The one memory request Boa can see: ArrayBuffer is checked against the
    // host's `max_buffer_size` (256 MiB) before any allocation. It is an
    // ordinary RangeError, catchable, and not a reason to stop script.
    match peer.run("new ArrayBuffer(2**31).byteLength") {
        px_ipc::script::ScriptResult::Error { message } => {
            assert!(message.as_str().contains("RangeError"), "{message:?}");
        }
        other => panic!("expected a RangeError, got {other:?}"),
    }
    assert_eq!(
        peer.run("try { new Uint8Array(2**31); 'no' } catch (e) { 'refused' }"),
        value("refused")
    );
    assert_eq!(peer.run("1+1"), value("2"));
}

#[test]
fn script_limits_memory_allocations_the_engine_does_not_see_are_not_capped() {
    // This documents the gap; it does not close it (ADR 040 layer 3: Phase 17,
    // cgroup `memory.max` / job object). Nothing in the engine or in this
    // process caps the heap: strings, plain arrays and objects are not checked
    // against `max_buffer_size`. A script that allocates a few hundred MiB in a
    // few loop iterations is simply allowed. 2**24 characters here (a doubling
    // loop, 24 iterations) is small enough for a shared runner.
    let mut peer = Peer::spawn(1);
    assert_eq!(
        peer.run("var s = 'x'; for (var i = 0; i < 24; i++) s += s; s.length"),
        value("16777216")
    );
    assert_eq!(peer.run("1+1"), value("2"));
}

/// Needs the short watchdog, or an unbounded doubling loop would be left to
/// run for ten seconds. Measured on the unoptimised build: about 1.1 GiB
/// resident after 1.5 s. It is bounded here by the watchdog's clock, which is
/// time, not memory: on a faster machine it reaches further. That is the gap.
#[cfg(feature = "testing")]
#[test]
fn script_limits_memory_an_unbounded_doubling_loop_is_ended_only_by_time() {
    let mut peer = Peer::spawn(1);
    peer.set_watchdog_ms(600);
    let ended = peer.run_runaway("var s = 'x'; for (;;) s += s;", GENEROUS);
    // Dead by the watchdog's abort, or by the allocator giving up (also an
    // abort). Both are SIGABRT; neither is the engine containing a heap.
    assert_aborted(ended, &mut peer);
    the_next_process_runs_one_plus_one();
}
