//! The HSTS preload list: hosts that must never be reached over plaintext.
//!
//! # What the preload list is actually for
//!
//! A `Strict-Transport-Security` header only protects the *second* visit. The
//! first request to a host goes out before any header has been seen, and if it
//! goes out over plaintext it is interceptable — the sslstrip position, where
//! an attacker on the path rewrites the response and the user never sees a
//! padlock they were not shown.
//!
//! The preload list is the only thing that closes that window. It is the set
//! of hosts a browser knows, before contacting them at all, that plaintext is
//! never correct for.
//!
//! # Refusing rather than upgrading
//!
//! HSTS is specified as an *upgrade*: rewrite `http://` to `https://` and
//! proceed. This module reports, and [`crate::fetch`] refuses. Upgrading inside
//! the fetch would change the origin — scheme and port are both part of it —
//! and therefore change the partition the request belongs to, after the caller
//! had already built a key for the other one. A request that silently moves
//! between partitions is worse than one that fails loudly.
//!
//! So the caller consults [`PreloadList::requires_https`] *before* building the
//! key, and [`PreloadList::upgrade`] gives it the origin it should have used.
//! Same outcome, with the partition decision made where it can be seen.
//!
//! # Matching rules
//!
//! An entry is either an exact host, or — written with a leading `.` — a host
//! and everything under it. `example.com` matches only `example.com`;
//! `.example.com` matches `example.com`, `a.example.com` and `a.b.example.com`.
//! Nothing matches across a label boundary: `notexample.com` is not covered by
//! `.example.com`, which is the bug a naive suffix comparison produces.

use std::collections::HashMap;

use crate::partition::Origin;

/// The preloaded hosts.
#[derive(Debug, Default)]
pub struct PreloadList {
    /// Host to "does it cover subdomains". A host present under both forms
    /// upstream resolves to covering them, which is the safer reading.
    hosts: HashMap<String, bool>,
}

/// Why a list could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreloadError {
    /// The list held no entries. An empty list is not a permissive one, it is
    /// a broken one — and it would silently remove first-visit protection from
    /// every host at once.
    Empty,
}

impl std::fmt::Display for PreloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the HSTS preload list contained no entries"),
        }
    }
}

impl std::error::Error for PreloadError {}

impl PreloadList {
    /// Parse the shipped form: one host per line, `.` prefix for subdomains.
    pub fn parse(text: &str) -> Result<Self, PreloadError> {
        let mut hosts: HashMap<String, bool> = HashMap::new();

        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (host, subdomains) = match line.strip_prefix('.') {
                Some(rest) => (rest, true),
                None => (line, false),
            };
            if host.is_empty() {
                continue;
            }
            let key = host.to_ascii_lowercase();
            // If a host appears both ways, subdomain coverage wins. Widening
            // protection on ambiguous data is the safe direction; narrowing it
            // would silently drop hosts.
            hosts
                .entry(key)
                .and_modify(|existing| *existing = *existing || subdomains)
                .or_insert(subdomains);
        }

        if hosts.is_empty() {
            return Err(PreloadError::Empty);
        }
        Ok(Self { hosts })
    }

    /// How many hosts are preloaded.
    pub fn len(&self) -> usize {
        self.hosts.len()
    }

    /// Whether the list is empty. Never true for a list from [`Self::parse`].
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }

    /// Whether `host` must be reached over HTTPS.
    ///
    /// Walks the host upward one label at a time. The exact host matches
    /// whichever form it was listed as; a parent matches only if that parent
    /// covers subdomains. Stepping label by label rather than comparing
    /// suffixes is what stops `notexample.com` matching `.example.com`.
    pub fn requires_https(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() {
            return false;
        }

        if let Some(_exact) = self.hosts.get(&host) {
            return true;
        }

        let mut rest = host.as_str();
        while let Some((_label, parent)) = rest.split_once('.') {
            if parent.is_empty() {
                break;
            }
            if self.hosts.get(parent).copied().unwrap_or(false) {
                return true;
            }
            rest = parent;
        }
        false
    }

    /// The origin the caller should have used, if this one is preloaded and
    /// plaintext.
    ///
    /// Returns `None` when the origin is already secure or not preloaded —
    /// that is, when there is nothing to correct.
    pub fn upgrade(&self, origin: &Origin) -> Option<Origin> {
        if origin.is_secure() || !self.requires_https(origin.host()) {
            return None;
        }
        // Port 80 becomes 443; a non-default port is kept, because a host
        // deliberately served on 8080 is not served on 8443 by implication.
        let port = if origin.port() == 80 {
            443
        } else {
            origin.port()
        };
        Origin::new("https", origin.host(), port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> Option<PreloadList> {
        PreloadList::parse(".example.com\nexact.test\n.co.uk\n").ok()
    }

    #[test]
    fn an_exact_entry_matches_only_itself() {
        let list = list().expect("a valid list");
        assert!(list.requires_https("exact.test"));
        assert!(
            !list.requires_https("sub.exact.test"),
            "an entry without a leading dot must not cover subdomains"
        );
    }

    #[test]
    fn a_subdomain_entry_covers_the_host_and_everything_under_it() {
        let list = list().expect("a valid list");
        assert!(list.requires_https("example.com"));
        assert!(list.requires_https("a.example.com"));
        assert!(list.requires_https("a.b.c.example.com"));
    }

    /// The bug a suffix comparison produces, and the reason this walks labels.
    #[test]
    fn matching_does_not_cross_a_label_boundary() {
        let list = list().expect("a valid list");
        assert!(
            !list.requires_https("notexample.com"),
            "'notexample.com' ends with 'example.com' as a string and is a \
             different site; a suffix comparison would wrongly cover it"
        );
        assert!(!list.requires_https("example.com.evil.test"));
    }

    #[test]
    fn an_unlisted_host_is_not_preloaded() {
        let list = list().expect("a valid list");
        assert!(!list.requires_https("example.org"));
        assert!(!list.requires_https(""));
    }

    #[test]
    fn case_and_a_trailing_dot_do_not_change_the_answer() {
        let list = list().expect("a valid list");
        assert!(list.requires_https("A.Example.COM."));
    }

    #[test]
    fn an_empty_list_is_refused_rather_than_accepted() {
        assert!(PreloadList::parse("# only a comment").is_err());
    }

    #[test]
    fn upgrading_rewrites_the_scheme_and_the_default_port_only() {
        let list = list().expect("a valid list");

        let plain = Origin::new("http", "a.example.com", 80).expect("origin");
        let upgraded = list.upgrade(&plain).expect("a preloaded host upgrades");
        assert_eq!(upgraded.scheme(), "https");
        assert_eq!(upgraded.port(), 443);

        // A deliberate non-default port is kept: a host served on 8080 is not
        // served on 8443 by implication.
        let odd = Origin::new("http", "a.example.com", 8080).expect("origin");
        let upgraded = list.upgrade(&odd).expect("still preloaded");
        assert_eq!(upgraded.port(), 8080);

        // Nothing to correct.
        let secure = Origin::new("https", "a.example.com", 443).expect("origin");
        assert_eq!(list.upgrade(&secure), None);
        let unlisted = Origin::new("http", "example.org", 80).expect("origin");
        assert_eq!(list.upgrade(&unlisted), None);
    }
}
