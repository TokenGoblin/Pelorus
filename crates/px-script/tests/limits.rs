//! Phase 10 unit 4, layer 1: the engine's own limits, made terminal by the host
//! (ADR 040, "Resource limits against runaway scripts").
//!
//! What Boa 0.22.0 does, read from its source and then run here: the loop,
//! recursion and stack limits raise an `EngineError::RuntimeLimit`, which the
//! VM treats as UNCATCHABLE (`JsError::is_catchable`, `vm/mod.rs`
//! `handle_error`): it unwinds every frame to the Rust caller, and
//! `try { } catch { }` never sees it. (`docs/research/phase-10-boa.md` called it
//! catchable; the source and these tests say otherwise.) The host still does
//! not depend on that: it inspects every error that crosses the native
//! boundary, and an engine error of any kind stops script for good.
//!
//! What a limit does NOT bound is also pinned here, so that nobody reads layer 1
//! as more than it is: the loop count is per call frame, and native builtins
//! are not interrupted at all (`tests/script_limits.rs` in `px-content` shows
//! the watchdog is what ends those).

use std::rc::Rc;

use px_script::clock::ManualClock;
use px_script::{Host, HostOptions, Limits, LoopEnd, Outcome, TerminateReason};

#[cfg(test)]
fn host_with(limits: Limits) -> Host {
    Host::with_options(HostOptions {
        clock: Rc::new(ManualClock::at(0)),
        limits,
        ..HostOptions::new(1)
    })
    .expect("host")
}

/// Small limits so a test finishes quickly even in an unoptimised build.
#[cfg(test)]
fn small() -> Limits {
    Limits {
        loop_iterations: 50_000,
        recursion: 64,
        ..Limits::default()
    }
}

#[cfg(test)]
fn stopped() -> Outcome {
    Outcome::Terminated(TerminateReason::RuntimeLimit)
}

#[test]
fn script_limits_engine_defaults_are_the_adr_values() {
    let d = Limits::default();
    assert_eq!(d.loop_iterations, 10_000_000);
    assert_eq!(d.recursion, 256);
    assert_eq!(d.max_buffer_bytes, 256 * 1024 * 1024);
    let host = Host::new(1).expect("host");
    assert_eq!(host.limits(), d);
}

#[test]
fn script_limits_engine_an_endless_loop_terminates_the_host() {
    let mut host = host_with(small());
    assert_eq!(host.eval("while (true) {}"), stopped());
    assert_eq!(host.terminated(), Some(TerminateReason::RuntimeLimit));
    // Nothing runs afterwards, however innocent.
    assert_eq!(host.eval("1 + 1"), stopped());
}

#[test]
fn script_limits_engine_every_loop_form_is_counted() {
    for source in [
        "for (;;) {}",
        "do {} while (true)",
        "for (var i = 0; ; i++) {}",
        "var o = {a:1}; for (var k in o) { while (true) {} }",
        "for (var x of (function*(){ while (true) yield 1; })()) {}",
    ] {
        let mut host = host_with(small());
        assert_eq!(host.eval(source), stopped(), "{source}");
    }
}

#[test]
fn script_limits_engine_the_limit_error_is_not_catchable_by_script() {
    let mut host = host_with(small());
    // Boa's loop-limit error is (in 0.22.0) an engine error. If it were an
    // ordinary exception this would loop again after the catch.
    assert_eq!(
        host.eval(
            "globalThis.after = 0; try { while (true) {} } catch (e) { after = 1 } finally { after = 2 }"
        ),
        stopped()
    );
    assert_eq!(host.eval("after"), stopped());
}

#[test]
fn script_limits_engine_catch_and_continue_in_a_loop_terminates() {
    let mut host = host_with(small());
    let source = "function f() { while (true) {} } \
                  for (;;) { try { f() } catch (e) {} }";
    assert_eq!(host.eval(source), stopped());
}

#[test]
fn script_limits_engine_runaway_recursion_terminates() {
    let mut host = host_with(small());
    assert_eq!(host.eval("function f() { f() } f()"), stopped());
    let mut host = host_with(small());
    assert_eq!(
        host.eval("function f() { try { f() } catch (e) { f() } } f()"),
        stopped()
    );
    let mut host = host_with(small());
    // Through a native frame: the callback re-enters the engine.
    assert_eq!(host.eval("function g() { [1].forEach(g) } g()"), stopped());
}

#[test]
fn script_limits_engine_recursion_inside_the_limit_is_ordinary() {
    let mut host = host_with(small());
    assert_eq!(
        host.eval("function f(n) { return n == 0 ? 0 : 1 + f(n - 1) } f(30)"),
        Outcome::Value("30".into())
    );
    assert_eq!(host.terminated(), None);
}

#[test]
fn script_limits_engine_a_finite_loop_under_the_limit_is_ordinary() {
    let mut host = host_with(small());
    assert_eq!(
        host.eval("var s = 0; for (var i = 0; i < 40000; i++) s += 1; s"),
        Outcome::Value("40000".into())
    );
    assert_eq!(host.terminated(), None);
}

#[test]
fn script_limits_engine_a_limit_hit_inside_a_promise_job_terminates() {
    let mut host = host_with(small());
    // The script itself finishes; the job it queued is what runs away.
    let outcome = host.eval("Promise.resolve().then(function () { while (true) {} }); 1");
    assert_eq!(outcome, stopped());
    assert_eq!(host.eval("2"), stopped());
}

#[test]
fn script_limits_engine_a_limit_hit_inside_a_promise_executor_terminates() {
    let mut host = host_with(small());
    // The Promise constructor turns a throw in the executor into a rejection;
    // it must not do that to an engine error.
    assert_eq!(
        host.eval("new Promise(function () { while (true) {} }).catch(function () {}); 1"),
        stopped()
    );
}

#[test]
fn script_limits_engine_a_limit_hit_inside_a_timer_terminates_the_loop() {
    let mut host = host_with(small());
    assert_eq!(
        host.eval("setTimeout(function () { while (true) {} }, 0); 1"),
        Outcome::Value("1".into())
    );
    match host.run_loop(10) {
        LoopEnd::Terminated { reason, .. } => assert_eq!(reason, TerminateReason::RuntimeLimit),
        other => panic!("expected the loop to terminate, got {other:?}"),
    }
    assert_eq!(host.terminated(), Some(TerminateReason::RuntimeLimit));
    assert_eq!(host.eval("1"), stopped());
}

#[test]
fn script_limits_engine_a_limit_hit_in_a_queued_microtask_terminates() {
    let mut host = host_with(small());
    assert_eq!(
        host.eval("queueMicrotask(function () { while (true) {} }); 1"),
        stopped()
    );
}

#[test]
fn script_limits_engine_termination_discards_what_was_queued() {
    let mut host = host_with(small());
    host.eval(
        "setTimeout(function () { globalThis.ran = 1 }, 0); setTimeout(function () {}, 0); while (true) {}",
    );
    assert_eq!(host.terminated(), Some(TerminateReason::RuntimeLimit));
    assert!(matches!(
        host.run_loop(10),
        LoopEnd::Terminated { ran: 0, .. }
    ));
}

#[test]
fn script_limits_engine_the_loop_count_is_per_frame_so_nested_calls_are_not_bounded() {
    // The documented hole: each call frame starts at zero, so work that is
    // spread over calls never reaches the per-frame limit. 700 calls of a
    // 700-iteration loop is 490,000 iterations against a limit of 1,000, and
    // nothing stops it. Only the watchdog bounds this shape.
    let mut host = host_with(Limits {
        loop_iterations: 1_000,
        ..small()
    });
    let outcome = host.eval(
        "function f() { for (var i = 0; i < 700; i++) {} } \
         for (var j = 0; j < 700; j++) f(); 'done'",
    );
    assert_eq!(outcome, Outcome::Value("done".into()));
    assert_eq!(host.terminated(), None);
}

#[test]
fn script_limits_engine_places_the_spec_says_swallow_errors_do_not_swallow_the_limit() {
    // IteratorClose on a throw completion discards the iterator's own error
    // (ECMA-262 7.4.11); a Promise combinator turns a throw into a rejection;
    // a getter runs under a native `Object.keys`-style call. None of these may
    // hide a limit error from the host.
    let looping_return = "var it = { [Symbol.iterator]() { return { \
        next() { return {value: 1, done: false} }, \
        return() { while (true) {} } } } };";
    for body in [
        "try { for (var x of it) { throw 'a' } } catch (e) {} 1",
        "try { var [a] = it; throw 'a' } catch (e) {} 1",
        "var o = { get p() { while (true) {} } }; try { JSON.stringify(o) } catch (e) {} 1",
        "var o = { toString() { while (true) {} } }; try { String(o) } catch (e) {} 1",
        "Promise.all([{ then() { while (true) {} } }]).catch(function () {}); 1",
        "try { [3, 2, 1].sort(function () { while (true) {} }) } catch (e) {} 1",
    ] {
        let mut host = host_with(small());
        let outcome = host.eval(&format!("{looping_return} {body}"));
        assert_eq!(outcome, stopped(), "{body}");
        assert_eq!(host.eval("1"), stopped(), "{body}");
    }
}

#[test]
fn script_limits_engine_a_buffer_over_the_cap_is_refused_as_an_ordinary_error() {
    let mut host = host_with(Limits {
        max_buffer_bytes: 1024 * 1024,
        ..small()
    });
    assert_eq!(
        host.eval("new ArrayBuffer(1024).byteLength"),
        Outcome::Value("1024".into())
    );
    match host.eval("new ArrayBuffer(2 * 1024 * 1024)") {
        Outcome::Error(message) => assert!(message.contains("RangeError"), "{message}"),
        other => panic!("expected a RangeError, got {other:?}"),
    }
    // A refused allocation is the script's own error, catchable, and not a
    // reason to stop the host.
    assert_eq!(
        host.eval("try { new ArrayBuffer(2 * 1024 * 1024); 'no' } catch (e) { 'caught' }"),
        Outcome::Value("caught".into())
    );
    assert_eq!(host.terminated(), None);
    // The same cap applies to a typed array's backing store.
    assert!(matches!(
        host.eval("new Uint8Array(2 * 1024 * 1024)"),
        Outcome::Error(_)
    ));
}
