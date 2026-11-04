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
