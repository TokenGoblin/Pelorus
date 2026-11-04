//! Blocking statistics: bounded, deterministic, in memory only.
//!
//! Counts blocked requests per rule, per list and per domain, plus totals.
//! Memory is bounded by construction: at most [`MAX_TRACKED_RULES`] rule
//! counters and [`MAX_TRACKED_DOMAINS`] domain counters are kept. Once a table
//! is full, *new* keys are counted in an overflow counter and existing keys
//! keep counting; nothing is ever evicted, so the result does not depend on
//! arrival timing beyond the order of events themselves. All maps are
//! `BTreeMap`s and counters saturate, so equal event sequences give equal
//! `Stats` and equal rankings.
//!
//! Nothing here leaves the process or touches disk; persistence, if any, is
//! the integration's decision.

use crate::matcher::{Matcher, RuleId, Verdict};
use std::collections::BTreeMap;

/// Most distinct rules tracked.
pub const MAX_TRACKED_RULES: usize = 4096;
/// Most distinct domains tracked.
pub const MAX_TRACKED_DOMAINS: usize = 1024;
/// Longest domain key kept (longer is counted as overflow).
pub const MAX_DOMAIN_KEY_BYTES: usize = 253;

/// Counters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    blocked: u64,
    excepted: u64,
    allowed: u64,
    per_rule: BTreeMap<RuleId, u64>,
    per_list: BTreeMap<u16, u64>,
    per_domain: BTreeMap<Box<str>, u64>,
    rule_overflow: u64,
    domain_overflow: u64,
}

impl Stats {
    /// Empty statistics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the outcome for a request. `domain` is the blocked request's
    /// site (eTLD+1) or host, lower case; it is used only for blocked
    /// requests.
    pub fn record(&mut self, matcher: &Matcher, verdict: &Verdict, domain: &str) {
        match verdict {
            Verdict::Allow => self.allowed = self.allowed.saturating_add(1),
            Verdict::AllowedByException { .. } => self.excepted = self.excepted.saturating_add(1),
            Verdict::Block { rule_id } => {
                self.blocked = self.blocked.saturating_add(1);
                bump_bounded(
                    &mut self.per_rule,
                    *rule_id,
                    MAX_TRACKED_RULES,
                    &mut self.rule_overflow,
                );
                if let Some(info) = matcher.rule_info(*rule_id) {
                    let n = self.per_list.entry(info.list).or_insert(0);
                    *n = n.saturating_add(1);
                }
                if domain.len() <= MAX_DOMAIN_KEY_BYTES {
                    bump_bounded(
                        &mut self.per_domain,
                        domain.to_ascii_lowercase().into_boxed_str(),
                        MAX_TRACKED_DOMAINS,
                        &mut self.domain_overflow,
                    );
                } else {
                    self.domain_overflow = self.domain_overflow.saturating_add(1);
                }
            }
        }
    }

    /// Requests blocked.
    pub fn blocked(&self) -> u64 {
        self.blocked
    }

    /// Requests allowed by an exception.
    pub fn excepted(&self) -> u64 {
        self.excepted
    }

    /// Requests no rule touched.
    pub fn allowed(&self) -> u64 {
        self.allowed
    }

    /// Blocks attributed to `rule` (0 if untracked).
    pub fn rule_count(&self, rule: RuleId) -> u64 {
        self.per_rule.get(&rule).copied().unwrap_or(0)
    }

    /// Blocks attributed to list `idx`. Lists are few, so never capped.
    pub fn list_count(&self, idx: u16) -> u64 {
        self.per_list.get(&idx).copied().unwrap_or(0)
    }

    /// Blocks attributed to `domain` (0 if untracked).
    pub fn domain_count(&self, domain: &str) -> u64 {
        self.per_domain
            .get(domain.to_ascii_lowercase().as_str())
            .copied()
            .unwrap_or(0)
    }

    /// Blocks whose rule did not fit in the rule table.
    pub fn rule_overflow(&self) -> u64 {
        self.rule_overflow
    }

    /// Blocks whose domain did not fit in the domain table.
    pub fn domain_overflow(&self) -> u64 {
        self.domain_overflow
    }

    /// Up to `n` rules by count descending, ties by id ascending.
    pub fn top_rules(&self, n: usize) -> Vec<(RuleId, u64)> {
        let mut v: Vec<(RuleId, u64)> = self.per_rule.iter().map(|(k, c)| (*k, *c)).collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// Up to `n` domains by count descending, ties by name ascending.
    pub fn top_domains(&self, n: usize) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self
            .per_domain
            .iter()
            .map(|(k, c)| (k.to_string(), *c))
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.truncate(n);
        v
    }

    /// Distinct rule and domain entries held (for memory-bound assertions).
    pub fn tracked(&self) -> (usize, usize) {
        (self.per_rule.len(), self.per_domain.len())
    }

    /// Forget everything.
    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

fn bump_bounded<K: Ord>(map: &mut BTreeMap<K, u64>, key: K, cap: usize, overflow: &mut u64) {
    if let Some(n) = map.get_mut(&key) {
        *n = n.saturating_add(1);
    } else if map.len() < cap {
        map.insert(key, 1);
    } else {
        *overflow = overflow.saturating_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::parse_list;

    fn matcher(n: usize) -> Matcher {
        let text: String = (0..n).map(|i| format!("||t{i}.net^\n")).collect();
        Matcher::from_list("l", parse_list(&text))
    }

    #[test]
    fn counts_by_rule_list_and_domain() {
        let m = matcher(3);
        let mut s = Stats::new();
        let b = |i| Verdict::Block { rule_id: RuleId(i) };
        s.record(&m, &b(1), "t1.net");
        s.record(&m, &b(1), "T1.net");
        s.record(&m, &b(0), "t0.net");
        s.record(&m, &Verdict::Allow, "x.org");
        s.record(
            &m,
            &Verdict::AllowedByException { rule_id: RuleId(2) },
            "t2.net",
        );
        assert_eq!((s.blocked(), s.excepted(), s.allowed()), (3, 1, 1));
        assert_eq!(s.rule_count(RuleId(1)), 2);
        assert_eq!(s.rule_count(RuleId(2)), 0);
        assert_eq!(s.list_count(0), 3);
        assert_eq!(s.domain_count("t1.net"), 2);
        assert_eq!(s.top_rules(5), [(RuleId(1), 2), (RuleId(0), 1)]);
        assert_eq!(s.top_domains(1), [("t1.net".to_string(), 2)]);
        s.clear();
        assert_eq!(s, Stats::new());
    }

    #[test]
    fn memory_is_bounded_and_overflow_counted() {
        let m = matcher(MAX_TRACKED_RULES + 100);
        let mut s = Stats::new();
        for i in 0..(MAX_TRACKED_RULES + 100) {
            let id = u32::try_from(i).unwrap();
            s.record(
                &m,
                &Verdict::Block {
                    rule_id: RuleId(id),
                },
                &format!("d{i}.net"),
            );
        }
        let (r, d) = s.tracked();
        assert_eq!(r, MAX_TRACKED_RULES);
        assert_eq!(d, MAX_TRACKED_DOMAINS);
        assert_eq!(s.rule_overflow(), 100);
        assert_eq!(
            s.domain_overflow(),
            (MAX_TRACKED_RULES + 100 - MAX_TRACKED_DOMAINS) as u64
        );
        assert_eq!(s.blocked(), (MAX_TRACKED_RULES + 100) as u64);
        // Tracked keys still count after the table is full.
        s.record(&m, &Verdict::Block { rule_id: RuleId(0) }, "d0.net");
        assert_eq!(s.rule_count(RuleId(0)), 2);
        assert_eq!(s.domain_count("d0.net"), 2);
        // Oversized domain keys are overflow, not memory.
        let before = s.domain_overflow();
        s.record(
            &m,
            &Verdict::Block { rule_id: RuleId(0) },
            &"a".repeat(1000),
        );
        assert_eq!(s.domain_overflow(), before + 1);
        assert_eq!(s.tracked().1, MAX_TRACKED_DOMAINS);
    }

    #[test]
    fn deterministic_ranking_and_equality() {
        let m = matcher(10);
        let events: Vec<u32> = vec![3, 1, 4, 1, 5, 9, 2, 6, 5, 3, 5];
        let run = || {
            let mut s = Stats::new();
            for e in &events {
                s.record(
                    &m,
                    &Verdict::Block {
                        rule_id: RuleId(*e),
                    },
                    &format!("t{e}.net"),
                );
            }
            s
        };
        let a = run();
        let b = run();
        assert_eq!(a, b);
        // 5 x3, then 1 x2 and 3 x2 (id order), then singles by id.
        assert_eq!(
            a.top_rules(6),
            [
                (RuleId(5), 3),
                (RuleId(1), 2),
                (RuleId(3), 2),
                (RuleId(2), 1),
                (RuleId(4), 1),
                (RuleId(6), 1)
            ]
        );
        assert_eq!(a.top_domains(3)[0], ("t5.net".to_string(), 3));
        assert_eq!(a.top_domains(3)[1], ("t1.net".to_string(), 2));
        assert_eq!(a.top_domains(3)[2], ("t3.net".to_string(), 2));
    }
}
