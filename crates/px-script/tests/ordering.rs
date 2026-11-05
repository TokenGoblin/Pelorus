//! Event ordering: properties of the loop, which need no DOM (ADR 040,
//! docs/phase-10-plan.md).
//!
//! These are *loop* ordering. HTML's full ordering (event dispatch,
//! `<script>` blocking, `requestAnimationFrame`) is re-checked in Phase 11 and
//! is not "passed" because these are green.
//!
//! Time is a `ManualClock`, which moves only when the host would wait, so a
//! test that "waits" for a 10 ms timer takes no time and is exact: the
//! expectations below are derived by hand from the HTML timer rules, not read
//! off a run.

use std::rc::Rc;
use std::time::Duration;

use px_script::clock::ManualClock;
use px_script::queue::Caps;
use px_script::{Host, HostOptions, LoopEnd, Outcome, Step};

#[cfg(test)]
fn host() -> (Host, Rc<ManualClock>) {
    let clock = Rc::new(ManualClock::at(0));
    let host = Host::with_options(HostOptions {
        caps: Caps::default(),
        clock: clock.clone(),
        ..HostOptions::new(1)
    })
    .expect("host");
    (host, clock)
}

#[cfg(test)]
fn value(text: &str) -> Outcome {
    Outcome::Value(text.to_string())
}

/// Run `source`, drive the loop to idle, return `log.join()`.
#[cfg(test)]
fn log_of(source: &str) -> String {
    let (mut host, _clock) = host();
    assert_eq!(host.eval("var log = [];"), value("undefined"));
    let first = host.eval(source);
    assert!(
        !matches!(first, Outcome::Error(_)),
        "script threw: {first:?}"
    );
    assert!(matches!(host.run_loop(1000), LoopEnd::Idle { .. }));
    match host.eval("log.join()") {
        Outcome::Value(text) => text,
        other => panic!("expected the log, got {other:?}"),
    }
}

#[test]
fn script_order_microtasks_run_before_the_next_task() {
    // A zero-delay timer is a task; the promise reaction is a microtask run at
    // the checkpoint after the script. HTML: sync, then micro, then the task.
    assert_eq!(
        log_of(
            "setTimeout(() => log.push('timeout'), 0);
             Promise.resolve().then(() => log.push('micro'));
             log.push('sync');"
        ),
        "sync,micro,timeout"
    );
}

#[test]
fn script_order_the_checkpoint_after_a_task_precedes_the_next_task() {
    // t1 queues a microtask. Both timers are due, but m1 runs before t2.
    assert_eq!(
        log_of(
            "setTimeout(() => { log.push('t1'); Promise.resolve().then(() => log.push('m1')); }, 0);
             setTimeout(() => log.push('t2'), 0);"
        ),
        "t1,m1,t2"
    );
}

#[test]
fn script_order_two_timers_by_due_time_then_insertion() {
    assert_eq!(
        log_of(
            "setTimeout(() => log.push('a10'), 10);
             setTimeout(() => log.push('b5'), 5);
             setTimeout(() => log.push('c5'), 5);
             setTimeout(() => log.push('d0'), 0);"
        ),
        "d0,b5,c5,a10"
    );
}

#[test]
fn script_order_clear_timeout_inside_a_callback_cancels_a_queued_timer() {
    // b is due at the same time as a and already queued as a task when a runs;
    // clearing it from a must still stop it.
    assert_eq!(
        log_of(
            "var idB;
             setTimeout(() => { log.push('a'); clearTimeout(idB); }, 0);
             idB = setTimeout(() => log.push('b'), 0);
             setTimeout(() => log.push('c'), 0);"
        ),
        "a,c"
    );
}

#[test]
fn script_order_clear_interval_inside_its_own_callback_stops_it() {
    assert_eq!(
        log_of(
            "var n = 0, id;
             id = setInterval(() => { log.push('i' + (++n)); if (n == 3) clearInterval(id); }, 10);"
        ),
        "i1,i2,i3"
    );
}

#[test]
fn script_order_promise_reactions_run_in_registration_order_across_chains() {
    // Two chains off one resolved promise interleave a1 b1 a2 b2.
    assert_eq!(
        log_of(
            "var p = Promise.resolve();
             p.then(() => log.push('a1')).then(() => log.push('a2'));
             p.then(() => log.push('b1')).then(() => log.push('b2'));
             log.push('sync');"
        ),
        "sync,a1,b1,a2,b2"
    );
}

#[test]
fn script_order_queue_microtask_shares_one_fifo_with_promise_jobs() {
    assert_eq!(
        log_of(
            "Promise.resolve().then(() => log.push('p'));
             queueMicrotask(() => log.push('q'));
             Promise.resolve().then(() => log.push('r'));"
        ),
        "p,q,r"
    );
}

#[test]
fn script_order_await_resumes_one_tick_later() {
    // The async body runs to its first await synchronously; the continuation is
    // a microtask queued before p's reaction.
    assert_eq!(
        log_of(
            "(async () => { log.push('a1'); await null; log.push('a2'); })();
             Promise.resolve().then(() => log.push('p'));
             log.push('s');"
        ),
        "a1,s,a2,p"
    );
}

#[test]
fn script_order_a_microtask_queued_by_a_microtask_runs_in_the_same_checkpoint() {
    assert_eq!(
        log_of(
            "queueMicrotask(() => { log.push('m1'); queueMicrotask(() => log.push('m2')); });
             setTimeout(() => log.push('t'), 0);"
        ),
        "m1,m2,t"
    );
}

#[test]
fn script_order_timer_arguments_are_passed_through() {
    assert_eq!(
        log_of("setTimeout((a, b) => log.push(a + b), 0, 'x', 'y');"),
        "xy"
    );
}

#[test]
fn script_order_a_timer_waits_exactly_its_delay_on_the_clock() {
    let (mut host, clock) = host();
    host.eval("setTimeout(() => {}, 10); setInterval(() => {}, 25);");
    // Nothing is due at t = 0.
    assert_eq!(host.step(), Step::Waiting(Duration::from_millis(10)));
    // The host sees time through the coarse clock, which is exact on its 100 us
    // grid: a test that moves the fake in whole quanta gets exact waits.
    clock.advance(9_900_000);
    assert_eq!(host.step(), Step::Waiting(Duration::from_micros(100)));
    clock.advance(100_000);
    assert_eq!(host.step(), Step::Ran);
    assert_eq!(host.step(), Step::Waiting(Duration::from_millis(15)));
}

#[test]
fn script_order_nested_timer_clamp_is_four_ms_from_the_sixth_level() {
    // f re-arms itself with delay 0. Timers 1 to 5 are armed at nesting 1 to 5
    // and are due at once; timer 6 is armed at nesting 6 and waits 4 ms, and so
    // does every one after it (HTML timer initialisation steps).
    let (mut host, clock) = host();
    host.eval("var n = 0; function f() { n++; setTimeout(f, 0); } setTimeout(f, 0);");
    for fired in 1..=5 {
        assert_eq!(host.step(), Step::Ran, "timer {fired} is due at once");
    }
    for round in 0..4 {
        assert_eq!(
            host.step(),
            Step::Waiting(Duration::from_millis(4)),
            "timer {} waits the 4 ms floor",
            6 + round
        );
        clock.advance(4_000_000);
        assert_eq!(host.step(), Step::Ran);
    }
    assert_eq!(host.eval("n"), value("9"));
}

#[test]
fn script_order_a_fresh_top_level_timer_is_not_clamped_by_an_old_chain() {
    let (mut host, clock) = host();
    host.eval("function f() { setTimeout(f, 0); } setTimeout(f, 0);");
    for _ in 0..5 {
        assert_eq!(host.step(), Step::Ran);
    }
    assert_eq!(host.step(), Step::Waiting(Duration::from_millis(4)));
    // A script run from the top level starts again at nesting one.
    host.eval("setTimeout(() => {}, 0);");
    assert_eq!(host.step(), Step::Ran, "the new timer is due at once");
    let _ = clock;
}

#[test]
fn script_order_interval_repeats_by_period_on_the_clock() {
    let (mut host, clock) = host();
    host.eval("var log = []; var n = 0; var id = setInterval(() => { log.push(n++); if (n == 3) clearInterval(id); }, 10);");
    assert!(matches!(host.run_loop(100), LoopEnd::Idle { ran: 3 }));
    assert_eq!(host.eval("log.join()"), value("0,1,2"));
    assert_eq!(clock_ms(&clock), 30, "10 ms, 20 ms, 30 ms");
}

#[cfg(test)]
fn clock_ms(clock: &ManualClock) -> u64 {
    use px_script::clock::RawClock;
    clock.now_ns() / 1_000_000
}

#[test]
fn script_order_a_throwing_timer_does_not_stop_the_next_one() {
    let (mut host, _clock) = host();
    host.eval(
        "var log = [];
         setTimeout(() => { throw new Error('first'); }, 0);
         setTimeout(() => log.push('second'), 0);",
    );
    assert!(matches!(host.run_loop(10), LoopEnd::Idle { ran: 2 }));
    assert_eq!(host.eval("log.join()"), value("second"));
    assert_eq!(host.uncaught_error_count(), 1);
    assert!(host.uncaught_errors()[0].contains("first"));
}

#[test]
fn script_order_a_throwing_microtask_does_not_stop_the_next_one() {
    let (mut host, _clock) = host();
    host.eval(
        "var log = [];
         queueMicrotask(() => { throw new Error('boom'); });
         queueMicrotask(() => log.push('after'));",
    );
    assert_eq!(host.eval("log.join()"), value("after"));
    assert_eq!(host.uncaught_error_count(), 1);
}

#[test]
fn script_order_the_timer_functions_validate_their_callback() {
    let (mut host, _clock) = host();
    for call in [
        "setTimeout('1+1', 0)",
        "setInterval(undefined)",
        "queueMicrotask(42)",
    ] {
        assert!(
            matches!(host.eval(call), Outcome::Error(m) if m.contains("TypeError")),
            "{call} must throw a TypeError"
        );
    }
    // clearTimeout is forgiving, as in browsers.
    assert_eq!(
        host.eval("clearTimeout(undefined); clearTimeout(99999); 1"),
        value("1")
    );
}

#[test]
fn script_order_timer_ids_are_positive_and_distinct() {
    let (mut host, _clock) = host();
    assert_eq!(
        host.eval("var a = setTimeout(() => {}, 5), b = setInterval(() => {}, 5); [a > 0, b > a, a !== b].join()"),
        value("true,true,true")
    );
}

#[test]
fn script_order_a_negative_or_nan_delay_is_zero() {
    assert_eq!(
        log_of(
            "setTimeout(() => log.push('nan'), NaN);
             setTimeout(() => log.push('neg'), -5);
             setTimeout(() => log.push('undef'));"
        ),
        "nan,neg,undef"
    );
}

/// An import of a module file by absolute path (a relative one needs a
/// referrer, which a bare script does not have). The file sets `leaked`.
#[cfg(test)]
fn import_script() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/side_effect_module"
    );
    // The path sits inside a JS string literal, so its backslashes (every
    // separator of a Windows manifest dir, e.g. `D:\a\...`) are escapes
    // unless doubled. Left alone they name a different file, the control
    // below cannot read it, and the test above would pass for no reason.
    let path = path.replace('\\', "\\\\").replace('\'', "\\'");
    format!("var leaked = false; import('{path}').catch(() => {{}});")
}

#[test]
fn script_order_dynamic_import_cannot_read_the_filesystem() {
    // The engine's default module loader reads from ".". This host replaces it,
    // so a module file next to the test is never evaluated.
    let (mut host, _clock) = host();
    host.eval(&import_script());
    let _ = host.run_loop(100);
    assert_eq!(host.eval("String(leaked)"), value("false"));
}

/// The positive control for the test above: with the engine's *default* loader
/// the same import does evaluate the file, so the refusal is the host's doing
/// and the test above can fail.
#[test]
fn script_order_the_default_engine_would_have_read_the_file() {
    use boa_engine::{Context, Source};
    let mut context = Context::default();
    let _ = context.eval(Source::from_bytes(&import_script()));
    let _ = context.run_jobs();
    let leaked = context
        .eval(Source::from_bytes("String(leaked)"))
        .expect("eval")
        .to_string(&mut context)
        .expect("string")
        .to_std_string_escaped();
    assert_eq!(
        leaked, "true",
        "the control cannot fail, so the test above proves nothing"
    );
}
