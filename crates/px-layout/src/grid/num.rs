//! Wide integer arithmetic for the grid algorithm.
//!
//! `fr` shares, percentages and space distribution multiply an `Au` by a factor
//! and divide by another. Done on `Au` directly the product saturates early (a
//! 10,000px leftover times 3,000 milli-fr leaves the `i32` range), and done in
//! floating point it makes repeat runs depend on rounding. So the operands are
//! widened to `i128` once, here, combined, and clamped back through
//! [`crate::geom::au`].
//!
//! Reading `Au`'s field to widen it is the one place this crate looks inside the
//! type; it never computes on the field in place. Every `Au` is within
//! `+-(2^30 - 1)`, so the products below cannot overflow `i128`.

use app_units::Au;

use crate::geom::au;

/// An `Au` as a wide integer, in app units.
#[must_use]
pub(crate) fn wide(value: Au) -> i64 {
    i64::from(value.0)
}

/// A wide integer back to an `Au`, saturating at the type's range.
#[must_use]
pub(crate) fn narrow(value: i128) -> Au {
    let clamped = value.clamp(i128::from(i32::MIN), i128::from(i32::MAX));
    au(i32::try_from(clamped).unwrap_or(0))
}

/// Units of work per grid container: roughly a second on a slow machine.
pub(crate) const FUEL: u64 = 40_000_000;

/// A bound on the work one grid layout may do.
///
/// Auto-placement and track sizing are polynomial in the number of items and
/// tracks, and a page controls both (`repeat(10000, 1px)` and 100,000 items). A
/// page that makes every loop run to its maximum can hold a layout thread for
/// minutes. Every inner loop spends from this budget; when it runs out the loop
/// stops early and the algorithm carries on with what it has. The result is wrong
/// for such a page and identical on every run, which is the trade a hostile input
/// is owed.
pub(crate) struct Fuel {
    left: u64,
}

impl Fuel {
    #[must_use]
    pub(crate) fn new() -> Self {
        Self { left: FUEL }
    }

    /// Spend `amount`. `false` means the budget is exhausted: stop looping.
    pub(crate) fn spend(&mut self, amount: usize) -> bool {
        let amount = u64::try_from(amount).unwrap_or(u64::MAX);
        if self.left < amount {
            self.left = 0;
            return false;
        }
        self.left -= amount;
        true
    }
}
