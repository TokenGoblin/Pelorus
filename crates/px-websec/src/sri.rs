//! Subresource Integrity (W3C SRI).
//!
//! Digests come from `csp::sha2` (the CSP engine's FIPS 180-4
//! implementation); [`verify_with`] also lets the integration supply its own
//! digest function.
//!
//! # Deviations / decisions
//! - An empty `integrity` value is "no metadata" and the load proceeds. A
//!   value with only unknown algorithms (`md5-...`) is an empty *set*: it
//!   proceeds for an eligible response but is refused for an opaque one
//!   (SRI orders the eligibility test first). A recognised algorithm with an
//!   undecodable or wrong-length digest is kept and can never match, so the
//!   load fails instead of silently skipping the check. Callers must consult
//!   [`Metadata::needs_check`], not `is_enforced`.
//! - Input is bounded ([`MAX_INTEGRITY_BYTES`], [`MAX_TOKENS`]); beyond that
//!   parsing *fails* (the load is denied) rather than ignoring the excess,
//!   because an attacker-padded value must not turn enforcement off.
//! - Options (`?...`) are accepted and ignored. Both standard and URL-safe
//!   base64 are accepted, padded or not; digests are compared as bytes.

use crate::cors::Tainting;
use crate::csp::sha2;
use crate::origin::trace;

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &["sri::parse", "sri::eligible", "sri::verify"];

/// Longest `integrity` value examined.
pub const MAX_INTEGRITY_BYTES: usize = 16 * 1024;
/// Most whitespace-separated tokens examined.
pub const MAX_TOKENS: usize = 128;

/// Hash algorithms, ordered weakest to strongest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Algorithm {
    /// SHA-256
    Sha256,
    /// SHA-384
    Sha384,
    /// SHA-512
    Sha512,
}

impl Algorithm {
    /// Digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }

    fn from_token(t: &str) -> Option<Self> {
        if t.eq_ignore_ascii_case("sha256") {
            Some(Self::Sha256)
        } else if t.eq_ignore_ascii_case("sha384") {
            Some(Self::Sha384)
        } else if t.eq_ignore_ascii_case("sha512") {
            Some(Self::Sha512)
        } else {
            None
        }
    }

    /// Digest of `data` with the built-in implementation.
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => sha2::sha256(data).to_vec(),
            Self::Sha384 => sha2::sha384(data).to_vec(),
            Self::Sha512 => sha2::sha512(data).to_vec(),
        }
    }
}

/// One hash expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashExpr {
    /// Algorithm.
    pub algorithm: Algorithm,
    /// Decoded digest bytes (exactly `algorithm.digest_len()` long).
    pub digest: Vec<u8>,
}

/// Parsed integrity metadata: the recognised hash expressions.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Metadata {
    /// Recognised-algorithm expressions in order. A token whose digest does
    /// not decode, or has the wrong length, is kept with an empty `digest`:
    /// it can never match, so the load fails (it must not silently disable
    /// the check).
    pub hashes: Vec<HashExpr>,
    /// Whether the value held any token at all. SRI "parse metadata" returns
    /// "no metadata" only for an empty value; a value with only unknown
    /// algorithms is an empty *set*, which still requires an eligible
    /// response.
    pub had_tokens: bool,
}

impl Metadata {
    /// Whether any integrity check will actually be performed.
    pub fn is_enforced(&self) -> bool {
        !self.hashes.is_empty()
    }

    /// Whether [`verify`] must be consulted at all (it is the only place the
    /// eligibility rule is applied).
    pub fn needs_check(&self) -> bool {
        self.had_tokens
    }

    /// The strongest algorithm present.
    pub fn strongest(&self) -> Option<Algorithm> {
        self.hashes.iter().map(|h| h.algorithm).max()
    }
}

/// Why integrity handling refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SriError {
    /// Attribute or token count over the bounds.
    TooLarge,
    /// The response is not CORS-same-origin (opaque), so it cannot be
    /// validated.
    NotEligible,
    /// No hash of the strongest algorithm matched.
    Mismatch,
}

fn b64_val(b: u8) -> Option<u8> {
    match b {
        b'A'..=b'Z' => Some(b - b'A'),
        b'a'..=b'z' => Some(b - b'a' + 26),
        b'0'..=b'9' => Some(b - b'0' + 52),
        b'+' | b'-' => Some(62),
        b'/' | b'_' => Some(63),
        _ => None,
    }
}

/// Decode standard or URL-safe base64, padding optional (at most two `=`,
/// only at the end). `None` on anything malformed, including non-zero
/// trailing bits.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let body = s.trim_end_matches('=');
    if s.len() - body.len() > 2 {
        return None;
    }
    if body.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(body.len() / 4 * 3 + 2);
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for b in body.bytes() {
        acc = (acc << 6) | u32::from(b64_val(b)?);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    if acc != 0 {
        return None;
    }
    Some(out)
}

/// Parse an `integrity` attribute value.
pub fn parse(value: &str) -> Result<Metadata, SriError> {
    trace::hit("sri::parse");
    if value.len() > MAX_INTEGRITY_BYTES {
        return Err(SriError::TooLarge);
    }
    let mut hashes = Vec::new();
    let mut count = 0usize;
    for token in value.split([' ', '\t', '\n', '\r', '\u{c}']) {
        if token.is_empty() {
            continue;
        }
        count += 1;
        if count > MAX_TOKENS {
            return Err(SriError::TooLarge);
        }
        let token = token.split('?').next().unwrap_or("");
        let Some((alg, b64)) = token.split_once('-') else {
            continue;
        };
        let Some(algorithm) = Algorithm::from_token(alg) else {
            continue;
        };
        // Grammar: 1*(ALPHA / DIGIT / "+" / "/" / "-" / "_") *2("="). A token
        // that fails it is not valid metadata and is skipped.
        let body = b64.trim_end_matches('=');
        if body.is_empty()
            || b64.len() - body.len() > 2
            || !body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_'))
        {
            continue;
        }
        let digest = base64_decode(b64)
            .filter(|d| d.len() == algorithm.digest_len())
            .unwrap_or_default();
        hashes.push(HashExpr { algorithm, digest });
    }
    Ok(Metadata {
        hashes,
        had_tokens: count > 0,
    })
}

/// A response is eligible for integrity validation only if it is
/// CORS-same-origin: same-origin (basic) or CORS-approved. Opaque is not.
pub fn eligible(tainting: Tainting) -> bool {
    trace::hit("sri::eligible");
    !matches!(tainting, Tainting::Opaque)
}

/// Verify `body` against `metadata` using `digest` for hashing.
pub fn verify_with(
    metadata: &Metadata,
    tainting: Tainting,
    body: &[u8],
    digest: &dyn Fn(Algorithm, &[u8]) -> Vec<u8>,
) -> Result<(), SriError> {
    trace::hit("sri::verify");
    // SRI "does response match metadataList": no metadata => true; then
    // eligibility; only then does an empty set mean true.
    if !metadata.had_tokens && metadata.hashes.is_empty() {
        return Ok(());
    }
    if !eligible(tainting) {
        return Err(SriError::NotEligible);
    }
    let Some(strongest) = metadata.strongest() else {
        return Ok(());
    };
    let actual = digest(strongest, body);
    if metadata
        .hashes
        .iter()
        .filter(|h| h.algorithm == strongest)
        .any(|h| h.digest == actual)
    {
        Ok(())
    } else {
        Err(SriError::Mismatch)
    }
}

/// [`verify_with`] using the built-in digests.
pub fn verify(metadata: &Metadata, tainting: Tainting, body: &[u8]) -> Result<(), SriError> {
    verify_with(metadata, tainting, body, &|a, d| a.digest(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Known answers (FIPS 180-4 examples), base64 of the digest of "abc".
    const S256: &str = "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";
    const S384: &str = "ywB1P0WjXou1oD1pmsZQBycsMqsO3tFjGotgWkP/W+2AhgcroefMI1i67KE0yCWn";
    const S512: &str =
        "3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw==";
    const EMPTY256: &str = "47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=";

    #[test]
    fn base64_basics() {
        assert_eq!(base64_decode("TWFu"), Some(b"Man".to_vec()));
        assert_eq!(base64_decode("TWE="), Some(b"Ma".to_vec()));
        assert_eq!(base64_decode("TWE"), Some(b"Ma".to_vec()));
        assert_eq!(base64_decode("TQ=="), Some(b"M".to_vec()));
        assert_eq!(base64_decode(""), Some(vec![]));
        assert_eq!(base64_decode("TWF"), None, "trailing bits non-zero");
        assert_eq!(base64_decode("T"), None);
        assert_eq!(base64_decode("TQ==="), None);
        assert_eq!(base64_decode("TW=E"), None);
        assert_eq!(base64_decode("TR=="), None, "non-zero trailing bits");
        assert_eq!(base64_decode("a b"), None);
        // URL-safe alphabet.
        assert_eq!(base64_decode("-_-_"), base64_decode("+/+/"));
    }

    #[test]
    fn digests_match_known_answers() {
        for (a, b64) in [
            (Algorithm::Sha256, S256),
            (Algorithm::Sha384, S384),
            (Algorithm::Sha512, S512),
        ] {
            assert_eq!(Some(a.digest(b"abc")), base64_decode(b64), "{a:?}");
        }
        assert_eq!(Some(Algorithm::Sha256.digest(b"")), base64_decode(EMPTY256));
    }

    #[test]
    fn parse_accepts_and_ignores() {
        let m = parse(&format!("sha256-{S256} sha384-{S384}?opt=1 md5-AAAA bogus")).unwrap();
        assert_eq!(m.hashes.len(), 2);
        assert_eq!(m.strongest(), Some(Algorithm::Sha384));
        assert!(parse("").unwrap().hashes.is_empty());
        assert!(!parse("md5-AAAA").unwrap().is_enforced());
        // Case-insensitive algorithm; a wrong-length digest is still metadata
        // (it can never match, so the load fails: SRI "parse metadata").
        assert!(parse(&format!("SHA256-{S256}")).unwrap().is_enforced());
        assert!(parse(&format!("sha256-{S384}")).unwrap().is_enforced());
        // Whitespace variety.
        assert_eq!(
            parse(&format!("\tsha256-{S256}\n\nsha256-{S256}  "))
                .unwrap()
                .hashes
                .len(),
            2
        );
    }

    #[test]
    fn parse_is_bounded_and_fails_closed() {
        assert_eq!(
            parse(&"a".repeat(MAX_INTEGRITY_BYTES + 1)),
            Err(SriError::TooLarge)
        );
        let many = vec!["x"; MAX_TOKENS + 1].join(" ");
        assert_eq!(parse(&many), Err(SriError::TooLarge));
        let ok = vec!["x"; MAX_TOKENS].join(" ");
        assert!(parse(&ok).is_ok());
    }

    #[test]
    fn verify_match_and_mismatch() {
        let m = parse(&format!("sha256-{S256}")).unwrap();
        assert_eq!(verify(&m, Tainting::Basic, b"abc"), Ok(()));
        assert_eq!(verify(&m, Tainting::Cors, b"abc"), Ok(()));
        assert_eq!(verify(&m, Tainting::Basic, b"abd"), Err(SriError::Mismatch));
        assert_eq!(verify(&m, Tainting::Basic, b""), Err(SriError::Mismatch));
    }

    #[test]
    fn strongest_wins_weaker_is_ignored() {
        // sha512 is wrong, sha256 right: strongest decides, so mismatch.
        let wrong512 = format!("sha512-{}", "A".repeat(86));
        let m = parse(&format!("sha256-{S256} {wrong512}")).unwrap();
        assert_eq!(m.strongest(), Some(Algorithm::Sha512));
        assert_eq!(verify(&m, Tainting::Basic, b"abc"), Err(SriError::Mismatch));
        // Any of the strongest may match.
        let m2 = parse(&format!("{wrong512} sha512-{S512}")).unwrap();
        assert_eq!(verify(&m2, Tainting::Basic, b"abc"), Ok(()));
        // A right weaker hash plus a right stronger one.
        let m3 = parse(&format!("sha256-{S256} sha384-{S384}")).unwrap();
        assert_eq!(verify(&m3, Tainting::Basic, b"abc"), Ok(()));
    }

    #[test]
    fn opaque_responses_are_ineligible() {
        let m = parse(&format!("sha256-{S256}")).unwrap();
        assert_eq!(
            verify(&m, Tainting::Opaque, b"abc"),
            Err(SriError::NotEligible)
        );
        // Without enforced metadata, nothing to validate.
        assert_eq!(verify(&Metadata::default(), Tainting::Opaque, b"x"), Ok(()));
        assert!(!eligible(Tainting::Opaque));
        assert!(eligible(Tainting::Basic) && eligible(Tainting::Cors));
    }

    #[test]
    fn supplied_digest_function_is_used() {
        let m = parse(&format!("sha256-{S256}")).unwrap();
        let fake = |_: Algorithm, _: &[u8]| Algorithm::Sha256.digest(b"abc");
        assert_eq!(verify_with(&m, Tainting::Basic, b"anything", &fake), Ok(()));
        let broken = |_: Algorithm, _: &[u8]| Vec::new();
        assert_eq!(
            verify_with(&m, Tainting::Basic, b"abc", &broken),
            Err(SriError::Mismatch)
        );
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let m = parse(&format!("sha256-{S256}")).unwrap();
        let _ = verify(&m, Tainting::Basic, b"abc");
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x: u64 = 0xA5A5_5A5A_1234_5678;
        let alphabet = b"sha256-384-512ABCxyz0129+/_=? \t\xc3\xa9";
        for _ in 0..10_000 {
            let mut s = String::new();
            for _ in 0..(x % 200) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize] as char);
            }
            let _ = base64_decode(&s);
            if let Ok(m) = parse(&s) {
                let _ = verify(&m, Tainting::Cors, s.as_bytes());
            }
        }
    }
}
