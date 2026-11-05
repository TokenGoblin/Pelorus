//! Test-only enforcement-point recorder.
//!
//! The crate invariant is that a check which exists but is not reached is not
//! a check. Each module lists its enforcement points in a
//! `pub const ENFORCEMENT_POINTS`, calls [`hit`] from inside the function
//! that enforces, and has a test that drives scenarios and then asserts every
//! listed point was actually reached ([`assert_all_hit`]). In a non-test build
//! `hit` is an empty function the optimiser removes.

#[cfg(any(test, feature = "trace"))]
use std::cell::RefCell;

#[cfg(any(test, feature = "trace"))]
thread_local! {
    static HITS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Record that the enforcement point `name` was reached.
#[inline]
pub(crate) fn hit(name: &'static str) {
    #[cfg(any(test, feature = "trace"))]
    HITS.with(|h| h.borrow_mut().push(name));
    #[cfg(not(any(test, feature = "trace")))]
    let _ = name;
}

/// Forget everything recorded on this thread. Present only with the `trace`
/// feature, which px-net enables from its own `testing` feature so that an
/// integration test can show an enforcement point was reached *through a
/// fetch* (px-net's `tests/enforcement_points.rs`). The log is per thread:
/// a fetch must run on the thread that reads the log.
#[cfg(feature = "trace")]
pub fn clear() {
    HITS.with(|h| h.borrow_mut().clear());
}

/// Every enforcement point reached on this thread since [`clear`], in order.
#[cfg(feature = "trace")]
pub fn snapshot() -> Vec<&'static str> {
    HITS.with(|h| h.borrow().clone())
}

/// Forget everything recorded on this thread.
#[cfg(test)]
pub(crate) fn reset() {
    HITS.with(|h| h.borrow_mut().clear());
}

/// Fail unless every name in `points` was recorded since the last [`reset`],
/// and every recorded name is listed (so the list cannot rot in either
/// direction).
#[cfg(test)]
pub(crate) fn assert_all_hit(points: &[&str]) {
    HITS.with(|h| {
        let hits = h.borrow();
        for p in points {
            assert!(hits.contains(p), "enforcement point never reached: {p}");
        }
        // Only this module's own points: policies call each other.
        let module = points.first().and_then(|p| p.split("::").next());
        for r in hits.iter().filter(|r| r.split("::").next() == module) {
            assert!(
                points.contains(r),
                "reached point missing from ENFORCEMENT_POINTS: {r}"
            );
        }
    });
}
