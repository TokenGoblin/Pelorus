//! The content process's watchdog (ADR 040, layer 2 of the runaway-script
//! limits).
//!
//! Boa has no interrupt. A native builtin (`/(a+)+$/`, `'x'.repeat(2**30)`, a
//! 3-million-element `sort`) or a loop of calls that the per-frame loop limit
//! never sees runs until it finishes, and nothing in the engine can stop it.
//! The engine's `Context` is `!Send`, so another thread cannot interrupt it
//! either. What another thread can do is watch the script thread's
//! [`Heartbeat`] and, when script has run for [`WATCHDOG_LIMIT`] without a task
//! or microtask boundary, end the process.
//!
//! Ending the process is the right response and not a failure of design: this
//! process is sandboxed and holds nothing the broker cannot rebuild, ADR 004
//! builds it with `panic = "abort"`, and a dead content process is routine to
//! the broker (build-spec 4.3), which keeps the frame tree and shows "page
//! unresponsive". `std::process::abort()` is used rather than `exit` so no
//! destructor, `atexit` handler or buffered writer runs on the way out: the
//! thread that is wedged owns state this thread must not touch.
//!
//! The decision is a pure function of a [`Beat`] and a clock reading
//! ([`Tracker::observe`]) so it is tested without threads or sleeping; the
//! thread is the thin shell around it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use px_script::Heartbeat;
use px_script::watch::Beat;

/// How long script may run without passing a task or microtask boundary before
/// the process aborts itself.
///
/// ADR 040's starting value, "to be tuned by measurement". It is deliberately
/// generous: a boundary is passed after every microtask and every timer
/// callback, so a page that is merely slow passes many; only a single
/// uninterruptible call (or a call tree the loop limit cannot see) lasts this
/// long. Tests do not wait this long: a `testing` build lets the peer shorten
/// it (`ScriptRequest::WatchdogMillis`), and a release build has no such knob.
pub const WATCHDOG_LIMIT: Duration = Duration::from_secs(10);

/// The limit in force, shared between the thread that reads it and the testing
/// knob that may change it. Milliseconds.
pub type SharedLimit = Arc<AtomicU64>;

/// A fresh limit holding [`WATCHDOG_LIMIT`].
pub fn default_limit() -> SharedLimit {
    let ms = u64::try_from(WATCHDOG_LIMIT.as_millis()).unwrap_or(u64::MAX);
    Arc::new(AtomicU64::new(ms))
}

/// What one look at the heartbeat concluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Idle, or making progress.
    Fine,
    /// Script has been running with no boundary for at least the limit.
    Stalled,
}

/// Remembers when the heartbeat last changed.
#[derive(Debug)]
pub struct Tracker {
    last: Beat,
    /// The clock reading at which `last` was first seen.
    since: Duration,
}

impl Tracker {
    /// A tracker that has seen an idle process at `now`.
    pub fn new(now: Duration) -> Self {
        Self {
            last: Beat {
                count: 0,
                active: false,
            },
            since: now,
        }
    }

    /// Look at the heartbeat at clock reading `now` (any monotonic origin).
    ///
    /// The clock restarts whenever the count moves or the script thread is idle:
    /// an idle content process, waiting on the broker, must never be aborted,
    /// and entering script counts as a beat, so "idle, then active with the
    /// same count" cannot be taken for one long stall.
    pub fn observe(&mut self, now: Duration, beat: Beat, limit: Duration) -> Verdict {
        let moved = beat.count != self.last.count || !self.last.active;
        self.last = beat;
        if !beat.active || moved {
            self.since = now;
            return Verdict::Fine;
        }
        if now.saturating_sub(self.since) >= limit {
            Verdict::Stalled
        } else {
            Verdict::Fine
        }
    }
}

/// How often the thread looks: an eighth of the limit, between 2 ms and
/// 250 ms. Fine enough that a stall is caught within about one eighth of the
/// limit late, coarse enough to cost nothing.
pub fn tick(limit: Duration) -> Duration {
    (limit / 8).clamp(Duration::from_millis(2), Duration::from_millis(250))
}

/// Start the watchdog thread. It lives as long as the process.
///
/// If the thread cannot be created the process ends: running script with no
/// watchdog is the one state this design refuses (fail closed).
pub fn spawn(heartbeat: Arc<Heartbeat>, limit: SharedLimit) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("watchdog".into())
        .spawn(move || watch(&heartbeat, &limit))
        .map(|_| ())
}

fn watch(heartbeat: &Heartbeat, limit: &AtomicU64) {
    let origin = Instant::now();
    let mut tracker = Tracker::new(Duration::ZERO);
    loop {
        // A limit of zero would abort an idle-to-active transition at once; the
        // knob refuses zero, and this keeps the invariant if it were bypassed.
        let limit = Duration::from_millis(limit.load(Ordering::SeqCst).max(1));
        std::thread::sleep(tick(limit));
        if tracker.observe(origin.elapsed(), heartbeat.snapshot(), limit) == Verdict::Stalled {
            eprintln!(
                "px-content: script ran {} ms without a task boundary; aborting",
                limit.as_millis()
            );
            std::process::abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: Duration = Duration::from_millis(100);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn running(count: u64) -> Beat {
        Beat {
            count,
            active: true,
        }
    }

    fn idle(count: u64) -> Beat {
        Beat {
            count,
            active: false,
        }
    }

    #[test]
    fn script_limits_watchdog_default_is_ten_seconds() {
        assert_eq!(WATCHDOG_LIMIT, Duration::from_secs(10));
        assert_eq!(default_limit().load(Ordering::SeqCst), 10_000);
    }

    #[test]
    fn script_limits_watchdog_an_idle_process_is_never_stalled() {
        let mut t = Tracker::new(ms(0));
        for step in 0..1000 {
            assert_eq!(
                t.observe(ms(step * 1000), idle(3), LIMIT),
                Verdict::Fine,
                "idle at {step}"
            );
        }
    }

    #[test]
    fn script_limits_watchdog_a_run_with_no_beat_stalls_at_the_limit_not_before() {
        let mut t = Tracker::new(ms(0));
        assert_eq!(t.observe(ms(0), running(1), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(50), running(1), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(99), running(1), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(100), running(1), LIMIT), Verdict::Stalled);
        assert_eq!(t.observe(ms(500), running(1), LIMIT), Verdict::Stalled);
    }

    #[test]
    fn script_limits_watchdog_each_beat_restarts_the_clock() {
        let mut t = Tracker::new(ms(0));
        t.observe(ms(0), running(1), LIMIT);
        // A new boundary every 90 ms keeps a long-running page alive forever.
        for i in 1..100u64 {
            assert_eq!(t.observe(ms(i * 90), running(1 + i), LIMIT), Verdict::Fine);
        }
        // And then it stops beating.
        let stopped = 99 * 90; // the last beat, count 100
        assert_eq!(
            t.observe(ms(stopped + 99), running(100), LIMIT),
            Verdict::Fine
        );
        assert_eq!(
            t.observe(ms(stopped + 100), running(100), LIMIT),
            Verdict::Stalled
        );
    }

    #[test]
    fn script_limits_watchdog_going_idle_then_active_with_the_same_count_restarts_the_clock() {
        let mut t = Tracker::new(ms(0));
        t.observe(ms(0), running(5), LIMIT);
        assert_eq!(t.observe(ms(90), running(5), LIMIT), Verdict::Fine);
        // The run ended; a long idle follows.
        assert_eq!(t.observe(ms(10_000), idle(5), LIMIT), Verdict::Fine);
        // A new run starts. Same count seen again, but not a continuation.
        assert_eq!(t.observe(ms(10_010), running(5), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(10_100), running(5), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(10_110), running(5), LIMIT), Verdict::Stalled);
    }

    #[test]
    fn script_limits_watchdog_a_clock_that_steps_back_does_not_panic_or_stall() {
        let mut t = Tracker::new(ms(1000));
        assert_eq!(t.observe(ms(0), running(1), LIMIT), Verdict::Fine);
        assert_eq!(t.observe(ms(0), running(1), LIMIT), Verdict::Fine);
    }

    #[test]
    fn script_limits_watchdog_tick_is_an_eighth_of_the_limit_within_bounds() {
        assert_eq!(tick(Duration::from_secs(10)), ms(250));
        assert_eq!(tick(ms(800)), ms(100));
        assert_eq!(tick(ms(8)), ms(2));
        assert_eq!(tick(ms(1)), ms(2));
        // The thread always looks several times within one limit.
        for limit in [20u64, 100, 800, 10_000] {
            assert!(tick(ms(limit)) * 4 <= ms(limit), "{limit}");
        }
    }
}
