//! The rule index and `check_request`.
//!
//! # Design
//!
//! Each rule is filed under at most one *index token*: the rarest valid token
//! of its pattern (see [`Pattern::index_tokens`]), where rarity is the number
//! of rules in the whole matcher that could use that token, ties broken by
//! longer token then lexicographic order. Buckets live in a
//! `BTreeMap<Box<str>, Vec<u32>>`: lookups are `O(log buckets)` and
//! iteration order is the key order, never a hash seed. Rules with no valid
//! token (e.g. `/ads*`, `*$third-party`) go in an `untokenized` list that is
//! scanned for every request.
//!
//! A request is tokenised once (distinct `[a-z0-9%]` runs of its lower-cased
//! URL, [`url_tokens`]); the candidate set is the union of the matching
//! buckets plus `untokenized`, sorted by rule id and deduplicated. Candidates
//! are verified cheapest-first: type mask, third-party, `domain=`, then the
//! linear glob match. Cost per request is
//! `O(url_len + tokens * log B + candidates * url_len * longest_segment)`;
//! with rare-token selection, candidates is a handful on real lists.
//!
//! # Determinism
//!
//! Rule ids are assigned in (list order, file order). Candidates are examined
//! in ascending id order; the reported rule is always the lowest-id matching
//! rule of its class. Nothing depends on hashing or addresses.
//!
//! # Precedence
//!
//! `$important` block > exception > block. (`@@...$important` is rejected by
//! the parser, so nothing can override an important block.)
//!
//! # What is matched
//!
//! The string `scheme://host[:port]path[?query]`: userinfo and fragment are
//! not part of it. Only `http`, `https`, `ws`, `wss` and `ftp` requests are
//! examined; any other scheme (`data:`, `blob:`, `file:`...) is `Allow`.
//! Percent-encoding is not normalised (matching is on the URL as given, as in
//! Adblock Plus); `%61ds` does not match `ads`.

use crate::filter::{ParsedList, ResourceType, Rule};
use crate::glob::url_tokens;
use px_websec::origin::url::Host;
use px_websec::origin::{Origin, SiteResolver, Url, schemelessly_same_site};
use std::collections::BTreeMap;

/// Most rules a matcher holds across all lists.
pub const MAX_TOTAL_RULES: usize = 1_000_000;

/// Stable identifier of a rule within one [`Matcher`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleId(pub u32);

/// The decision for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// No rule matched.
    Allow,
    /// Blocked by this rule.
    Block {
        /// The blocking rule.
        rule_id: RuleId,
    },
    /// A block rule matched, or none did, and an exception rule matched.
    AllowedByException {
        /// The exception rule.
        rule_id: RuleId,
    },
}

/// A request to be checked.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The URL being requested.
    pub url: &'a Url,
    /// Origin of the page issuing the request (`None` for a top-level load).
    pub source_origin: Option<&'a Origin>,
    /// What is being requested.
    pub resource_type: ResourceType,
    /// Whether the request is cross-site relative to the source. Computed by
    /// the caller; [`is_third_party`] is the standard way.
    pub is_third_party: bool,
}

/// Third-party determination: the request's site differs from the source's
/// (eTLD+1 via `resolver`, schemes ignored). No source is first-party; an
/// opaque source origin is third-party (fail toward the stricter reading).
pub fn is_third_party(url: &Url, source: Option<&Origin>, resolver: &dyn SiteResolver) -> bool {
    match source {
        None => false,
        Some(s) if s.is_opaque() => true,
        Some(s) => !schemelessly_same_site(&Origin::from_url(url), s, resolver),
    }
}

/// Where a rule came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleInfo<'a> {
    /// Index of the list in the order lists were added.
    pub list: u16,
    /// Name given when the list was added.
    pub list_name: &'a str,
    /// 1-based line in that list.
    pub line: u32,
    /// The filter text.
    pub text: &'a str,
}

/// The URL string a rule is matched against.
#[derive(Debug, Clone)]
pub struct Target {
    full: String,
    lower: String,
    host: (usize, usize),
}

impl Target {
    /// Build from a URL; `None` if the scheme is not a network scheme or the
    /// URL has no host.
    pub fn from_url(url: &Url) -> Option<Target> {
        let host = url.host()?;
        Self::build(url, &host_string(host))
    }

    /// As [`Target::from_url`] with the hostname replaced (CNAME checks).
    pub fn with_host(url: &Url, host: &str) -> Option<Target> {
        url.host()?;
        Self::build(url, host)
    }

    fn build(url: &Url, host: &str) -> Option<Target> {
        if !matches!(url.scheme(), "http" | "https" | "ws" | "wss" | "ftp") {
            return None;
        }
        let mut full = String::with_capacity(url.scheme().len() + host.len() + url.path().len() + 16);
        full.push_str(url.scheme());
        full.push_str("://");
        let hs = full.len();
        full.push_str(host);
        let he = full.len();
        if let Some(p) = url.port() {
            full.push(':');
            full.push_str(&p.to_string());
        }
        full.push_str(url.path());
        if let Some(q) = url.query() {
            full.push('?');
            full.push_str(q);
        }
        let lower = full.to_ascii_lowercase();
        Some(Target {
            full,
            lower,
            host: (hs, he),
        })
    }
}

fn host_string(h: &Host) -> String {
    h.serialize()
}

struct Entry {
    rule: Rule,
    list: u16,
}

/// Collects lists and builds a [`Matcher`].
#[derive(Default)]
pub struct MatcherBuilder {
    lists: Vec<(String, ParsedList)>,
}

impl MatcherBuilder {
    /// A builder with no lists.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a list. Earlier lists get lower rule ids. Lists beyond `u16::MAX`
    /// are ignored.
    pub fn add_list(mut self, name: &str, list: ParsedList) -> Self {
        if self.lists.len() < usize::from(u16::MAX) {
            self.lists.push((name.to_string(), list));
        }
        self
    }

    /// Build the index.
    pub fn build(self) -> Matcher {
        let mut entries: Vec<Entry> = Vec::new();
        let mut names: Vec<Box<str>> = Vec::new();
        let mut dropped = 0usize;
        for (li, (name, list)) in self.lists.into_iter().enumerate() {
            names.push(name.into());
            let li = u16::try_from(li).unwrap_or(u16::MAX);
            for rule in list.rules {
                if entries.len() >= MAX_TOTAL_RULES {
                    dropped += 1;
                } else {
                    entries.push(Entry { rule, list: li });
                }
            }
        }
        // Pass 1: how many rules could use each token.
        let mut freq: BTreeMap<String, u32> = BTreeMap::new();
        for e in &entries {
            for t in e.rule.pattern.index_tokens() {
                *freq.entry(t).or_insert(0) += 1;
            }
        }
        // Pass 2: file each rule under its rarest token.
        let mut index: BTreeMap<Box<str>, Vec<u32>> = BTreeMap::new();
        let mut untokenized: Vec<u32> = Vec::new();
        for (i, e) in entries.iter().enumerate() {
            let id = u32::try_from(i).unwrap_or(u32::MAX);
            let best = e
                .rule
                .pattern
                .index_tokens()
                .into_iter()
                .min_by(|a, b| {
                    let fa = freq.get(a).copied().unwrap_or(0);
                    let fb = freq.get(b).copied().unwrap_or(0);
                    fa.cmp(&fb)
                        .then_with(|| b.len().cmp(&a.len()))
                        .then_with(|| a.cmp(b))
                });
            match best {
                Some(t) => index.entry(t.into()).or_default().push(id),
                None => untokenized.push(id),
            }
        }
        Matcher {
            entries,
            list_names: names,
            index,
            untokenized,
            dropped,
        }
    }
}

/// An immutable, deterministic rule index.
pub struct Matcher {
    entries: Vec<Entry>,
    list_names: Vec<Box<str>>,
    index: BTreeMap<Box<str>, Vec<u32>>,
    untokenized: Vec<u32>,
    dropped: usize,
}

impl Matcher {
    /// A matcher with one list.
    pub fn from_list(name: &str, list: ParsedList) -> Matcher {
        MatcherBuilder::new().add_list(name, list).build()
    }

    /// Number of rules held.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no rules are held.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Rules dropped for exceeding [`MAX_TOTAL_RULES`].
    pub fn dropped_rules(&self) -> usize {
        self.dropped
    }

    /// Number of rules with no index token (scanned on every request).
    pub fn untokenized_len(&self) -> usize {
        self.untokenized.len()
    }

    /// Number of distinct index tokens.
    pub fn bucket_count(&self) -> usize {
        self.index.len()
    }

    /// Size of the largest bucket.
    pub fn max_bucket(&self) -> usize {
        self.index.values().map(Vec::len).max().unwrap_or(0)
    }

    /// Number of lists.
    pub fn list_count(&self) -> usize {
        self.list_names.len()
    }

    /// Name of list `idx`.
    pub fn list_name(&self, idx: u16) -> Option<&str> {
        self.list_names.get(usize::from(idx)).map(|s| &**s)
    }

    /// The rule behind `id`.
    pub fn rule(&self, id: RuleId) -> Option<&Rule> {
        self.entries.get(id.0 as usize).map(|e| &e.rule)
    }

    /// Provenance of `id`.
    pub fn rule_info(&self, id: RuleId) -> Option<RuleInfo<'_>> {
        let e = self.entries.get(id.0 as usize)?;
        Some(RuleInfo {
            list: e.list,
            list_name: self.list_name(e.list).unwrap_or(""),
            line: e.rule.line,
            text: &e.rule.text,
        })
    }

    pub(crate) fn check_target(
        &self,
        target: &Target,
        source_host: Option<&str>,
        ty: ResourceType,
        third_party: bool,
    ) -> Verdict {
        if ty == ResourceType::Document {
            return Verdict::Allow;
        }
        let mut cands: Vec<u32> = self.untokenized.clone();
        for t in url_tokens(&target.lower) {
            if let Some(ids) = self.index.get(t) {
                cands.extend_from_slice(ids);
            }
        }
        cands.sort_unstable();
        cands.dedup();

        let mut exception: Option<u32> = None;
        let mut block: Option<u32> = None;
        for id in cands {
            let Some(e) = self.entries.get(id as usize) else {
                continue;
            };
            let r = &e.rule;
            if !r.types.contains(ty) {
                continue;
            }
            if r.third_party.is_some_and(|want| want != third_party) {
                continue;
            }
            if !r.domains.allows(source_host) {
                continue;
            }
            let text = if r.match_case {
                &target.full
            } else {
                &target.lower
            };
            if !r.pattern.matches(text.as_bytes(), Some(target.host)) {
                continue;
            }
            if r.exception {
                exception.get_or_insert(id);
            } else if r.important {
                return Verdict::Block { rule_id: RuleId(id) };
            } else {
                block.get_or_insert(id);
            }
        }
        match (exception, block) {
            (Some(id), _) => Verdict::AllowedByException { rule_id: RuleId(id) },
            (None, Some(id)) => Verdict::Block { rule_id: RuleId(id) },
            (None, None) => Verdict::Allow,
        }
    }
}

/// Hostname of an origin as the `domain=` option sees it.
pub(crate) fn source_host(origin: Option<&Origin>) -> Option<String> {
    match origin?.host()? {
        Host::Empty => None,
        h => Some(h.serialize()),
    }
}

/// Decide a request.
pub fn check_request(matcher: &Matcher, req: &Request<'_>) -> Verdict {
    let Some(target) = Target::from_url(req.url) else {
        return Verdict::Allow;
    };
    let host = source_host(req.source_origin);
    matcher.check_target(&target, host.as_deref(), req.resource_type, req.is_third_party)
}
