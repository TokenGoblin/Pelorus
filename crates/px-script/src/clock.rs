//! The time sources under the host (ADR 040).
//!
//! [`RawClock`] is the one place real time enters this crate, so that everything
//! that depends on it can be tested against a fake. Production wires
//! [`StdClock`]; tests wire [`ManualClock`].

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A monotonic source of integer nanoseconds.
pub trait RawClock {
    /// Nanoseconds since an arbitrary fixed start. Never decreases.
    fn now_ns(&self) -> u64;

    /// Block the calling thread until [`Self::now_ns`] reaches `target_ns`.
    ///
    /// The default sleeps. A fake overrides it to move its own time forward,
    /// which is what lets a test run a loop that "waits" 10 seconds in no time.
    fn wait_until_ns(&self, target_ns: u64) {
        let now = self.now_ns();
        if target_ns > now {
            std::thread::sleep(Duration::from_nanos(target_ns - now));
        }
    }
}

impl<T: RawClock + ?Sized> RawClock for Rc<T> {
    fn now_ns(&self) -> u64 {
        (**self).now_ns()
    }

    fn wait_until_ns(&self, target_ns: u64) {
        (**self).wait_until_ns(target_ns);
    }
}

/// The production raw clock: `std::time::Instant` from process start of the
/// host.
#[derive(Debug)]
pub struct StdClock {
    start: Instant,
}

impl StdClock {
    /// A clock whose zero is now.
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for StdClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RawClock for StdClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.start.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

/// A clock that moves only when told to, and when the host would wait. For
/// tests: deterministic, and instant.
#[derive(Debug, Default)]
pub struct ManualClock {
    ns: Cell<u64>,
}

impl ManualClock {
    /// A clock reading `ns`.
    pub fn at(ns: u64) -> Self {
        Self { ns: Cell::new(ns) }
    }

    /// Move forward by `delta_ns`.
    pub fn advance(&self, delta_ns: u64) {
        self.ns.set(self.ns.get().saturating_add(delta_ns));
    }

    /// Set the reading. Moving backward is refused (a clock is monotonic).
    pub fn set(&self, ns: u64) {
        if ns >= self.ns.get() {
            self.ns.set(ns);
        }
    }
}

impl RawClock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.ns.get()
    }

    fn wait_until_ns(&self, target_ns: u64) {
        self.set(target_ns);
    }
}

/// The coarse clock's grid: 100 microseconds (ADR 040).
pub const QUANTUM_NS: u64 = 100_000;

/// A fixed 64-bit mixer (splitmix64's finaliser) that we own: no dependency, no
/// state, and the same output on every platform.
pub(crate) fn mix(mut z: u64) -> u64 {
    z ^= z >> 30;
    z = z.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z ^= z >> 27;
    z = z.wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// The jitter hash for quantum `q`: a pure function of the seed and the
/// quantum, **not** of the read, so it is the same for every read of that
/// quantum.
pub fn quantum_hash(seed: u64, q: u64) -> u64 {
    mix(seed.wrapping_add(q.wrapping_mul(0x9E37_79B9_7F4A_7C15)))
}

/// The coarsening rule, with the hash given, so the arithmetic can be checked
/// by hand. For a raw instant `t` in nanoseconds and the quantum's hash `h`:
///
/// ```text
/// q    = t / Q          frac = t % Q
/// up   = h % Q < frac
/// out  = max(previous, (q + up) * Q)
/// ```
///
/// The output is a multiple of `Q`, within `Q` of `t`, never below `previous`,
/// and (because `up` flips from false to true at most once as `frac` grows) it
/// changes at most once inside a quantum, at an instant only the seed's holder
/// can place.
pub fn coarsen_with_hash(t_ns: u64, hash: u64, previous: u64) -> u64 {
    let q = t_ns / QUANTUM_NS;
    let frac = t_ns % QUANTUM_NS;
    let up = hash % QUANTUM_NS < frac;
    let rounded = q.saturating_add(u64::from(up)).saturating_mul(QUANTUM_NS);
    rounded.max(previous)
}

/// Whether a read `frac` nanoseconds into quantum `q` rounds up. For the jitter
/// property tests.
pub fn rounds_up(seed: u64, q: u64, frac_ns: u64) -> bool {
    quantum_hash(seed, q) % QUANTUM_NS < frac_ns
}

/// Every time source a script can read, derived from one raw clock (ADR 040
/// "Timer coarsening"): `performance.now`, `Date.now`, Boa's own `Clock`, and
/// timer due times.
///
/// Interior mutability only for the monotonic floor; one thread owns it.
#[derive(Debug)]
pub struct CoarseClock<R: RawClock> {
    raw: R,
    seed: u64,
    previous: Cell<u64>,
}

impl<R: RawClock> CoarseClock<R> {
    /// A coarse view of `raw`, jittered by `seed` (given to this process by the
    /// broker; the process needs no entropy source of its own).
    pub fn new(raw: R, seed: u64) -> Self {
        Self {
            raw,
            seed,
            previous: Cell::new(0),
        }
    }

    /// Coarse nanoseconds: a multiple of [`QUANTUM_NS`], within one quantum of
    /// the raw time, and never less than any earlier return.
    pub fn now_ns(&self) -> u64 {
        let t = self.raw.now_ns();
        let hash = quantum_hash(self.seed, t / QUANTUM_NS);
        let out = coarsen_with_hash(t, hash, self.previous.get());
        self.previous.set(out);
        out
    }

    /// The raw clock, for waiting.
    pub fn raw(&self) -> &R {
        &self.raw
    }

    /// Block until the coarse time reaches `target_ns`. The raw clock is asked
    /// to wait only as long as the coarse clock still has to go, and asked again
    /// if jitter left it short, so this returns with `now_ns() >= target_ns`.
    pub fn wait_until_ns(&self, target_ns: u64) {
        loop {
            let now = self.now_ns();
            if now >= target_ns {
                return;
            }
            let raw_now = self.raw.now_ns();
            self.raw
                .wait_until_ns(raw_now.saturating_add(target_ns - now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_clock_manual_waits_by_jumping_and_never_goes_back() {
        let clock = ManualClock::at(10);
        clock.wait_until_ns(50);
        assert_eq!(clock.now_ns(), 50);
        clock.wait_until_ns(20);
        assert_eq!(clock.now_ns(), 50);
        clock.set(5);
        assert_eq!(clock.now_ns(), 50);
        clock.advance(7);
        assert_eq!(clock.now_ns(), 57);
    }

    // ---- the arithmetic, by hand -------------------------------------------

    #[test]
    fn script_clock_prop_the_rule_by_hand() {
        let q = QUANTUM_NS;
        // t = 250_000: quantum 2, 50_000 into it.
        // hash % Q = 10_000 < 50_000, so up: (2 + 1) * Q = 300_000.
        assert_eq!(coarsen_with_hash(250_000, 10_000, 0), 300_000);
        // hash % Q = 60_000 is not < 50_000, so down: 2 * Q = 200_000.
        assert_eq!(coarsen_with_hash(250_000, 60_000, 0), 200_000);
        // The comparison is strict: hash % Q == frac rounds down.
        assert_eq!(coarsen_with_hash(250_000, 50_000, 0), 200_000);
        // The hash is reduced mod Q first: Q + 10_000 behaves as 10_000.
        assert_eq!(coarsen_with_hash(250_000, q + 10_000, 0), 300_000);
        // On the grid (frac = 0) nothing is below it, so it never rounds up.
        assert_eq!(coarsen_with_hash(300_000, 0, 0), 300_000);
        assert_eq!(coarsen_with_hash(0, 0, 0), 0);
        // Monotonic floor: a down decision after an up one does not step back.
        assert_eq!(coarsen_with_hash(250_000, 60_000, 300_000), 300_000);
        // The last nanosecond of a quantum with the largest possible hash.
        assert_eq!(coarsen_with_hash(299_999, q - 1, 0), 200_000);
        assert_eq!(coarsen_with_hash(299_999, q - 2, 0), 300_000);
    }

    #[test]
    fn script_clock_prop_the_hash_is_per_quantum_and_per_seed() {
        assert_eq!(quantum_hash(7, 12), quantum_hash(7, 12));
        assert_ne!(quantum_hash(7, 12), quantum_hash(7, 13));
        assert_ne!(quantum_hash(7, 12), quantum_hash(8, 12));
    }

    // ---- the seven properties of ADR 040, over a fake raw clock ------------

    /// Reads `clock` at each raw time in `times`, moving the fake there first.
    fn sweep(seed: u64, times: impl Iterator<Item = u64>) -> Vec<(u64, u64)> {
        let clock = CoarseClock::new(ManualClock::at(0), seed);
        times
            .map(|t| {
                clock.raw().set(t);
                (t, clock.now_ns())
            })
            .collect()
    }

    /// 1 ns steps over 50 ms are 50 million reads; 7 ns is coprime with the
    /// quantum, visits every phase within every quantum, and keeps an
    /// unoptimised build under a few seconds.
    const SEEDS: [u64; 3] = [0, 1, 0xDEAD_BEEF_F00D_CAFE];

    #[test]
    fn script_clock_prop_every_output_is_a_multiple_of_the_quantum() {
        for seed in SEEDS {
            for (t, out) in sweep(seed, (0..50_000_000).step_by(7)) {
                assert_eq!(out % QUANTUM_NS, 0, "seed {seed} at t = {t}: {out}");
            }
        }
    }

    #[test]
    fn script_clock_prop_the_output_is_within_one_quantum_of_the_true_time() {
        for seed in SEEDS {
            // Start away from zero so "below" is not clipped at the origin.
            for (t, out) in sweep(seed, (1_000_000..51_000_000).step_by(7)) {
                assert!(
                    out.abs_diff(t) <= QUANTUM_NS,
                    "seed {seed}: t = {t}, out = {out}"
                );
            }
        }
    }

    #[test]
    fn script_clock_prop_outputs_never_decrease() {
        for seed in SEEDS {
            let outs = sweep(seed, (0..50_000_000).step_by(7));
            assert!(outs.windows(2).all(|w| w[0].1 <= w[1].1), "seed {seed}");
        }
    }

    #[test]
    fn script_clock_prop_a_quantum_changes_at_most_once_and_only_upward_so_rereading_cannot_average()
     {
        // ADR 040 says reads "within one quantum" are identical. They cannot be
        // identical for every read and also give 100 distinct values per 10 ms
        // (property 5), so the rule is the stated formula `hash % Q < frac`:
        // inside a quantum the output is `q * Q` until `frac` passes a threshold
        // only the seed's holder knows, then `(q + 1) * Q`. What does hold, and
        // what defeats averaging, is: the same raw time always gives the same
        // answer, and each quantum has one switch point, not a noisy flicker.
        for seed in SEEDS {
            let outs = sweep(seed, 0..2_000_000u64);
            for q in 0..20u64 {
                let in_quantum: Vec<u64> = outs
                    .iter()
                    .filter(|(t, _)| t / QUANTUM_NS == q)
                    .map(|(_, out)| *out)
                    .collect();
                let mut distinct = in_quantum.clone();
                distinct.dedup();
                assert!(distinct.len() <= 2, "seed {seed} quantum {q}: {distinct:?}");
                assert!(
                    distinct
                        .iter()
                        .all(|v| *v == q * QUANTUM_NS || *v == (q + 1) * QUANTUM_NS),
                    "seed {seed} quantum {q}: {distinct:?}"
                );
            }
            // Re-reading one raw time repeatedly is stable.
            let clock = CoarseClock::new(ManualClock::at(1_234_567), seed);
            let first = clock.now_ns();
            assert!((0..100).all(|_| clock.now_ns() == first));
        }
    }

    #[test]
    fn script_clock_prop_ten_milliseconds_give_a_hundred_distinct_values() {
        for seed in SEEDS {
            let mut distinct: Vec<u64> = sweep(seed, (0..10_000_000).step_by(1000))
                .into_iter()
                .map(|(_, out)| out)
                .collect();
            distinct.dedup();
            // Every grid point 0 ..= 99 Q is read exactly on the grid, and the
            // last quantum may end on 100 Q: 100, or 101 at the very most.
            assert!(
                (99..=101).contains(&distinct.len()),
                "seed {seed}: {} distinct values",
                distinct.len()
            );
        }
    }

    #[test]
    fn script_clock_prop_the_same_seed_gives_the_same_output_and_another_seed_does_not() {
        let times = || (0..20_000_000u64).step_by(997);
        assert_eq!(sweep(5, times()), sweep(5, times()));
        let a = sweep(5, times());
        let b = sweep(6, times());
        let differing = a.iter().zip(&b).filter(|(x, y)| x.1 != y.1).count();
        assert!(
            differing > 1000,
            "two seeds agreed on all but {differing} reads"
        );
    }

    #[test]
    fn script_clock_prop_the_jitter_is_not_constant_and_is_about_half_at_half_a_quantum() {
        // Property 7: at frac = Q/2 the round-up decision is a coin flip across
        // quanta. 100,000 quanta; a fair coin is 50,000 +- 158 (one sigma), so
        // +-1,000 is a six-sigma tolerance.
        for seed in SEEDS {
            let ups = (0..100_000u64)
                .filter(|q| rounds_up(seed, *q, QUANTUM_NS / 2))
                .count();
            assert!(
                (49_000..=51_000).contains(&ups),
                "seed {seed}: {ups} of 100000 rounded up"
            );
        }
    }

    #[test]
    fn script_clock_prop_a_later_phase_rounds_up_whenever_an_earlier_one_did() {
        // Threshold form: if a read rounds up at `frac`, every later read of the
        // same quantum does. That is what makes one switch per quantum.
        for q in 0..2000u64 {
            let mut was_up = false;
            for frac in (0..QUANTUM_NS).step_by(500) {
                let up = rounds_up(3, q, frac);
                assert!(!was_up || up, "quantum {q} flickered at {frac}");
                was_up = up;
            }
        }
    }

    #[test]
    fn script_clock_prop_waiting_reaches_the_target_on_the_coarse_clock() {
        for seed in SEEDS {
            let clock = CoarseClock::new(ManualClock::at(12_345), seed);
            let target = 10 * QUANTUM_NS;
            clock.wait_until_ns(target);
            assert!(clock.now_ns() >= target);
            // And it did not run far past it: within the quantum plus a jitter.
            assert!(clock.now_ns() <= target + 2 * QUANTUM_NS);
        }
    }

    #[test]
    fn script_clock_std_never_decreases() {
        let clock = StdClock::new();
        let mut last = clock.now_ns();
        for _ in 0..1000 {
            let now = clock.now_ns();
            assert!(now >= last);
            last = now;
        }
    }
}
