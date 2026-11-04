//! Trusted keys, detached signature files, and key rotation.
//!
//! # Model
//!
//! The client holds a [`Trust`]: an `epoch`, a set of 1..=8 Ed25519 public
//! keys, a `threshold`, and the list of keys revoked so far. The compiled-in
//! root set is epoch 1. A manifest is accepted only if at least `threshold`
//! DISTINCT keys of the current set signed its exact bytes.
//!
//! The signed message is `DOMAIN || manifest bytes`; the domain prefix keeps a
//! manifest signature from being valid in any other protocol that happens to
//! use the same key.
//!
//! # Detached signature file
//!
//! ```text
//! sig <key id: 16 hex> <signature: 128 hex>      (1..=8 lines, key ids ascending)
//! ```
//!
//! The key id is the first 8 bytes of SHA-256 of the public key. It is a
//! lookup hint only; it is never trusted on its own, a signature must still
//! verify under the key that the id selects from the trusted set.
//!
//! # Rotation
//!
//! A rotation manifest (`type rotation`) names the COMPLETE next key set and
//! threshold, and is signed by a threshold of the CURRENT set, never by the
//! new one: only keys already trusted can introduce keys. Rules, each tested:
//!
//! * `key-epoch` must be exactly the current epoch + 1. Skipping an epoch is
//!   rejected (a client that missed one must fetch the missing one first, in
//!   order); repeating or lowering is rejected (rollback: replaying the old
//!   rotation that re-trusts a key since revoked).
//! * Every key in the new set must pass `ed25519::check_public_key`.
//! * A key present in the old set and absent in the new set is revoked. It is
//!   remembered forever and can never be re-introduced by a later rotation.
//! * A release manifest carries the `key-epoch` it was signed under and is
//!   accepted only when it equals the client's current epoch, so a manifest
//!   signed by a since-revoked key cannot be replayed once the client has
//!   rotated, even though its signature is mathematically valid.
//! * A rotation is subject to the same freshness window as a release.
//!
//! What this does not give: recovery when `threshold` keys are lost or
//! compromised at once. That needs a new release signed out of band and
//! installed by hand; see the ADR.

use crate::ed25519;
use crate::hex;
use crate::manifest::{MAX_KEYS, MAX_MANIFEST_BYTES, Manifest, ParseError, Release, Rotation};
use crate::sha2::sha256;

/// Domain-separation prefix for every manifest signature.
pub const SIGN_DOMAIN: &[u8] = b"px-update/manifest/v1\n";
/// How far in the future `issued` may be before the manifest is refused.
pub const MAX_FUTURE_SKEW_SECS: u64 = 86_400;
const MAX_SIG_FILE_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct KeyId(pub [u8; 8]);

pub fn key_id(public_key: &[u8; 32]) -> KeyId {
    let h = sha256(public_key);
    let mut id = [0u8; 8];
    for (d, s) in id.iter_mut().zip(h.iter()) {
        *d = *s;
    }
    KeyId(id)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrustError {
    BadKeySet(&'static str),
    BadSignatureFile(&'static str),
    TooLarge,
    /// Fewer than `need` distinct trusted keys produced a valid signature.
    ThresholdNotMet {
        valid: usize,
        need: usize,
    },
    Parse(ParseError),
    WrongType,
    Expired,
    NotYetValid,
    RotationSkipsEpoch,
    RotationRollback,
    RevokedKeyReintroduced,
    /// A release was signed under a different key epoch than the client trusts.
    EpochMismatch {
        manifest: u32,
        trusted: u32,
    },
}

/// The set of keys the client currently trusts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trust {
    epoch: u32,
    keys: Vec<[u8; 32]>,
    threshold: u8,
    revoked: Vec<[u8; 32]>,
}

fn validate_set(keys: &[[u8; 32]], threshold: u8) -> Result<(), TrustError> {
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return Err(TrustError::BadKeySet("a key set holds 1 to 8 keys"));
    }
    if threshold == 0 || usize::from(threshold) > keys.len() {
        return Err(TrustError::BadKeySet("threshold is outside 1..=keys"));
    }
    for (i, k) in keys.iter().enumerate() {
        if ed25519::check_public_key(k).is_err() {
            return Err(TrustError::BadKeySet("a key is not a valid Ed25519 key"));
        }
        let id = key_id(k);
        if keys.iter().skip(i + 1).any(|o| key_id(o) == id) {
            return Err(TrustError::BadKeySet("duplicate key or key id"));
        }
    }
    Ok(())
}

impl Trust {
    /// The initial root set (epoch 1) or any later epoch restored from state.
    pub fn new(
        epoch: u32,
        keys: Vec<[u8; 32]>,
        threshold: u8,
        revoked: Vec<[u8; 32]>,
    ) -> Result<Trust, TrustError> {
        if epoch == 0 {
            return Err(TrustError::BadKeySet("epochs start at 1"));
        }
        validate_set(&keys, threshold)?;
        if keys.iter().any(|k| revoked.contains(k)) {
            return Err(TrustError::BadKeySet("a trusted key is also revoked"));
        }
        Ok(Trust {
            epoch,
            keys,
            threshold,
            revoked,
        })
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }
    pub fn keys(&self) -> &[[u8; 32]] {
        &self.keys
    }
    pub fn threshold(&self) -> u8 {
        self.threshold
    }
    pub fn revoked(&self) -> &[[u8; 32]] {
        &self.revoked
    }

    /// Require `threshold` distinct trusted keys to have signed `signed`.
    /// Signatures by keys outside the set (including revoked keys) are
    /// ignored, not counted, and not an error.
    pub fn verify(&self, signed: &[u8], sigs: &SignatureSet) -> Result<(), TrustError> {
        if signed.len() > MAX_MANIFEST_BYTES {
            return Err(TrustError::TooLarge);
        }
        let mut message = Vec::with_capacity(SIGN_DOMAIN.len().saturating_add(signed.len()));
        message.extend_from_slice(SIGN_DOMAIN);
        message.extend_from_slice(signed);
        let mut valid = 0usize;
        for (id, sig) in &sigs.0 {
            let Some(key) = self.keys.iter().find(|k| key_id(k) == *id) else {
                continue;
            };
            if ed25519::verify(key, &message, sig).is_ok() {
                valid += 1;
            }
        }
        let need = usize::from(self.threshold);
        if valid >= need {
            Ok(())
        } else {
            Err(TrustError::ThresholdNotMet { valid, need })
        }
    }

    /// Authenticate first, parse second: the signature is checked over the raw
    /// bytes before the parser sees them, then freshness is checked.
    pub fn open(
        &self,
        signed: &[u8],
        sigs: &SignatureSet,
        now: u64,
    ) -> Result<Manifest, TrustError> {
        self.verify(signed, sigs)?;
        let m = Manifest::parse(signed).map_err(TrustError::Parse)?;
        let (issued, expires) = match &m {
            Manifest::Release(r) => (r.issued, r.expires),
            Manifest::Rotation(r) => (r.issued, r.expires),
        };
        check_fresh(issued, expires, now)?;
        Ok(m)
    }

    /// `open`, requiring a release signed under the current epoch.
    pub fn open_release(
        &self,
        signed: &[u8],
        sigs: &SignatureSet,
        now: u64,
    ) -> Result<Release, TrustError> {
        match self.open(signed, sigs, now)? {
            Manifest::Release(r) if r.key_epoch == self.epoch => Ok(r),
            Manifest::Release(r) => Err(TrustError::EpochMismatch {
                manifest: r.key_epoch,
                trusted: self.epoch,
            }),
            Manifest::Rotation(_) => Err(TrustError::WrongType),
        }
    }

    /// Verify a rotation against the CURRENT set and return the next `Trust`.
    pub fn apply_rotation(
        &self,
        signed: &[u8],
        sigs: &SignatureSet,
        now: u64,
    ) -> Result<Trust, TrustError> {
        let Manifest::Rotation(Rotation {
            key_epoch,
            threshold,
            keys,
            ..
        }) = self.open(signed, sigs, now)?
        else {
            return Err(TrustError::WrongType);
        };
        if key_epoch <= self.epoch {
            return Err(TrustError::RotationRollback);
        }
        if self.epoch.checked_add(1) != Some(key_epoch) {
            return Err(TrustError::RotationSkipsEpoch);
        }
        validate_set(&keys, threshold)?;
        if keys.iter().any(|k| self.revoked.contains(k)) {
            return Err(TrustError::RevokedKeyReintroduced);
        }
        let mut revoked = self.revoked.clone();
        for old in &self.keys {
            if !keys.contains(old) && !revoked.contains(old) {
                revoked.push(*old);
            }
        }
        Ok(Trust {
            epoch: key_epoch,
            keys,
            threshold,
            revoked,
        })
    }
}

/// `issued - skew <= now < expires`.
pub fn check_fresh(issued: u64, expires: u64, now: u64) -> Result<(), TrustError> {
    if now >= expires {
        return Err(TrustError::Expired);
    }
    if issued > now.saturating_add(MAX_FUTURE_SKEW_SECS) {
        return Err(TrustError::NotYetValid);
    }
    Ok(())
}

/// A parsed detached signature file. Construction guarantees 1..=8 entries
/// with strictly ascending key ids (so a key id appears at most once and one
/// key cannot be counted twice toward the threshold).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureSet(pub(crate) Vec<(KeyId, [u8; 64])>);

impl SignatureSet {
    pub fn parse(bytes: &[u8]) -> Result<SignatureSet, TrustError> {
        let bad = TrustError::BadSignatureFile;
        if bytes.len() > MAX_SIG_FILE_BYTES {
            return Err(TrustError::TooLarge);
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Err(bad("not ASCII"));
        };
        let Some(body) = text.strip_suffix('\n') else {
            return Err(bad("no final newline"));
        };
        let mut out: Vec<(KeyId, [u8; 64])> = Vec::new();
        for line in body.split('\n') {
            if out.len() >= MAX_KEYS {
                return Err(bad("too many signatures"));
            }
            let f: Vec<&str> = line.split(' ').collect();
            let ["sig", id, sig] = f.as_slice() else {
                return Err(bad("line is not `sig <key id> <signature>`"));
            };
            let (Some(id), Some(sig)) = (hex::decode_fixed::<8>(id), hex::decode_fixed::<64>(sig))
            else {
                return Err(bad(
                    "key id or signature is not lowercase hex of the right length",
                ));
            };
            let id = KeyId(id);
            if out.last().is_some_and(|(p, _)| *p >= id) {
                return Err(bad("key ids are not strictly ascending"));
            }
            out.push((id, sig));
        }
        Ok(SignatureSet(out))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::ed25519::tests::{public_key_of, sign};
    use crate::manifest::tests::{release, rotation};

    pub(crate) const NOW: u64 = 1_790_100_000;

    pub(crate) fn seed(n: u8) -> [u8; 32] {
        [n; 32]
    }

    pub(crate) fn pk(n: u8) -> [u8; 32] {
        public_key_of(&seed(n))
    }

    /// Signature file signed by the given seeds over `DOMAIN || bytes`.
    pub(crate) fn sigfile(bytes: &[u8], signers: &[u8]) -> SignatureSet {
        let mut msg = SIGN_DOMAIN.to_vec();
        msg.extend_from_slice(bytes);
        let mut v: Vec<(KeyId, [u8; 64])> = signers
            .iter()
            .map(|n| (key_id(&pk(*n)), sign(&seed(*n), &msg)))
            .collect();
        v.sort();
        let text: String = v
            .iter()
            .map(|(id, s)| format!("sig {} {}\n", hex::encode(&id.0), hex::encode(s)))
            .collect();
        SignatureSet::parse(text.as_bytes()).unwrap()
    }

    pub(crate) fn trust1(signers: &[u8], threshold: u8) -> Trust {
        Trust::new(
            1,
            signers.iter().map(|n| pk(*n)).collect(),
            threshold,
            vec![],
        )
        .unwrap()
    }

    fn rot_bytes(epoch: u32, new: &[u8], threshold: u8) -> Vec<u8> {
        let mut keys: Vec<[u8; 32]> = new.iter().map(|n| pk(*n)).collect();
        keys.sort();
        let mut r = rotation();
        r.key_epoch = epoch;
        r.threshold = threshold;
        r.keys = keys;
        Manifest::Rotation(r).encode().into_bytes()
    }

    fn rel_bytes(epoch: u32) -> Vec<u8> {
        let mut r = release();
        r.key_epoch = epoch;
        Manifest::Release(r).encode().into_bytes()
    }

    #[test]
    fn accepts_a_signed_release() {
        let t = trust1(&[1], 1);
        let b = rel_bytes(1);
        let r = t.open_release(&b, &sigfile(&b, &[1]), NOW).unwrap();
        assert_eq!(r.sequence, 7);
    }

    #[test]
    fn tampered_bytes_unknown_signer_and_garbage_are_rejected() {
        let t = trust1(&[1], 1);
        let b = rel_bytes(1);
        let sigs = sigfile(&b, &[1]);
        let mut tampered = b.clone();
        tampered[20] ^= 1;
        assert!(matches!(
            t.open_release(&tampered, &sigs, NOW),
            Err(TrustError::ThresholdNotMet { valid: 0, need: 1 })
        ));
        // Signed by a key that is not trusted.
        assert!(t.open_release(&b, &sigfile(&b, &[9]), NOW).is_err());
        // Signature for different bytes.
        let other = rel_bytes(1)
            .iter()
            .map(|c| c.to_ascii_uppercase())
            .collect::<Vec<_>>();
        assert!(t.open_release(&b, &sigfile(&other, &[1]), NOW).is_err());
        // Domain separation: a bare signature over the manifest is not enough.
        let bare = sign(&seed(1), &b);
        let text = format!(
            "sig {} {}\n",
            hex::encode(&key_id(&pk(1)).0),
            hex::encode(&bare)
        );
        assert!(
            t.open_release(&b, &SignatureSet::parse(text.as_bytes()).unwrap(), NOW)
                .is_err()
        );
    }

    #[test]
    fn threshold_counts_distinct_trusted_keys() {
        let t = trust1(&[1, 2, 3], 2);
        let b = rel_bytes(1);
        assert!(t.open_release(&b, &sigfile(&b, &[1]), NOW).is_err());
        assert!(t.open_release(&b, &sigfile(&b, &[1, 9]), NOW).is_err());
        assert!(t.open_release(&b, &sigfile(&b, &[1, 3]), NOW).is_ok());
        assert!(t.open_release(&b, &sigfile(&b, &[1, 2, 3]), NOW).is_ok());
        // The same signature twice cannot be parsed into a set at all.
        let one = sigfile(&b, &[1]);
        let line = format!(
            "sig {} {}\n",
            hex::encode(&one.0[0].0.0),
            hex::encode(&one.0[0].1)
        );
        assert!(SignatureSet::parse(format!("{line}{line}").as_bytes()).is_err());
    }

    #[test]
    fn freshness_window() {
        let t = trust1(&[1], 1);
        let b = rel_bytes(1);
        let s = sigfile(&b, &[1]);
        let r = release();
        assert_eq!(t.open_release(&b, &s, r.expires), Err(TrustError::Expired));
        assert_eq!(
            t.open_release(&b, &s, r.expires + 1_000_000),
            Err(TrustError::Expired)
        );
        assert!(t.open_release(&b, &s, r.expires - 1).is_ok());
        assert_eq!(
            t.open_release(&b, &s, r.issued - MAX_FUTURE_SKEW_SECS - 1),
            Err(TrustError::NotYetValid)
        );
        assert!(
            t.open_release(&b, &s, r.issued - MAX_FUTURE_SKEW_SECS)
                .is_ok()
        );
    }

    #[test]
    fn signature_file_parsing_is_strict() {
        let b = rel_bytes(1);
        let good = sigfile(&b, &[1]);
        let line = format!(
            "sig {} {}\n",
            hex::encode(&good.0[0].0.0),
            hex::encode(&good.0[0].1)
        );
        assert!(SignatureSet::parse(line.as_bytes()).is_ok());
        for bad in [
            line.trim_end().to_string(),
            line.to_uppercase(),
            format!("sig  {}", &line[4..]),
            format!("{line}\n"),
            line.replace("sig ", "sgn "),
            String::new(),
            "\n".to_string(),
            "x".repeat(2000),
        ] {
            assert!(SignatureSet::parse(bad.as_bytes()).is_err(), "{bad:?}");
        }
        // Descending key ids.
        let two = sigfile(&b, &[1, 2]);
        let l = |i: usize| {
            format!(
                "sig {} {}\n",
                hex::encode(&two.0[i].0.0),
                hex::encode(&two.0[i].1)
            )
        };
        assert!(SignatureSet::parse(format!("{}{}", l(0), l(1)).as_bytes()).is_ok());
        assert!(SignatureSet::parse(format!("{}{}", l(1), l(0)).as_bytes()).is_err());
    }

    #[test]
    fn key_set_validation() {
        assert!(Trust::new(1, vec![], 1, vec![]).is_err());
        assert!(Trust::new(1, vec![pk(1)], 0, vec![]).is_err());
        assert!(Trust::new(1, vec![pk(1)], 2, vec![]).is_err());
        assert!(Trust::new(0, vec![pk(1)], 1, vec![]).is_err());
        assert!(Trust::new(1, vec![pk(1), pk(1)], 1, vec![]).is_err());
        assert!(Trust::new(1, vec![[0u8; 32]], 1, vec![]).is_err());
        let mut identity = [0u8; 32];
        identity[0] = 1;
        assert!(Trust::new(1, vec![identity], 1, vec![]).is_err());
        assert!(Trust::new(1, vec![pk(1)], 1, vec![pk(1)]).is_err());
    }

    #[test]
    fn old_key_rotates_to_new_key() {
        let t1 = trust1(&[1], 1);
        let rot = rot_bytes(2, &[2], 1);
        let t2 = t1.apply_rotation(&rot, &sigfile(&rot, &[1]), NOW).unwrap();
        assert_eq!(t2.epoch(), 2);
        assert_eq!(t2.keys(), &[pk(2)]);
        assert_eq!(t2.revoked(), &[pk(1)]);
        // The new key now signs releases under epoch 2.
        let b = rel_bytes(2);
        assert!(t2.open_release(&b, &sigfile(&b, &[2]), NOW).is_ok());
    }

    #[test]
    fn revoked_key_is_rejected_after_rotation() {
        let t1 = trust1(&[1], 1);
        let rot = rot_bytes(2, &[2], 1);
        let t2 = t1.apply_rotation(&rot, &sigfile(&rot, &[1]), NOW).unwrap();
        // Old key signing a release (any epoch) no longer counts.
        for epoch in [1, 2] {
            let b = rel_bytes(epoch);
            assert!(t2.open_release(&b, &sigfile(&b, &[1]), NOW).is_err());
        }
        // A manifest under the old epoch signed by the new key is refused too.
        let b = rel_bytes(1);
        assert_eq!(
            t2.open_release(&b, &sigfile(&b, &[2]), NOW),
            Err(TrustError::EpochMismatch {
                manifest: 1,
                trusted: 2
            })
        );
        // Old key cannot rotate any more.
        let rot3 = rot_bytes(3, &[3], 1);
        assert!(
            t2.apply_rotation(&rot3, &sigfile(&rot3, &[1]), NOW)
                .is_err()
        );
    }

    #[test]
    fn rotation_must_be_signed_by_the_old_set_not_the_new() {
        let t1 = trust1(&[1], 1);
        let rot = rot_bytes(2, &[2], 1);
        assert!(t1.apply_rotation(&rot, &sigfile(&rot, &[2]), NOW).is_err());
        // Threshold of the old set applies.
        let t = trust1(&[1, 2, 3], 2);
        let rot = rot_bytes(2, &[4], 1);
        assert!(t.apply_rotation(&rot, &sigfile(&rot, &[1]), NOW).is_err());
        assert!(t.apply_rotation(&rot, &sigfile(&rot, &[1, 2]), NOW).is_ok());
    }

    #[test]
    fn rotation_cannot_skip_an_epoch() {
        let t1 = trust1(&[1], 1);
        let rot = rot_bytes(3, &[2], 1);
        assert_eq!(
            t1.apply_rotation(&rot, &sigfile(&rot, &[1]), NOW),
            Err(TrustError::RotationSkipsEpoch)
        );
    }

    #[test]
    fn rotation_cannot_be_rolled_back_or_replayed() {
        let t1 = trust1(&[1], 1);
        let rot2 = rot_bytes(2, &[2], 1);
        let s2 = sigfile(&rot2, &[1]);
        let t2 = t1.apply_rotation(&rot2, &s2, NOW).unwrap();
        // Replaying rotation 2 against the epoch-2 trust.
        assert_eq!(
            t2.apply_rotation(&rot2, &s2, NOW),
            Err(TrustError::ThresholdNotMet { valid: 0, need: 1 })
        );
        // Even signed by the current key, an epoch <= current is a rollback.
        let again = rot_bytes(2, &[1], 1);
        assert_eq!(
            t2.apply_rotation(&again, &sigfile(&again, &[2]), NOW),
            Err(TrustError::RotationRollback)
        );
        let older = rot_bytes(1, &[1], 1);
        // epoch 1 is not even a parseable rotation ("creates epoch 2 or later").
        assert!(
            t2.apply_rotation(&older, &sigfile(&older, &[2]), NOW)
                .is_err()
        );
    }

    #[test]
    fn revoked_key_cannot_be_reintroduced() {
        let t1 = trust1(&[1], 1);
        let rot2 = rot_bytes(2, &[2], 1);
        let t2 = t1
            .apply_rotation(&rot2, &sigfile(&rot2, &[1]), NOW)
            .unwrap();
        let rot3 = rot_bytes(3, &[1, 2], 1);
        assert_eq!(
            t2.apply_rotation(&rot3, &sigfile(&rot3, &[2]), NOW),
            Err(TrustError::RevokedKeyReintroduced)
        );
    }

    #[test]
    fn rotation_with_invalid_new_key_or_stale_is_rejected() {
        let t1 = trust1(&[1], 1);
        let mut r = rotation();
        r.keys = vec![[0u8; 32]];
        let bytes = Manifest::Rotation(r).encode().into_bytes();
        assert!(
            t1.apply_rotation(&bytes, &sigfile(&bytes, &[1]), NOW)
                .is_err()
        );
        let rot = rot_bytes(2, &[2], 1);
        let exp = rotation().expires;
        assert_eq!(
            t1.apply_rotation(&rot, &sigfile(&rot, &[1]), exp),
            Err(TrustError::Expired)
        );
        // A release is not a rotation.
        let b = rel_bytes(1);
        assert_eq!(
            t1.apply_rotation(&b, &sigfile(&b, &[1]), NOW),
            Err(TrustError::WrongType)
        );
        // And a rotation is not a release.
        assert_eq!(
            t1.open_release(&rot, &sigfile(&rot, &[1]), NOW),
            Err(TrustError::WrongType)
        );
    }

    #[test]
    fn rotation_can_change_the_threshold_and_keeps_surviving_keys() {
        let t = trust1(&[1, 2], 1);
        let rot = rot_bytes(2, &[2, 3], 2);
        let t2 = t.apply_rotation(&rot, &sigfile(&rot, &[2]), NOW).unwrap();
        assert_eq!(t2.threshold(), 2);
        assert_eq!(t2.revoked(), &[pk(1)]);
        let b = rel_bytes(2);
        assert!(t2.open_release(&b, &sigfile(&b, &[2]), NOW).is_err());
        assert!(t2.open_release(&b, &sigfile(&b, &[2, 3]), NOW).is_ok());
    }
}
