//! The fixture table (`crates/px-script/tests/fixtures/table.rs`) through the
//! real content binary, over its real pipes, on the real clock (unit 6).
//!
//! The in-process driver proves each row on a manual clock. This one proves the
//! loop end to end: the process's own loop runs timers between requests, and the
//! script's log is read back over the channel. Only rows marked `process` run
//! here: those whose expectation stays true on a loaded machine (their gaps are
//! large against scheduling noise, or they do not depend on elapsed time).
//!
//! The expectations are the spec-derived ones in the table; none is read back
//! from this process.

// `cfg(test)` is how clippy knows helper code in an integration test is test
// code (clippy.toml allows expect in tests); it is always true here.
#[cfg(test)]
mod common;

use std::time::{Duration, Instant};

use common::{Peer, value};
use px_ipc::script::{ScriptResponse, ScriptResult};

#[allow(dead_code)] // `uncaught` is counted by the in-process driver only
mod table {
    include!("../../px-script/tests/fixtures/table.rs");
}
use table::{End, ROWS};

/// A row that never reached its expectation in this long failed.
#[cfg(test)]
const DEADLINE: Duration = Duration::from_secs(60);

/// After the expectation first appears the log must stay as it is this long: a
/// wrong order whose prefix equals the expectation would otherwise pass.
#[cfg(test)]
const SETTLE: Duration = Duration::from_millis(150);

#[cfg(test)]
fn run_row_in_process(name: &str) {
    let row = ROWS
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no fixture row named {name}"));
    assert!(row.process, "{name} is not marked for the process driver");
    let mut peer = Peer::spawn(1);
    assert_eq!(peer.run("var log = [];"), value("undefined"));
    let first = peer.run(row.script);
    match row.end {
        End::Terminated(reason) => {
            // The bound is hit by the request itself, so the reply is the
            // termination; if a timer hit it instead, it arrives unasked.
            let result = match first {
                ScriptResult::Terminated { .. } => first,
                _ => match peer.recv().expect("an unsolicited termination") {
                    ScriptResponse::Result(result) => result,
                    other => panic!("{name}: {other:?}"),
                },
            };
            match result {
                ScriptResult::Terminated { reason: r } => {
                    assert_eq!(format!("{r:?}"), reason, "{name}: wrong reason");
                }
                other => panic!("{name}: expected termination, got {other:?}"),
            }
            // And the process is up: only script stopped.
            assert!(peer.child.try_wait().expect("try_wait").is_none());
        }
        End::Idle => {
            assert!(
                matches!(first, ScriptResult::Value { .. }),
                "{name}: the script itself failed: {first:?}"
            );
            let started = Instant::now();
            let mut last = peer.run(row.probe);
            while last != value(row.expect) {
                assert!(
                    started.elapsed() < DEADLINE,
                    "{name}: after {DEADLINE:?} the probe is {last:?}, spec says `{}`\n{}",
                    row.expect,
                    row.spec
                );
                std::thread::sleep(Duration::from_millis(10));
                last = peer.run(row.probe);
            }
            std::thread::sleep(SETTLE);
            assert_eq!(
                peer.run(row.probe),
                value(row.expect),
                "{name}: the log changed after reaching the expectation"
            );
        }
        End::TaskLimit(_) => panic!("{name}: a task-limit row cannot run in a process"),
    }
}

#[test]
fn script_fixture_process_table_marks_match_the_tests_below() {
    let source = include_str!("script_fixture_process.rs");
    for row in ROWS.iter().filter(|r| r.process) {
        assert!(
            source.contains(&format!("fn script_fixture_process_{}()", row.name)),
            "row {} is marked `process` but has no test here",
            row.name
        );
        assert!(!matches!(row.end, End::TaskLimit(_)), "{}", row.name);
    }
}

#[test]
fn script_fixture_process_math_random_same_seed_same_sequence_across_processes() {
    // The broker's seed is the whole entropy: two content processes with one
    // seed print the same stream, another seed a different one.
    let script = "var r = []; for (let i = 0; i < 20; i++) r.push(Math.random()); r.join()";
    let a = Peer::spawn(0xDEAD_BEEF).run(script);
    let b = Peer::spawn(0xDEAD_BEEF).run(script);
    let c = Peer::spawn(7).run(script);
    assert!(matches!(a, ScriptResult::Value { .. }), "{a:?}");
    assert_eq!(a, b);
    assert_ne!(a, c);
}

#[test]
fn script_fixture_process_performance_now_is_monotonic_across_timer_tasks() {
    // Sampled in timer tasks (not one tight loop): never backward, always on
    // the 100 us grid, and not earlier than the delays asked for.
    let mut peer = Peer::spawn(3);
    assert_eq!(
        peer.run(
            "var t = [];
             function tick() { t.push(performance.now()); if (t.length < 20) setTimeout(tick, 1); }
             tick();
             globalThis.ok = () => t.length == 20
               && t.every((v, i) => (i == 0 || v >= t[i - 1]) && Math.abs(v * 10 - Math.round(v * 10)) < 1e-6)
               && t[19] - t[0] >= 19 - 0.2;
             0"
        ),
        value("0")
    );
    let started = Instant::now();
    while peer.run("ok()") != value("true") {
        assert!(started.elapsed() < DEADLINE, "the clock series never held");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn script_fixture_process_micro_before_timeout_zero() {
    run_row_in_process("micro_before_timeout_zero");
}

#[test]
fn script_fixture_process_nested_microtasks_drain_before_the_next_task() {
    run_row_in_process("nested_microtasks_drain_before_the_next_task");
}

#[test]
fn script_fixture_process_timers_by_due_time_then_insertion() {
    run_row_in_process("timers_by_due_time_then_insertion");
}

#[test]
fn script_fixture_process_equal_delays_fire_in_insertion_order() {
    run_row_in_process("equal_delays_fire_in_insertion_order");
}

#[test]
fn script_fixture_process_clear_timeout_before_it_fires() {
    run_row_in_process("clear_timeout_before_it_fires");
}

#[test]
fn script_fixture_process_clear_timeout_inside_a_callback_cancels_a_due_timer() {
    run_row_in_process("clear_timeout_inside_a_callback_cancels_a_due_timer");
}

#[test]
fn script_fixture_process_interval_rearm_and_clear_in_its_own_callback() {
    run_row_in_process("interval_rearm_and_clear_in_its_own_callback");
}

#[test]
fn script_fixture_process_await_a_native_promise_takes_one_tick() {
    run_row_in_process("await_a_native_promise_takes_one_tick");
}

#[test]
fn script_fixture_process_await_a_plain_value_interleaves_with_a_promise_chain() {
    run_row_in_process("await_a_plain_value_interleaves_with_a_promise_chain");
}

#[test]
fn script_fixture_process_await_a_timer_interleaves_two_async_functions() {
    run_row_in_process("await_a_timer_interleaves_two_async_functions");
}

#[test]
fn script_fixture_process_promise_all_result_follows_input_order_not_completion_order() {
    run_row_in_process("promise_all_result_follows_input_order_not_completion_order");
}

#[test]
fn script_fixture_process_promise_race_settles_with_the_first_to_settle() {
    run_row_in_process("promise_race_settles_with_the_first_to_settle");
}

#[test]
fn script_fixture_process_promise_all_settled_reports_every_outcome_in_input_order() {
    run_row_in_process("promise_all_settled_reports_every_outcome_in_input_order");
}

#[test]
fn script_fixture_process_promise_any_takes_the_first_fulfilment_ignoring_earlier_rejections() {
    run_row_in_process("promise_any_takes_the_first_fulfilment_ignoring_earlier_rejections");
}

#[test]
fn script_fixture_process_a_self_requeuing_promise_job_hits_the_checkpoint_budget() {
    run_row_in_process("a_self_requeuing_promise_job_hits_the_checkpoint_budget");
}

#[test]
fn script_fixture_process_twenty_thousand_timers_hit_the_timer_cap() {
    run_row_in_process("twenty_thousand_timers_hit_the_timer_cap");
}
