//! Cookie persistence: px-net's cookie entries, stored per partition.
//!
//! px-net's `Cookie` has no serialization and its expiry is an `Instant`,
//! which means nothing across a restart. This module owns both problems and
//! does not touch px-net:
//!
//! * [`StoredCookie`] is a small hand-written binary encoding with the expiry
//!   as an absolute Unix time (seconds). Callers pass the clock in, so this
//!   module never reads one and its tests are deterministic.
//! * **Saving** takes the cookies the caller holds (`&[&Cookie]`): px-net's
//!   `Jar` has no "list this partition" accessor, only `cookies_for`, which
//!   filters by path. The caller decides which cookies to persist; a small
//!   read accessor on `Jar` would let the broker pass everything (see the
//!   backlog entry).
//! * **Loading** does not build `Cookie` values directly. It rebuilds a
//!   `Set-Cookie` header and hands it to `Jar::store`, so every persisted
//!   cookie is re-validated by px-net's parser against the *current* public
//!   suffix list and rules (supercookie, prefix, Secure-over-plaintext). A
//!   record that no longer passes is dropped and counted, never loaded
//!   "mostly".
//!
//! Cookies live in [`Namespace::Cookies`], so page storage cannot overwrite
//! them even inside the same partition.

use std::time::{Duration, Instant};

use px_net::cookie::{Cookie, Jar, SameSite};
use px_net::partition::PartitionKey;
use px_net::psl::PublicSuffixList;

use crate::engine::Namespace;
use crate::error::StoreError;
use crate::format::Reader;
use crate::store::Store;

const VERSION: u8 = 1;
const F_HOST_ONLY: u8 = 1;
const F_SECURE: u8 = 2;
const F_HTTP_ONLY: u8 = 4;
const F_EXPIRES: u8 = 8;

/// A cookie as persisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCookie {
    /// Name.
    pub name: String,
    /// Value.
    pub value: String,
    /// Scoped host.
    pub domain: String,
    /// Exact host only.
    pub host_only: bool,
    /// Path prefix.
    pub path: String,
    /// Secure.
    pub secure: bool,
    /// HttpOnly.
    pub http_only: bool,
    /// SameSite.
    pub same_site: SameSite,
    /// Absolute expiry, Unix seconds. `None` is a session cookie.
    pub expires_unix: Option<u64>,
}

fn same_site_code(s: SameSite) -> u8 {
    match s {
        SameSite::Strict => 0,
        SameSite::Lax => 1,
        SameSite::None => 2,
    }
}

fn push_str(out: &mut Vec<u8>, s: &str) -> Option<()> {
    out.extend_from_slice(&u16::try_from(s.len()).ok()?.to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    Some(())
}

fn read_str(r: &mut Reader<'_>) -> Option<String> {
    let len = usize::from(r.u16()?);
    String::from_utf8(r.take(len)?.to_vec()).ok()
}

impl StoredCookie {
    /// Capture a live cookie. `None` if it has already expired at `now`.
    /// `now` and `now_unix` must be the same instant on the two clocks.
    pub fn from_cookie(cookie: &Cookie, now: Instant, now_unix: u64) -> Option<Self> {
        let expires_unix = match cookie.expires_at {
            Some(at) if at <= now => return None,
            Some(at) => {
                let left = at.saturating_duration_since(now);
                let secs = left.as_secs() + u64::from(left.subsec_nanos() > 0);
                Some(now_unix.saturating_add(secs))
            }
            None => None,
        };
        Some(Self {
            name: cookie.name.clone(),
            value: cookie.value.clone(),
            domain: cookie.domain.clone(),
            host_only: cookie.host_only,
            path: cookie.path.clone(),
            secure: cookie.secure,
            http_only: cookie.http_only,
            same_site: cookie.same_site,
            expires_unix,
        })
    }

    /// Binary form: `version flags same_site [expires u64] name domain path value`,
    /// each string as `u16 length + bytes`.
    pub fn encode(&self) -> Option<Vec<u8>> {
        let mut flags = 0;
        if self.host_only {
            flags |= F_HOST_ONLY;
        }
        if self.secure {
            flags |= F_SECURE;
        }
        if self.http_only {
            flags |= F_HTTP_ONLY;
        }
        if self.expires_unix.is_some() {
            flags |= F_EXPIRES;
        }
        let mut out = vec![VERSION, flags, same_site_code(self.same_site)];
        if let Some(at) = self.expires_unix {
            out.extend_from_slice(&at.to_le_bytes());
        }
        for s in [&self.name, &self.domain, &self.path, &self.value] {
            push_str(&mut out, s)?;
        }
        Some(out)
    }

    /// Parse the binary form. Anything unexpected is `None`.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = Reader::new(bytes);
        if r.u8()? != VERSION {
            return None;
        }
        let flags = r.u8()?;
        if flags & !(F_HOST_ONLY | F_SECURE | F_HTTP_ONLY | F_EXPIRES) != 0 {
            return None;
        }
        let same_site = match r.u8()? {
            0 => SameSite::Strict,
            1 => SameSite::Lax,
            2 => SameSite::None,
            _ => return None,
        };
        let expires_unix = if flags & F_EXPIRES != 0 {
            Some(u64::from_le_bytes(r.take(8)?.try_into().ok()?))
        } else {
            None
        };
        let name = read_str(&mut r)?;
        let domain = read_str(&mut r)?;
        let path = read_str(&mut r)?;
        let value = read_str(&mut r)?;
        if !r.is_empty() {
            return None;
        }
        Some(Self {
            name,
            value,
            domain,
            host_only: flags & F_HOST_ONLY != 0,
            path,
            secure: flags & F_SECURE != 0,
            http_only: flags & F_HTTP_ONLY != 0,
            same_site,
            expires_unix,
        })
    }

    /// The `Set-Cookie` header that recreates this cookie at `now_unix`.
    /// `None` when it has expired. A persistent cookie becomes `Max-Age` of
    /// the remaining seconds, which px-net's parser understands.
    pub fn to_set_cookie(&self, now_unix: u64) -> Option<String> {
        let mut h = format!("{}={}", self.name, self.value);
        if !self.host_only {
            h.push_str(&format!("; Domain={}", self.domain));
        }
        h.push_str(&format!("; Path={}", self.path));
        if self.secure {
            h.push_str("; Secure");
        }
        if self.http_only {
            h.push_str("; HttpOnly");
        }
        h.push_str(match self.same_site {
            SameSite::Strict => "; SameSite=Strict",
            SameSite::Lax => "; SameSite=Lax",
            SameSite::None => "; SameSite=None",
        });
        if let Some(at) = self.expires_unix {
            let left = at.checked_sub(now_unix).filter(|&s| s > 0)?;
            h.push_str(&format!("; Max-Age={left}"));
        }
        Some(h)
    }

    /// The store key: `name`, `domain`, `path` as length-prefixed strings.
    pub fn store_key(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for s in [&self.name, &self.domain, &self.path] {
            push_str(&mut out, s)?;
        }
        Some(out)
    }
}

/// What [`load`] did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LoadReport {
    /// Cookies placed in the jar.
    pub loaded: usize,
    /// Persisted but expired by `now_unix`.
    pub expired: usize,
    /// Session cookies skipped because the caller did not ask for them.
    pub skipped_session: usize,
    /// Undecodable, or refused by px-net's current rules.
    pub rejected: usize,
}

/// Make the persisted cookies of `partition` exactly `cookies` (minus any
/// already expired). Returns how many are now stored. On a quota error the
/// partition may hold a mix of old and new; nothing is silently dropped.
pub fn save(
    store: &mut Store,
    partition: &PartitionKey,
    cookies: &[&Cookie],
    now: Instant,
    now_unix: u64,
) -> Result<usize, StoreError> {
    let mut wanted: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
    for cookie in cookies {
        let Some(stored) = StoredCookie::from_cookie(cookie, now, now_unix) else {
            continue;
        };
        if let (Some(key), Some(value)) = (stored.store_key(), stored.encode()) {
            wanted.push((key, value));
        }
    }
    let stale: Vec<Vec<u8>> = store
        .keys(partition, Namespace::Cookies)
        .into_iter()
        .filter(|k| !wanted.iter().any(|(w, _)| w.as_slice() == *k))
        .map(<[u8]>::to_vec)
        .collect();
    for key in &stale {
        store.delete(partition, Namespace::Cookies, key)?;
    }
    let mut stored = 0;
    for (key, value) in &wanted {
        if store.get(partition, Namespace::Cookies, key) != Some(value.as_slice()) {
            store.put(partition, Namespace::Cookies, key, value)?;
        }
        stored += 1;
    }
    Ok(stored)
}

/// Load `partition`'s persisted cookies into `jar`, re-validating each one
/// through `Jar::store`. Session cookies are loaded only if `include_session`.
pub fn load(
    store: &Store,
    psl: &PublicSuffixList,
    partition: &PartitionKey,
    jar: &mut Jar,
    now_unix: u64,
    include_session: bool,
) -> LoadReport {
    let mut report = LoadReport::default();
    for key in store.keys(partition, Namespace::Cookies) {
        let Some(stored) = store
            .get(partition, Namespace::Cookies, key)
            .and_then(StoredCookie::decode)
        else {
            report.rejected += 1;
            continue;
        };
        if stored.expires_unix.is_none() && !include_session {
            report.skipped_session += 1;
            continue;
        }
        let Some(header) = stored.to_set_cookie(now_unix) else {
            report.expired += 1;
            continue;
        };
        // A host-only cookie must belong to this partition's origin; the jar
        // would derive its host from the origin, so a mismatch is refused here
        // rather than silently re-scoped.
        if stored.host_only && stored.domain != partition.origin().host() {
            report.rejected += 1;
            continue;
        }
        match jar.store(psl, partition, &header) {
            Ok(()) => report.loaded += 1,
            Err(_) => report.rejected += 1,
        }
    }
    report
}

/// Remaining lifetime helper for callers that hold an `Instant`.
pub fn remaining(expires_unix: u64, now_unix: u64) -> Duration {
    Duration::from_secs(expires_unix.saturating_sub(now_unix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_net::cookie::parse;
    use px_net::partition::Origin;

    fn psl() -> PublicSuffixList {
        PublicSuffixList::parse("com\nexample\n").unwrap()
    }

    fn origin() -> Origin {
        Origin::new("https", "www.cdn.example", 443).unwrap()
    }

    fn sample() -> StoredCookie {
        StoredCookie {
            name: "sid".into(),
            value: "abc123".into(),
            domain: "cdn.example".into(),
            host_only: false,
            path: "/app".into(),
            secure: true,
            http_only: true,
            same_site: SameSite::None,
            expires_unix: Some(1_100),
        }
    }

    #[test]
    fn binary_form_by_hand() {
        let c = StoredCookie {
            name: "n".into(),
            value: "v".into(),
            domain: "d".into(),
            host_only: true,
            path: "/".into(),
            secure: false,
            http_only: true,
            same_site: SameSite::Lax,
            expires_unix: None,
        };
        // version 1, flags host_only|http_only = 5, same_site Lax = 1, then
        // "n" "d" "/" "v" each with a u16 length.
        assert_eq!(
            c.encode().unwrap(),
            b"\x01\x05\x01\x01\x00n\x01\x00d\x01\x00/\x01\x00v".to_vec()
        );
    }

    #[test]
    fn round_trip_and_hostile_decode() {
        let c = sample();
        let bytes = c.encode().unwrap();
        assert_eq!(StoredCookie::decode(&bytes), Some(c));
        // Every truncation and a trailing byte are refused; nothing panics.
        for n in 0..bytes.len() {
            assert_eq!(StoredCookie::decode(&bytes[..n]), None, "n={n}");
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert_eq!(StoredCookie::decode(&extra), None);
        let mut bad_version = bytes.clone();
        bad_version[0] = 9;
        assert_eq!(StoredCookie::decode(&bad_version), None);
        let mut bad_flag = bytes;
        bad_flag[1] |= 0x80;
        assert_eq!(StoredCookie::decode(&bad_flag), None);
    }

    #[test]
    fn header_reconstruction_and_expiry_arithmetic() {
        let c = sample();
        // 1100 - 1040 = 60 seconds left.
        assert_eq!(
            c.to_set_cookie(1_040).unwrap(),
            "sid=abc123; Domain=cdn.example; Path=/app; Secure; HttpOnly; SameSite=None; Max-Age=60"
        );
        assert_eq!(
            c.to_set_cookie(1_100),
            None,
            "expiring exactly now is expired"
        );
        assert_eq!(c.to_set_cookie(5_000), None);
    }

    #[test]
    fn live_cookie_survives_the_round_trip_through_px_net_parser() {
        let header = "sid=abc123; Domain=cdn.example; Path=/app; Secure; HttpOnly; SameSite=None; Max-Age=100";
        let live = parse(&psl(), &origin(), header).unwrap();
        let now = Instant::now();
        let stored = StoredCookie::from_cookie(&live, now, 1_000).unwrap();
        // Parsed an instant ago, so 100 s minus a hair, rounded up: 100 or 101.
        assert!(
            matches!(stored.expires_unix, Some(1_100 | 1_101)),
            "{stored:?}"
        );
        let again = parse(&psl(), &origin(), &stored.to_set_cookie(1_000).unwrap()).unwrap();
        for (a, b) in [
            (&live.name, &again.name),
            (&live.value, &again.value),
            (&live.domain, &again.domain),
            (&live.path, &again.path),
        ] {
            assert_eq!(a, b);
        }
        assert_eq!(
            (live.host_only, live.secure, live.http_only, live.same_site),
            (
                again.host_only,
                again.secure,
                again.http_only,
                again.same_site
            )
        );
        assert!(again.expires_at.is_some());
    }

    #[test]
    fn expired_cookies_are_not_captured() {
        let past = parse(&psl(), &origin(), "a=b; Max-Age=0").unwrap();
        assert_eq!(
            StoredCookie::from_cookie(&past, Instant::now(), 1_000),
            None
        );
    }

    #[test]
    fn store_key_distinguishes_name_domain_path() {
        let a = sample();
        let mut b = sample();
        b.path = "/other".into();
        let mut c = sample();
        c.domain = "x.example".into();
        let keys = [a.store_key(), b.store_key(), c.store_key()];
        assert_ne!(keys[0], keys[1]);
        assert_ne!(keys[0], keys[2]);
        // Not fooled by a boundary shifted between fields.
        let mut d = sample();
        d.name = "sidcdn.example".into();
        d.domain = String::new();
        assert_ne!(a.store_key(), d.store_key());
    }
}
