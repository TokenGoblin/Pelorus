//! App units: the geometry type for layout.
//!
//! `i32` at 1/60 px (build-spec §4.2). Every operator saturates, so there is no
//! input that panics or wraps; a hostile page that asks for a 2-billion-pixel
//! margin gets a clamped box, and the clamp is testable.

use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

/// App units per CSS pixel.
pub const AU_PER_PX: i32 = 60;

/// A length in app units.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Au(pub i32);

impl Au {
    pub const ZERO: Au = Au(0);
    pub const MAX: Au = Au(i32::MAX);
    pub const MIN: Au = Au(i32::MIN);

    /// Whole CSS pixels, saturating.
    pub const fn from_px(px: i32) -> Au {
        Au(px.saturating_mul(AU_PER_PX))
    }

    /// Fractional CSS pixels, rounded to the nearest app unit. NaN is zero and
    /// infinities saturate.
    pub fn from_f32_px(px: f32) -> Au {
        if px.is_nan() {
            return Au::ZERO;
        }
        let scaled = (px * AU_PER_PX as f32).round();
        if scaled >= i32::MAX as f32 {
            Au::MAX
        } else if scaled <= Au::MIN.0 as f32 {
            Au::MIN
        } else {
            Au(scaled as i32)
        }
    }

    pub fn to_f32_px(self) -> f32 {
        self.0 as f32 / AU_PER_PX as f32
    }

    pub const fn saturating_add(self, other: Au) -> Au {
        Au(self.0.saturating_add(other.0))
    }

    pub const fn saturating_sub(self, other: Au) -> Au {
        Au(self.0.saturating_sub(other.0))
    }

    /// `self * numerator / denominator`, computed in 64 bits so the product
    /// cannot overflow, then clamped. A zero denominator gives zero.
    pub fn scale(self, numerator: i32, denominator: i32) -> Au {
        if denominator == 0 {
            return Au::ZERO;
        }
        let v = i64::from(self.0) * i64::from(numerator) / i64::from(denominator);
        Au(v.clamp(i64::from(Au::MIN.0), i64::from(i32::MAX)) as i32)
    }

    /// A fraction of this length, e.g. a percentage of a containing block.
    /// Rounded to the nearest app unit; non-finite fractions give zero.
    pub fn mul_f32(self, fraction: f32) -> Au {
        if !fraction.is_finite() {
            return Au::ZERO;
        }
        Au::from_f32_px(self.to_f32_px() * fraction)
    }

    /// Clamp negative values to zero.
    pub fn non_negative(self) -> Au {
        Au(self.0.max(0))
    }
}

impl Add for Au {
    type Output = Au;
    fn add(self, rhs: Au) -> Au {
        self.saturating_add(rhs)
    }
}
impl Sub for Au {
    type Output = Au;
    fn sub(self, rhs: Au) -> Au {
        self.saturating_sub(rhs)
    }
}
impl AddAssign for Au {
    fn add_assign(&mut self, rhs: Au) {
        *self = *self + rhs;
    }
}
impl SubAssign for Au {
    fn sub_assign(&mut self, rhs: Au) {
        *self = *self - rhs;
    }
}
impl Neg for Au {
    type Output = Au;
    fn neg(self) -> Au {
        Au(self.0.saturating_neg())
    }
}
impl Mul<i32> for Au {
    type Output = Au;
    fn mul(self, rhs: i32) -> Au {
        Au(self.0.saturating_mul(rhs))
    }
}
impl Div<i32> for Au {
    type Output = Au;
    fn div(self, rhs: i32) -> Au {
        if rhs == 0 {
            Au::ZERO
        } else {
            Au(self.0.saturating_div(rhs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_px_is_sixty_units() {
        assert_eq!(Au::from_px(1), Au(60));
        assert_eq!(Au::from_px(-3), Au(-180));
    }

    #[test]
    fn addition_saturates_instead_of_wrapping() {
        assert_eq!(Au::MAX + Au(1), Au::MAX);
        assert_eq!(Au::MIN - Au(1), Au::MIN);
        let mut a = Au::MAX;
        a += Au::MAX;
        assert_eq!(a, Au::MAX);
    }

    #[test]
    fn from_px_saturates() {
        assert_eq!(Au::from_px(i32::MAX), Au::MAX);
        assert_eq!(Au::from_px(i32::MIN), Au::MIN);
    }

    #[test]
    fn negation_of_the_minimum_does_not_overflow() {
        assert_eq!(-Au(i32::MIN), Au::MAX);
        assert_eq!(-Au::MIN, Au::MAX);
    }

    #[test]
    fn multiplication_and_division_are_defined_everywhere() {
        assert_eq!(Au::MAX * 2, Au::MAX);
        assert_eq!(Au(10) / 0, Au::ZERO);
        assert_eq!(Au(10) / 3, Au(3));
        assert_eq!(Au::MIN / -1, Au::MAX);
    }

    #[test]
    fn scale_cannot_overflow_in_the_intermediate_product() {
        assert_eq!(Au(i32::MAX).scale(2, 2), Au(i32::MAX));
        assert_eq!(Au(1000).scale(1, 0), Au::ZERO);
        assert_eq!(Au(1000).scale(50, 100), Au(500));
    }

    #[test]
    fn fractional_pixels_round_to_nearest_unit() {
        assert_eq!(Au::from_f32_px(1.0), Au(60));
        assert_eq!(Au::from_f32_px(0.5), Au(30));
        assert_eq!(Au::from_f32_px(1.0 / 120.0), Au(1)); // half a unit rounds away from zero
        assert_eq!(Au::from_f32_px(f32::NAN), Au::ZERO);
        assert_eq!(Au::from_f32_px(f32::INFINITY), Au::MAX);
        assert_eq!(Au::from_f32_px(f32::NEG_INFINITY), Au::MIN);
    }

    #[test]
    fn percentages_of_a_length() {
        assert_eq!(Au::from_px(200).mul_f32(0.5), Au::from_px(100));
        assert_eq!(Au::from_px(200).mul_f32(f32::NAN), Au::ZERO);
    }

    #[test]
    fn round_trip_through_f32_is_exact_for_whole_pixels() {
        for px in [-1000, -1, 0, 1, 7, 800, 10_000] {
            assert_eq!(
                Au::from_f32_px(Au::from_px(px).to_f32_px()),
                Au::from_px(px)
            );
        }
    }
}
