//! Arithmetic in GF(2^255 - 19), radix 2^51, five `u64` limbs.
//!
//! Variable time. Every input to signature *verification* is public (public
//! key, message, signature), so there is no secret to leak through timing.
//! This module must not be reused for signing or key generation.
//!
//! No indexing: limbs are fixed-size arrays addressed by constant index or
//! walked with iterators.

const MASK: u64 = (1 << 51) - 1;

/// A field element. Limbs are kept "weakly reduced": below 2^51 plus a few
/// bits, which keeps every product in `mul` far below 2^128.
#[derive(Clone, Copy, Debug)]
pub(super) struct Fe(pub(super) [u64; 5]);

/// Exponent p - 2 (little endian): Fermat inversion.
const P_MINUS_2: [u8; 32] = exp_bytes(0xeb, 0x7f);
/// Exponent (p - 5) / 8 = 2^252 - 3: the square-root candidate exponent.
const P_MINUS_5_OVER_8: [u8; 32] = exp_bytes(0xfd, 0x0f);
/// Exponent (p - 1) / 4 = 2^253 - 5: 2^this is a square root of -1.
const P_MINUS_1_OVER_4: [u8; 32] = exp_bytes(0xfb, 0x1f);

/// `low`, then 30 bytes of 0xff, then `high`.
const fn exp_bytes(low: u8, high: u8) -> [u8; 32] {
    let mut out = [0xffu8; 32];
    out[0] = low;
    out[31] = high;
    out
}

impl Fe {
    pub(super) const ZERO: Fe = Fe([0; 5]);
    pub(super) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    /// A small constant, below 2^51.
    pub(super) const fn small(v: u64) -> Fe {
        Fe([v, 0, 0, 0, 0])
    }

    /// Decode 255 bits little endian. Bit 255 is ignored here; callers that
    /// need the canonical-encoding rule compare against `to_bytes`.
    pub(super) fn from_bytes(b: &[u8; 32]) -> Fe {
        let mut w = [0u64; 4];
        for (slot, chunk) in w.iter_mut().zip(b.as_chunks::<8>().0) {
            *slot = u64::from_le_bytes(*chunk);
        }
        let [w0, w1, w2, w3] = w;
        Fe([
            w0 & MASK,
            ((w0 >> 51) | (w1 << 13)) & MASK,
            ((w1 >> 38) | (w2 << 26)) & MASK,
            ((w2 >> 25) | (w3 << 39)) & MASK,
            (w3 >> 12) & MASK,
        ])
    }

    fn carry(self) -> Fe {
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.0;
        h1 += h0 >> 51;
        h0 &= MASK;
        h2 += h1 >> 51;
        h1 &= MASK;
        h3 += h2 >> 51;
        h2 &= MASK;
        h4 += h3 >> 51;
        h3 &= MASK;
        h0 += (h4 >> 51) * 19;
        h4 &= MASK;
        h1 += h0 >> 51;
        h0 &= MASK;
        Fe([h0, h1, h2, h3, h4])
    }

    pub(super) fn add(&self, o: &Fe) -> Fe {
        let mut r = [0u64; 5];
        for (d, (a, b)) in r.iter_mut().zip(self.0.iter().zip(o.0.iter())) {
            *d = a + b;
        }
        Fe(r).carry()
    }

    /// `self - o`, computed as `self + 2p - o` so no limb underflows.
    pub(super) fn sub(&self, o: &Fe) -> Fe {
        const TWO_P: [u64; 5] = [
            0xf_ffff_ffff_ffda,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
            0xf_ffff_ffff_fffe,
        ];
        let mut r = [0u64; 5];
        for (d, ((a, b), t)) in r
            .iter_mut()
            .zip(self.0.iter().zip(o.0.iter()).zip(TWO_P.iter()))
        {
            *d = a + t - b;
        }
        Fe(r).carry()
    }

    pub(super) fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(super) fn mul(&self, o: &Fe) -> Fe {
        let [a0, a1, a2, a3, a4] = self.0.map(u128::from);
        let [b0, b1, b2, b3, b4] = o.0.map(u128::from);
        let (b1_19, b2_19, b3_19, b4_19) = (b1 * 19, b2 * 19, b3 * 19, b4 * 19);
        let c0 = a0 * b0 + a1 * b4_19 + a2 * b3_19 + a3 * b2_19 + a4 * b1_19;
        let mut c1 = a0 * b1 + a1 * b0 + a2 * b4_19 + a3 * b3_19 + a4 * b2_19;
        let mut c2 = a0 * b2 + a1 * b1 + a2 * b0 + a3 * b4_19 + a4 * b3_19;
        let mut c3 = a0 * b3 + a1 * b2 + a2 * b1 + a3 * b0 + a4 * b4_19;
        let mut c4 = a0 * b4 + a1 * b3 + a2 * b2 + a3 * b1 + a4 * b0;
        let m = u128::from(MASK);
        c1 += c0 >> 51;
        c2 += c1 >> 51;
        c3 += c2 >> 51;
        c4 += c3 >> 51;
        // c4 >> 51 is below 2^60 for weakly reduced inputs, so times 19 it
        // still fits a u64 and `narrow` never actually falls back.
        let top = c4 >> 51;
        let narrow = |v: u128| u64::try_from(v).unwrap_or(0);
        Fe([
            narrow((c0 & m) + top * 19),
            narrow(c1 & m),
            narrow(c2 & m),
            narrow(c3 & m),
            narrow(c4 & m),
        ])
        .carry()
    }

    pub(super) fn square(&self) -> Fe {
        self.mul(self)
    }

    /// `self ^ exp` for a 256-bit little-endian exponent. Public data only.
    pub(super) fn pow(&self, exp: &[u8; 32]) -> Fe {
        let mut acc = Fe::ONE;
        for byte in exp.iter().rev() {
            for i in (0..8).rev() {
                acc = acc.square();
                if (byte >> i) & 1 == 1 {
                    acc = acc.mul(self);
                }
            }
        }
        acc
    }

    pub(super) fn invert(&self) -> Fe {
        self.pow(&P_MINUS_2)
    }

    /// `self ^ ((p - 5) / 8)`.
    pub(super) fn pow_p58(&self) -> Fe {
        self.pow(&P_MINUS_5_OVER_8)
    }

    /// A square root of -1: `2 ^ ((p - 1) / 4)`.
    pub(super) fn sqrt_m1() -> Fe {
        Fe::small(2).pow(&P_MINUS_1_OVER_4)
    }

    /// The unique canonical 32-byte encoding (value fully reduced below p).
    pub(super) fn to_bytes(self) -> [u8; 32] {
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.carry().0;
        // q = 1 exactly when h >= p, found by propagating the carry of h + 19.
        let mut q = (h0 + 19) >> 51;
        q = (h1 + q) >> 51;
        q = (h2 + q) >> 51;
        q = (h3 + q) >> 51;
        q = (h4 + q) >> 51;
        h0 += 19 * q;
        h1 += h0 >> 51;
        h0 &= MASK;
        h2 += h1 >> 51;
        h1 &= MASK;
        h3 += h2 >> 51;
        h2 &= MASK;
        h4 += h3 >> 51;
        h3 &= MASK;
        h4 &= MASK;
        let words = [
            h0 | (h1 << 51),
            (h1 >> 13) | (h2 << 38),
            (h2 >> 26) | (h3 << 25),
            (h3 >> 39) | (h4 << 12),
        ];
        let mut out = [0u8; 32];
        for (chunk, w) in out.as_chunks_mut::<8>().0.iter_mut().zip(words) {
            *chunk = w.to_le_bytes();
        }
        out
    }

    pub(super) fn eq(&self, o: &Fe) -> bool {
        self.to_bytes() == o.to_bytes()
    }

    pub(super) fn is_zero(&self) -> bool {
        self.to_bytes() == [0u8; 32]
    }

    /// The "sign" of RFC 8032: the low bit of the canonical encoding.
    pub(super) fn is_negative(&self) -> bool {
        self.to_bytes().first().is_some_and(|b| b & 1 == 1)
    }
}
