//! The coarse clock as script sees it: `performance.now`, `Date.now`, timer due
//! times, and the seeded `Math.random` (ADR 040).
//!
//! The pure properties of the clock itself are in `src/clock.rs`; this file is
//! about the wiring: that every time a script can read comes from that clock.

use std::rc::Rc;

use px_script::clock::{ManualClock, QUANTUM_NS};
use px_script::{Host, HostOptions, Outcome};

#[cfg(test)]
fn host_with(seed: u64, clock: Rc<ManualClock>, wall_epoch_ms: i64) -> Host {
    Host::with_options(HostOptions {
        clock,
        wall_epoch_ms,
        ..HostOptions::new(seed)
    })
    .expect("host")
}

#[cfg(test)]
fn number(host: &mut Host, source: &str) -> f64 {
    match host.eval(source) {
        Outcome::Value(text) => text.parse().expect("a number"),
        other => panic!("expected a number from {source}, got {other:?}"),
    }
}

#[test]
fn script_clock_host_performance_now_is_zero_at_the_start_and_on_the_grid_after() {
    let clock = Rc::new(ManualClock::at(0));
    let mut host = host_with(1, clock.clone(), 0);
    assert_eq!(number(&mut host, "performance.now()"), 0.0);
    // Whole quanta: the coarse clock is exact on its grid.
    clock.advance(25 * QUANTUM_NS);
    assert_eq!(number(&mut host, "performance.now()"), 2.5);
    clock.advance(QUANTUM_NS);
    assert_eq!(number(&mut host, "performance.now()"), 2.6);
}

#[test]
fn script_clock_host_between_grid_points_it_is_one_of_the_two_neighbours() {
    // 3.3 quanta in: the answer is 3 Q or 4 Q, that is 0.3 ms or 0.4 ms.
    for seed in 0..20 {
        let clock = Rc::new(ManualClock::at(0));
        let mut host = host_with(seed, clock.clone(), 0);
        clock.set(330_000);
        let ms = number(&mut host, "performance.now()");
        assert!(ms == 0.3 || ms == 0.4, "seed {seed}: {ms}");
    }
}

#[test]
fn script_clock_host_performance_now_never_goes_backward_and_has_no_sub_grid_bits() {
    let clock = Rc::new(ManualClock::at(0));
    let mut host = host_with(9, clock.clone(), 0);
    let mut last = 0.0;
    for step in 0..2000u64 {
        clock.set(step * 3_137); // an awkward stride
        let now = number(&mut host, "performance.now()");
        assert!(now >= last);
        // Multiples of 0.1 ms: ten times the value is an integer within float
        // noise.
        assert!((now * 10.0 - (now * 10.0).round()).abs() < 1e-6, "{now}");
        last = now;
    }
}

#[test]
fn script_clock_host_date_now_is_the_wall_epoch_plus_coarse_elapsed_milliseconds() {
    let clock = Rc::new(ManualClock::at(0));
    let mut host = host_with(1, clock.clone(), 1_700_000_000_000);
    assert_eq!(number(&mut host, "Date.now()"), 1_700_000_000_000.0);
    clock.advance(1_500 * 1_000_000 + 4 * QUANTUM_NS);
    // 1.5004 s on the grid: floored to whole milliseconds.
    assert_eq!(number(&mut host, "Date.now()"), 1_700_000_001_500.0);
}

#[test]
fn script_clock_host_date_now_never_goes_backward_across_reads() {
    let clock = Rc::new(ManualClock::at(0));
    let mut host = host_with(4, clock.clone(), 5_000);
    let mut last = 0.0;
    for step in 0..500u64 {
        clock.set(step * 1_234_567);
        let now = number(&mut host, "Date.now()");
        assert!(now >= last);
        last = now;
    }
}

#[test]
fn script_clock_host_math_random_is_replaced_and_seeded() {
    let first = |seed: u64| {
        let mut host = host_with(seed, Rc::new(ManualClock::at(0)), 0);
        (0..5)
            .map(|_| number(&mut host, "Math.random()"))
            .collect::<Vec<f64>>()
    };
    // Reproducible from the seed.
    assert_eq!(first(7), first(7));
    // And different under another.
    assert_ne!(first(7), first(8));
    // In [0, 1).
    assert!(first(7).iter().all(|v| (0.0..1.0).contains(v)));
}

#[test]
fn script_clock_host_math_random_is_ours_and_looks_uniform() {
    let mut host = host_with(3, Rc::new(ManualClock::at(0)), 0);
    assert_eq!(
        host.eval("Math.random.name + ',' + Math.random.length + ',' + typeof Math.random"),
        Outcome::Value("random,0,function".into())
    );
    let mean = number(
        &mut host,
        "var s = 0; for (var i = 0; i < 20000; i++) s += Math.random(); s / 20000",
    );
    assert!((mean - 0.5).abs() < 0.02, "mean {mean}");
    // Not a constant, and not a short cycle.
    assert_eq!(
        host.eval("var seen = {}; for (var i = 0; i < 1000; i++) seen[Math.random()] = 1; Object.keys(seen).length"),
        Outcome::Value("1000".into())
    );
}

#[test]
fn script_clock_host_timer_due_times_are_on_the_coarse_clock() {
    use px_script::Step;
    let clock = Rc::new(ManualClock::at(0));
    let mut host = host_with(1, clock.clone(), 0);
    host.eval("setTimeout(() => {}, 3);");
    // 3 ms is 30 quanta, exactly, however the jitter falls.
    assert_eq!(
        host.step(),
        Step::Waiting(std::time::Duration::from_millis(3))
    );
}
