//! Serialized-policy parsing and serialization (CSP3 §2.2, §3).
//!
//! # Limits
//!
//! The input is attacker-controlled (a response header or a `<meta>`
//! attribute). Everything is bounded by [`MAX_POLICY_BYTES`] per policy and
//! [`MAX_POLICIES`] per header list. Exceeding either is a [`ParseError`],
//! never a silent truncation: dropping the tail of a policy would drop
//! restrictions, which fails open. Callers that must always produce *some*
//! policy use [`PolicySet::from_headers_fail_closed`], which substitutes
//! [`Policy::deny_all`].
//!
//! Directives this crate does not know are kept (so that round-tripping and
//! reporting see the original policy) up to [`MAX_UNKNOWN_DIRECTIVES`]; past
//! that they are dropped, which is safe because an unknown directive has no
//! effect on enforcement. Known directives are never dropped.

use std::collections::HashSet;

use super::sandbox::SandboxFlags;
use super::source::SourceList;

/// Maximum accepted size of one serialized policy (or header list), bytes.
/// 1 MiB: larger than any real header (servers and proxies cap headers far
/// below this) and large enough for the documented 100,000-token test.
pub const MAX_POLICY_BYTES: usize = 1 << 20;
/// Maximum number of policies in one comma-separated header list.
pub const MAX_POLICIES: usize = 128;
/// Maximum number of directives with unknown names that are retained.
pub const MAX_UNKNOWN_DIRECTIVES: usize = 64;

/// Directive names this crate acts on or parses specially.
pub const KNOWN_DIRECTIVES: &[&str] = &[
    "default-src",
    "script-src",
    "script-src-elem",
    "script-src-attr",
    "style-src",
    "style-src-elem",
    "style-src-attr",
    "img-src",
    "font-src",
    "connect-src",
    "media-src",
    "object-src",
    "frame-src",
    "child-src",
    "worker-src",
    "manifest-src",
    "prefetch-src",
    "base-uri",
    "form-action",
    "frame-ancestors",
    "sandbox",
    "upgrade-insecure-requests",
    "block-all-mixed-content",
    "report-uri",
    "report-to",
    "require-trusted-types-for",
    "trusted-types",
    "plugin-types",
    "navigate-to",
];

/// Why a serialized policy was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseError {
    /// Input longer than [`MAX_POLICY_BYTES`].
    TooLarge,
    /// More than [`MAX_POLICIES`] policies in a list.
    TooManyPolicies,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParseError::TooLarge => write!(f, "policy exceeds {MAX_POLICY_BYTES} bytes"),
            ParseError::TooManyPolicies => write!(f, "more than {MAX_POLICIES} policies"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Whether a policy blocks (`Enforce`) or only reports (`Report`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Disposition {
    /// `Content-Security-Policy`.
    Enforce,
    /// `Content-Security-Policy-Report-Only`.
    Report,
}

/// Where the policy came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicySource {
    /// An HTTP response header.
    Header,
    /// A `<meta http-equiv="content-security-policy">` element.
    Meta,
}

/// One directive: lowercased name and its value tokens as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    /// Lowercased name (`[a-z0-9-]+`).
    pub name: String,
    /// Value tokens, split on ASCII whitespace, case preserved.
    pub values: Vec<String>,
}

/// A parsed policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Policy {
    directives: Vec<Directive>,
    disposition: Disposition,
    source: PolicySource,
}

/// ASCII whitespace per the Infra Standard: tab, LF, FF, CR, space.
/// (Not U+000B: a vertical tab is part of a token.)
fn is_ascii_ws(b: u8) -> bool {
    matches!(b, b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn trim_ws(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if is_ascii_ws(*first) {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if is_ascii_ws(*last) {
            s = rest;
        } else {
            break;
        }
    }
    s
}

fn valid_directive_name(n: &[u8]) -> bool {
    !n.is_empty() && n.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

impl Policy {
    /// CSP3 §2.2.1 "parse a serialized CSP" over raw bytes.
    ///
    /// * the input is split on `;`; each token is trimmed of ASCII whitespace;
    /// * a token containing any non-ASCII byte is skipped whole (the spec
    ///   skips "non-ASCII strings"), as is one whose name is not
    ///   `[A-Za-z0-9-]+`;
    /// * the name is lowercased; the value is split on ASCII whitespace;
    /// * a repeated directive name is ignored: the first one wins.
    ///
    /// A policy with no directives is returned as an empty policy; list
    /// parsing drops those.
    pub fn parse(
        input: &[u8],
        disposition: Disposition,
        source: PolicySource,
    ) -> Result<Policy, ParseError> {
        if input.len() > MAX_POLICY_BYTES {
            return Err(ParseError::TooLarge);
        }
        let mut directives: Vec<Directive> = Vec::new();
        let mut seen: HashSet<Vec<u8>> = HashSet::new();
        let mut unknown = 0usize;
        for token in input.split(|b| *b == b';') {
            let token = trim_ws(token);
            if token.is_empty() || !token.is_ascii() {
                continue;
            }
            let name_end = token
                .iter()
                .position(|b| is_ascii_ws(*b))
                .unwrap_or(token.len());
            let (name, rest) = token.split_at(name_end);
            if !valid_directive_name(name) {
                continue;
            }
            let name = name.to_ascii_lowercase();
            if seen.contains(&name) {
                continue;
            }
            let Ok(name_str) = String::from_utf8(name.clone()) else {
                continue;
            };
            if !KNOWN_DIRECTIVES.contains(&name_str.as_str()) {
                if unknown >= MAX_UNKNOWN_DIRECTIVES {
                    continue;
                }
                unknown += 1;
            }
            seen.insert(name);
            let values: Vec<String> = rest
                .split(|b| is_ascii_ws(*b))
                .filter(|v| !v.is_empty())
                .filter_map(|v| std::str::from_utf8(v).ok().map(str::to_string))
                .collect();
            directives.push(Directive {
                name: name_str,
                values,
            });
        }
        Ok(Policy {
            directives,
            disposition,
            source,
        })
    }

    /// Parse a `<meta>` policy (CSP3 §3.3): one policy, no comma splitting,
    /// always enforced, and `report-uri`, `frame-ancestors` and `sandbox`
    /// are removed (they are not honoured from `<meta>`).
    pub fn parse_meta(content: &[u8]) -> Result<Policy, ParseError> {
        let mut p = Policy::parse(content, Disposition::Enforce, PolicySource::Meta)?;
        p.directives.retain(|d| {
            !matches!(
                d.name.as_str(),
                "report-uri" | "frame-ancestors" | "sandbox"
            )
        });
        Ok(p)
    }

    /// The policy that blocks everything this crate can block: used when a
    /// header could not be parsed at all.
    pub fn deny_all() -> Policy {
        let d = |n: &str, v: &[&str]| Directive {
            name: n.to_string(),
            values: v.iter().map(|s| s.to_string()).collect(),
        };
        Policy {
            directives: vec![
                d("default-src", &["'none'"]),
                d("base-uri", &["'none'"]),
                d("form-action", &["'none'"]),
                d("frame-ancestors", &["'none'"]),
                d("sandbox", &[]),
                d("upgrade-insecure-requests", &[]),
                d("block-all-mixed-content", &[]),
            ],
            disposition: Disposition::Enforce,
            source: PolicySource::Header,
        }
    }

    /// Build a policy directly from directives (first occurrence of a name
    /// wins, as in parsing).
    pub fn from_directives(
        directives: Vec<Directive>,
        disposition: Disposition,
        source: PolicySource,
    ) -> Policy {
        let mut seen = HashSet::new();
        let directives = directives
            .into_iter()
            .filter(|d| seen.insert(d.name.clone()))
            .collect();
        Policy {
            directives,
            disposition,
            source,
        }
    }

    /// All directives in source order.
    pub fn directives(&self) -> &[Directive] {
        &self.directives
    }

    /// Enforce or report-only.
    pub fn disposition(&self) -> Disposition {
        self.disposition
    }

    /// Header or `<meta>`.
    pub fn source(&self) -> PolicySource {
        self.source
    }

    /// Whether the policy has no directives.
    pub fn is_empty(&self) -> bool {
        self.directives.is_empty()
    }

    /// The directive with this (lowercase) name.
    pub fn directive(&self, name: &str) -> Option<&Directive> {
        self.directives.iter().find(|d| d.name == name)
    }

    /// Whether the directive is present.
    pub fn has(&self, name: &str) -> bool {
        self.directive(name).is_some()
    }

    /// The directive's value parsed as a source list.
    pub fn source_list(&self, name: &str) -> Option<SourceList> {
        self.directive(name).map(|d| SourceList::parse(&d.values))
    }

    /// Value tokens of `report-uri` (URLs, as written).
    pub fn report_uri(&self) -> &[String] {
        self.directive("report-uri").map_or(&[], |d| &d.values)
    }

    /// Value of `report-to` (first token).
    pub fn report_to(&self) -> Option<&str> {
        self.directive("report-to")
            .and_then(|d| d.values.first())
            .map(String::as_str)
    }

    /// Whether `'report-sample'` is in the source list governing `name`.
    pub fn wants_sample(&self, name: &str) -> bool {
        self.source_list(name)
            .is_some_and(|l| l.has_keyword(super::source::Keyword::ReportSample))
    }

    /// The sandbox flags this policy imposes on the document, `None` when it
    /// imposes none. Ignored for report-only policies and `<meta>` (CSP3
    /// §6.3.2).
    pub fn sandbox_flags(&self) -> Option<SandboxFlags> {
        if self.disposition == Disposition::Report || self.source == PolicySource::Meta {
            return None;
        }
        self.directive("sandbox")
            .map(|d| SandboxFlags::from_tokens(&d.values))
    }

    /// Serialize as `name v1 v2; name2 v`. Parsing the result yields an equal
    /// policy (for equal disposition and source).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        for (i, d) in self.directives.iter().enumerate() {
            if i > 0 {
                out.push_str("; ");
            }
            out.push_str(&d.name);
            for v in &d.values {
                out.push(' ');
                out.push_str(v);
            }
        }
        out
    }
}

/// CSP3 §2.2.2 "parse a serialized CSP list": split on commas, parse each,
/// drop policies with no directives.
pub fn parse_policy_list(
    input: &[u8],
    disposition: Disposition,
) -> Result<Vec<Policy>, ParseError> {
    if input.len() > MAX_POLICY_BYTES {
        return Err(ParseError::TooLarge);
    }
    let mut out = Vec::new();
    for piece in input.split(|b| *b == b',') {
        let p = Policy::parse(piece, disposition, PolicySource::Header)?;
        if p.is_empty() {
            continue;
        }
        if out.len() >= MAX_POLICIES {
            return Err(ParseError::TooManyPolicies);
        }
        out.push(p);
    }
    Ok(out)
}

/// Serialize a policy list for a header. `None` if any value contains a
/// comma (only possible for `<meta>` policies), since the result would not
/// parse back to the same list.
pub fn serialize_policy_list(policies: &[Policy]) -> Option<String> {
    let mut parts = Vec::with_capacity(policies.len());
    for p in policies {
        let s = p.serialize();
        if s.contains(',') {
            return None;
        }
        parts.push(s);
    }
    Some(parts.join(", "))
}

/// The policies of one global: every enforced and report-only policy from
/// every header, plus `<meta>` policies.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PolicySet {
    policies: Vec<Policy>,
}

impl PolicySet {
    /// An empty set (no CSP).
    pub fn new() -> PolicySet {
        PolicySet::default()
    }

    /// Parse every `Content-Security-Policy` header value (`enforce`) and
    /// every `Content-Security-Policy-Report-Only` value (`report_only`).
    /// Multiple headers behave like one comma-joined header.
    pub fn from_headers(enforce: &[&[u8]], report_only: &[&[u8]]) -> Result<PolicySet, ParseError> {
        let mut policies = Vec::new();
        for (values, disp) in [
            (enforce, Disposition::Enforce),
            (report_only, Disposition::Report),
        ] {
            for v in values {
                policies.extend(parse_policy_list(v, disp)?);
                if policies.len() > MAX_POLICIES {
                    return Err(ParseError::TooManyPolicies);
                }
            }
        }
        Ok(PolicySet { policies })
    }

    /// As [`PolicySet::from_headers`], but a parse failure yields a set with
    /// only [`Policy::deny_all`] instead of an error.
    pub fn from_headers_fail_closed(enforce: &[&[u8]], report_only: &[&[u8]]) -> PolicySet {
        PolicySet::from_headers(enforce, report_only).unwrap_or_else(|_| PolicySet {
            policies: vec![Policy::deny_all()],
        })
    }

    /// Add a `<meta>` policy. A failed parse adds [`Policy::deny_all`].
    pub fn add_meta(&mut self, content: &[u8]) {
        let p = Policy::parse_meta(content).unwrap_or_else(|_| Policy::deny_all());
        if !p.is_empty() && self.policies.len() < MAX_POLICIES {
            self.policies.push(p);
        } else if !p.is_empty() {
            self.policies.push(Policy::deny_all());
        }
    }

    /// Append a policy.
    pub fn push(&mut self, p: Policy) {
        self.policies.push(p);
    }

    /// The policies, in order.
    pub fn policies(&self) -> &[Policy] {
        &self.policies
    }

    /// Whether there are no policies.
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Policy {
        Policy::parse(s.as_bytes(), Disposition::Enforce, PolicySource::Header).unwrap()
    }
    fn names(p: &Policy) -> Vec<&str> {
        p.directives().iter().map(|d| d.name.as_str()).collect()
    }

    // CSP3 §2.2.1 / the introduction's example policy.
    #[test]
    fn spec_example_policy() {
        let pol =
            p("default-src 'self'; script-src https://example.com/ 'unsafe-inline'; img-src *");
        assert_eq!(names(&pol), ["default-src", "script-src", "img-src"]);
        assert_eq!(
            pol.directive("script-src").unwrap().values,
            ["https://example.com/", "'unsafe-inline'"]
        );
        assert_eq!(pol.directive("img-src").unwrap().values, ["*"]);
    }

    #[test]
    fn directive_names_are_case_insensitive_values_keep_case() {
        let pol = p("Script-SRC 'Self' HTTPS://Example.com/Path");
        assert_eq!(names(&pol), ["script-src"]);
        assert_eq!(
            pol.directive("script-src").unwrap().values,
            ["'Self'", "HTTPS://Example.com/Path"]
        );
    }

    #[test]
    fn duplicate_directives_ignored_after_the_first() {
        let pol = p("script-src 'none'; SCRIPT-SRC *; img-src a; img-src b");
        assert_eq!(pol.directive("script-src").unwrap().values, ["'none'"]);
        assert_eq!(pol.directive("img-src").unwrap().values, ["a"]);
        assert_eq!(pol.directives().len(), 2);
    }

    #[test]
    fn whitespace_rules() {
        // ASCII whitespace only: tab, LF, FF, CR, space.
        let pol = p(" \t\r\n\x0c script-src \t a.test \n\r b.test ;; ; \t ;img-src\x0cc.test");
        assert_eq!(
            pol.directive("script-src").unwrap().values,
            ["a.test", "b.test"]
        );
        assert_eq!(pol.directive("img-src").unwrap().values, ["c.test"]);
        // vertical tab and NBSP are not whitespace.
        let pol = p("script-src\x0ba.test");
        assert!(pol.directive("script-src").is_none());
        assert_eq!(names(&pol), [] as [&str; 0]);
        let pol = Policy::parse(
            b"script-src\xc2\xa0a.test",
            Disposition::Enforce,
            PolicySource::Header,
        )
        .unwrap();
        assert!(pol.is_empty(), "non-ASCII token skipped whole");
    }

    #[test]
    fn non_ascii_token_skipped_others_kept() {
        let pol = Policy::parse(
            "script-src 'self'; img-src caf\u{e9}.test; font-src a.test".as_bytes(),
            Disposition::Enforce,
            PolicySource::Header,
        )
        .unwrap();
        assert_eq!(names(&pol), ["script-src", "font-src"]);
    }

    #[test]
    fn invalid_names_skipped() {
        let pol = p("; =x; a_b c; script-src 'self'; \0 x; img*src y; -x z");
        assert_eq!(names(&pol), ["script-src", "-x"]);
    }

    #[test]
    fn empty_value_directives() {
        let pol = p("upgrade-insecure-requests; sandbox ; block-all-mixed-content");
        assert_eq!(
            names(&pol),
            [
                "upgrade-insecure-requests",
                "sandbox",
                "block-all-mixed-content"
            ]
        );
        assert!(pol.directive("sandbox").unwrap().values.is_empty());
    }

    #[test]
    fn list_of_policies_from_one_header() {
        let v = parse_policy_list(b"script-src a, , img-src b;, ;", Disposition::Enforce).unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(names(&v[0]), ["script-src"]);
        assert_eq!(names(&v[1]), ["img-src"]);
    }

    #[test]
    fn multiple_headers_enforce_and_report() {
        let set = PolicySet::from_headers(
            &[b"script-src a", b"img-src b, font-src c"],
            &[b"default-src d"],
        )
        .unwrap();
        let ds: Vec<_> = set.policies().iter().map(|p| p.disposition()).collect();
        assert_eq!(
            ds,
            [
                Disposition::Enforce,
                Disposition::Enforce,
                Disposition::Enforce,
                Disposition::Report
            ]
        );
    }

    #[test]
    fn meta_policy_subset() {
        let pol = Policy::parse_meta(
            b"default-src 'self'; frame-ancestors 'none'; sandbox; report-uri /r; script-src a, b",
        )
        .unwrap();
        assert_eq!(names(&pol), ["default-src", "script-src"]);
        assert_eq!(pol.source(), PolicySource::Meta);
        assert_eq!(pol.disposition(), Disposition::Enforce);
        // the comma is not a policy separator in <meta>
        assert_eq!(pol.directive("script-src").unwrap().values, ["a,", "b"]);
        assert!(pol.sandbox_flags().is_none());
    }

    #[test]
    fn sandbox_ignored_for_report_only() {
        let rep = Policy::parse(
            b"sandbox allow-scripts",
            Disposition::Report,
            PolicySource::Header,
        )
        .unwrap();
        assert!(rep.sandbox_flags().is_none());
        let enf = p("sandbox allow-scripts");
        assert!(enf.sandbox_flags().is_some());
    }

    #[test]
    fn size_bounds() {
        let big = vec![b'a'; MAX_POLICY_BYTES + 1];
        assert_eq!(
            Policy::parse(&big, Disposition::Enforce, PolicySource::Header),
            Err(ParseError::TooLarge)
        );
        assert_eq!(
            parse_policy_list(&big, Disposition::Enforce),
            Err(ParseError::TooLarge)
        );
        let ok = vec![b'a'; MAX_POLICY_BYTES];
        assert!(Policy::parse(&ok, Disposition::Enforce, PolicySource::Header).is_ok());
        let many = "a b,".repeat(MAX_POLICIES + 1);
        assert_eq!(
            parse_policy_list(many.as_bytes(), Disposition::Enforce),
            Err(ParseError::TooManyPolicies)
        );
        let exactly = "a b,".repeat(MAX_POLICIES);
        assert_eq!(
            parse_policy_list(exactly.as_bytes(), Disposition::Enforce)
                .unwrap()
                .len(),
            MAX_POLICIES
        );
    }

    #[test]
    fn oversized_header_fails_closed() {
        let big = vec![b'a'; MAX_POLICY_BYTES + 1];
        let set = PolicySet::from_headers_fail_closed(&[&big], &[]);
        assert_eq!(set.policies().len(), 1);
        assert!(set.policies()[0].has("default-src"));
        assert_eq!(set.policies()[0].disposition(), Disposition::Enforce);
    }

    #[test]
    fn unknown_directives_are_capped_known_are_not() {
        let mut s = String::new();
        for i in 0..(MAX_UNKNOWN_DIRECTIVES + 50) {
            s.push_str(&format!("x{i} v;"));
        }
        s.push_str("script-src 'none'");
        let pol = p(&s);
        assert_eq!(pol.directives().len(), MAX_UNKNOWN_DIRECTIVES + 1);
        assert!(pol.has("script-src"), "known directive after the cap kept");
    }

    #[test]
    fn serialize_round_trip() {
        let pol = p("default-src 'self'; Script-Src a.test 'nonce-abc'; upgrade-insecure-requests");
        assert_eq!(
            pol.serialize(),
            "default-src 'self'; script-src a.test 'nonce-abc'; upgrade-insecure-requests"
        );
        assert_eq!(p(&pol.serialize()), pol);
        assert_eq!(
            serialize_policy_list(&[pol.clone(), p("img-src x")]).unwrap(),
            "default-src 'self'; script-src a.test 'nonce-abc'; upgrade-insecure-requests, img-src x"
        );
        let meta = Policy::parse_meta(b"script-src a, b").unwrap();
        assert!(serialize_policy_list(&[meta]).is_none());
    }

    #[test]
    fn deny_all_blocks_by_construction() {
        let d = Policy::deny_all();
        assert_eq!(d.directive("default-src").unwrap().values, ["'none'"]);
        assert!(d.has("sandbox") && d.has("frame-ancestors"));
    }
}
