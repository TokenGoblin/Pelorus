//! Name resolution, and the choice of who answers (ADR 014).
//!
//! # The default is the system resolver, and that is deliberate
//!
//! ADR 014 settles it: the system resolver is preselected, DoH is offered, and
//! **no provider is compiled in as a default endpoint**. The reasoning is worth
//! repeating where the code is, because the default is not the obvious pick and
//! looks like an omission otherwise.
//!
//! Whoever answers a lookup learns every site the user visits, in order, with
//! timestamps. There is no way to load a page without telling somebody. The
//! system resolver is not the most private option and this module does not
//! claim it is — it is the only option that cannot be a *downgrade* for
//! anybody, because the user's DNS already goes there, for every other program
//! on the machine. Every alternative improves matters for a user on a hostile
//! ISP and worsens them for a user on a trusted network, and we cannot tell
//! which one is reading.
//!
//! Shipping with DoH pointed at one company would encrypt the lookup, hide it
//! from the ISP, and hand the complete browsing history of every user to a
//! party none of them chose. §9 Phase 3 calls that transferring the tracking
//! rather than eliminating it.
//!
//! # What DoH buys, and what it does not
//!
//! Confidentiality from the network, not authenticity from the resolver. The
//! resolver still sees every name, and a DoH answer is not validated — there is
//! no DNSSEC here (see [`crate::dns`]). Choosing DoH moves who can watch; it
//! does not remove the watcher.
//!
//! # Partitioning
//!
//! Invariant 2 lists the DNS cache among the things keyed by partition. A
//! shared cache is a cross-site oracle whichever resolver filled it: a site can
//! learn that a name is already cached, and therefore that the user visited
//! somewhere that uses it. [`Cache`] takes a [`PartitionKey`] and there is no
//! variant that does not.

use std::collections::HashMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::time::{Duration, Instant};

use crate::partition::PartitionKey;

/// How long a resolved name is reused.
///
/// Not the record's TTL. Honouring a server-chosen TTL means a resolver can
/// pin a name in this cache for as long as it likes, and a short fixed ceiling
/// is easier to reason about than a value an attacker picks. Revisit when
/// something needs the performance.
const CACHE_TTL: Duration = Duration::from_secs(60);

/// Most names cached per partition.
const MAX_PER_KEY: usize = 64;

/// Who answers a lookup.
///
/// No `Default` implementation, deliberately: ADR 014's decision is that the
/// choice is *made*, and a type that quietly defaults would let a caller skip
/// making it. [`Resolver::system`] is the preselected option and is spelled
/// out at every call site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolver {
    /// Whatever the operating system is configured to use.
    ///
    /// Adds no party the user is not already trusting. Usually the ISP, in
    /// plaintext — stated rather than glossed.
    System,
    /// A DoH endpoint the user selected, by URL.
    ///
    /// Holds the endpoint rather than a provider name, so a curated entry and
    /// a custom one are the same thing to this code. There is no "trusted
    /// provider" branch to get wrong.
    Doh {
        /// Host of the DoH endpoint.
        host: String,
        /// Path, conventionally `/dns-query`.
        path: String,
    },
}

impl Resolver {
    /// The preselected option (ADR 014).
    pub fn system() -> Self {
        Self::System
    }

    /// Whether this resolver sends lookups over TLS to a chosen third party.
    pub fn is_encrypted(&self) -> bool {
        matches!(self, Self::Doh { .. })
    }
}

/// A resolver the user may pick, as offered in settings.
///
/// Data rather than code: ADR 014 keeps the curated list in `data/` so a
/// provider that turns out to be untrustworthy can be dropped without a
/// release. Inclusion is not endorsement, and [`Provider::policy`] is what the
/// provider *publishes* rather than anything this project verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// Display name.
    pub name: String,
    /// Endpoint host.
    pub host: String,
    /// Endpoint path.
    pub path: String,
    /// Where the operator is, which is what decides whose law reaches the logs.
    pub jurisdiction: String,
    /// What the operator's published policy says it retains. A claim.
    pub policy: String,
}

/// Parse the shipped provider list.
///
/// Four tab-separated fields per line, `#` for comments. A malformed line is
/// skipped rather than failing the whole list: a provider this cannot read
/// should be absent from the menu, not prevent the menu from existing.
pub fn parse_providers(text: &str) -> Vec<Provider> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').map(str::trim).collect();
        let (Some(name), Some(endpoint), Some(jurisdiction), Some(policy)) =
            (fields.first(), fields.get(1), fields.get(2), fields.get(3))
        else {
            continue;
        };
        let Some((host, path)) = endpoint.split_once('/') else {
            continue;
        };
        if name.is_empty() || host.is_empty() {
            continue;
        }
        out.push(Provider {
            name: (*name).to_owned(),
            host: host.to_owned(),
            path: format!("/{path}"),
            jurisdiction: (*jurisdiction).to_owned(),
            policy: (*policy).to_owned(),
        });
    }
    out
}

/// Why a lookup failed.
#[derive(Debug)]
pub enum ResolveError {
    /// The name did not resolve.
    NotFound,
    /// The name could not be used — not ASCII, malformed, too long.
    BadName(&'static str),
    /// The system resolver failed.
    System(std::io::Error),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "the name did not resolve"),
            Self::BadName(why) => write!(f, "the name cannot be looked up: {why}"),
            Self::System(error) => write!(f, "the system resolver failed: {error}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// A partitioned cache of resolved names.
///
/// Keyed by `(partition, name)`. Invariant 2 lists the DNS cache explicitly,
/// and the reason is an oracle rather than a leak of content: a shared cache
/// lets a site learn that a name is already resolved, and therefore that the
/// user has been somewhere that uses it.
#[derive(Debug, Default)]
pub struct Cache {
    entries: HashMap<(PartitionKey, String), (Vec<IpAddr>, Instant)>,
}

impl Cache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look a name up in this partition, if it is cached and fresh.
    pub fn get(&self, key: &PartitionKey, name: &str) -> Option<&[IpAddr]> {
        let entry = self
            .entries
            .get(&(key.clone(), name.to_ascii_lowercase()))?;
        if entry.1 <= Instant::now() {
            return None;
        }
        Some(&entry.0)
    }

    /// Record a resolution in this partition.
    pub fn put(&mut self, key: &PartitionKey, name: &str, addresses: Vec<IpAddr>) {
        let partition = key.clone();
        let count = self
            .entries
            .keys()
            .filter(|(stored, _)| stored == &partition)
            .count();
        if count >= MAX_PER_KEY {
            return;
        }
        let expiry = Instant::now()
            .checked_add(CACHE_TTL)
            .unwrap_or_else(Instant::now);
        self.entries
            .insert((partition, name.to_ascii_lowercase()), (addresses, expiry));
    }

    /// Drop everything cached for one partition.
    pub fn forget(&mut self, key: &PartitionKey) {
        self.entries.retain(|(stored, _), _| stored != key);
    }

    /// How many entries are cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Resolve `name` using the system resolver.
///
/// The DoH path is not wired to the network here. Sending a DoH query means
/// making an HTTPS request, and an HTTPS request needs a partition key, which
/// needs the resolver — so the two have a cycle that wants breaking
/// deliberately rather than by whichever call site is written first. The
/// message encoding and parsing are done and tested (`crate::dns`); what is
/// missing is the decision about which partition a DoH lookup belongs to, and
/// that is worth its own thought rather than an answer smuggled in here.
pub fn resolve_system(name: &str, port: u16) -> Result<Vec<IpAddr>, ResolveError> {
    if name.is_empty() {
        return Err(ResolveError::BadName("empty"));
    }
    if !name.is_ascii() {
        return Err(ResolveError::BadName(
            "not ASCII; the IDNA step was skipped",
        ));
    }

    let addresses: Vec<IpAddr> = (name, port)
        .to_socket_addrs()
        .map_err(ResolveError::System)?
        .map(|socket| socket.ip())
        .collect();

    if addresses.is_empty() {
        return Err(ResolveError::NotFound);
    }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preselected_resolver_is_the_system_one() {
        assert_eq!(Resolver::system(), Resolver::System);
        assert!(
            !Resolver::system().is_encrypted(),
            "the default sends plaintext DNS through the OS, and ADR 014 says \
             so rather than implying otherwise"
        );
    }

    #[test]
    fn a_doh_resolver_is_encrypted() {
        let doh = Resolver::Doh {
            host: "example.net".to_owned(),
            path: "/dns-query".to_owned(),
        };
        assert!(doh.is_encrypted());
    }

    #[test]
    fn providers_parse_from_the_shipped_form() {
        let text = "# comment\n\
                    Example\texample.net/dns-query\tSomewhere\tSays it keeps nothing\n\
                    Broken line without tabs\n\
                    Other\tother.example/q\tElsewhere\tSays it keeps 24h\n";
        let providers = parse_providers(text);
        assert_eq!(providers.len(), 2, "a malformed line is skipped, not fatal");
        assert_eq!(
            providers.first().map(|p| p.host.as_str()),
            Some("example.net")
        );
        assert_eq!(
            providers.first().map(|p| p.path.as_str()),
            Some("/dns-query")
        );
    }

    #[test]
    fn a_unicode_name_is_refused_rather_than_looked_up() {
        assert!(matches!(
            resolve_system("exämple.com", 443),
            Err(ResolveError::BadName(_))
        ));
    }

    #[test]
    fn loopback_resolves() {
        let addresses = resolve_system("localhost", 80).expect("localhost must resolve");
        assert!(addresses.iter().any(|address| address.is_loopback()));
    }
}
