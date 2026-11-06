//! Points and affine matrices in Q16.16.

use super::fixed::{Q, deg_to_rad, sincos};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct P {
    pub x: Q,
    pub y: Q,
}

pub fn p(x: Q, y: Q) -> P {
    P { x, y }
}

impl P {
    pub fn add(self, o: P) -> P {
        p(self.x + o.x, self.y + o.y)
    }
    pub fn sub(self, o: P) -> P {
        p(self.x - o.x, self.y - o.y)
    }
    pub fn scale(self, k: Q) -> P {
        p(self.x * k, self.y * k)
    }
    pub fn len(self) -> Q {
        Q::hypot(self.x, self.y)
    }
    pub fn cross(self, o: P) -> Q {
        (self.x * o.y) - (self.y * o.x)
    }
    pub fn dot(self, o: P) -> Q {
        (self.x * o.x) + (self.y * o.y)
    }
    /// The unit vector, or zero for a zero vector.
    pub fn unit(self) -> P {
        let l = self.len();
        if l.0 == 0 {
            return P::default();
        }
        p(self.x / l, self.y / l)
    }
    /// The point `num / den` of the way to `o` (exact to the last bit).
    pub fn lerp(self, o: P, num: Q, den: Q) -> P {
        if den.0 == 0 {
            return self;
        }
        let f = |a: Q, b: Q| {
            Q::sat(
                i128::from(a.0) + (i128::from((b - a).0) * i128::from(num.0)) / i128::from(den.0),
            )
        };
        p(f(self.x, o.x), f(self.y, o.y))
    }
}

/// `x' = a x + c y + e`, `y' = b x + d y + f`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mat {
    pub a: Q,
    pub b: Q,
    pub c: Q,
    pub d: Q,
    pub e: Q,
    pub f: Q,
}

impl Mat {
    pub const IDENTITY: Mat = Mat {
        a: Q::ONE,
        b: Q::ZERO,
        c: Q::ZERO,
        d: Q::ONE,
        e: Q::ZERO,
        f: Q::ZERO,
    };

    pub fn translate(x: Q, y: Q) -> Mat {
        Mat {
            e: x,
            f: y,
            ..Mat::IDENTITY
        }
    }
    pub fn scaling(x: Q, y: Q) -> Mat {
        Mat {
            a: x,
            d: y,
            ..Mat::IDENTITY
        }
    }
    pub fn rotate(deg: Q) -> Mat {
        let (s, c) = sincos(deg_to_rad(deg));
        Mat {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: Q::ZERO,
            f: Q::ZERO,
        }
    }
    pub fn skew_x(deg: Q) -> Mat {
        let (s, c) = sincos(deg_to_rad(deg));
        Mat {
            c: s / c,
            ..Mat::IDENTITY
        }
    }
    pub fn skew_y(deg: Q) -> Mat {
        let (s, c) = sincos(deg_to_rad(deg));
        Mat {
            b: s / c,
            ..Mat::IDENTITY
        }
    }

    /// `self` applied after `o` (so `self.then(o)` maps a point through `o`
    /// first): the matrix product `self * o`.
    pub fn mul(&self, o: &Mat) -> Mat {
        Mat {
            a: (self.a * o.a) + (self.c * o.b),
            b: (self.b * o.a) + (self.d * o.b),
            c: (self.a * o.c) + (self.c * o.d),
            d: (self.b * o.c) + (self.d * o.d),
            e: (self.a * o.e) + (self.c * o.f) + self.e,
            f: (self.b * o.e) + (self.d * o.f) + self.f,
        }
    }

    pub fn apply(&self, pt: P) -> P {
        p(
            (self.a * pt.x) + (self.c * pt.y) + self.e,
            (self.b * pt.x) + (self.d * pt.y) + self.f,
        )
    }

    pub fn det(&self) -> Q {
        (self.a * self.d) - (self.b * self.c)
    }

    pub fn invert(&self) -> Option<Mat> {
        let det = self.det();
        // Anything below 2^-12 of a unit area is singular for our purposes.
        if det.0.abs() < 16 {
            return None;
        }
        let a = self.d / det;
        let b = -(self.b / det);
        let c = -(self.c / det);
        let d = self.a / det;
        Some(Mat {
            a,
            b,
            c,
            d,
            e: -((a * self.e) + (c * self.f)),
            f: -((b * self.e) + (d * self.f)),
        })
    }

    /// An upper bound on how much the matrix stretches a vector.
    pub fn max_scale(&self) -> Q {
        let x = self.a.abs() + self.c.abs();
        let y = self.b.abs() + self.d.abs();
        if x > y { x } else { y }
    }
}
