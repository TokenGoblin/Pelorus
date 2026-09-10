//! Cookies: parsing `Set-Cookie`, and the jar that partitions them.
//!
//! # The two things that actually matter
//!
//! **A cookie must never be set on a public suffix.** `Domain=.com` would be a
//! cookie every `.com` site could read and write — the supercookie, and the
//! original reason the Public Suffix List exists. This is checked against the
//! real list rather than a heuristic about dot counts, which is the check
//! browsers historically got wrong.
//!
//! **The jar is keyed by [`PartitionKey`]**, so a cookie set for `cdn.example`
//! while the user is on `news.example` is not the same cookie as one set for
//! `cdn.example` while they are on `other.example`. That is invariant 2, and
//! without it a third party embedded on two sites can join them — which is the
//! entire mechanism of third-party tracking.
//!
//! # Everything here fails closed
//!
//! An attribute that cannot be parsed does not become a default, it rejects
//! the cookie. A `Domain` that does not domain-match the origin rejects the
//! cookie. A cookie is not "mostly stored". The alternative — accept what we
//! can and ignore the rest — is how a `Secure` flag gets dropped silently.
//!
//! # What is not here yet
//!
//! No `Expires` parsing: it needs a date parser for three legacy formats, and
//! `Max-Age` covers the same ground with unambiguous syntax. A cookie carrying
//! only `Expires` is treated as a session cookie, which is the safe direction —
//! it is forgotten sooner than asked, never later.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::partition::{Origin, PartitionKey};
use crate::psl::PublicSuffixList;

/// Longest `Set-Cookie` header accepted, in bytes.
const MAX_COOKIE_BYTES: usize = 4096;

/// Most cookies stored per partition key.
///
/// Per key, not per host: partitioning multiplies the number of jars, so a cap
/// that was reasonable unpartitioned is not the cap that matters now.
const MAX_PER_KEY: usize = 50;

/// How a cookie restricts cross-site sending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Never sent on a cross-site request.
    Strict,
    /// Sent on a top-level navigation only.
    Lax,
    /// Sent on any request. Requires `Secure`; a `SameSite=None` cookie
    /// without it is rejected, which is what the specification says and what
    /// stops the attribute being used to opt out of the default.
    None,
}

/// Why a `Set-Cookie` was not stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejected {
    /// The header was longer than [`MAX_COOKIE_BYTES`].
    TooLarge,
    /// There was no `name=value`.
    Malformed,
    /// An attribute was present but unparseable. Not ignored — see the module
    /// documentation.
    BadAttribute(&'static str),
    /// `Domain` named a public suffix. The supercookie.
    PublicSuffixDomain,
    /// `Domain` did not domain-match the origin that sent it.
    DomainMismatch,
    /// `SameSite=None` without `Secure`.
    InsecureSameSiteNone,
    /// A `__Secure-` or `__Host-` prefix whose requirements were not met.
    PrefixViolation(&'static str),
    /// The jar for this partition is full.
    JarFull,
}

impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "the Set-Cookie header is too large"),
            Self::Malformed => write!(f, "no name=value in the Set-Cookie header"),
            Self::BadAttribute(which) => write!(f, "unparseable {which} attribute"),
            Self::PublicSuffixDomain => {
                write!(f, "Domain names a public suffix; that is a supercookie")
            }
            Self::DomainMismatch => write!(f, "Domain does not match the origin that sent it"),
            Self::InsecureSameSiteNone => write!(f, "SameSite=None requires Secure"),
            Self::PrefixViolation(which) => write!(f, "{which}"),
            Self::JarFull => write!(f, "this partition already holds the maximum cookies"),
        }
    }
}

impl std::error::Error for Rejected {}

/// A stored cookie.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    /// The name, as sent.
    pub name: String,
    /// The value, as sent.
    pub value: String,
    /// The host this cookie is scoped to, lowercased.
    pub domain: String,
    /// Whether it covers subdomains of [`Cookie::domain`].
    pub host_only: bool,
    /// The path prefix it applies to.
    pub path: String,
    /// Sent only over TLS.
    pub secure: bool,
    /// Not readable from script. Stored so a later phase can honour it; this
    /// crate has no script to hide it from yet.
    pub http_only: bool,
    /// Cross-site sending rule.
    pub same_site: SameSite,
    /// When it expires, if it is not a session cookie.
    pub expires_at: Option<Instant>,
}

impl Cookie {
    /// Whether this cookie applies to `host`.
    ///
    /// Label-wise, not by string suffix: `notexample.com` must not match a
    /// cookie for `example.com`, which is the bug an `ends_with` produces.
    /// Public because it is a security rule, and a security rule that can only
    /// be tested through three other layers is one that gets tested loosely.
    pub fn matches_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        if host == self.domain {
            return true;
        }
        if self.host_only {
            return false;
        }
        host.strip_suffix(&self.domain)
            .and_then(|prefix| prefix.strip_suffix('.'))
            .is_some_and(|prefix| !prefix.is_empty())
    }

    /// Whether this cookie applies to `path`, per the RFC 6265 rule.
    pub fn matches_path(&self, path: &str) -> bool {
        if path == self.path {
            return true;
        }
        let Some(rest) = path.strip_prefix(&self.path) else {
            return false;
        };
        self.path.ends_with('/') || rest.starts_with('/')
    }

    fn is_expired(&self, now: Instant) -> bool {
        self.expires_at.is_some_and(|at| at <= now)
    }
}

/// Cookies, partitioned.
#[derive(Debug, Default)]
pub struct Jar {
    by_partition: HashMap<PartitionKey, Vec<Cookie>>,
}

impl Jar {
    /// An empty jar.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store a `Set-Cookie` received from `key`'s origin.
    ///
    /// The partition key is required, not optional. There is no unpartitioned
    /// store to call by accident — the same design rule as the connection
    /// pool.
    pub fn store(
        &mut self,
        psl: &PublicSuffixList,
        key: &PartitionKey,
        header: &str,
    ) -> Result<(), Rejected> {
        let cookie = parse(psl, key.origin(), header)?;

        let jar = self.by_partition.entry(key.clone()).or_default();
        // Replacing an existing cookie is not "adding one", so the cap is
        // checked after the replacement is accounted for.
        if let Some(slot) = jar.iter_mut().find(|stored| {
            stored.name == cookie.name
                && stored.domain == cookie.domain
                && stored.path == cookie.path
        }) {
            *slot = cookie;
            return Ok(());
        }
        if jar.len() >= MAX_PER_KEY {
            return Err(Rejected::JarFull);
        }
        jar.push(cookie);
        Ok(())
    }

    /// The cookies to send with a request to `key`'s origin at `path`.
    ///
    /// `cross_site` says whether this request is cross-site, which decides
    /// what `SameSite` permits. The caller knows; the jar cannot work it out,
    /// and guessing would make the attribute decorative.
    pub fn cookies_for(&self, key: &PartitionKey, path: &str, cross_site: bool) -> Vec<&Cookie> {
        let now = Instant::now();
        let origin = key.origin();
        let Some(jar) = self.by_partition.get(key) else {
            return Vec::new();
        };

        jar.iter()
            .filter(|cookie| !cookie.is_expired(now))
            .filter(|cookie| cookie.matches_host(origin.host()))
            .filter(|cookie| cookie.matches_path(path))
            // A Secure cookie never travels in plaintext. This is the rule
            // that stops a network attacker reading a session cookie by
            // forcing one plaintext request.
            .filter(|cookie| !cookie.secure || origin.is_secure())
            .filter(|cookie| match cookie.same_site {
                SameSite::None => true,
                SameSite::Lax | SameSite::Strict => !cross_site,
            })
            .collect()
    }

    /// Drop everything stored for one partition.
    pub fn forget(&mut self, key: &PartitionKey) {
        self.by_partition.remove(key);
    }

    /// How many cookies are stored in total.
    pub fn len(&self) -> usize {
        self.by_partition.values().map(Vec::len).sum()
    }

    /// Whether the jar holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop expired cookies.
    pub fn expire(&mut self) {
        let now = Instant::now();
        for jar in self.by_partition.values_mut() {
            jar.retain(|cookie| !cookie.is_expired(now));
        }
        self.by_partition.retain(|_, jar| !jar.is_empty());
    }
}

/// Parse one `Set-Cookie` value against the origin that sent it.
pub fn parse(psl: &PublicSuffixList, origin: &Origin, header: &str) -> Result<Cookie, Rejected> {
    if header.len() > MAX_COOKIE_BYTES {
        return Err(Rejected::TooLarge);
    }

    let mut parts = header.split(';');
    let pair = parts.next().unwrap_or_default();
    let (name, value) = pair.split_once('=').ok_or(Rejected::Malformed)?;
    let name = name.trim().to_owned();
    let value = value.trim().to_owned();
    if name.is_empty() {
        return Err(Rejected::Malformed);
    }

    let mut domain: Option<String> = None;
    let mut path: Option<String> = None;
    let mut secure = false;
    let mut http_only = false;
    let mut same_site = SameSite::Lax;
    let mut max_age: Option<i64> = None;

    for attribute in parts {
        let attribute = attribute.trim();
        if attribute.is_empty() {
            continue;
        }
        let (key, val) = match attribute.split_once('=') {
            Some((key, val)) => (key.trim().to_ascii_lowercase(), val.trim()),
            None => (attribute.to_ascii_lowercase(), ""),
        };

        match key.as_str() {
            "domain" => {
                let host = val
                    .trim_start_matches('.')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if host.is_empty() {
                    return Err(Rejected::BadAttribute("Domain"));
                }
                domain = Some(host);
            }
            "path" => {
                if !val.starts_with('/') {
                    return Err(Rejected::BadAttribute("Path"));
                }
                path = Some(val.to_owned());
            }
            "secure" => secure = true,
            "httponly" => http_only = true,
            "samesite" => {
                same_site = match val.to_ascii_lowercase().as_str() {
                    "strict" => SameSite::Strict,
                    "lax" => SameSite::Lax,
                    "none" => SameSite::None,
                    _ => return Err(Rejected::BadAttribute("SameSite")),
                }
            }
            "max-age" => {
                max_age = Some(val.parse().map_err(|_| Rejected::BadAttribute("Max-Age"))?);
            }
            // Expires is accepted and ignored; see the module docs. Unknown
            // attributes are ignored, which the RFC requires.
            _ => {}
        }
    }

    // SameSite=None is a request to be sent cross-site. Without Secure that is
    // a cookie readable by a network attacker on every third-party request.
    if same_site == SameSite::None && !secure {
        return Err(Rejected::InsecureSameSiteNone);
    }

    // Cookie name prefixes, which are the only integrity mechanism cookies
    // have. A browser that does not enforce them makes them worse than absent,
    // because a site relying on one believes it is protected.
    if name.starts_with("__Secure-") && !secure {
        return Err(Rejected::PrefixViolation(
            "a __Secure- cookie must set Secure",
        ));
    }
    if name.starts_with("__Host-") {
        if !secure {
            return Err(Rejected::PrefixViolation(
                "a __Host- cookie must set Secure",
            ));
        }
        if domain.is_some() {
            return Err(Rejected::PrefixViolation(
                "a __Host- cookie must not set Domain",
            ));
        }
        if path.as_deref() != Some("/") {
            return Err(Rejected::PrefixViolation(
                "a __Host- cookie must set Path=/",
            ));
        }
    }

    let host = origin.host().to_ascii_lowercase();
    let (domain, host_only) = match domain {
        Some(requested) => {
            // The supercookie check, against the real list. A cookie for a
            // public suffix would be readable and writable by every site under
            // it.
            if psl.registrable_domain(&requested).is_none() {
                return Err(Rejected::PublicSuffixDomain);
            }
            // And it must cover the host that sent it: a site may widen a
            // cookie to its own parent domain, never to somebody else's.
            if host != requested
                && !host
                    .strip_suffix(&requested)
                    .and_then(|prefix| prefix.strip_suffix('.'))
                    .is_some_and(|prefix| !prefix.is_empty())
            {
                return Err(Rejected::DomainMismatch);
            }
            (requested, false)
        }
        // No Domain attribute means the cookie is for exactly this host.
        None => (host, true),
    };

    let expires_at = match max_age {
        // A non-positive Max-Age is the documented way to delete a cookie, and
        // it is represented as one that has already expired rather than as a
        // special case the storage layer has to know about.
        Some(seconds) if seconds <= 0 => Some(Instant::now()),
        Some(seconds) => u64::try_from(seconds)
            .ok()
            .and_then(|seconds| Instant::now().checked_add(Duration::from_secs(seconds))),
        None => None,
    };

    Ok(Cookie {
        name,
        value,
        domain,
        host_only,
        path: path.unwrap_or_else(|| "/".to_owned()),
        secure,
        http_only,
        same_site,
        expires_at,
    })
}
