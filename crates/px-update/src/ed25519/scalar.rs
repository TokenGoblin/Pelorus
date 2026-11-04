//! Scalars modulo the group order
//! L = 2^252 + 27742317777372353535851937790883648493.
//!
//! Only what verification needs: the canonical check `S < L` and reduction of
//! a 512-bit hash modulo L. Reduction is bit-serial shift-and-subtract:
//! slow (about 512 iterations) and obviously correct, which is the right
//! trade for a path that runs once per update check.

/// L as four little-endian 64-bit limbs.
pub(super) const L: [u64; 4] = [
    0x5812_631a_5cf5_d3ed,
    0x14de_f9de_a2f7_9cd6,
    0,
    0x1000_0000_0000_0000,
];

pub(super) fn to_limbs(b: &[u8; 32]) -> [u64; 4] {
    let mut w = [0u64; 4];
    for (slot, chunk) in w.iter_mut().zip(b.as_chunks::<8>().0) {
        *slot = u64::from_le_bytes(*chunk);
    }
    w
}

pub(super) fn from_limbs(w: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (chunk, v) in out.as_chunks_mut::<8>().0.iter_mut().zip(w) {
        *chunk = v.to_le_bytes();
    }
    out
}

/// `a >= b` as 256-bit integers.
fn gte(a: &[u64; 4], b: &[u64; 4]) -> bool {
    for (x, y) in a.iter().rev().zip(b.iter().rev()) {
        if x != y {
            return x > y;
        }
    }
    true
}

/// `a - b`, caller guarantees `a >= b`.
fn sub(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut out = [0u64; 4];
    let mut borrow = false;
    for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b.iter())) {
        let (d1, b1) = x.overflowing_sub(*y);
        let (d2, b2) = d1.overflowing_sub(u64::from(borrow));
        *o = d2;
        borrow = b1 || b2;
    }
    out
}

/// `(a + b) mod L` for a, b already below L. Used only by the test signer.
#[cfg(test)]
pub(super) fn add_mod(a: &[u64; 4], b: &[u64; 4]) -> [u64; 4] {
    let mut out = [0u64; 4];
    let mut carry = false;
    for (o, (x, y)) in out.iter_mut().zip(a.iter().zip(b.iter())) {
        let (s1, c1) = x.overflowing_add(*y);
        let (s2, c2) = s1.overflowing_add(u64::from(carry));
        *o = s2;
        carry = c1 || c2;
    }
    if gte(&out, &L) {
        out = sub(&out, &L);
    }
    out
}

/// True when the 32 little-endian bytes encode an integer strictly below L.
pub(super) fn is_canonical(s: &[u8; 32]) -> bool {
    !gte(&to_limbs(s), &L)
}

/// Reduce a little-endian integer of up to 64 bytes modulo L. Longer input is
/// not reduced (returns zero); no caller passes more.
pub(super) fn reduce(bytes: &[u8]) -> [u8; 32] {
    if bytes.len() > 64 {
        return [0u8; 32];
    }
    let mut acc = [0u64; 4];
    for byte in bytes.iter().rev() {
        for i in (0..8).rev() {
            // acc < L < 2^253, so doubling cannot lose a bit.
            let mut carry = u64::from((byte >> i) & 1);
            for limb in acc.iter_mut() {
                let next = *limb >> 63;
                *limb = (*limb << 1) | carry;
                carry = next;
            }
            if gte(&acc, &L) {
                acc = sub(&acc, &L);
            }
        }
    }
    from_limbs(&acc)
}
