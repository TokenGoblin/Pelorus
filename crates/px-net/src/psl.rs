//! The Public Suffix List, and the registrable domain it yields.
//!
//! # Why this is a security boundary and not a lookup table
//!
//! Invariant 2 keys every partition — cookies, cache, storage, connection
//! pools, DNS cache, TLS sessions — by `(eTLD+1 of top-level, origin)`. This
//! module computes the eTLD+1. Get it wrong in one direction and two unrelated
//! sites share a partition; get it wrong in the other and one site is split
//! into two that cannot see each other.
//!
//! The first is the dangerous direction. `foo.github.io` and `bar.github.io`
//! belong to different people, and a list that does not know `github.io` is a
//! suffix will put them in one partition and let them read each other's
//! cookies. That is not a degradation, it is a same-origin-policy hole, and
//! nothing observable breaks when it happens — which is why the list's
//! freshness is asserted by the gate rather than assumed.
//!
//! # The rules, which are not obvious
//!
//! A PSL entry is one of three things, and the precedence between them is the
//! part implementations get wrong:
//!
//! - a **normal** rule, `com`, matching that label sequence;
//! - a **wildcard** rule, `*.ck`, where the `*` matches exactly one label;
//! - an **exception** rule, `!www.ck`, which *removes* a match the wildcard
//!   would otherwise make.
//!
//! Exceptions win over wildcards, and the longest match wins otherwise. A host
//! matching no rule at all is treated as though it matched `*`, so an unknown
//! TLD yields the last two labels rather than the whole host — that is the
//! algorithm publicsuffix.org specifies, and it fails in the safe direction:
//! a narrower partition rather than a wider one.
//!
//! # What this does not do
//!
//! It does not handle Unicode. Hosts arrive here already in Punycode, because
//! comparing a Unicode host against an ASCII list is a source of confusable
//! mismatches, and `idna` is where that conversion belongs.

use std::collections::HashMap;

/// The list, parsed into a form that can answer questions about a host.
#[derive(Debug, Default)]
pub struct PublicSuffixList {
    /// Keyed by the full rule text with any leading `*.` or `!` stripped, so a
    /// lookup is a hash rather than a scan of sixteen thousand entries.
    rules: HashMap<String, RuleKind>,
}

/// Which of the three kinds of rule an entry is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleKind {
    /// `com`
    Normal,
    /// `*.ck` — the label before it is also part of the suffix.
    Wildcard,
    /// `!www.ck` — this is *not* a suffix, despite a wildcard saying so.
    Exception,
}

/// Why a list could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PslError {
    /// The list held no usable rules. An empty list is not a permissive list;
    /// it is a broken one, and it would silently widen every partition.
    Empty,
}

impl std::fmt::Display for PslError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "the public suffix list contained no rules"),
        }
    }
}

impl std::error::Error for PslError {}

impl PublicSuffixList {
    /// Parse the list from its published text form.
    ///
    /// Refuses an empty result rather than returning one. A list that parsed
    /// to nothing would make every host its own registrable domain, which
    /// merges every subdomain of every site into one partition — failing open,
    /// silently, in exactly the direction that matters.
    pub fn parse(text: &str) -> Result<Self, PslError> {
        let mut rules = HashMap::new();

        for line in text.lines() {
            let line = line.trim();
            // Comments are `//`, and blank lines separate sections. The
            // ICANN/PRIVATE section markers are comments too; both sections are
            // used, because a private-section suffix like `github.io` is
            // exactly the case this list exists to get right.
            if line.is_empty() || line.starts_with("//") {
                continue;
            }

            let (kind, body) = if let Some(rest) = line.strip_prefix('!') {
                (RuleKind::Exception, rest)
            } else if let Some(rest) = line.strip_prefix("*.") {
                (RuleKind::Wildcard, rest)
            } else {
                (RuleKind::Normal, line)
            };

            if body.is_empty() {
                continue;
            }
            rules.insert(body.to_ascii_lowercase(), kind);
        }

        if rules.is_empty() {
            return Err(PslError::Empty);
        }
        Ok(Self { rules })
    }

    /// How many rules were parsed. For the freshness assertion.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// Whether the list is empty. Never true for a list from [`Self::parse`].
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// The registrable domain — eTLD+1 — for `host`.
    ///
    /// `None` when the host *is* a public suffix and has nothing registrable
    /// below it, when it is an IP literal, or when it is malformed. Every
    /// caller must treat `None` as "cannot be partitioned" and refuse, rather
    /// than falling back to the whole host: a fallback here is the failure
    /// this module exists to prevent.
    pub fn registrable_domain(&self, host: &str) -> Option<String> {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if host.is_empty() || host.starts_with('.') || host.contains("..") {
            return None;
        }
        // An IP literal has no registrable domain. Partitioning by one would
        // be partitioning by a number that means nothing about ownership.
        if host.parse::<std::net::IpAddr>().is_ok() || host.starts_with('[') {
            return None;
        }

        let labels: Vec<&str> = host.split('.').collect();
        if labels.iter().any(|label| label.is_empty()) {
            return None;
        }

        // Walk suffixes from the longest to the shortest, so the first
        // exception found wins and the longest normal or wildcard match is
        // taken. Starting at index 0 would find `com` before `co.uk`.
        let mut suffix_labels: Option<usize> = None;
        for start in 0..labels.len() {
            let candidate = labels.get(start..)?.join(".");
            match self.rules.get(&candidate) {
                Some(RuleKind::Exception) => {
                    // The exception's own first label becomes registrable, so
                    // the suffix is one label shorter than the rule.
                    suffix_labels = Some(labels.len().checked_sub(start)?.checked_sub(1)?);
                    break;
                }
                Some(RuleKind::Wildcard) => {
                    // The wildcard consumes one more label to the left.
                    suffix_labels = Some(labels.len().checked_sub(start)?.checked_add(1)?);
                    break;
                }
                Some(RuleKind::Normal) => {
                    suffix_labels = Some(labels.len().checked_sub(start)?);
                    break;
                }
                None => {}
            }
        }

        // No rule matched. The algorithm treats an unknown TLD as `*`, giving
        // a one-label suffix and a two-label registrable domain. That is the
        // narrow direction: an unknown TLD splits into more partitions rather
        // than fewer.
        let suffix_labels = suffix_labels.unwrap_or(1);

        // The host must have at least one label below the suffix, or there is
        // nothing registrable.
        if labels.len() <= suffix_labels {
            return None;
        }
        let start = labels.len().checked_sub(suffix_labels.checked_add(1)?)?;
        Some(labels.get(start..)?.join("."))
    }
}
