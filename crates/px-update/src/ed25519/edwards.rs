//! The twisted Edwards curve -x^2 + y^2 = 1 + d x^2 y^2 over GF(2^255 - 19),
//! in extended coordinates (X : Y : Z : T) with x = X/Z, y = Y/Z, xy = T/Z.
//!
//! One addition formula (add-2008-hwcd-3, unified and complete for this
//! curve) serves for addition and doubling. Variable time, public data only.

use super::field::Fe;
use std::sync::OnceLock;

struct Consts {
    d: Fe,
    d2: Fe,
    sqrt_m1: Fe,
}

fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| {
        // d = -121665 / 121666
        let d = Fe::small(121665).neg().mul(&Fe::small(121666).invert());
        Consts {
            d,
            d2: d.add(&d),
            sqrt_m1: Fe::sqrt_m1(),
        }
    })
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

/// RFC 8032 encoding of the base point B (y = 4/5, x even).
const BASEPOINT: [u8; 32] = {
    let mut b = [0x66u8; 32];
    b[0] = 0x58;
    b
};

impl Point {
    pub(super) const IDENTITY: Point = Point {
        x: Fe::ZERO,
        y: Fe::ONE,
        z: Fe::ONE,
        t: Fe::ZERO,
    };

    pub(super) fn basepoint() -> Option<Point> {
        Point::decompress(&BASEPOINT)
    }

    /// Strict RFC 8032 §5.1.3 decoding: rejects y >= p, rejects points not on
    /// the curve, and rejects the sign bit set on x = 0.
    pub(super) fn decompress(enc: &[u8; 32]) -> Option<Point> {
        let c = consts();
        let sign = enc.last().is_some_and(|b| b >> 7 == 1);
        let mut yb = *enc;
        if let Some(last) = yb.last_mut() {
            *last &= 0x7f;
        }
        let y = Fe::from_bytes(&yb);
        if y.to_bytes() != yb {
            return None;
        }
        let y2 = y.square();
        let u = y2.sub(&Fe::ONE);
        let v = c.d.mul(&y2).add(&Fe::ONE);
        let v3 = v.square().mul(&v);
        let v7 = v3.square().mul(&v);
        let mut x = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
        let vxx = v.mul(&x.square());
        if !vxx.eq(&u) {
            if vxx.eq(&u.neg()) {
                x = x.mul(&c.sqrt_m1);
            } else {
                return None;
            }
        }
        if x.is_zero() && sign {
            return None;
        }
        if x.is_negative() != sign {
            x = x.neg();
        }
        Some(Point {
            x,
            y,
            z: Fe::ONE,
            t: x.mul(&y),
        })
    }

    /// Encoding is only needed by the test signer; verification compares
    /// points projectively and never re-encodes.
    #[cfg(test)]
    pub(super) fn compress(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(&zi);
        let y = self.y.mul(&zi);
        let mut out = y.to_bytes();
        if let (true, Some(last)) = (x.is_negative(), out.last_mut()) {
            *last |= 0x80;
        }
        out
    }

    pub(super) fn add(&self, o: &Point) -> Point {
        let c = consts();
        let a = self.y.sub(&self.x).mul(&o.y.sub(&o.x));
        let b = self.y.add(&self.x).mul(&o.y.add(&o.x));
        let cc = self.t.mul(&c.d2).mul(&o.t);
        let zz = self.z.mul(&o.z);
        let dd = zz.add(&zz);
        let e = b.sub(&a);
        let f = dd.sub(&cc);
        let g = dd.add(&cc);
        let h = b.add(&a);
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            z: f.mul(&g),
            t: e.mul(&h),
        }
    }

    pub(super) fn double(&self) -> Point {
        self.add(self)
    }

    /// `[k] self` for a 256-bit little-endian scalar (not required reduced).
    pub(super) fn mul_scalar(&self, k: &[u8; 32]) -> Point {
        let mut acc = Point::IDENTITY;
        for byte in k.iter().rev() {
            for i in (0..8).rev() {
                acc = acc.double();
                if (byte >> i) & 1 == 1 {
                    acc = acc.add(self);
                }
            }
        }
        acc
    }

    pub(super) fn is_identity(&self) -> bool {
        self.x.is_zero() && self.y.eq(&self.z)
    }

    /// Projective equality (cross-multiplied; no inversion).
    pub(super) fn equals(&self, o: &Point) -> bool {
        self.x.mul(&o.z).eq(&o.x.mul(&self.z)) && self.y.mul(&o.z).eq(&o.y.mul(&self.z))
    }

    /// True when the point's order divides 8 (it lies in the torsion
    /// subgroup of order 8, which includes the identity).
    pub(super) fn is_small_order(&self) -> bool {
        self.double().double().double().is_identity()
    }

    /// True when `[L] P` is the identity, i.e. P has no torsion component.
    pub(super) fn is_torsion_free(&self) -> bool {
        let l = super::scalar::from_limbs(&super::scalar::L);
        self.mul_scalar(&l).is_identity()
    }
}
