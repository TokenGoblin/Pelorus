//! Phase 10 unit 2 through the real content binary: timers run between
//! requests on the process's own loop, a bound hit by a timer is reported
//! without being asked, and the testing counters report their marks.

// `cfg(test)` is how clippy knows helper code in an integration test is test
// code (clippy.toml allows expect in tests); it is always true here.
#[cfg(test)]
mod common;

use std::time::{Duration, Instant};

use common::{Peer, value};
use px_ipc::script::{ScriptRequest, ScriptResponse, ScriptResult, TerminateReason};

#[test]
fn script_loop_a_timer_fires_between_requests_in_the_real_process() {
    let mut peer = Peer::spawn(1);
    assert_eq!(
        peer.run("var x = 0; setTimeout(() => { x = 1; }, 20); x"),
        value("0")
    );
    // The timer is a task of the process's own loop, not of any request.
    let started = Instant::now();
    loop {
        if peer.run("x") == value("1") {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the timer never fired"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn script_loop_a_self_rearming_timer_does_not_starve_requests() {
    let mut peer = Peer::spawn(1);
    peer.run("var n = 0; function f() { n++; setTimeout(f, 0); } setTimeout(f, 0); 0");
    // Requests are answered while the page spins, and it does spin.
    let before = peer.run("n");
    std::thread::sleep(Duration::from_millis(100));
    let after = peer.run("n");
    assert_ne!(before, after, "the timer chain stopped running");
}

#[test]
fn script_loop_a_bound_hit_by_a_timer_is_reported_unasked_and_stays_reported() {
    let mut peer = Peer::spawn(1);
    // The timer, not a request, overflows the microtask queue (65,536).
    assert_eq!(
        peer.run(
            "setTimeout(() => { for (let i = 0; i < 70000; i++) queueMicrotask(() => {}); }, 0); 0"
        ),
        value("0")
    );
    let expected = ScriptResponse::Result(ScriptResult::Terminated {
        reason: TerminateReason::MicrotaskQueueOverflow,
    });
    assert_eq!(peer.recv().expect("an unsolicited report"), expected);
    // From then on every request is answered with the same fact, and nothing
    // is evaluated.
    assert_eq!(
        peer.run("globalThis.after = 1"),
        ScriptResult::Terminated {
            reason: TerminateReason::MicrotaskQueueOverflow
        }
    );
    assert!(
        peer.child.try_wait().expect("try_wait").is_none(),
        "the process stays up; only script stopped"
    );
}

#[test]
fn script_loop_a_bound_hit_by_a_request_is_the_reply_not_an_extra_message() {
    let mut peer = Peer::spawn(1);
    let stopped = ScriptResult::Terminated {
        reason: TerminateReason::MicrotaskQueueOverflow,
    };
    assert_eq!(
        peer.run("for (let i = 0; i < 70000; i++) queueMicrotask(() => {});"),
        stopped
    );
    // The next message is the answer to the next request, not a second report.
    assert_eq!(peer.run("1"), stopped);
}

/// With the feature, the loop's high-water marks come back over the wire, with
/// the marker `cargo xtask sandbox` scans release binaries for.
#[cfg(feature = "testing")]
#[test]
fn script_loop_the_counters_report_marks_at_or_under_their_caps() {
    let mut peer = Peer::spawn(1);
    peer.run("var n = 0; function f() { if (++n < 2000) setTimeout(f, 0); } setTimeout(f, 0); 0");
    peer.run("1");
    peer.send(&ScriptRequest::Counters);
    let ScriptResponse::Counters(report) = peer.recv().expect("counters") else {
        panic!("expected counters");
    };
    assert!(
        report
            .marker
            .as_str()
            .starts_with("px-script-testing-high-water-counters")
    );
    for (mark, cap) in [
        report.tasks,
        report.microtasks,
        report.timers,
        report.checkpoint,
    ] {
        assert!(mark <= cap, "a mark {mark} is over its cap {cap}");
    }
    assert_eq!(report.tasks.1, 4096);
    assert_eq!(report.timers.1, 10_000);
    assert!(report.timers.0 >= 1);
}

/// Without the feature the counters do not exist: asking for them is a frame
/// the decoder refuses, and the process ends. (Variant 2 of `ScriptRequest`.)
#[cfg(not(feature = "testing"))]
#[test]
fn script_loop_the_counters_do_not_exist_without_the_testing_feature() {
    let mut peer = Peer::spawn(1);
    let mut frame = 2u32.to_le_bytes().to_vec();
    frame.push(<ScriptRequest as px_ipc::Message>::TAG);
    frame.push(2);
    peer.send_raw(&frame);
    assert!(!peer.wait_exit().success());
}
