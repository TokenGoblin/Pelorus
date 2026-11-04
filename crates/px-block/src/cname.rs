//! CNAME-cloaking decision.
//!
//! A tracker hides behind a first-party subdomain (`metrics.shop.example`)
//! whose DNS record is a CNAME to the tracker's host. A blocklist keyed on
//! hostnames never sees the tracker's name in the request URL. The *DNS*
//! part -- resolving and recording the chain -- is px-net's; this module is
//! the decision: given the request and the chain the resolver reported, would
//! the same request to any host in the chain be blocked?
//!
//! # Rules
//!
//! - Each chain element is re-checked as if the request had been sent to that
//!   host (same scheme, port, path, query, resource type), with third-party
//!   status recomputed for *that* host against the page, so a first-party
//!   alias pointing at a third-party tracker is judged as third-party.
//! - If any element is blocked the verdict is [`CnameVerdict::Cloaked`]
//!   (lowest hop wins); a block on a later hop is not excused by an
//!   exception on an earlier one. Otherwise an exception match yields
//!   [`CnameVerdict::Excepted`], otherwise [`CnameVerdict::Clean`].
//! - Fail closed: a chain longer than [`MAX_CNAME_CHAIN`] or containing a name
//!   that is not a plain hostname is not examined further and the verdict
//!   says to block ([`CnameVerdict::should_block`]).
//! - The caller combines this with the verdict for the original URL. An
//!   exception for the original host that the user wrote on purpose
//!   (`@@||metrics.shop.example^`) is the caller's to honour; this function
//!   does not look at the original host's rules.

use crate::matcher::{Matcher, Request, RuleId, Target, Verdict, source_host};
use px_websec::origin::{SiteResolver, Url};

/// Longest chain examined; longer is treated as hostile.
pub const MAX_CNAME_CHAIN: usize = 16;

/// Outcome of examining a CNAME chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CnameVerdict {
    /// No chain element is blocked.
    Clean,
    /// Chain element `hop` (0-based) is blocked by `rule_id`.
    Cloaked {
        /// Index into the supplied chain.
        hop: usize,
        /// The blocking rule.
        rule_id: RuleId,
    },
    /// No element is blocked and element `hop` matched an exception.
    Excepted {
        /// Index into the supplied chain.
        hop: usize,
        /// The exception rule.
        rule_id: RuleId,
    },
    /// An element is not a valid hostname. Fail closed.
    Malformed {
        /// Index into the supplied chain.
        hop: usize,
    },
    /// More than [`MAX_CNAME_CHAIN`] elements. Fail closed.
    ChainTooLong,
}

impl CnameVerdict {
    /// Whether the request must not be sent.
    pub fn should_block(&self) -> bool {
        matches!(
            self,
            Self::Cloaked { .. } | Self::Malformed { .. } | Self::ChainTooLong
        )
    }
}

/// Normalise a DNS name: one trailing dot dropped, lower case; `None` unless
/// it is a plain hostname (letters, digits, `-`, `_`, `.`; no empty label;
/// at most 253 bytes).
fn normalise_host(name: &str) -> Option<String> {
    let n = name.strip_suffix('.').unwrap_or(name);
    let ok = !n.is_empty()
        && n.len() <= 253
        && n.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        });
    ok.then(|| n.to_ascii_lowercase())
}

/// Decide whether the CNAME `chain` (the names the resolver followed from the
/// request's host, excluding or including the host itself; an element equal
/// to the request's own host is skipped) cloaks a blocked host.
pub fn check_cname_chain(
    matcher: &Matcher,
    req: &Request<'_>,
    chain: &[&str],
    resolver: &dyn SiteResolver,
) -> CnameVerdict {
    if chain.len() > MAX_CNAME_CHAIN {
        return CnameVerdict::ChainTooLong;
    }
    let own_host = req.url.host().map(px_websec::origin::Host::serialize);
    let src_host = source_host(req.source_origin);
    let src_site = src_host.as_deref().map(|h| {
        resolver
            .registrable_domain(h)
            .unwrap_or_else(|| h.to_string())
    });

    let mut first_exception: Option<(usize, RuleId)> = None;
    for (hop, raw) in chain.iter().enumerate() {
        let Some(host) = normalise_host(raw) else {
            return CnameVerdict::Malformed { hop };
        };
        if own_host.as_deref() == Some(host.as_str()) {
            continue;
        }
        let Some(target) = Target::with_host(req.url, &host) else {
            continue;
        };
        let hop_site = resolver
            .registrable_domain(&host)
            .unwrap_or_else(|| host.clone());
        let third_party = match (&req.source_origin, &src_site) {
            (None, _) => false,
            (Some(o), _) if o.is_opaque() => true,
            (Some(_), Some(s)) => *s != hop_site,
            (Some(_), None) => true,
        };
        match matcher.check_target(&target, src_host.as_deref(), req.resource_type, third_party) {
            Verdict::Block { rule_id } => return CnameVerdict::Cloaked { hop, rule_id },
            Verdict::AllowedByException { rule_id } => {
                first_exception.get_or_insert((hop, rule_id));
            }
            Verdict::Allow => {}
        }
    }
    match first_exception {
        Some((hop, rule_id)) => CnameVerdict::Excepted { hop, rule_id },
        None => CnameVerdict::Clean,
    }
}

/// The URL a hop corresponds to, for diagnostics and tests.
pub fn url_for_hop(url: &Url, hop_host: &str) -> Option<String> {
    let host = normalise_host(hop_host)?;
    Target::with_host(url, &host)?;
    let mut u = url.serialize(true);
    let own = url.host()?.serialize();
    u = u.replacen(&own, &host, 1);
    Some(u)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::{ResourceType, parse_list};
    use crate::matcher::is_third_party;
    use px_websec::origin::Origin;

    fn psl(host: &str) -> Option<String> {
        let labels: Vec<&str> = host.split('.').collect();
        if labels.len() < 2 {
            return None;
        }
        Some(labels[labels.len() - 2..].join("."))
    }

    fn run(list: &str, page: &str, url: &str, chain: &[&str]) -> CnameVerdict {
        let m = Matcher::from_list("t", parse_list(list));
        let u = Url::parse(url).unwrap();
        let src = Origin::from_url(&Url::parse(page).unwrap());
        let req = Request {
            url: &u,
            source_origin: Some(&src),
            resource_type: ResourceType::Script,
            is_third_party: is_third_party(&u, Some(&src), &psl),
        };
        check_cname_chain(&m, &req, chain, &psl)
    }

    const PAGE: &str = "https://shop.example/";
    const URL: &str = "https://metrics.shop.example/t.js";

    #[test]
    fn alias_to_listed_tracker_is_cloaked() {
        let v = run(
            "||tracker.net^\n",
            PAGE,
            URL,
            &["metrics.shop.example", "edge.tracker.net."],
        );
        assert_eq!(
            v,
            CnameVerdict::Cloaked {
                hop: 1,
                rule_id: RuleId(0)
            }
        );
        assert!(v.should_block());
    }

    #[test]
    fn clean_chain() {
        let v = run(
            "||tracker.net^\n",
            PAGE,
            URL,
            &["cdn.provider.org", "x.provider.org"],
        );
        assert_eq!(v, CnameVerdict::Clean);
        assert!(!v.should_block());
        assert_eq!(run("||tracker.net^\n", PAGE, URL, &[]), CnameVerdict::Clean);
    }

    #[test]
    fn third_party_status_is_recomputed_per_hop() {
        // The request itself is first-party; the alias target is third-party,
        // so a $third-party rule on the target applies.
        let v = run(
            "||tracker.net^$third-party\n",
            PAGE,
            URL,
            &["a.tracker.net"],
        );
        assert!(matches!(v, CnameVerdict::Cloaked { hop: 0, .. }));
        // An alias into the page's own site is first-party: not blocked.
        let v = run(
            "||static.shop.example^$third-party\n",
            PAGE,
            URL,
            &["static.shop.example"],
        );
        assert_eq!(v, CnameVerdict::Clean);
    }

    #[test]
    fn exceptions() {
        let list = "||tracker.net^\n@@||ok.tracker.net^\n";
        let v = run(list, PAGE, URL, &["ok.tracker.net"]);
        assert_eq!(
            v,
            CnameVerdict::Excepted {
                hop: 0,
                rule_id: RuleId(1)
            }
        );
        assert!(!v.should_block());
        // A later block is not excused by an earlier exception.
        let v = run(list, PAGE, URL, &["ok.tracker.net", "bad.tracker.net"]);
        assert_eq!(
            v,
            CnameVerdict::Cloaked {
                hop: 1,
                rule_id: RuleId(0)
            }
        );
    }

    #[test]
    fn type_path_and_query_carry_over() {
        let v = run(
            "||tracker.net/t.js^$script\n",
            PAGE,
            URL,
            &["x.tracker.net"],
        );
        assert!(matches!(v, CnameVerdict::Cloaked { .. }));
        let v = run("||tracker.net/other.js^\n", PAGE, URL, &["x.tracker.net"]);
        assert_eq!(v, CnameVerdict::Clean);
        let v = run("||tracker.net^$image\n", PAGE, URL, &["x.tracker.net"]);
        assert_eq!(v, CnameVerdict::Clean);
    }

    #[test]
    fn fail_closed_on_hostile_chains() {
        let long: Vec<&str> = vec!["a.example"; MAX_CNAME_CHAIN + 1];
        let v = run("||tracker.net^\n", PAGE, URL, &long);
        assert_eq!(v, CnameVerdict::ChainTooLong);
        assert!(v.should_block());
        let at: Vec<&str> = vec!["a.example"; MAX_CNAME_CHAIN];
        assert_eq!(run("||tracker.net^\n", PAGE, URL, &at), CnameVerdict::Clean);
        for bad in [
            "",
            "a b.net",
            "a..net",
            "a.net/x",
            "tracker.net@x",
            "a.net:80",
            "\0",
            "-.",
            ".net",
        ] {
            let v = run("||tracker.net^\n", PAGE, URL, &["ok.example", bad]);
            assert_eq!(v, CnameVerdict::Malformed { hop: 1 }, "{bad:?}");
            assert!(v.should_block());
        }
        let huge = format!("{}.net", "a".repeat(300));
        assert!(run("||tracker.net^\n", PAGE, URL, &[&huge]).should_block());
    }

    #[test]
    fn name_normalisation() {
        let v = run("||tracker.net^\n", PAGE, URL, &["EDGE.Tracker.NET."]);
        assert!(matches!(v, CnameVerdict::Cloaked { hop: 0, .. }));
        // Only one trailing dot is tolerated.
        assert!(matches!(
            run("||tracker.net^\n", PAGE, URL, &["edge.tracker.net.."]),
            CnameVerdict::Malformed { hop: 0 }
        ));
    }

    #[test]
    fn hop_url_helper() {
        let u = Url::parse(URL).unwrap();
        assert_eq!(
            url_for_hop(&u, "Edge.Tracker.net.").as_deref(),
            Some("https://edge.tracker.net/t.js")
        );
        assert_eq!(url_for_hop(&u, "a b"), None);
    }
}
