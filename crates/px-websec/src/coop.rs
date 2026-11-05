//! COOP, COEP and CORP: header parsing and the pure decision functions, plus
//! secure-context gating.
//!
//! Specifications: Fetch "Cross-Origin-Resource-Policy header" and "cross-origin
//! resource policy check" (read from the Fetch source); HTML "cross-origin
//! opener policies" / "cross-origin embedder policies" and Secure Contexts
//! (the HTML text itself could not be fetched in this session, so COOP/COEP
//! parsing and the group-switch rule are written from the specification as
//! known and are flagged as such in the phase report).
//!
//! # Deviations
//! - COOP `noopener-allow-popups`, `restrict-properties` and `same-origin`
//!   report-only machinery are not implemented; they parse as `unsafe-none`,
//!   which fails toward *less* isolation only for values this engine does not
//!   claim to support.
//! - Reporting (`report-to`) is parsed and returned but nothing is sent.
//! - Header values are Structured Fields *items*: tokens are case-sensitive
//!   (`Same-Origin` is `unsafe-none`), a quoted string where a token is
//!   expected is a parse failure, lists are failures.

use crate::cors::Tainting;
use crate::mixed::ContextNode;
use crate::origin::{Origin, SiteResolver, Url, schemelessly_same_site, trace};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "coop::parse_coep",
    "coop::parse_coop",
    "coop::parse_corp",
    "coop::group_switch",
    "coop::corp_check",
    "coop::coep_response",
    "coop::secure_context",
];

/// Bound on header text examined; longer is a parse failure (=> `unsafe-none`
/// for COOP/COEP, which is the spec's own failure value).
pub const MAX_HEADER_BYTES: usize = 2048;

// ------------------------------------------------------------ SF item

#[derive(Debug, Default)]
struct SfItem {
    token: Option<String>,
    report_to: Option<String>,
}

fn is_sf_token_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'*'
}
fn is_sf_token_char(c: u8) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
                | b':'
                | b'/'
        )
}

/// Parse a Structured Fields *item* whose bare value must be a token. Any
/// deviation returns `None`. Only the `report-to` string parameter is kept.
fn parse_sf_token_item(input: &str) -> Option<SfItem> {
    if input.len() > MAX_HEADER_BYTES || !input.is_ascii() {
        return None;
    }
    let s = input.trim_matches([' ', '\t']).as_bytes();
    let first = *s.first()?;
    if !is_sf_token_start(first) {
        return None;
    }
    let mut i = 0;
    while s.get(i).is_some_and(|&c| is_sf_token_char(c)) {
        i += 1;
    }
    let token = String::from_utf8(s.get(..i)?.to_vec()).ok()?;
    let mut item = SfItem {
        token: Some(token),
        report_to: None,
    };
    // Parameters.
    loop {
        while matches!(s.get(i), Some(b' ' | b'\t')) {
            i += 1;
        }
        match s.get(i) {
            None => return Some(item),
            Some(b';') => i += 1,
            Some(_) => return None,
        }
        while matches!(s.get(i), Some(b' ')) {
            i += 1;
        }
        let ks = i;
        match s.get(i) {
            Some(c) if c.is_ascii_lowercase() || *c == b'*' => i += 1,
            _ => return None,
        }
        while s.get(i).is_some_and(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'-' | b'.' | b'*')
        }) {
            i += 1;
        }
        let key = std::str::from_utf8(s.get(ks..i)?).ok()?.to_string();
        if s.get(i) == Some(&b'=') {
            i += 1;
            if s.get(i) == Some(&b'"') {
                i += 1;
                let mut val = String::new();
                loop {
                    match s.get(i) {
                        None => return None,
                        Some(b'"') => {
                            i += 1;
                            break;
                        }
                        Some(b'\\') => {
                            match s.get(i + 1) {
                                Some(&c @ (b'"' | b'\\')) => val.push(c as char),
                                _ => return None,
                            }
                            i += 2;
                        }
                        Some(&c) if (0x20..0x7f).contains(&c) => {
                            val.push(c as char);
                            i += 1;
                        }
                        Some(_) => return None,
                    }
                }
                if key == "report-to" {
                    item.report_to = Some(val);
                }
            } else {
                // Bare item parameter value: token, number or boolean.
                let vs = i;
                while s.get(i).is_some_and(|&c| is_sf_token_char(c) || c == b'?') {
                    i += 1;
                }
                if vs == i {
                    return None;
                }
            }
        }
    }
}

// ------------------------------------------------------------ COEP

/// Cross-origin embedder policy value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coep {
    /// No restriction.
    UnsafeNone,
    /// Cross-origin no-cors responses need a permissive CORP.
    RequireCorp,
    /// Cross-origin no-cors requests are sent without credentials.
    Credentialless,
}

/// An embedder policy and its reporting endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedderPolicy {
    /// Value.
    pub value: Coep,
    /// `report-to` endpoint name.
    pub report_to: Option<String>,
}

/// Parse `Cross-Origin-Embedder-Policy` (or its `-Report-Only` twin).
/// Absent or unparsable is `unsafe-none`.
pub fn parse_coep(header: Option<&str>) -> EmbedderPolicy {
    trace::hit("coop::parse_coep");
    let none = EmbedderPolicy {
        value: Coep::UnsafeNone,
        report_to: None,
    };
    let Some(item) = header.and_then(parse_sf_token_item) else {
        return none;
    };
    let value = match item.token.as_deref() {
        Some("require-corp") => Coep::RequireCorp,
        Some("credentialless") => Coep::Credentialless,
        _ => Coep::UnsafeNone,
    };
    EmbedderPolicy {
        value,
        report_to: item.report_to,
    }
}

impl Coep {
    /// Whether this embedder policy is strong enough for cross-origin
    /// isolation.
    pub fn is_isolation_compatible(self) -> bool {
        matches!(self, Coep::RequireCorp | Coep::Credentialless)
    }
}

// ------------------------------------------------------------ COOP

/// Cross-origin opener policy value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Coop {
    /// No isolation.
    UnsafeNone,
    /// Same-origin documents only, but may open unrelated popups.
    SameOriginAllowPopups,
    /// Same-origin documents only.
    SameOrigin,
    /// `same-origin` together with a COEP that enables cross-origin
    /// isolation.
    SameOriginPlusCoep,
}

/// An opener policy and its reporting endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenerPolicy {
    /// Value.
    pub value: Coop,
    /// `report-to` endpoint name.
    pub report_to: Option<String>,
}

/// Parse `Cross-Origin-Opener-Policy`, given the document's parsed COEP
/// (which upgrades `same-origin` to `same-origin-plus-coep`).
pub fn parse_coop(header: Option<&str>, coep: Coep) -> OpenerPolicy {
    trace::hit("coop::parse_coop");
    let none = OpenerPolicy {
        value: Coop::UnsafeNone,
        report_to: None,
    };
    let Some(item) = header.and_then(parse_sf_token_item) else {
        return none;
    };
    let value = match item.token.as_deref() {
        Some("same-origin") if coep.is_isolation_compatible() => Coop::SameOriginPlusCoep,
        Some("same-origin") => Coop::SameOrigin,
        Some("same-origin-allow-popups") => Coop::SameOriginAllowPopups,
        _ => Coop::UnsafeNone,
    };
    OpenerPolicy {
        value,
        report_to: item.report_to,
    }
}

/// Whether the document is cross-origin isolated.
pub fn is_cross_origin_isolated(coop: Coop) -> bool {
    coop == Coop::SameOriginPlusCoep
}

/// The COOP-relevant state of a document or of a navigation response.
#[derive(Debug, Clone)]
pub struct CoopState {
    /// Policy value.
    pub value: Coop,
    /// Origin of the document / response.
    pub origin: Origin,
    /// The document is the initial `about:blank` (its policy was inherited
    /// from an opener).
    pub initial_about_blank: bool,
}

/// "Matching cross-origin opener policy": the two sides may share a
/// browsing context group.
fn coop_matches(a: &CoopState, b: &CoopState) -> bool {
    match (a.value, b.value) {
        (Coop::UnsafeNone, Coop::UnsafeNone) => true,
        (Coop::UnsafeNone, _) | (_, Coop::UnsafeNone) => false,
        (x, y) => x == y && a.origin.same_origin(&b.origin),
    }
}

/// Must a navigation from `current` to `response` switch to a new browsing
/// context group (severing `window.opener`)? COOP only applies to top-level
/// navigations.
pub fn needs_group_switch(current: &CoopState, response: &CoopState, top_level: bool) -> bool {
    trace::hit("coop::group_switch");
    if !top_level {
        return false;
    }
    if coop_matches(current, response) {
        return false;
    }
    // A popup opened by a same-origin-allow-popups document starts as
    // initial about:blank carrying that policy; moving it to an
    // unsafe-none document is the intended escape hatch.
    if current.initial_about_blank
        && current.value == Coop::SameOriginAllowPopups
        && response.value == Coop::UnsafeNone
    {
        return false;
    }
    true
}

// ------------------------------------------------------------ CORP

/// `Cross-Origin-Resource-Policy` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corp {
    /// `same-origin`
    SameOrigin,
    /// `same-site`
    SameSite,
    /// `cross-origin`
    CrossOrigin,
}

/// Parse CORP from every header line of that name. Multiple lines combine
/// with `, ` and therefore match nothing (`None`), as the spec notes. Values
/// are case-sensitive.
pub fn parse_corp(header_lines: &[&str]) -> Option<Corp> {
    trace::hit("coop::parse_corp");
    let mut it = header_lines.iter();
    let first = it.next()?;
    if it.next().is_some() {
        return None;
    }
    match first.trim_matches([' ', '\t', '\n', '\r']) {
        "same-origin" => Some(Corp::SameOrigin),
        "same-site" => Some(Corp::SameSite),
        "cross-origin" => Some(Corp::CrossOrigin),
        _ => None,
    }
}

/// Verdict of a CORP/COEP check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpVerdict {
    /// Proceed.
    Allowed,
    /// Refuse.
    Blocked,
}

/// Facts about the response a CORP check needs.
#[derive(Debug, Clone)]
pub struct CorpResponse<'a> {
    /// The response URL (after redirects).
    pub url: &'a Url,
    /// Parsed `Cross-Origin-Resource-Policy`.
    pub corp: Option<Corp>,
    /// The request included credentials (for `credentialless`).
    pub request_includes_credentials: bool,
}

fn corp_internal(
    origin: &Origin,
    embedder: Coep,
    response: &CorpResponse<'_>,
    for_navigation: bool,
    resolver: &dyn SiteResolver,
) -> CorpVerdict {
    if for_navigation && embedder == Coep::UnsafeNone {
        return CorpVerdict::Allowed;
    }
    let mut policy = response.corp;
    if policy.is_none() {
        match embedder {
            Coep::UnsafeNone => {}
            Coep::Credentialless => {
                if response.request_includes_credentials || for_navigation {
                    policy = Some(Corp::SameOrigin);
                }
            }
            Coep::RequireCorp => policy = Some(Corp::SameOrigin),
        }
    }
    let resp_origin = Origin::from_url(response.url);
    match policy {
        None | Some(Corp::CrossOrigin) => CorpVerdict::Allowed,
        Some(Corp::SameOrigin) => {
            if origin.same_origin(&resp_origin) {
                CorpVerdict::Allowed
            } else {
                CorpVerdict::Blocked
            }
        }
        Some(Corp::SameSite) => {
            if schemelessly_same_site(origin, &resp_origin, resolver)
                && (origin.scheme() == Some("https") || response.url.scheme() != "https")
            {
                CorpVerdict::Allowed
            } else {
                CorpVerdict::Blocked
            }
        }
    }
}

/// Fetch "cross-origin resource policy check": `origin` is the requester's
/// origin and `embedder` its enforced COEP. Blocked if the response's own CORP
/// refuses regardless of COEP, or if COEP does.
pub fn corp_check(
    origin: &Origin,
    embedder: Coep,
    response: &CorpResponse<'_>,
    for_navigation: bool,
    resolver: &dyn SiteResolver,
) -> CorpVerdict {
    trace::hit("coop::corp_check");
    if corp_internal(origin, Coep::UnsafeNone, response, for_navigation, resolver)
        == CorpVerdict::Blocked
    {
        return CorpVerdict::Blocked;
    }
    corp_internal(origin, embedder, response, for_navigation, resolver)
}

/// Would the *report-only* embedder policy have blocked this response? (For
/// the caller to queue a report; never blocks the load.)
pub fn corp_report_only_would_block(
    origin: &Origin,
    report_only: Coep,
    response: &CorpResponse<'_>,
    for_navigation: bool,
    resolver: &dyn SiteResolver,
) -> bool {
    corp_internal(origin, report_only, response, for_navigation, resolver) == CorpVerdict::Blocked
}

/// COEP `require-corp`/`credentialless` check for a response, applied by
/// response tainting: only opaque (no-cors, cross-origin) responses are
/// subject to CORP; CORS-approved and same-origin ones already passed a
/// stronger check.
pub fn coep_check_response(
    origin: &Origin,
    embedder: Coep,
    tainting: Tainting,
    response: &CorpResponse<'_>,
    resolver: &dyn SiteResolver,
) -> CorpVerdict {
    trace::hit("coop::coep_response");
    match tainting {
        Tainting::Opaque => corp_check(origin, embedder, response, false, resolver),
        Tainting::Basic | Tainting::Cors => CorpVerdict::Allowed,
    }
}

/// STUB
pub fn coep_navigation_check(_parent: Coep, _response: Coep) -> CorpVerdict {
    CorpVerdict::Allowed
}

// ------------------------------------------------------------ secure context

/// Secure-context gating. `chain[0]` is the document itself, the rest are
/// its ancestors (nearest first). A document is a secure context only if its
/// own origin and *every* ancestor's are potentially trustworthy; an empty
/// chain is not secure.
pub fn is_secure_context(chain: &[ContextNode]) -> bool {
    trace::hit("coop::secure_context");
    !chain.is_empty() && chain.iter().all(ContextNode::is_trustworthy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }
    fn o(s: &str) -> Origin {
        Origin::from_url(&u(s))
    }
    fn psl(host: &str) -> Option<String> {
        let l: Vec<&str> = host.split('.').collect();
        (l.len() >= 2).then(|| l[l.len() - 2..].join("."))
    }
    fn node(s: &str) -> ContextNode {
        ContextNode {
            url: u(s),
            origin: o(s),
        }
    }

    #[test]
    fn coep_parsing() {
        let v = |h: Option<&str>| parse_coep(h).value;
        assert_eq!(v(Some("require-corp")), Coep::RequireCorp);
        assert_eq!(v(Some("credentialless")), Coep::Credentialless);
        assert_eq!(v(Some("unsafe-none")), Coep::UnsafeNone);
        assert_eq!(v(None), Coep::UnsafeNone);
        assert_eq!(
            v(Some("Require-Corp")),
            Coep::UnsafeNone,
            "tokens are case-sensitive"
        );
        assert_eq!(
            v(Some("\"require-corp\"")),
            Coep::UnsafeNone,
            "string is not a token"
        );
        assert_eq!(
            v(Some("require-corp, credentialless")),
            Coep::UnsafeNone,
            "list"
        );
        assert_eq!(v(Some("")), Coep::UnsafeNone);
        assert_eq!(v(Some("require-corp junk")), Coep::UnsafeNone);
        let p = parse_coep(Some("require-corp; report-to=\"ep\""));
        assert_eq!(
            (p.value, p.report_to.as_deref()),
            (Coep::RequireCorp, Some("ep"))
        );
        assert_eq!(
            parse_coep(Some("require-corp;report-to=ep")).report_to,
            None,
            "bare token is not a string"
        );
        assert_eq!(
            v(Some("require-corp; report-to=\"unterminated")),
            Coep::UnsafeNone
        );
        assert_eq!(
            v(Some(&format!(
                "require-corp; a=\"{}\"",
                "x".repeat(MAX_HEADER_BYTES)
            ))),
            Coep::UnsafeNone
        );
    }

    #[test]
    fn coop_parsing() {
        let v = |h: &str, c: Coep| parse_coop(Some(h), c).value;
        assert_eq!(v("same-origin", Coep::UnsafeNone), Coop::SameOrigin);
        assert_eq!(
            v("same-origin", Coep::RequireCorp),
            Coop::SameOriginPlusCoep
        );
        assert_eq!(
            v("same-origin", Coep::Credentialless),
            Coop::SameOriginPlusCoep
        );
        assert_eq!(
            v("same-origin-allow-popups", Coep::RequireCorp),
            Coop::SameOriginAllowPopups
        );
        assert_eq!(v("unsafe-none", Coep::RequireCorp), Coop::UnsafeNone);
        assert_eq!(v("Same-Origin", Coep::UnsafeNone), Coop::UnsafeNone);
        assert_eq!(
            v("noopener-allow-popups", Coep::UnsafeNone),
            Coop::UnsafeNone
        );
        assert_eq!(
            v("same-origin, same-origin", Coep::UnsafeNone),
            Coop::UnsafeNone
        );
        assert_eq!(parse_coop(None, Coep::RequireCorp).value, Coop::UnsafeNone);
        let p = parse_coop(Some("same-origin; report-to=\"coop\""), Coep::UnsafeNone);
        assert_eq!(p.report_to.as_deref(), Some("coop"));
        assert!(is_cross_origin_isolated(Coop::SameOriginPlusCoep));
        assert!(!is_cross_origin_isolated(Coop::SameOrigin));
    }

    fn st(v: Coop, url: &str, initial: bool) -> CoopState {
        CoopState {
            value: v,
            origin: o(url),
            initial_about_blank: initial,
        }
    }

    #[test]
    fn group_switch_matrix() {
        use Coop::*;
        let a = "https://a.example/";
        let b = "https://b.example/";
        let sw = |c: Coop, co: &str, r: Coop, ro: &str| {
            needs_group_switch(&st(c, co, false), &st(r, ro, false), true)
        };
        // Both unsafe-none: never.
        assert!(!sw(UnsafeNone, a, UnsafeNone, b));
        // Same value, same origin: stay.
        assert!(!sw(SameOrigin, a, SameOrigin, a));
        assert!(!sw(SameOriginAllowPopups, a, SameOriginAllowPopups, a));
        assert!(!sw(SameOriginPlusCoep, a, SameOriginPlusCoep, a));
        // Same value, different origin: switch.
        assert!(sw(SameOrigin, a, SameOrigin, b));
        // One side unsafe-none: switch.
        assert!(sw(SameOrigin, a, UnsafeNone, a));
        assert!(sw(UnsafeNone, a, SameOrigin, a));
        assert!(sw(SameOriginAllowPopups, a, UnsafeNone, a));
        // Different non-none values: switch.
        assert!(sw(SameOrigin, a, SameOriginAllowPopups, a));
        assert!(sw(SameOrigin, a, SameOriginPlusCoep, a));
    }

    #[test]
    fn group_switch_special_cases() {
        use Coop::*;
        let a = "https://a.example/";
        // Non-top-level navigations are unaffected.
        assert!(!needs_group_switch(
            &st(SameOrigin, a, false),
            &st(UnsafeNone, a, false),
            false
        ));
        // Popup from an allow-popups opener may go to unsafe-none.
        assert!(!needs_group_switch(
            &st(SameOriginAllowPopups, a, true),
            &st(UnsafeNone, "https://b.example/", false),
            true
        ));
        // ...but not once it is a real document.
        assert!(needs_group_switch(
            &st(SameOriginAllowPopups, a, false),
            &st(UnsafeNone, a, false),
            true
        ));
        // ...and the exception does not cover other policies.
        assert!(needs_group_switch(
            &st(SameOrigin, a, true),
            &st(UnsafeNone, a, false),
            true
        ));
        // Opaque origins never match each other.
        let op = CoopState {
            value: SameOrigin,
            origin: Origin::new_opaque(),
            initial_about_blank: false,
        };
        let op2 = CoopState {
            value: SameOrigin,
            origin: Origin::new_opaque(),
            initial_about_blank: false,
        };
        assert!(needs_group_switch(&op, &op2, true));
    }

    #[test]
    fn corp_parsing() {
        assert_eq!(parse_corp(&["same-origin"]), Some(Corp::SameOrigin));
        assert_eq!(parse_corp(&[" same-site "]), Some(Corp::SameSite));
        assert_eq!(parse_corp(&["cross-origin"]), Some(Corp::CrossOrigin));
        assert_eq!(parse_corp(&["Same-Origin"]), None);
        assert_eq!(parse_corp(&["same-site, same-origin"]), None);
        assert_eq!(
            parse_corp(&["same-origin", "same-origin"]),
            None,
            "two headers combine and match nothing"
        );
        assert_eq!(parse_corp(&[]), None);
        assert_eq!(parse_corp(&[""]), None);
    }

    fn check(
        origin: &str,
        embedder: Coep,
        resp_url: &str,
        corp: Option<Corp>,
        creds: bool,
        nav: bool,
    ) -> CorpVerdict {
        let url = u(resp_url);
        corp_check(
            &o(origin),
            embedder,
            &CorpResponse {
                url: &url,
                corp,
                request_includes_credentials: creds,
            },
            nav,
            &psl,
        )
    }

    #[test]
    fn corp_same_origin() {
        use CorpVerdict::*;
        let r = "https://a.example/img";
        assert_eq!(
            check(
                "https://a.example/",
                Coep::UnsafeNone,
                r,
                Some(Corp::SameOrigin),
                false,
                false
            ),
            Allowed
        );
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                r,
                Some(Corp::SameOrigin),
                false,
                false
            ),
            Blocked
        );
        assert_eq!(
            check(
                "https://a.example:8443/",
                Coep::UnsafeNone,
                r,
                Some(Corp::SameOrigin),
                false,
                false
            ),
            Blocked
        );
        assert_eq!(
            check(
                "http://a.example/",
                Coep::UnsafeNone,
                r,
                Some(Corp::SameOrigin),
                false,
                false
            ),
            Blocked
        );
    }

    #[test]
    fn corp_same_site() {
        use CorpVerdict::*;
        let r = "https://img.a.example/x";
        let ss = Some(Corp::SameSite);
        assert_eq!(
            check(
                "https://www.a.example/",
                Coep::UnsafeNone,
                r,
                ss,
                false,
                false
            ),
            Allowed
        );
        assert_eq!(
            check("https://b.example/", Coep::UnsafeNone, r, ss, false, false),
            Blocked
        );
        // https resource, http requester: same host but secure -> insecure mismatch blocked.
        assert_eq!(
            check(
                "http://www.a.example/",
                Coep::UnsafeNone,
                r,
                ss,
                false,
                false
            ),
            Blocked
        );
        // http resource, http requester: fine.
        assert_eq!(
            check(
                "http://www.a.example/",
                Coep::UnsafeNone,
                "http://img.a.example/x",
                ss,
                false,
                false
            ),
            Allowed
        );
        // http resource, https requester: fine (schemelessly same site).
        assert_eq!(
            check(
                "https://www.a.example/",
                Coep::UnsafeNone,
                "http://img.a.example/x",
                ss,
                false,
                false
            ),
            Allowed
        );
    }

    #[test]
    fn corp_cross_origin_and_absent() {
        use CorpVerdict::*;
        let r = "https://a.example/x";
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                r,
                Some(Corp::CrossOrigin),
                false,
                false
            ),
            Allowed
        );
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                r,
                None,
                false,
                false
            ),
            Allowed
        );
        assert_eq!(
            check(
                "https://b.example/",
                Coep::RequireCorp,
                r,
                Some(Corp::CrossOrigin),
                false,
                false
            ),
            Allowed
        );
    }

    #[test]
    fn coep_require_corp() {
        use CorpVerdict::*;
        let r = "https://a.example/x";
        // No CORP header under require-corp: treated as same-origin.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::RequireCorp,
                r,
                None,
                false,
                false
            ),
            Blocked
        );
        assert_eq!(
            check(
                "https://a.example/",
                Coep::RequireCorp,
                r,
                None,
                false,
                false
            ),
            Allowed
        );
        // A same-origin CORP still blocks even when COEP is off.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                r,
                Some(Corp::SameOrigin),
                false,
                false
            ),
            Blocked
        );
        // same-site satisfied.
        assert_eq!(
            check(
                "https://b.a.example/",
                Coep::RequireCorp,
                r,
                Some(Corp::SameSite),
                false,
                false
            ),
            Allowed
        );
    }

    #[test]
    fn coep_credentialless() {
        use CorpVerdict::*;
        let r = "https://a.example/x";
        // Credential-less no-cors request needs no CORP.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::Credentialless,
                r,
                None,
                false,
                false
            ),
            Allowed
        );
        // With credentials, a missing CORP means same-origin.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::Credentialless,
                r,
                None,
                true,
                false
            ),
            Blocked
        );
        // Navigations are treated like credentialed.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::Credentialless,
                r,
                None,
                false,
                true
            ),
            Blocked
        );
        // Explicit cross-origin passes.
        assert_eq!(
            check(
                "https://b.example/",
                Coep::Credentialless,
                r,
                Some(Corp::CrossOrigin),
                true,
                false
            ),
            Allowed
        );
    }

    #[test]
    fn navigation_with_unsafe_none_is_allowed() {
        use CorpVerdict::*;
        // Fetch: with forNavigation and an unsafe-none embedder policy the
        // internal check returns allowed immediately, even for an explicit
        // same-origin CORP (it only matters to embedders that opted in).
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                "https://a.example/",
                None,
                false,
                true
            ),
            Allowed
        );
        assert_eq!(
            check(
                "https://b.example/",
                Coep::UnsafeNone,
                "https://a.example/",
                Some(Corp::SameOrigin),
                false,
                true
            ),
            Allowed
        );
        assert_eq!(
            check(
                "https://b.example/",
                Coep::RequireCorp,
                "https://a.example/",
                None,
                false,
                true
            ),
            Blocked
        );
    }

    #[test]
    fn report_only_never_changes_the_decision() {
        let url = u("https://a.example/x");
        let resp = CorpResponse {
            url: &url,
            corp: None,
            request_includes_credentials: false,
        };
        assert!(corp_report_only_would_block(
            &o("https://b.example/"),
            Coep::RequireCorp,
            &resp,
            false,
            &psl
        ));
        assert!(!corp_report_only_would_block(
            &o("https://b.example/"),
            Coep::UnsafeNone,
            &resp,
            false,
            &psl
        ));
        assert_eq!(
            corp_check(
                &o("https://b.example/"),
                Coep::UnsafeNone,
                &resp,
                false,
                &psl
            ),
            CorpVerdict::Allowed
        );
    }

    #[test]
    fn coep_response_applies_only_to_opaque() {
        let url = u("https://a.example/x");
        let resp = CorpResponse {
            url: &url,
            corp: None,
            request_includes_credentials: false,
        };
        let b = o("https://b.example/");
        assert_eq!(
            coep_check_response(&b, Coep::RequireCorp, Tainting::Opaque, &resp, &psl),
            CorpVerdict::Blocked
        );
        assert_eq!(
            coep_check_response(&b, Coep::RequireCorp, Tainting::Cors, &resp, &psl),
            CorpVerdict::Allowed
        );
        assert_eq!(
            coep_check_response(&b, Coep::RequireCorp, Tainting::Basic, &resp, &psl),
            CorpVerdict::Allowed
        );
    }

    #[test]
    fn secure_context_gating() {
        assert!(is_secure_context(&[node("https://a.example/")]));
        assert!(is_secure_context(&[node("http://localhost:3000/")]));
        assert!(is_secure_context(&[node("file:///tmp/a.html")]));
        assert!(!is_secure_context(&[node("http://a.example/")]));
        assert!(!is_secure_context(&[]));
        // Secure frame under an insecure parent is not secure.
        assert!(!is_secure_context(&[
            node("https://a.example/"),
            node("http://top.example/")
        ]));
        // Ancestor chain all secure.
        assert!(is_secure_context(&[
            node("https://a.example/"),
            node("https://top.example/"),
            node("https://t.example/")
        ]));
        // One bad apple anywhere.
        assert!(!is_secure_context(&[
            node("https://a.example/"),
            node("https://mid.example/"),
            node("http://top.example/")
        ]));
        // A data: document has an opaque origin: not secure.
        assert!(!is_secure_context(&[node("data:text/html,x")]));
        // ...and nests nothing secure.
        assert!(!is_secure_context(&[
            node("https://a.example/"),
            node("data:text/html,x")
        ]));
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let _ = parse_coep(Some("require-corp"));
        let _ = parse_coop(Some("same-origin"), Coep::RequireCorp);
        let _ = parse_corp(&["same-origin"]);
        let a = st(Coop::SameOrigin, "https://a.example/", false);
        let _ = needs_group_switch(&a, &a, true);
        let url = u("https://a.example/x");
        let resp = CorpResponse {
            url: &url,
            corp: None,
            request_includes_credentials: false,
        };
        let _ = coep_check_response(
            &o("https://b.example/"),
            Coep::RequireCorp,
            Tainting::Opaque,
            &resp,
            &psl,
        );
        let _ = is_secure_context(&[node("https://a.example/")]);
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x: u64 = 0xFEED_FACE_0BAD_F00D;
        let alphabet = b"same-origin-allow-popups;=\"\\ ,report-to*:/?1.0\t\xff\xc3\xa9";
        let url = u("https://a.example/x");
        for _ in 0..10_000 {
            let mut s = String::new();
            for _ in 0..(x % 150) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize] as char);
            }
            let coep = parse_coep(Some(&s)).value;
            let _ = parse_coop(Some(&s), coep);
            let corp = parse_corp(&[&s]);
            let resp = CorpResponse {
                url: &url,
                corp,
                request_includes_credentials: x & 1 == 0,
            };
            let _ = corp_check(&o("https://b.example/"), coep, &resp, x & 2 == 0, &psl);
        }
    }
}
