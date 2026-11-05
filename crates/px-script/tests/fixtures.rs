//! The Phase 10 fixture table through the in-process host (unit 6).
//!
//! `fixtures/table.rs` holds the rows: a script, the expected observable, and
//! the specification passage the expectation was derived from (HTML timers and
//! event loops, the ECMAScript job queue). This file runs every row on a
//! `ManualClock`, which moves only when the host would wait, so a row that
//! "waits" 30 s of timer time takes none and is exact. The same table runs
//! through the real content process in
//! `px-content/tests/script_fixture_process.rs` for the rows marked `process`.
//!
//! One `#[test]` per row, so a failing row is named and a known-failing row can
//! be `#[ignore]`d on its own with the finding recorded in `docs/backlog.md`.
//! Loop-level ordering only: DOM-level ordering is Phase 11.

use std::rc::Rc;

use px_script::clock::ManualClock;
use px_script::queue::Caps;
use px_script::{Host, HostOptions, LoopEnd, Outcome};

include!("fixtures/table.rs");

/// Tasks a row may run before the driver calls the page runaway.
#[cfg(test)]
const TASK_BUDGET: usize = 1_000_000;

#[cfg(test)]
fn run_row(name: &str) {
    let row = ROWS
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no fixture row named {name}"));
    let mut host = Host::with_options(HostOptions {
        caps: Caps::default(),
        clock: Rc::new(ManualClock::at(0)),
        ..HostOptions::new(1)
    })
    .expect("host");
    assert_eq!(
        host.eval("var log = [];"),
        Outcome::Value("undefined".into())
    );
    let first = host.eval(row.script);
    match row.end {
        End::Terminated(reason) => {
            let ended = match first {
                Outcome::Terminated(r) => format!("{r:?}"),
                _ => match host.run_loop(TASK_BUDGET) {
                    LoopEnd::Terminated { reason, .. } => format!("{reason:?}"),
                    other => panic!("{name}: expected termination by {reason}, got {other:?}"),
                },
            };
            assert_eq!(ended, reason, "{name}: terminated for the wrong reason");
            let why = host.terminated().expect("terminated");
            assert_eq!(
                host.eval("1"),
                Outcome::Terminated(why),
                "{name}: nothing may run after termination"
            );
        }
        End::Idle | End::TaskLimit(_) => {
            assert!(
                !matches!(first, Outcome::Error(_) | Outcome::Terminated(_)),
                "{name}: the script itself failed: {first:?}"
            );
            match row.end {
                End::TaskLimit(n) => {
                    let end = host.run_loop(n);
                    assert_eq!(end, LoopEnd::TaskLimit { ran: n }, "{name}");
                    assert!(host.terminated().is_none(), "{name}: a bound was hit");
                }
                _ => {
                    let end = host.run_loop(TASK_BUDGET);
                    assert!(matches!(end, LoopEnd::Idle { .. }), "{name}: {end:?}");
                }
            }
            assert_eq!(
                host.eval(row.probe),
                Outcome::Value(row.expect.to_string()),
                "{name}: spec says `{}`\nderivation: {}",
                row.expect,
                row.spec
            );
            assert_eq!(
                host.uncaught_error_count(),
                row.uncaught,
                "{name}: uncaught error count"
            );
        }
    }
}

#[test]
fn script_fixture_micro_before_timeout_zero() {
    run_row("micro_before_timeout_zero");
}

#[test]
fn script_fixture_nested_microtasks_drain_before_the_next_task() {
    run_row("nested_microtasks_drain_before_the_next_task");
}

#[test]
fn script_fixture_promise_chain_runs_to_the_end_before_a_timer() {
    run_row("promise_chain_runs_to_the_end_before_a_timer");
}

#[test]
fn script_fixture_microtask_queued_from_a_timer_runs_before_the_next_timer() {
    run_row("microtask_queued_from_a_timer_runs_before_the_next_timer");
}

#[test]
fn script_fixture_timer_set_in_a_microtask_runs_after_an_earlier_timer_of_equal_delay() {
    run_row("timer_set_in_a_microtask_runs_after_an_earlier_timer_of_equal_delay");
}

#[test]
fn script_fixture_timers_by_due_time_then_insertion() {
    run_row("timers_by_due_time_then_insertion");
}

#[test]
fn script_fixture_equal_delays_fire_in_insertion_order() {
    run_row("equal_delays_fire_in_insertion_order");
}

#[test]
fn script_fixture_a_later_timer_with_a_short_delay_overtakes_an_earlier_long_one() {
    run_row("a_later_timer_with_a_short_delay_overtakes_an_earlier_long_one");
}

#[test]
fn script_fixture_clear_timeout_before_it_fires() {
    run_row("clear_timeout_before_it_fires");
}

#[test]
fn script_fixture_clear_timeout_inside_a_callback_cancels_a_due_timer() {
    run_row("clear_timeout_inside_a_callback_cancels_a_due_timer");
}

#[test]
fn script_fixture_clearing_a_timer_that_already_fired_does_nothing() {
    run_row("clearing_a_timer_that_already_fired_does_nothing");
}

#[test]
fn script_fixture_interval_rearm_and_clear_in_its_own_callback() {
    run_row("interval_rearm_and_clear_in_its_own_callback");
}

#[test]
fn script_fixture_interval_interleaves_with_a_timeout_by_due_time() {
    run_row("interval_interleaves_with_a_timeout_by_due_time");
}

#[test]
fn script_fixture_nested_timer_clamp_four_ms_from_the_sixth_level() {
    run_row("nested_timer_clamp_four_ms_from_the_sixth_level");
}

#[test]
fn script_fixture_a_timer_set_from_the_script_is_not_clamped_beside_a_clamped_chain() {
    run_row("a_timer_set_from_the_script_is_not_clamped_beside_a_clamped_chain");
}

#[test]
fn script_fixture_delay_two_to_the_31_wraps_to_zero() {
    run_row("delay_two_to_the_31_wraps_to_zero");
}

#[test]
fn script_fixture_negative_nan_and_missing_delays_are_zero() {
    run_row("negative_nan_and_missing_delays_are_zero");
}

#[test]
fn script_fixture_timer_arguments_are_passed_through() {
    run_row("timer_arguments_are_passed_through");
}

#[test]
fn script_fixture_await_a_native_promise_takes_one_tick() {
    run_row("await_a_native_promise_takes_one_tick");
}

#[test]
fn script_fixture_await_a_plain_value_interleaves_with_a_promise_chain() {
    run_row("await_a_plain_value_interleaves_with_a_promise_chain");
}

#[test]
fn script_fixture_async_function_returning_a_promise_costs_three_ticks() {
    run_row("async_function_returning_a_promise_costs_three_ticks");
}

#[test]
fn script_fixture_async_function_that_throws_rejects_its_promise() {
    run_row("async_function_that_throws_rejects_its_promise");
}

#[test]
fn script_fixture_await_a_timer_interleaves_two_async_functions() {
    run_row("await_a_timer_interleaves_two_async_functions");
}

#[test]
fn script_fixture_promise_all_result_follows_input_order_not_completion_order() {
    run_row("promise_all_result_follows_input_order_not_completion_order");
}

#[test]
fn script_fixture_promise_all_rejects_with_the_first_rejection() {
    run_row("promise_all_rejects_with_the_first_rejection");
}

#[test]
fn script_fixture_promise_all_with_plain_values_resolves_after_one_tick_per_element() {
    run_row("promise_all_with_plain_values_resolves_after_one_tick_per_element");
}

#[test]
fn script_fixture_promise_race_settles_with_the_first_to_settle() {
    run_row("promise_race_settles_with_the_first_to_settle");
}

#[test]
fn script_fixture_promise_race_a_first_rejection_wins() {
    run_row("promise_race_a_first_rejection_wins");
}

#[test]
fn script_fixture_promise_all_settled_reports_every_outcome_in_input_order() {
    run_row("promise_all_settled_reports_every_outcome_in_input_order");
}

#[test]
fn script_fixture_promise_any_takes_the_first_fulfilment_ignoring_earlier_rejections() {
    run_row("promise_any_takes_the_first_fulfilment_ignoring_earlier_rejections");
}

#[test]
fn script_fixture_promise_any_all_rejected_is_an_aggregate_error_in_input_order() {
    run_row("promise_any_all_rejected_is_an_aggregate_error_in_input_order");
}

#[test]
fn script_fixture_a_throwing_then_callback_rejects_the_next_promise_and_skips_then_handlers() {
    run_row("a_throwing_then_callback_rejects_the_next_promise_and_skips_then_handlers");
}

#[test]
fn script_fixture_finally_runs_then_passes_the_value_through() {
    run_row("finally_runs_then_passes_the_value_through");
}

#[test]
fn script_fixture_a_throwing_timer_is_reported_and_the_next_timer_still_runs() {
    run_row("a_throwing_timer_is_reported_and_the_next_timer_still_runs");
}

#[test]
fn script_fixture_a_throwing_queue_microtask_is_reported_and_the_next_microtask_runs() {
    run_row("a_throwing_queue_microtask_is_reported_and_the_next_microtask_runs");
}

#[test]
fn script_fixture_a_throwing_promise_reaction_is_a_rejection_not_an_uncaught_error() {
    run_row("a_throwing_promise_reaction_is_a_rejection_not_an_uncaught_error");
}

#[test]
fn script_fixture_unhandled_rejection_does_not_stop_the_loop_and_is_not_an_uncaught_error() {
    run_row("unhandled_rejection_does_not_stop_the_loop_and_is_not_an_uncaught_error");
}

#[test]
fn script_fixture_a_rejection_handled_in_a_later_task_is_still_delivered() {
    run_row("a_rejection_handled_in_a_later_task_is_still_delivered");
}

#[test]
fn script_fixture_a_self_rearming_zero_timer_never_ends_and_never_grows() {
    run_row("a_self_rearming_zero_timer_never_ends_and_never_grows");
}

#[test]
fn script_fixture_a_self_requeuing_microtask_hits_the_checkpoint_budget() {
    run_row("a_self_requeuing_microtask_hits_the_checkpoint_budget");
}

#[test]
fn script_fixture_a_self_requeuing_promise_job_hits_the_checkpoint_budget() {
    run_row("a_self_requeuing_promise_job_hits_the_checkpoint_budget");
}

#[test]
fn script_fixture_a_timer_that_arms_a_hundred_of_itself_hits_the_timer_cap() {
    run_row("a_timer_that_arms_a_hundred_of_itself_hits_the_timer_cap");
}

#[test]
fn script_fixture_twenty_thousand_timers_hit_the_timer_cap() {
    run_row("twenty_thousand_timers_hit_the_timer_cap");
}

#[test]
fn script_fixture_nine_thousand_timers_all_fire_in_due_time_then_insertion_order() {
    run_row("nine_thousand_timers_all_fire_in_due_time_then_insertion_order");
}

#[test]
fn script_fixture_a_deep_promise_chain_runs_to_the_end_in_one_checkpoint() {
    run_row("a_deep_promise_chain_runs_to_the_end_in_one_checkpoint");
}

#[test]
fn script_fixture_a_flat_fan_out_under_the_microtask_cap_runs_every_job() {
    run_row("a_flat_fan_out_under_the_microtask_cap_runs_every_job");
}

#[test]
fn script_fixture_performance_now_is_monotonic_on_the_100_microsecond_grid() {
    run_row("performance_now_is_monotonic_on_the_100_microsecond_grid");
}

#[test]
fn script_fixture_date_now_is_an_integer_number_of_milliseconds_and_never_goes_back() {
    run_row("date_now_is_an_integer_number_of_milliseconds_and_never_goes_back");
}

#[test]
fn script_fixture_math_random_is_in_the_unit_interval_and_not_constant() {
    run_row("math_random_is_in_the_unit_interval_and_not_constant");
}

// ---- properties of the table, and tests that are not one-script rows --------

#[test]
fn script_fixture_table_every_row_has_a_test_and_a_derivation() {
    let source = include_str!("fixtures.rs");
    let mut seen = std::collections::HashSet::new();
    for row in ROWS {
        assert!(seen.insert(row.name), "duplicate row {}", row.name);
        assert!(
            source.contains(&format!("fn script_fixture_{}()", row.name)),
            "row {} has no #[test] in this file",
            row.name
        );
        assert!(row.spec.len() > 30, "row {} has no derivation", row.name);
        assert!(!row.script.is_empty(), "row {} has no script", row.name);
        if matches!(row.end, End::Idle) {
            assert!(!row.expect.is_empty(), "row {} expects nothing", row.name);
        }
    }
    // A table that quietly lost its stress or process rows would stay green.
    assert!(ROWS.len() >= 45, "only {} rows", ROWS.len());
    assert!(ROWS.iter().filter(|r| r.process).count() >= 12);
}

#[cfg(test)]
fn random_host(seed: u64) -> Host {
    Host::with_options(HostOptions {
        clock: Rc::new(ManualClock::at(0)),
        ..HostOptions::new(seed)
    })
    .expect("host")
}

#[cfg(test)]
fn random_sequence(seed: u64) -> String {
    let mut host = random_host(seed);
    match host.eval("var r = []; for (let i = 0; i < 20; i++) r.push(Math.random()); r.join()") {
        Outcome::Value(text) => text,
        other => panic!("{other:?}"),
    }
}

#[test]
fn script_fixture_math_random_same_seed_same_sequence_across_hosts() {
    // ADR 040: Math.random draws from the broker's seed, so two hosts built
    // with one seed (one after the other, with another host built in between)
    // produce byte-identical sequences, and another seed does not.
    let a = random_sequence(0xDEAD_BEEF);
    let other = random_sequence(7);
    let b = random_sequence(0xDEAD_BEEF);
    assert_eq!(a, b, "same seed, different sequence");
    assert_ne!(a, other, "different seeds gave the same sequence");
    assert_eq!(a.split(',').count(), 20);
}

#[test]
fn script_fixture_math_random_state_is_per_host_not_shared() {
    // Interleaving a second host must not disturb the first one's sequence.
    let solo = random_sequence(11);
    let mut one = random_host(11);
    let mut noise = random_host(12);
    let mut out = Vec::new();
    for _ in 0..20 {
        let _ = noise.eval("Math.random()");
        match one.eval("Math.random()") {
            Outcome::Value(text) => out.push(text),
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(solo, out.join(","));
}
