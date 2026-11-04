//! Test-only enforcement-point recorder.
//!
//! The crate invariant is that a check which exists but is not reached is not
//! a check. Each module lists its enforcement points in a
//! `pub const ENFORCEMENT_POINTS`, calls [`hit`] from inside the function
//! that enforces, and has a test that drives scenarios and then asserts every
//! listed point was actually reached ([`assert_all_hit`]). In a non-test build
//! `hit` is an empty function the optimiser removes.

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
thread_local! {
    static HITS: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Record that the enforcement point `name` was reached.
#[inline]
pub(crate) fn hit(name: &'static str) {
    #[cfg(test)]
    HITS.with(|h| h.borrow_mut().push(name));
    #[cfg(not(test))]
    let _ = name;
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
        for r in hits.iter() {
            assert!(
                points.contains(r),
                "reached point missing from ENFORCEMENT_POINTS: {r}"
            );
        }
    });
}
