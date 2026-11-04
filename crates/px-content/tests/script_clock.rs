//! "Timer resolution asserted", on the real path (ADR 040, Phase 10 gate).
//!
//! The pure properties of the coarse clock are checked against a fake raw clock
//! in `px-script`. Here the real content binary, with the real monotonic clock
//! and the seed the broker sent, is asked for the time in a tight loop, and the
//! values it handed to script are read **at the native boundary**, before
//! conversion to a JS float: `0.1` ms is not exact in binary, so a float
//! assertion would be asserting on rounding.
//!
//! The raw log exists only in `testing` builds (build-spec 14.4); a shipping
//! build has no variant to ask for it, which the second half of this file
//! checks, and the JS-visible value is asserted either way.

// `cfg(test)` is how clippy knows helper code in an integration test is test
// code (clippy.toml allows expect in tests); it is always true here.
#[cfg(test)]
mod common;

use common::{Peer, value};
use px_ipc::script::ScriptResult;

#[cfg(feature = "testing")]
const QUANTUM_NS: u64 = 100_000;

/// Spin on `performance.now()` for at least `ms` milliseconds of script time.
fn spin(ms: u32) -> String {
    format!("var t0 = performance.now(); while (performance.now() - t0 < {ms}) {{}} 1")
}

#[test]
fn script_clock_process_performance_now_is_on_the_grid_as_script_sees_it() {
    let mut peer = Peer::spawn(0x1234_5678_9ABC_DEF0);
    assert_eq!(peer.run(&spin(3)), value("1"));
    // Ten thousandths of a millisecond are not representable, so the check is
    // that ten times the value is an integer to within float noise, over many
    // reads, and that the sequence only moves forward.
    let verdict = peer.run(
        "var last = -1, bad = 0, n = 0;
         var end = performance.now() + 2;
         while (performance.now() < end) {
           var v = performance.now();
           if (Math.abs(v * 10 - Math.round(v * 10)) > 1e-6) bad++;
           if (v < last) bad++;
           last = v; n++;
         }
         bad + ',' + (n > 20)",
    );
    assert_eq!(verdict, value("0,true"));
}

#[test]
fn script_clock_process_the_clock_has_the_resolution_the_adr_states() {
    // Over 5 ms of real time a script sees at most one new value per 0.1 ms, so
    // at most about 51: the UPPER bound is the coarsening check (thousands would
    // mean no coarsening). The lower bound only guards against 'no clock at all',
    // and must NOT depend on interpreter speed: a loaded CI runner loops slowly
    // enough to see well under 15 values, and the first version of this test
    // failed that way under heavy parallel builds. The exact count is the pure
    // property in px-script (`script_clock_prop_`).
    let mut peer = Peer::spawn(7);
    let distinct = peer.run(
        "var seen = {}, count = 0;
         var t0 = performance.now();
         while (performance.now() - t0 < 5) {
           var v = performance.now();
           if (!seen[v]) { seen[v] = 1; count++; }
         }
         count",
    );
    let ScriptResult::Value { text } = distinct else {
        panic!("expected a count, got {distinct:?}");
    };
    let count: u32 = text.as_str().parse().expect("a number");
    assert!(
        (3..=53).contains(&count),
        "{count} distinct values in 5 ms of a 100 us clock"
    );
}

#[test]
fn script_clock_process_the_seed_from_the_broker_seeds_math_random() {
    let first = |seed: u64| {
        let mut peer = Peer::spawn(seed);
        peer.run("Math.random() + ',' + Math.random() + ',' + Math.random()")
    };
    assert_eq!(first(99), first(99));
    assert_ne!(first(99), first(100));
}

#[test]
fn script_clock_process_date_now_is_the_wall_clock_at_millisecond_resolution() {
    let mut peer = Peer::spawn(1);
    let ScriptResult::Value { text } = peer.run("Date.now()") else {
        panic!("expected a number");
    };
    let ms: f64 = text.as_str().parse().expect("a number");
    let host_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after 1970")
        .as_millis() as f64;
    assert!(
        (host_now - ms).abs() < 60_000.0,
        "Date.now() {ms} vs {host_now}"
    );
    assert_eq!(ms, ms.floor());
}

/// The raw log: every value the clock returned to script is on the grid and the
/// sequence never decreases, on the real path.
#[cfg(feature = "testing")]
#[test]
fn script_clock_process_the_raw_log_is_on_the_grid_and_monotonic() {
    use px_ipc::script::{ScriptRequest, ScriptResponse};
    let mut peer = Peer::spawn(0xABCD);
    assert_eq!(peer.run(&spin(10)), value("1"));
    peer.send(&ScriptRequest::ClockLog);
    let ScriptResponse::ClockLog { nanos } = peer.recv().expect("the log") else {
        panic!("expected the clock log");
    };
    let log = nanos.as_slice();
    assert!(
        log.len() > 100,
        "only {} reads in 10 ms of spinning",
        log.len()
    );
    for (i, ns) in log.iter().enumerate() {
        assert_eq!(ns % QUANTUM_NS, 0, "read {i} returned {ns} ns");
    }
    assert!(
        log.windows(2).all(|w| w[0] <= w[1]),
        "the clock went backward"
    );
    // Within one quantum of the true time: the spin lasted at least 10 ms, so
    // the log spans that much, to within a quantum at each end.
    let (first, last) = (log[0], log[log.len() - 1]);
    assert!(
        last - first + QUANTUM_NS >= 10_000_000,
        "span {} ns",
        last - first
    );
    let mut distinct = log.to_vec();
    distinct.dedup();
    assert!(
        (30..=103).contains(&distinct.len()),
        "{} distinct values in 10 ms of a 100 us clock",
        distinct.len()
    );
}

/// Without the feature the log does not exist: asking for it is a frame the
/// decoder refuses. (Variant 3 of `ScriptRequest`.)
#[cfg(not(feature = "testing"))]
#[test]
fn script_clock_process_the_raw_log_does_not_exist_without_the_testing_feature() {
    let mut peer = Peer::spawn(1);
    let mut frame = 2u32.to_le_bytes().to_vec();
    frame.push(<px_ipc::script::ScriptRequest as px_ipc::Message>::TAG);
    frame.push(3);
    peer.send_raw(&frame);
    assert!(!peer.wait_exit().success());
}
