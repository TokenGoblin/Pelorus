//! Ed25519 signature VERIFICATION (RFC 8032 §5.1.7), hand-written, std only.
//!
//! Verification only. There is no signing code in a shipping build: signing is
//! the release tool's job and its key never touches this crate. A signer
//! exists under `#[cfg(test)]` so the tests can do round trips.
//!
//! # Strictness (the cofactored / ZIP215 / strict question)
//!
//! Ed25519 implementations disagree about edge cases: non-canonical
//! encodings, small-order points, and whether the check is cofactored
//! (`[8][S]B = [8]R + [8][k]A`, as ZIP215 and RFC 8032's permitted variant) or
//! cofactorless (`[S]B = R + [k]A`). Disagreement between a signer's tooling
//! and a verifier matters for consensus systems, where every node must accept
//! exactly the same set. It does not matter much here: there is one honest
//! signer, and the only goal is that nothing but that signer's signatures is
//! ever accepted. For that goal the safest rule is the narrowest one, so this
//! verifier is STRICT, accepting a signature only when ALL hold:
//!
//! 1. the input lengths are exactly 32 (key) and 64 (signature) bytes;
//! 2. `S < L` (no malleability by adding the group order);
//! 3. the public key `A` is a canonical encoding (`y < p`, on the curve,
//!    no sign bit on `x = 0`), is not of small order, and has no torsion
//!    component (`[L]A` is the identity);
//! 4. `R` is a canonical encoding and not of small order;
//! 5. the cofactorless equation `[S]B = R + [k]A` holds exactly, where
//!    `k = SHA-512(R || A || M) mod L` and `R`, `A` are the received bytes.
//!
//! Rules 3 and 4 are stricter than RFC 8032, which allows a verifier to accept
//! them; no honestly generated key or signature trips them. Signatures a
//! cofactored verifier would accept for torsion-laden inputs are rejected
//! here: that is the point. The differential test against `ring` therefore
//! covers honest and randomly corrupted signatures, where the two must agree
//! exactly; hand-built edge cases are covered by explicit tests instead.
//!
//! Timing: variable time, which is fine because every input is public.

mod edwards;
mod field;
mod scalar;

#[cfg(test)]
mod tests;

use crate::sha2::sha512;
use edwards::Point;

/// Public key length in bytes.
pub const PUBLIC_KEY_LEN: usize = 32;
/// Signature length in bytes.
pub const SIGNATURE_LEN: usize = 64;

/// Why a signature was refused. Callers treat every variant as "reject"; the
/// distinction exists for tests and for logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerifyError {
    BadLength,
    NonCanonicalS,
    BadPublicKey,
    SmallOrderPublicKey,
    BadR,
    SmallOrderR,
    Mismatch,
}

/// Verify a detached Ed25519 signature. `Ok(())` only when the strict rules in
/// the module documentation all hold.
///
/// `message` is hashed in memory; the caller bounds its size.
pub fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), VerifyError> {
    let a_bytes: &[u8; 32] = public_key.try_into().map_err(|_| VerifyError::BadLength)?;
    let sig: &[u8; 64] = signature.try_into().map_err(|_| VerifyError::BadLength)?;
    let (r_bytes, s_bytes) = sig.split_at(32);
    let r_bytes: &[u8; 32] = r_bytes.try_into().map_err(|_| VerifyError::BadLength)?;
    let s_bytes: &[u8; 32] = s_bytes.try_into().map_err(|_| VerifyError::BadLength)?;

    if !scalar::is_canonical(s_bytes) {
        return Err(VerifyError::NonCanonicalS);
    }
    let a = Point::decompress(a_bytes).ok_or(VerifyError::BadPublicKey)?;
    if a.is_small_order() {
        return Err(VerifyError::SmallOrderPublicKey);
    }
    if !a.is_torsion_free() {
        return Err(VerifyError::BadPublicKey);
    }
    let r = Point::decompress(r_bytes).ok_or(VerifyError::BadR)?;
    if r.is_small_order() {
        return Err(VerifyError::SmallOrderR);
    }

    let mut hashed = Vec::with_capacity(64usize.saturating_add(message.len()));
    hashed.extend_from_slice(r_bytes);
    hashed.extend_from_slice(a_bytes);
    hashed.extend_from_slice(message);
    let k = scalar::reduce(&sha512(&hashed));

    let base = Point::basepoint().ok_or(VerifyError::Mismatch)?;
    let lhs = base.mul_scalar(s_bytes);
    let rhs = r.add(&a.mul_scalar(&k));
    if lhs.equals(&rhs) {
        Ok(())
    } else {
        Err(VerifyError::Mismatch)
    }
}
