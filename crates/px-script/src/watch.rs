//! The liveness signal the content process's watchdog reads (ADR 040, layer 2).
//!
//! Boa has no interrupt, so a native builtin (`/(a+)+$/`, a huge `repeat`) runs
//! to completion however long it takes and nothing in this crate can stop it.
//! What this crate can do is say, from the thread that runs script, when it is
//! at a boundary: a task or microtask has just started or finished. A watchdog
//! on another thread (`px-content`) compares a [`Heartbeat`] snapshot with the
//! clock and ends the process when script has been running for too long
//! without one. This module is only the signal; it never aborts anything.
//!
//! Two facts are published, both plain atomics:
//!
//! * `beats`, a counter that moves at every boundary, and
//! * `active`, whether the script thread is inside script at all. A content
//!   process that is idle, waiting for the broker, must never be aborted.
//!
//! Entering script also bumps the counter, so a watchdog that sees "inactive"
//! and then "active" with the same counter cannot mistake a new run for the
//! old stall.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A snapshot of a [`Heartbeat`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Beat {
    /// How many boundaries have passed, ever (wrapping is irrelevant: only
    /// "changed" is asked).
    pub count: u64,
    /// Whether the script thread is inside script now.
    pub active: bool,
}

/// Written by the script thread, read by the watchdog.
#[derive(Debug, Default)]
pub struct Heartbeat {
    count: AtomicU64,
    active: AtomicBool,
}

impl Heartbeat {
    /// A heartbeat that is inactive and at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// A boundary passed.
    pub fn beat(&self) {
        self.count.fetch_add(1, Ordering::SeqCst);
    }

    /// Script is about to run. Counts as a beat. Dropping the guard marks the
    /// thread idle again (and counts as one).
    pub fn enter(&self) -> Active<'_> {
        self.beat();
        self.active.store(true, Ordering::SeqCst);
        Active(self)
    }

    /// What the watchdog sees now.
    pub fn snapshot(&self) -> Beat {
        // `active` first: a run that ends between the two reads then shows as
        // active with a stale count at worst, and the next sample corrects it.
        let active = self.active.load(Ordering::SeqCst);
        let count = self.count.load(Ordering::SeqCst);
        Beat { count, active }
    }
}

/// Script is running while this exists.
#[must_use]
pub struct Active<'a>(&'a Heartbeat);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::SeqCst);
        self.0.beat();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_limits_heartbeat_starts_idle() {
        let h = Heartbeat::new();
        assert_eq!(
            h.snapshot(),
            Beat {
                count: 0,
                active: false
            }
        );
    }

    #[test]
    fn script_limits_heartbeat_entering_and_leaving_are_beats() {
        let h = Heartbeat::new();
        let before = h.snapshot().count;
        {
            let _run = h.enter();
            let inside = h.snapshot();
            assert!(inside.active);
            assert_ne!(inside.count, before);
            h.beat();
            assert_ne!(h.snapshot().count, inside.count);
        }
        let after = h.snapshot();
        assert!(!after.active);
        assert!(after.count > before);
    }
}
