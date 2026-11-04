//! First-party isolation: which storage partition a request would get.
//!
//! This module expresses a *decision*; it holds no state. The state logic is
//! px-net's `PartitionKey` (`(eTLD+1 of the top-level site, origin)`), and
//! the integration builds that from the [`PartitionKeyParts`] returned here.
//! The two must agree, so the construction mirrors px-net's rules:
//!
//! - the key's site component is the eTLD+1 of the *top-level document*,
//!   never of the requested origin. The same tracker embedded on two sites
//!   therefore lands in two disjoint partitions, and nothing it stores on one
//!   is visible on the other. A third party never gets a partition of its own
//!   that spans sites;
//! - a top-level host with no registrable domain (IP literal, bare public
//!   suffix) yields [`Isolation::Unpartitionable`]: the request cannot be
//!   made. There is no unpartitioned fallback;
//! - a *blocked* request touches no storage at all ([`Isolation::Blocked`]):
//!   not read, not written, not created;
//! - an exception (`@@`) permits the request; it does not merge or widen the
//!   partition. The key is identical to the unfiltered case.

use crate::matcher::{RuleId, Verdict};
use px_websec::origin::{Origin, SiteResolver};

/// The components of a px-net partition key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionKeyParts {
    /// eTLD+1 of the top-level document.
    pub top_level_site: String,
    /// The origin being requested.
    pub origin: Origin,
}

/// The storage decision for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Isolation {
    /// The request is blocked; it gets no storage of any kind.
    Blocked {
        /// The blocking rule.
        rule_id: RuleId,
    },
    /// The request may proceed, with all state under this key.
    Partitioned {
        /// The key components.
        key: PartitionKeyParts,
        /// The requested origin is a different site from the top level.
        third_party: bool,
    },
    /// No registrable domain for the top level: the request cannot be made.
    Unpartitionable,
}

/// Decide the partition for a request made on behalf of the page at
/// `top_level_host` (lower-case ASCII host) to `request_origin`, given the
/// filtering `verdict`.
pub fn storage_isolation(
    top_level_host: &str,
    request_origin: &Origin,
    verdict: &Verdict,
    resolver: &dyn SiteResolver,
) -> Isolation {
    if let Verdict::Block { rule_id } = verdict {
        return Isolation::Blocked { rule_id: *rule_id };
    }
    let Some(top_level_site) = resolver.registrable_domain(top_level_host) else {
        return Isolation::Unpartitionable;
    };
    let req_site = request_origin.host().and_then(|h| match h {
        px_websec::origin::Host::Domain(d) => resolver.registrable_domain(d),
        _ => None,
    });
    let third_party = req_site.as_deref() != Some(top_level_site.as_str());
    Isolation::Partitioned {
        key: PartitionKeyParts {
            top_level_site,
            origin: request_origin.clone(),
        },
        third_party,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_websec::origin::Url;

    fn psl(host: &str) -> Option<String> {
        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2 {
            return None;
        }
        Some(labels[labels.len() - 2..].join("."))
    }

    fn o(s: &str) -> Origin {
        Origin::from_url(&Url::parse(s).unwrap())
    }

    #[test]
    fn same_tracker_on_two_sites_gets_two_partitions() {
        let tracker = o("https://px.tracker.net/");
        let a = storage_isolation("news.a.com", &tracker, &Verdict::Allow, &psl);
        let b = storage_isolation("b.org", &tracker, &Verdict::Allow, &psl);
        let (
            Isolation::Partitioned {
                key: ka,
                third_party: ta,
            },
            Isolation::Partitioned {
                key: kb,
                third_party: tb,
            },
        ) = (a, b)
        else {
            panic!("expected partitions");
        };
        assert_eq!(ka.top_level_site, "a.com");
        assert_eq!(kb.top_level_site, "b.org");
        assert_ne!(ka, kb);
        assert_eq!(ka.origin, kb.origin);
        assert!(ta && tb);
    }

    #[test]
    fn first_party_is_flagged_and_keyed_by_top_level() {
        let own = o("https://static.a.com/");
        let Isolation::Partitioned { key, third_party } =
            storage_isolation("www.a.com", &own, &Verdict::Allow, &psl)
        else {
            panic!("expected partition");
        };
        assert!(!third_party);
        assert_eq!(key.top_level_site, "a.com");
        assert_eq!(key.origin, own);
    }

    #[test]
    fn blocked_requests_get_no_storage() {
        let v = Verdict::Block { rule_id: RuleId(7) };
        assert_eq!(
            storage_isolation("a.com", &o("https://t.net/"), &v, &psl),
            Isolation::Blocked { rule_id: RuleId(7) }
        );
        // Even when the top level could not be partitioned: blocked wins.
        assert_eq!(
            storage_isolation("localhost", &o("https://t.net/"), &v, &psl),
            Isolation::Blocked { rule_id: RuleId(7) }
        );
    }

    #[test]
    fn exception_does_not_change_the_key() {
        let t = o("https://t.net/");
        let allow = storage_isolation("a.com", &t, &Verdict::Allow, &psl);
        let exc = storage_isolation(
            "a.com",
            &t,
            &Verdict::AllowedByException { rule_id: RuleId(1) },
            &psl,
        );
        assert_eq!(allow, exc);
    }

    #[test]
    fn no_registrable_domain_means_no_request() {
        let t = o("https://t.net/");
        assert_eq!(
            storage_isolation("localhost", &t, &Verdict::Allow, &psl),
            Isolation::Unpartitionable
        );
        assert_eq!(
            storage_isolation("", &t, &Verdict::Allow, &psl),
            Isolation::Unpartitionable
        );
    }

    #[test]
    fn ip_literal_request_origin_is_third_party() {
        let ip = o("https://10.0.0.1/");
        let Isolation::Partitioned { third_party, .. } =
            storage_isolation("a.com", &ip, &Verdict::Allow, &psl)
        else {
            panic!("expected partition");
        };
        assert!(third_party);
    }
}
