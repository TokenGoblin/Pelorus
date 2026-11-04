//! Filter-list parser: the network-filter subset of Adblock Plus / uBlock
//! Origin syntax.
//!
//! The input is untrusted text. Parsing never panics, allocates in proportion
//! to the (capped) input, and never *partially* accepts a rule: a rule with
//! any option this crate does not implement is rejected whole and counted,
//! because dropping an unknown option would silently broaden the rule.
//!
//! # Caps
//!
//! | cap | value | on excess |
//! |---|---|---|
//! | [`MAX_LIST_BYTES`] | 16 MiB | input cut at a line boundary, `truncated` set |
//! | [`MAX_LINE_BYTES`] | 4096 | line rejected (`LineTooLong`) |
//! | [`MAX_RULES_PER_LIST`] | 500 000 | rest rejected (`OverRuleCap`), `truncated` set |
//! | `MAX_PATTERN_BYTES` | 512 | rule rejected (`PatternTooLong`) |
//! | [`MAX_DOMAINS_PER_RULE`] | 128 | rule rejected (`TooManyDomains`) |

use crate::glob::{Pattern, PatternError};
use std::collections::BTreeMap;

/// Largest list text examined.
pub const MAX_LIST_BYTES: usize = 16 * 1024 * 1024;
/// Longest accepted line.
pub const MAX_LINE_BYTES: usize = 4096;
/// Most rules accepted from one list.
pub const MAX_RULES_PER_LIST: usize = 500_000;
/// Most `domain=` entries per rule.
pub const MAX_DOMAINS_PER_RULE: usize = 128;
/// Longest `domain=` entry.
pub const MAX_DOMAIN_BYTES: usize = 253;

/// What kind of request a resource is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ResourceType {
    /// `script`
    Script,
    /// `image`
    Image,
    /// `stylesheet` (`css`)
    Stylesheet,
    /// `xmlhttprequest` (`xhr`), fetch included
    XmlHttpRequest,
    /// `subdocument` (`frame`)
    Subdocument,
    /// `ping`
    Ping,
    /// `media`
    Media,
    /// `font`
    Font,
    /// `other`
    Other,
    /// A top-level navigation. No rule can match it (`$document` is not
    /// implemented); present so callers can pass every request through.
    Document,
}

impl ResourceType {
    /// The bit for this type in a [`TypeMask`].
    pub const fn bit(self) -> u16 {
        1 << (self as u16)
    }

    fn from_option(name: &str) -> Option<ResourceType> {
        Some(match name {
            "script" => Self::Script,
            "image" => Self::Image,
            "stylesheet" | "css" => Self::Stylesheet,
            "xmlhttprequest" | "xhr" => Self::XmlHttpRequest,
            "subdocument" | "frame" => Self::Subdocument,
            "ping" => Self::Ping,
            "media" => Self::Media,
            "font" => Self::Font,
            "other" => Self::Other,
            _ => return None,
        })
    }
}

/// A set of [`ResourceType`]s.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeMask(pub u16);

impl TypeMask {
    /// The nine subresource types: the default for a rule with no type option.
    pub const DEFAULT: TypeMask = TypeMask(
        ResourceType::Script.bit()
            | ResourceType::Image.bit()
            | ResourceType::Stylesheet.bit()
            | ResourceType::XmlHttpRequest.bit()
            | ResourceType::Subdocument.bit()
            | ResourceType::Ping.bit()
            | ResourceType::Media.bit()
            | ResourceType::Font.bit()
            | ResourceType::Other.bit(),
    );

    /// Whether `t` is in the set.
    pub const fn contains(self, t: ResourceType) -> bool {
        self.0 & t.bit() != 0
    }
}

/// `domain=` constraint: include and exclude lists of lowercase hostnames,
/// sorted and deduplicated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DomainConstraint {
    /// Rule applies only on these pages (and their subdomains).
    pub include: Vec<Box<str>>,
    /// Rule never applies on these pages (and their subdomains).
    pub exclude: Vec<Box<str>>,
}

fn host_under(host: &str, domain: &str) -> bool {
    host == domain || host.strip_suffix(domain).is_some_and(|p| p.ends_with('.'))
}

impl DomainConstraint {
    /// Whether the rule applies on a page whose host is `host` (`None` when
    /// there is no source host). The most specific (longest) matching entry
    /// decides; on a tie the exclusion wins. With no matching entry the rule
    /// applies iff there is no include list.
    pub fn allows(&self, host: Option<&str>) -> bool {
        let host = host.unwrap_or("");
        let mut best: Option<(usize, bool)> = None;
        for d in &self.include {
            if host_under(host, d) && best.is_none_or(|(l, _)| d.len() > l) {
                best = Some((d.len(), false));
            }
        }
        for d in &self.exclude {
            if host_under(host, d) && best.is_none_or(|(l, _)| d.len() >= l) {
                best = Some((d.len(), true));
            }
        }
        match best {
            Some((_, excluded)) => !excluded,
            None => self.include.is_empty(),
        }
    }

    fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

/// One accepted network filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// `@@` exception.
    pub exception: bool,
    /// `$important`.
    pub important: bool,
    /// `$match-case`.
    pub match_case: bool,
    /// The URL pattern.
    pub pattern: Pattern,
    /// Resource types the rule applies to.
    pub types: TypeMask,
    /// `Some(true)` = `$third-party`, `Some(false)` = `$~third-party`.
    pub third_party: Option<bool>,
    /// `$domain=`.
    pub domains: DomainConstraint,
    /// 1-based line number in the list.
    pub line: u32,
    /// The filter text as written (trimmed).
    pub text: Box<str>,
}

/// Why a line that looked like a network filter was not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RejectReason {
    /// `/regex/` filter: there is no regex engine (ReDoS surface).
    Regex,
    /// An option this crate does not implement.
    UnsupportedOption,
    /// Longer than [`MAX_LINE_BYTES`].
    LineTooLong,
    /// Pattern longer than the pattern cap.
    PatternTooLong,
    /// Control byte or space inside the pattern.
    InvalidCharacters,
    /// Pattern with no literal and no constraining option: would match every
    /// request.
    MatchAll,
    /// Options that can never be satisfied (`script,~script`, `3p` with
    /// `~3p`) or are given twice.
    ConflictingOptions,
    /// `@@...$important`: not implemented (see crate docs).
    ImportantException,
    /// More than [`MAX_DOMAINS_PER_RULE`] `domain=` entries.
    TooManyDomains,
    /// A `domain=` entry that is not a plain hostname.
    BadDomain,
    /// Past [`MAX_RULES_PER_LIST`].
    OverRuleCap,
}

/// Counters for one parse. Every input line lands in exactly one of
/// `blank`, `comments`, `headers`, `cosmetic_skipped`, `accepted` or one
/// `rejected` bucket.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParseStats {
    /// Lines seen.
    pub lines: u64,
    /// Empty lines.
    pub blank: u64,
    /// `!` and `# ` comment lines.
    pub comments: u64,
    /// `[Adblock Plus ...]`-style header lines.
    pub headers: u64,
    /// Cosmetic / HTML / scriptlet filter lines, recognised and skipped.
    pub cosmetic_skipped: u64,
    /// The subset of `cosmetic_skipped` that are scriptlet injections.
    pub scriptlet_skipped: u64,
    /// Network filters accepted.
    pub accepted: u64,
    /// The subset of `accepted` that are `@@` exceptions.
    pub exceptions: u64,
    /// Rejected lines by reason.
    pub rejected: BTreeMap<RejectReason, u64>,
    /// The input was cut at a size cap.
    pub truncated: bool,
}

impl ParseStats {
    /// Lines rejected for `reason`.
    pub fn rejected_for(&self, reason: RejectReason) -> u64 {
        self.rejected.get(&reason).copied().unwrap_or(0)
    }

    /// Lines rejected for any reason.
    pub fn rejected_total(&self) -> u64 {
        self.rejected.values().sum()
    }

    /// Regex filters seen and rejected.
    pub fn regex_rejected(&self) -> u64 {
        self.rejected_for(RejectReason::Regex)
    }
}

/// A parsed list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedList {
    /// Accepted rules in file order.
    pub rules: Vec<Rule>,
    /// What was seen.
    pub stats: ParseStats,
}

/// Parse list bytes. Invalid UTF-8 becomes U+FFFD (such a rule then fails or
/// matches nothing useful; it cannot panic).
pub fn parse_list_bytes(bytes: &[u8]) -> ParsedList {
    let cut = bytes.len() > MAX_LIST_BYTES;
    let slice = bytes.get(..MAX_LIST_BYTES).unwrap_or(bytes);
    let text = String::from_utf8_lossy(slice);
    let mut list = parse_list(&text);
    list.stats.truncated |= cut;
    list
}

/// Parse list text.
pub fn parse_list(text: &str) -> ParsedList {
    let mut stats = ParseStats::default();
    let mut text = text.strip_prefix('\u{feff}').unwrap_or(text);
    if text.len() > MAX_LIST_BYTES {
        let mut cut = MAX_LIST_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let head = text.get(..cut).unwrap_or("");
        // Drop the final, possibly partial, line.
        let end = head.rfind('\n').map_or(0, |i| i + 1);
        text = head.get(..end).unwrap_or("");
        stats.truncated = true;
    }
    let mut rules: Vec<Rule> = Vec::new();
    for (idx, raw) in text.split('\n').enumerate() {
        let lineno = u32::try_from(idx.saturating_add(1)).unwrap_or(u32::MAX);
        stats.lines += 1;
        if raw.len() > MAX_LINE_BYTES {
            *stats.rejected.entry(RejectReason::LineTooLong).or_insert(0) += 1;
            continue;
        }
        let line = raw.trim();
        match classify(line) {
            Class::Blank => stats.blank += 1,
            Class::Comment => stats.comments += 1,
            Class::Header => stats.headers += 1,
            Class::Cosmetic { scriptlet } => {
                stats.cosmetic_skipped += 1;
                if scriptlet {
                    stats.scriptlet_skipped += 1;
                }
            }
            Class::Network => {
                if rules.len() >= MAX_RULES_PER_LIST {
                    stats.truncated = true;
                    *stats.rejected.entry(RejectReason::OverRuleCap).or_insert(0) += 1;
                    continue;
                }
                match parse_network_filter(line, lineno) {
                    Ok(rule) => {
                        stats.accepted += 1;
                        if rule.exception {
                            stats.exceptions += 1;
                        }
                        rules.push(rule);
                    }
                    Err(reason) => *stats.rejected.entry(reason).or_insert(0) += 1,
                }
            }
        }
    }
    ParsedList { rules, stats }
}

enum Class {
    Blank,
    Comment,
    Header,
    Cosmetic { scriptlet: bool },
    Network,
}

fn is_domain_list_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ',' | '~' | '*' | ':' | '[' | ']')
}

fn classify(line: &str) -> Class {
    if line.is_empty() {
        return Class::Blank;
    }
    if line.starts_with('!') {
        return Class::Comment;
    }
    if line.starts_with('[') && line.ends_with(']') {
        return Class::Header;
    }
    if let Some(c) = cosmetic(line) {
        return c;
    }
    if line == "#" || line.starts_with("# ") || line.starts_with("#\t") {
        return Class::Comment;
    }
    Class::Network
}

/// Recognise `domains##sel`, `#@#`, `#?#`, `#$#`, `#%#`, `#@$#`, `#@?#`,
/// `#@%#` (cosmetic, procedural, snippet, scriptlet) and `domains$$html`,
/// `$@$` (HTML filters). The text before the marker must look like a domain
/// list, so a network filter containing `#` or `$$` deeper in a path is not
/// mistaken for one.
fn cosmetic(line: &str) -> Option<Class> {
    if let Some(i) = line.find('#') {
        let prefix = line.get(..i)?;
        let rest = line.get(i..)?;
        if prefix.chars().all(is_domain_list_char) {
            for marker in ["#@$#", "#@?#", "#@%#", "##", "#@#", "#?#", "#$#", "#%#"] {
                if let Some(body) = rest.strip_prefix(marker) {
                    let scriptlet = body.starts_with("+js(") || marker.ends_with("%#");
                    return Some(Class::Cosmetic { scriptlet });
                }
            }
        }
    }
    if let Some(i) = line.find('$') {
        let prefix = line.get(..i)?;
        let rest = line.get(i..)?;
        if prefix.chars().all(is_domain_list_char)
            && (rest.starts_with("$$") || rest.starts_with("$@$"))
        {
            return Some(Class::Cosmetic { scriptlet: false });
        }
    }
    None
}

fn is_regex_filter(body: &str) -> bool {
    if !body.starts_with('/') {
        return false;
    }
    match body.rfind('/') {
        Some(last) if last > 0 => {
            let after = body.get(last + 1..).unwrap_or("");
            after.is_empty() || after.starts_with('$')
        }
        _ => false,
    }
}

fn valid_domain(d: &str) -> bool {
    !d.is_empty()
        && d.len() <= MAX_DOMAIN_BYTES
        && !d.starts_with('.')
        && !d.ends_with('.')
        && !d.contains("..")
        && d.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// Parse one trimmed network-filter line.
pub fn parse_network_filter(line: &str, lineno: u32) -> Result<Rule, RejectReason> {
    let (exception, body) = match line.strip_prefix("@@") {
        Some(rest) => (true, rest),
        None => (false, line),
    };
    if is_regex_filter(body) {
        return Err(RejectReason::Regex);
    }
    let (pattern_text, options) = match body.rfind('$') {
        Some(i) => (body.get(..i).unwrap_or(""), body.get(i + 1..).unwrap_or("")),
        None => (body, ""),
    };

    let mut important = false;
    let mut match_case = false;
    let mut third_party: Option<bool> = None;
    let mut pos_types: u16 = 0;
    let mut neg_types: u16 = 0;
    let mut domains = DomainConstraint::default();
    let mut saw_domain = false;
    for opt in options.split(',') {
        if opt.is_empty() {
            continue;
        }
        let (neg, opt) = match opt.strip_prefix('~') {
            Some(r) => (true, r),
            None => (false, opt),
        };
        let (name, value) = match opt.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (opt, None),
        };
        let name = name.to_ascii_lowercase();
        match (name.as_str(), value) {
            ("third-party" | "3p", None) | ("first-party" | "1p", None) => {
                let mut want = !neg;
                if matches!(name.as_str(), "first-party" | "1p") {
                    want = !want;
                }
                if third_party.is_some_and(|prev| prev != want) {
                    return Err(RejectReason::ConflictingOptions);
                }
                third_party = Some(want);
            }
            ("important", None) if !neg => important = true,
            ("match-case", None) if !neg => match_case = true,
            ("domain", Some(v)) if !neg => {
                if saw_domain {
                    return Err(RejectReason::ConflictingOptions);
                }
                saw_domain = true;
                for entry in v.split('|') {
                    let (ex, d) = match entry.strip_prefix('~') {
                        Some(r) => (true, r),
                        None => (false, entry),
                    };
                    if d.contains('*') || d.contains('/') {
                        // Entities (`google.*`) and regex domains.
                        return Err(RejectReason::UnsupportedOption);
                    }
                    let d = d.to_ascii_lowercase();
                    if !valid_domain(&d) {
                        return Err(RejectReason::BadDomain);
                    }
                    if ex {
                        domains.exclude.push(d.into());
                    } else {
                        domains.include.push(d.into());
                    }
                    if domains.include.len() + domains.exclude.len() > MAX_DOMAINS_PER_RULE {
                        return Err(RejectReason::TooManyDomains);
                    }
                }
                domains.include.sort();
                domains.include.dedup();
                domains.exclude.sort();
                domains.exclude.dedup();
            }
            (n, None) => match ResourceType::from_option(n) {
                Some(t) if neg => neg_types |= t.bit(),
                Some(t) => pos_types |= t.bit(),
                None => return Err(RejectReason::UnsupportedOption),
            },
            _ => return Err(RejectReason::UnsupportedOption),
        }
    }
    if exception && important {
        return Err(RejectReason::ImportantException);
    }
    let base = if pos_types == 0 {
        TypeMask::DEFAULT.0
    } else {
        pos_types
    };
    let types = TypeMask(base & !neg_types);
    if types.0 == 0 {
        return Err(RejectReason::ConflictingOptions);
    }
    let constrained =
        pos_types != 0 || neg_types != 0 || third_party.is_some() || !domains.is_empty();

    let pattern = Pattern::from_text(pattern_text, match_case).map_err(|e| match e {
        PatternError::TooLong => RejectReason::PatternTooLong,
        PatternError::InvalidCharacter => RejectReason::InvalidCharacters,
    })?;
    if !pattern.has_literal() && !constrained {
        return Err(RejectReason::MatchAll);
    }
    Ok(Rule {
        exception,
        important,
        match_case,
        pattern,
        types,
        third_party,
        domains,
        line: lineno,
        text: line.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob::StartAnchor;

    fn one(line: &str) -> Result<Rule, RejectReason> {
        parse_network_filter(line, 1)
    }

    #[test]
    fn classifies_every_line_exactly_once() {
        let text = "\u{feff}[Adblock Plus 2.0]\n! Title: Test\n\n# hosts comment\n\
                    ||ads.example.com^\n@@||ads.example.com/ok^\n\
                    example.com##.banner\n#@#.x\n##+js(aopr, foo)\n\
                    example.com#?#div:has-text(Ad)\nexample.com$$script[tag-content=\"x\"]\n\
                    /ad[0-9]+/\n||x.com^$redirect=noop.js\n";
        let l = parse_list(text);
        let s = &l.stats;
        assert_eq!(s.headers, 1);
        assert_eq!(s.comments, 2);
        assert_eq!(s.blank, 2); // one blank line + the empty tail after final \n
        assert_eq!(s.cosmetic_skipped, 5);
        assert_eq!(s.scriptlet_skipped, 1);
        assert_eq!(s.accepted, 2);
        assert_eq!(s.exceptions, 1);
        assert_eq!(s.regex_rejected(), 1);
        assert_eq!(s.rejected_for(RejectReason::UnsupportedOption), 1);
        assert_eq!(s.rejected_total(), 2);
        assert_eq!(
            s.lines,
            s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
        );
        assert_eq!(l.rules.len(), 2);
        assert_eq!(l.rules.first().map(|r| r.line), Some(5));
    }

    #[test]
    fn network_filter_with_hash_or_dollars_is_not_cosmetic() {
        // '#' after a path character; prefix contains '/'.
        let l = parse_list("||example.com/a##b\n/ads$$/\n");
        assert_eq!(l.stats.cosmetic_skipped, 0);
        assert_eq!(l.stats.accepted + l.stats.rejected_total(), 2);
    }

    #[test]
    fn anchors_and_wildcards() {
        let r = one("||ads.example.com^").unwrap();
        assert_eq!(r.pattern.start_anchor(), StartAnchor::Host);
        assert!(!r.exception && !r.important && !r.match_case);
        let r = one("|http://a.com/x|").unwrap();
        assert_eq!(r.pattern.start_anchor(), StartAnchor::Start);
        assert!(r.pattern.end_anchored());
        let r = one("@@||a.com^").unwrap();
        assert!(r.exception);
    }

    #[test]
    fn type_options() {
        let all = TypeMask::DEFAULT;
        assert_eq!(one("||a.com^").unwrap().types, all);
        let r = one("||a.com^$script,image").unwrap();
        assert_eq!(
            r.types.0,
            ResourceType::Script.bit() | ResourceType::Image.bit()
        );
        let r = one("||a.com^$~script").unwrap();
        assert!(!r.types.contains(ResourceType::Script));
        assert!(r.types.contains(ResourceType::Image));
        assert!(!r.types.contains(ResourceType::Document));
        // Positives minus negatives.
        let r = one("||a.com^$script,image,~image").unwrap();
        assert_eq!(r.types.0, ResourceType::Script.bit());
        // Aliases.
        let r = one("||a.com^$xhr,css,frame").unwrap();
        assert!(r.types.contains(ResourceType::XmlHttpRequest));
        assert!(r.types.contains(ResourceType::Stylesheet));
        assert!(r.types.contains(ResourceType::Subdocument));
        // Case-insensitive option names, empty items tolerated.
        assert!(one("||a.com^$SCRIPT,").is_ok());
        assert_eq!(
            one("||a.com^$script,~script"),
            Err(RejectReason::ConflictingOptions)
        );
    }

    #[test]
    fn party_options() {
        assert_eq!(one("||a.com^$third-party").unwrap().third_party, Some(true));
        assert_eq!(
            one("||a.com^$~third-party").unwrap().third_party,
            Some(false)
        );
        assert_eq!(one("||a.com^$3p").unwrap().third_party, Some(true));
        assert_eq!(one("||a.com^$1p").unwrap().third_party, Some(false));
        assert_eq!(
            one("||a.com^$first-party").unwrap().third_party,
            Some(false)
        );
        assert_eq!(
            one("||a.com^$~first-party").unwrap().third_party,
            Some(true)
        );
        assert_eq!(
            one("||a.com^$3p,~3p"),
            Err(RejectReason::ConflictingOptions)
        );
        assert!(one("||a.com^$3p,third-party").is_ok());
    }

    #[test]
    fn domain_option() {
        let r = one("||a.com^$domain=Foo.com|~bar.foo.com|baz.org").unwrap();
        let inc: Vec<&str> = r.domains.include.iter().map(|d| &**d).collect();
        let exc: Vec<&str> = r.domains.exclude.iter().map(|d| &**d).collect();
        assert_eq!(inc, ["baz.org", "foo.com"]);
        assert_eq!(exc, ["bar.foo.com"]);
        assert_eq!(
            one("||a.com^$domain=google.*"),
            Err(RejectReason::UnsupportedOption)
        );
        assert_eq!(one("||a.com^$domain="), Err(RejectReason::BadDomain));
        assert_eq!(one("||a.com^$domain=a..b"), Err(RejectReason::BadDomain));
        assert_eq!(
            one("||a.com^$domain=a.com,domain=b.com"),
            Err(RejectReason::ConflictingOptions)
        );
        assert_eq!(
            one("||a.com^$~domain=a.com"),
            Err(RejectReason::UnsupportedOption)
        );
        let many: Vec<String> = (0..=MAX_DOMAINS_PER_RULE)
            .map(|i| format!("d{i}.com"))
            .collect();
        assert_eq!(
            one(&format!("||a.com^$domain={}", many.join("|"))),
            Err(RejectReason::TooManyDomains)
        );
    }

    #[test]
    fn domain_constraint_most_specific_wins() {
        let r = one("||t.com^$domain=a.com|~b.a.com|~c.com").unwrap();
        let d = &r.domains;
        assert!(d.allows(Some("a.com")));
        assert!(d.allows(Some("x.a.com")));
        assert!(!d.allows(Some("b.a.com")));
        assert!(!d.allows(Some("deep.b.a.com")));
        assert!(!d.allows(Some("c.com")));
        assert!(!d.allows(Some("other.org"))); // include list non-empty
        assert!(!d.allows(Some("nota.com"))); // label boundary
        assert!(!d.allows(None));
        // Exclusions only: everything else is allowed.
        let r = one("||t.com^$domain=~a.com").unwrap();
        assert!(r.domains.allows(Some("b.org")));
        assert!(r.domains.allows(None));
        assert!(!r.domains.allows(Some("a.com")));
        // Tie: exclusion wins.
        let r = one("||t.com^$domain=a.com|~a.com").unwrap();
        assert!(!r.domains.allows(Some("a.com")));
    }

    #[test]
    fn important_and_match_case() {
        let r = one("||a.com^$important,match-case").unwrap();
        assert!(r.important && r.match_case);
        assert_eq!(
            one("@@||a.com^$important"),
            Err(RejectReason::ImportantException)
        );
        assert_eq!(
            one("||a.com^$~important"),
            Err(RejectReason::UnsupportedOption)
        );
    }

    #[test]
    fn match_case_keeps_pattern_case() {
        let r = one("/Ad/x$match-case").unwrap();
        assert!(r.pattern.matches(b"x/Ad/x", None));
        assert!(!r.pattern.matches(b"x/ad/x", None));
    }

    #[test]
    fn unsupported_options_reject_whole_rule() {
        for opt in [
            "redirect=noop.js",
            "removeparam=utm_source",
            "csp=script-src 'none'",
            "badfilter",
            "document",
            "popup",
            "elemhide",
            "generichide",
            "websocket",
            "object",
            "denyallow=a.com",
            "to=a.com",
            "all",
            "ping=1",
            "script=x",
        ] {
            assert_eq!(
                one(&format!("||a.com^${opt}")),
                Err(RejectReason::UnsupportedOption),
                "{opt}"
            );
        }
    }

    #[test]
    fn regex_filters_are_rejected_and_counted() {
        assert_eq!(one("/banner[0-9]+/"), Err(RejectReason::Regex));
        assert_eq!(one("/banner/$script"), Err(RejectReason::Regex));
        assert_eq!(one("@@/banner/"), Err(RejectReason::Regex));
        // Ordinary path patterns are not regexes.
        assert!(one("/ads/banner.gif").is_ok());
        assert!(one("/ads").is_ok());
        let l = parse_list("/a.*b/\n/c/\n||x.com^\n");
        assert_eq!(l.stats.regex_rejected(), 2);
        assert_eq!(l.stats.accepted, 1);
    }

    #[test]
    fn match_all_is_rejected_unless_constrained() {
        assert_eq!(one("*"), Err(RejectReason::MatchAll));
        assert_eq!(one("||"), Err(RejectReason::MatchAll));
        assert_eq!(one("^"), Err(RejectReason::MatchAll));
        assert_eq!(one("@@*"), Err(RejectReason::MatchAll));
        assert_eq!(one("*$important"), Err(RejectReason::MatchAll));
        assert!(one("*$third-party").is_ok());
        assert!(one("*$image,domain=a.com").is_ok());
        assert!(one("@@*$domain=trusted.example").is_ok());
    }

    #[test]
    fn invalid_characters() {
        assert_eq!(one("||a.com/a b^"), Err(RejectReason::InvalidCharacters));
        assert_eq!(one("||a.com/\0^"), Err(RejectReason::InvalidCharacters));
        assert_eq!(
            one("0.0.0.0 ads.example.com"),
            Err(RejectReason::InvalidCharacters)
        );
    }

    #[test]
    fn caps() {
        let long = format!("||a.com/{}", "x".repeat(MAX_LINE_BYTES));
        let l = parse_list(&long);
        assert_eq!(l.stats.rejected_for(RejectReason::LineTooLong), 1);
        let mid = format!("||a.com/{}^", "x".repeat(600));
        assert_eq!(one(&mid), Err(RejectReason::PatternTooLong));
        // Exactly at the line cap is accepted as a line (pattern cap applies).
        let at = "a".repeat(MAX_LINE_BYTES);
        assert_eq!(
            parse_list(&at)
                .stats
                .rejected_for(RejectReason::LineTooLong),
            0
        );
    }

    #[test]
    fn list_size_cap_truncates_at_line_boundary() {
        // Cosmetic lines so the rule cap is not what stops the parse.
        let line = "example.com##.banner\n";
        let n = MAX_LIST_BYTES / line.len() + 10;
        let text = line.repeat(n);
        let l = parse_list(&text);
        assert!(l.stats.truncated);
        assert!(l.stats.cosmetic_skipped as usize <= MAX_LIST_BYTES / line.len());
        assert!(l.stats.cosmetic_skipped as usize > MAX_LIST_BYTES / line.len() - 2);
        assert_eq!(l.stats.rejected_total(), 0); // no partial last line
    }

    #[test]
    fn rule_cap() {
        // Cheap lines; just over the cap.
        let mut text = String::with_capacity(MAX_RULES_PER_LIST * 8);
        for i in 0..(MAX_RULES_PER_LIST + 5) {
            text.push_str(&format!("/a{i}\n"));
        }
        assert!(text.len() < MAX_LIST_BYTES);
        let l = parse_list(&text);
        assert_eq!(l.rules.len(), MAX_RULES_PER_LIST);
        assert_eq!(l.stats.rejected_for(RejectReason::OverRuleCap), 5);
        assert!(l.stats.truncated);
    }

    #[test]
    fn bytes_entry_point_handles_invalid_utf8() {
        let l = parse_list_bytes(b"||a.com^\n\xff\xfe||b.com^\n||c.com/\xc3\x28\n");
        assert_eq!(l.stats.lines, 4);
        assert!(l.stats.accepted >= 1);
    }

    #[test]
    fn crlf_and_whitespace() {
        let l = parse_list("  ||a.com^  \r\n! c\r\n");
        assert_eq!(l.stats.accepted, 1);
        assert_eq!(l.rules.first().map(|r| &*r.text), Some("||a.com^"));
    }
}
