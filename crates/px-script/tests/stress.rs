//! "No unbounded task-queue growth under a stress page" (ADR 040, Phase 10
//! gate).
//!
//! Each fixture is a page shape that would grow a queue or never yield if the
//! host let it. The assertions are counters, not wall-clock time: every
//! high-water mark is at or under its cap, and either the loop was still alive
//! and idle or bounded, or script was stopped for the stated reason and
//! nothing ran after.
//!
//! The high-water marks exist only in `testing` builds (build-spec 14.4); the
//! marks are asserted when they are compiled in, which is how `cargo xtask
//! script` runs this file. The behavioural assertions run either way.

use std::rc::Rc;

use px_script::clock::ManualClock;
use px_script::queue::Caps;
use px_script::{Host, HostOptions, LoopEnd, Outcome, Step, TerminateReason};

#[cfg(test)]
fn host_with(caps: Caps) -> Host {
    Host::with_options(HostOptions {
        caps,
        clock: Rc::new(ManualClock::at(0)),
    })
    .expect("host")
}

#[cfg(test)]
fn host() -> Host {
    host_with(Caps::default())
}

/// Every counter at or under its cap.
#[cfg(test)]
fn assert_marks_within_caps(host: &Host) {
    #[cfg(feature = "testing")]
    {
        let marks = host.high_water();
        let caps = host.caps();
        assert!(
            marks.tasks <= caps.tasks,
            "task queue {marks:?} over {caps:?}"
        );
        assert!(
            marks.microtasks <= caps.microtasks,
            "microtasks {marks:?} over {caps:?}"
        );
        assert!(
            marks.timers <= caps.timers,
            "timers {marks:?} over {caps:?}"
        );
        assert!(
            marks.checkpoint <= caps.checkpoint_budget,
            "checkpoint {marks:?} over {caps:?}"
        );
    }
    #[cfg(not(feature = "testing"))]
    let _ = host;
}

#[test]
fn script_stress_a_timer_that_rearms_itself_with_zero_delay_stays_at_depth_one() {
    let mut host = host();
    assert_eq!(
        host.eval("var n = 0; function f() { n++; setTimeout(f, 0); } setTimeout(f, 0);"),
        Outcome::Value("1".into()) // setTimeout returned timer id 1
    );
    // The page never goes idle: the caller's task limit is what ends this.
    assert_eq!(host.run_loop(5_000), LoopEnd::TaskLimit { ran: 5_000 });
    assert_eq!(
        host.terminated(),
        None,
        "a bound on size is not hit by depth one"
    );
    assert_eq!(host.eval("String(n)"), Outcome::Value("5000".into()));
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    {
        let marks = host.high_water();
        assert_eq!((marks.tasks, marks.timers), (1, 1));
    }
}

#[test]
fn script_stress_a_self_requeuing_microtask_is_stopped_by_the_budget() {
    // The queue never holds more than one job, so no size bound can fire. Only
    // the per-checkpoint budget catches `function f(){Promise.resolve().then(f)}`.
    let mut host = host_with(Caps {
        checkpoint_budget: 10_000,
        ..Caps::default()
    });
    assert_eq!(
        host.eval("function f() { Promise.resolve().then(f); } f();"),
        Outcome::Terminated(TerminateReason::MicrotaskBudget)
    );
    assert_eq!(host.terminated(), Some(TerminateReason::MicrotaskBudget));
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    {
        let marks = host.high_water();
        assert_eq!(marks.microtasks, 1, "depth one the whole time");
        assert_eq!(marks.checkpoint, 10_000, "exactly the budget ran");
    }
}

#[test]
fn script_stress_a_chain_exactly_at_the_budget_is_within_it_and_one_more_is_not() {
    // The same boundary as the default 1,000,000, at 20,000 so a debug build
    // runs it in a blink: a million Boa jobs take ~50 s unoptimised, twice.
    let budget = 20_000usize;
    let caps = Caps {
        checkpoint_budget: budget,
        ..Caps::default()
    };
    let chain = |limit: usize| {
        format!("var n = 0; function f() {{ if (++n < {limit}) Promise.resolve().then(f); }} f();")
    };
    // f runs once synchronously, then once per job: `limit - 1` jobs.
    let mut host = host_with(caps);
    assert!(matches!(host.eval(&chain(budget + 1)), Outcome::Value(_)));
    assert_eq!(host.terminated(), None);
    assert_eq!(
        host.eval("String(n)"),
        Outcome::Value((budget + 1).to_string())
    );
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    assert_eq!(host.high_water().checkpoint, budget);

    let mut host = host_with(caps);
    assert_eq!(
        host.eval(&chain(budget + 2)),
        Outcome::Terminated(TerminateReason::MicrotaskBudget)
    );
    #[cfg(feature = "testing")]
    assert_eq!(host.high_water().checkpoint, budget);
}

#[test]
fn script_stress_the_default_bounds_are_the_adr_040_starting_values() {
    assert_eq!(
        Caps::default(),
        Caps {
            tasks: 4_096,
            microtasks: 65_536,
            timers: 10_000,
            checkpoint_budget: 1_000_000
        }
    );
    assert_eq!(host().caps(), Caps::default());
}

#[test]
fn script_stress_a_flat_fan_out_under_the_cap_runs_to_completion() {
    let mut host = host();
    assert!(matches!(
        host.eval("var done = 0; for (let i = 0; i < 30000; i++) Promise.resolve().then(() => { done++; });"),
        Outcome::Value(_)
    ));
    assert_eq!(host.terminated(), None);
    assert_eq!(host.eval("String(done)"), Outcome::Value("30000".into()));
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    {
        let marks = host.high_water();
        assert_eq!((marks.microtasks, marks.checkpoint), (30_000, 30_000));
    }
}

#[test]
fn script_stress_a_million_job_fan_out_overflows_the_microtask_queue_and_stops() {
    // A tree of 10^6 reactions: every job queues ten more, six levels down. The
    // queue holds 65,536, so the tree is cut off at the cap long before it
    // grows to a million, and the jobs after that never run. (A flat loop of
    // 1e6 `then` calls reaches the same cap, but Boa keeps executing the loop
    // after termination and an unoptimised build takes ~45 s over it.)
    let mut host = host();
    assert_eq!(
        host.eval(
            "var jobs = 0;
             function g(depth) {
               jobs++;
               if (depth === 0) return;
               for (let i = 0; i < 10; i++) Promise.resolve().then(() => g(depth - 1));
             }
             g(6);"
        ),
        Outcome::Terminated(TerminateReason::MicrotaskQueueOverflow)
    );
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    assert_eq!(
        host.high_water().microtasks,
        65_536,
        "filled to the cap, never past it"
    );
    // Nothing runs afterwards, and nothing can be evaluated.
    assert_eq!(
        host.step(),
        Step::Terminated(TerminateReason::MicrotaskQueueOverflow)
    );
    assert_eq!(
        host.eval("1 + 1"),
        Outcome::Terminated(TerminateReason::MicrotaskQueueOverflow)
    );
}

#[test]
fn script_stress_twenty_thousand_intervals_hit_the_timer_cap() {
    let mut host = host();
    assert_eq!(
        host.eval("for (let i = 0; i < 20000; i++) setInterval(() => {}, 0);"),
        Outcome::Terminated(TerminateReason::TimerOverflow)
    );
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    assert_eq!(host.high_water().timers, 10_000);
    assert_eq!(
        host.run_loop(100),
        LoopEnd::Terminated {
            ran: 0,
            reason: TerminateReason::TimerOverflow
        }
    );
}

#[test]
fn script_stress_ten_thousand_intervals_fit_and_the_task_queue_applies_back_pressure() {
    // Exactly the timer cap. They are all due at once, but the task queue holds
    // 4,096: the rest wait in the table rather than overflow.
    let mut host = host();
    assert_eq!(
        host.eval(
            "var runs = 0; for (let i = 0; i < 10000; i++) setInterval(() => { runs++; }, 0); 0"
        ),
        Outcome::Value("0".into())
    );
    assert_eq!(host.run_loop(30_000), LoopEnd::TaskLimit { ran: 30_000 });
    assert_eq!(host.terminated(), None);
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    {
        let marks = host.high_water();
        assert_eq!(
            marks.tasks, 4_096,
            "the queue filled to its cap and no further"
        );
        assert_eq!(marks.timers, 10_000);
    }
}

#[test]
fn script_stress_a_timer_that_schedules_a_hundred_more_is_fine() {
    let mut host = host();
    host.eval(
        "var done = 0;
         setTimeout(() => { for (let i = 0; i < 100; i++) setTimeout(() => { done++; }, 0); }, 0);",
    );
    assert!(matches!(host.run_loop(1_000), LoopEnd::Idle { ran: 101 }));
    assert_eq!(host.eval("String(done)"), Outcome::Value("100".into()));
    assert_marks_within_caps(&host);
}

#[test]
fn script_stress_a_timer_that_schedules_a_hundred_of_itself_is_stopped_at_the_cap() {
    // 100-fold growth per generation: the timer table fills and script stops.
    let mut host = host();
    host.eval("function f() { for (let i = 0; i < 100; i++) setTimeout(f, 0); } f();");
    let end = host.run_loop(1_000_000);
    assert!(
        matches!(
            end,
            LoopEnd::Terminated {
                reason: TerminateReason::TimerOverflow,
                ..
            }
        ),
        "got {end:?}"
    );
    assert_marks_within_caps(&host);
}

#[test]
fn script_stress_set_and_clear_forever_does_not_grow_anything() {
    let mut host = host();
    assert_eq!(
        host.eval(
            "for (let i = 0; i < 200000; i++) clearTimeout(setTimeout(() => {}, 1000)); 'done'"
        ),
        Outcome::Value("done".into())
    );
    assert_eq!(host.terminated(), None);
    assert_marks_within_caps(&host);
    #[cfg(feature = "testing")]
    assert_eq!(host.high_water().timers, 1);
}

#[test]
fn script_stress_nothing_scheduled_before_termination_runs_after_it() {
    let mut host = host_with(Caps {
        microtasks: 10,
        ..Caps::default()
    });
    host.eval("var ran = false; setTimeout(() => { ran = true; }, 0);");
    assert_eq!(
        host.eval("for (let i = 0; i < 100; i++) Promise.resolve().then(() => {});"),
        Outcome::Terminated(TerminateReason::MicrotaskQueueOverflow)
    );
    assert_eq!(
        host.run_loop(10),
        LoopEnd::Terminated {
            ran: 0,
            reason: TerminateReason::MicrotaskQueueOverflow
        }
    );
    assert_eq!(
        host.step(),
        Step::Terminated(TerminateReason::MicrotaskQueueOverflow)
    );
}

#[test]
fn script_stress_a_timer_callback_that_overflows_terminates_from_inside_the_loop() {
    let mut host = host_with(Caps {
        microtasks: 8,
        ..Caps::default()
    });
    host.eval("setTimeout(() => { for (let i = 0; i < 100; i++) queueMicrotask(() => {}); }, 0);");
    assert_eq!(
        host.run_loop(10),
        LoopEnd::Terminated {
            ran: 0, // the task that overflowed is not counted: it did not return Ran
            reason: TerminateReason::MicrotaskQueueOverflow
        }
    );
}
