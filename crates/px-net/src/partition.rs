//! Partition keys, and the reason nothing here has a constructor that skips one.
//!
//! # Invariant 2, as a type
//!
//! > Cookies, cache, storage, connection pools, DNS cache, and TLS sessions are
//! > keyed by `(eTLD+1 of top-level, origin)`. **There is no unpartitioned code
//! > path to leave enabled by accident.**
//!
//! The second sentence is the hard part, and it is a design problem rather than
//! a discipline problem. Every browser that partitioned state late did it by
//! adding a key parameter to functions that already worked without one, which
//! leaves the old path compiling. Somebody then calls it — not maliciously,
//! just because it is shorter — and the partition silently does not apply.
//!
//! So [`PartitionKey`] is the only way to name a destination in this crate.
//! There is no `connect(host)`, no `Default`, and no way to build one that
//! does not go through the public suffix list. A caller that has not decided
//! which top-level site it is acting for cannot express a request at all.
//!
//! # Double-keying, and why the top-level site is the outer key
//!
//! The key is a pair: the site the user is *looking at*, and the origin being
//! *talked to*. `evil.example` embedding an image from `cdn.example` gets a
//! different connection, a different DNS cache entry and a different TLS
//! session than `news.example` embedding the same image from the same CDN.
//!
//! Without the outer key, that shared CDN connection is a cross-site
//! identifier: the CDN sees one connection reused across both, which is
//! exactly the linkage partitioning exists to break. It also removes a timing
//! oracle — whether a connection is already warm otherwise tells a site
//! whether the user has visited another.

use crate::psl::PublicSuffixList;

/// A scheme, host and port — the thing a connection is made *to*.
///
/// Ports are explicit rather than optional. A `None` port meaning "the default
/// for the scheme" is a second representation of the same origin, and two
/// representations of one key is how a cache ends up with two entries that
/// should have been one.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin {
    /// Lowercased scheme, without `://`.
    scheme: String,
    /// Lowercased host, in Punycode. Never Unicode — see [`PartitionKey`].
    host: String,
    /// Port, always explicit.
    port: u16,
}

impl Origin {
    /// Build an origin, normalising case and refusing what cannot be one.
    ///
    /// Returns `None` rather than guessing. An origin this crate cannot
    /// represent is a request that does not get made, which is invariant 8
    /// applied to a smaller thing.
    pub fn new(scheme: &str, host: &str, port: u16) -> Option<Self> {
        let scheme = scheme.trim().to_ascii_lowercase();
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();

        if scheme.is_empty() || host.is_empty() {
            return None;
        }
        // Only the two schemes this crate fetches. A partition key naming
        // `file` or `data` would be a category error: they have no network
        // origin to key on.
        if scheme != "https" && scheme != "http" {
            return None;
        }
        if host.contains("..") || host.starts_with('.') {
            return None;
        }
        // A Unicode host here means somebody skipped the IDNA step, and
        // comparing Unicode against an ASCII suffix list is how confusable
        // hosts end up in the wrong partition.
        if !host.is_ascii() {
            return None;
        }
        Some(Self { scheme, host, port })
    }

    /// The scheme, lowercased.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// The host, lowercased and in Punycode.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Whether this origin is carried over TLS.
    pub fn is_secure(&self) -> bool {
        self.scheme == "https"
    }
}

impl std::fmt::Display for Origin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}

/// `(eTLD+1 of the top-level site, origin)` — the key everything is stored
/// under.
///
/// Constructed only through [`PartitionKey::new`], which consults the public
/// suffix list. There is deliberately no way to assemble one from parts: a
/// constructor taking a pre-computed top-level site would let a caller pass
/// the wrong thing, and "the wrong thing" here means two sites sharing a
/// partition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PartitionKey {
    /// eTLD+1 of the top-level document — the *site*, not the origin. Scheme
    /// and port are deliberately not part of it: `http://example.com` and
    /// `https://example.com` are one site for partitioning, which is what
    /// makes an http page unable to escape into an https partition.
    top_level_site: String,
    /// The origin being connected to.
    origin: Origin,
}

impl PartitionKey {
    /// Derive a key for `origin`, loaded on behalf of the page at
    /// `top_level_host`.
    ///
    /// Returns `None` when the top-level host has no registrable domain — an
    /// IP literal, a bare public suffix, something malformed. **Every caller
    /// must treat that as "this request cannot be made"** rather than falling
    /// back to an unpartitioned path. The fallback is the hole; there is no
    /// API here that offers one.
    pub fn new(psl: &PublicSuffixList, top_level_host: &str, origin: Origin) -> Option<Self> {
        let top_level_site = psl.registrable_domain(top_level_host)?;
        Some(Self {
            top_level_site,
            origin,
        })
    }

    /// The eTLD+1 of the page this request is being made for.
    pub fn top_level_site(&self) -> &str {
        &self.top_level_site
    }

    /// The origin being connected to.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// Whether two keys are the same partition regardless of destination.
    ///
    /// For the storage layers that partition by top-level site alone. Not used
    /// by the pool, which needs the full pair.
    pub fn same_top_level(&self, other: &Self) -> bool {
        self.top_level_site == other.top_level_site
    }
}

impl std::fmt::Display for PartitionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{} | {}]", self.top_level_site, self.origin)
    }
}
