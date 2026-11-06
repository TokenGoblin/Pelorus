//! Q16.16 fixed-point numbers held in an `i64`, saturating at 2^24 units.
//!
//! Every operation clamps instead of overflowing (release builds trap on
//! overflow, ADR 042), so hostile coordinates cannot panic. Trigonometry is a
//! CORDIC in Q30 radians: shifts, adds and a constant table, so the same input
//! gives the same output on every platform.

use std::ops::{Add, Div, Mul, Neg, Sub};

const LIM: i64 = 1 << 40;

/// A Q16.16 number.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Q(pub i64);

impl Q {
    pub const ZERO: Q = Q(0);
    pub const ONE: Q = Q(1 << 16);

    pub fn sat(v: i128) -> Q {
        Q(v.clamp(-i128::from(LIM), i128::from(LIM)) as i64)
    }
    pub fn int(n: i64) -> Q {
        Q::sat(i128::from(n) << 16)
    }
    /// `num / den` as a Q, 0 when `den` is 0.
    pub fn ratio(num: i64, den: i64) -> Q {
        if den == 0 {
            return Q::ZERO;
        }
        Q::sat((i128::from(num) << 16) / i128::from(den))
    }
    pub fn abs(self) -> Q {
        Q(self.0.abs())
    }
    pub fn is_neg(self) -> bool {
        self.0 < 0
    }
    pub fn half(self) -> Q {
        Q(self.0 >> 1)
    }
    /// Nearest integer, halves up.
    pub fn round(self) -> i64 {
        (self.0 + (1 << 15)) >> 16
    }
    pub fn floor(self) -> i64 {
        self.0 >> 16
    }
    pub fn sqrt(self) -> Q {
        if self.0 <= 0 {
            return Q::ZERO;
        }
        Q::sat(((self.0 as u128) << 16).isqrt() as i128)
    }
    /// `sqrt(a*a + b*b)` without intermediate saturation.
    pub fn hypot(a: Q, b: Q) -> Q {
        let s = (i128::from(a.0) * i128::from(a.0)) + (i128::from(b.0) * i128::from(b.0));
        Q::sat((s as u128).isqrt() as i128)
    }
    /// Multiply by an integer ratio `n / d`.
    pub fn scale(self, n: i64, d: i64) -> Q {
        if d == 0 {
            return Q::ZERO;
        }
        Q::sat((i128::from(self.0) * i128::from(n)) / i128::from(d))
    }
}

impl Add for Q {
    type Output = Q;
    fn add(self, o: Q) -> Q {
        Q::sat(i128::from(self.0) + i128::from(o.0))
    }
}
impl Sub for Q {
    type Output = Q;
    fn sub(self, o: Q) -> Q {
        Q::sat(i128::from(self.0) - i128::from(o.0))
    }
}
impl Mul for Q {
    type Output = Q;
    fn mul(self, o: Q) -> Q {
        Q::sat((i128::from(self.0) * i128::from(o.0)) >> 16)
    }
}
impl Div for Q {
    type Output = Q;
    fn div(self, o: Q) -> Q {
        if o.0 == 0 {
            return Q::ZERO;
        }
        Q::sat((i128::from(self.0) << 16) / i128::from(o.0))
    }
}
impl Neg for Q {
    type Output = Q;
    fn neg(self) -> Q {
        Q(-self.0)
    }
}

const ATAN: [i64; 31] = [
    843314857, 497837829, 263043837, 133525159, 67021687, 33543516, 16775851, 8388437, 4194283,
    2097149, 1048576, 524288, 262144, 131072, 65536, 32768, 16384, 8192, 4096, 2048, 1024, 512,
    256, 128, 64, 32, 16, 8, 4, 2, 1,
];
/// CORDIC gain compensation, 0.607252935 in Q30.
const GAIN: i64 = 652_032_874;
/// pi, pi/2 and 2 pi in Q30 radians.
pub const PI: i64 = 3_373_259_426;
pub const HALF_PI: i64 = 1_686_629_713;
pub const TWO_PI: i64 = 6_746_518_852;
/// One degree in Q30 radians.
const DEG: i64 = 18_740_330;

/// Degrees (a Q) to Q30 radians, reduced to under one turn.
pub fn deg_to_rad(d: Q) -> i64 {
    let v = (i128::from(d.0) * i128::from(DEG)) >> 16;
    (v % i128::from(TWO_PI)) as i64
}

/// `(sin, cos)` of a Q30 radian angle, as Q16.16.
pub fn sincos(angle: i64) -> (Q, Q) {
    let mut a = angle % TWO_PI;
    if a > PI {
        a -= TWO_PI;
    } else if a < -PI {
        a += TWO_PI;
    }
    let mut flip = false;
    if a > HALF_PI {
        a = PI - a;
        flip = true;
    } else if a < -HALF_PI {
        a = -PI - a;
        flip = true;
    }
    let (mut x, mut y, mut z) = (GAIN, 0i64, a);
    for (i, t) in ATAN.iter().enumerate() {
        let (dx, dy) = (y >> i, x >> i);
        if z >= 0 {
            x -= dx;
            y += dy;
            z -= t;
        } else {
            x += dx;
            y -= dy;
            z += t;
        }
    }
    if flip {
        x = -x;
    }
    (Q(y >> 14), Q(x >> 14))
}

/// `atan2(y, x)` in Q30 radians, in `[-pi, pi]`.
pub fn atan2(y: Q, x: Q) -> i64 {
    let (mut x, mut y) = (x.0, y.0);
    if x == 0 && y == 0 {
        return 0;
    }
    let mut off = 0;
    if x < 0 {
        off = if y >= 0 { PI } else { -PI };
        x = -x;
        y = -y;
    }
    // Scale up so the shifts below keep their precision.
    let bits = 64 - x.max(y.abs()).leading_zeros();
    if bits < 41 {
        x <<= 41 - bits;
        y <<= 41 - bits;
    }
    let mut z = 0i64;
    for (i, t) in ATAN.iter().enumerate() {
        let (dx, dy) = (y >> i, x >> i);
        if y > 0 {
            x += dx;
            y -= dy;
            z += t;
        } else {
            x -= dx;
            y += dy;
            z -= t;
        }
    }
    z + off
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_saturates() {
        let big = Q(1 << 40);
        assert_eq!((big * big).0, 1 << 40);
        assert_eq!((big + big).0, 1 << 40);
        assert_eq!((-big - big).0, -(1 << 40));
        assert_eq!(Q::ONE / Q::ZERO, Q::ZERO);
        assert_eq!(Q::int(3) * Q::int(4), Q::int(12));
        assert_eq!(Q::int(9).sqrt(), Q::int(3));
        assert_eq!(Q::hypot(Q::int(3), Q::int(4)), Q::int(5));
    }

    #[test]
    fn trig_is_close() {
        let near = |a: Q, b: i64| (a.0 - b).abs() <= 8;
        let (s, c) = sincos(0);
        assert!(near(s, 0) && near(c, 65536));
        let (s, c) = sincos(HALF_PI);
        assert!(near(s, 65536) && near(c, 0));
        let (s, c) = sincos(PI);
        assert!(near(s, 0) && near(c, -65536));
        let (s, c) = sincos(-HALF_PI);
        assert!(near(s, -65536) && near(c, 0));
        let (s, _) = sincos(deg_to_rad(Q::int(30)));
        assert!(near(s, 32768));
        let a = atan2(Q::int(1), Q::int(1));
        assert!((a - PI / 4).abs() < 64);
        let a = atan2(Q::int(0), Q::int(-1));
        assert!((a - PI).abs() < 64);
        assert_eq!(atan2(Q::ZERO, Q::ZERO), 0);
    }
}
