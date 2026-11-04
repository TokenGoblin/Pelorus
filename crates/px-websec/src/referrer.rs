//! Referrer Policy (W3C Referrer Policy): token parsing and the referrer
//! computation.
//!
//! # Deviations
//! - Policy tokens are matched ASCII case-insensitively in the header, `meta`
//!   and attribute forms (the HTML keyword rule; WPT exercises it).
//! - Legacy `meta` keywords (`never`, `always`, `default`,
//!   `origin-when-crossorigin`) are not accepted: unknown means ignored.
//! - Fail closed beyond the spec: a referrer is produced only when the source
//!   scheme is `http`, `https`, `ws`, `wss` or `ftp` and the destination is
//!   `http` or `https`. `file:`, `data:`, `blob:`, `about:` sources never leak
//!   a path.

use crate::mixed::is_url_potentially_trustworthy;
use crate::origin::{Origin, Url, trace};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "referrer::parse_header",
    "referrer::parse_attribute",
    "referrer::compute",
];

/// Longest referrer URL sent in full; longer falls back to the origin.
pub const MAX_REFERRER_BYTES: usize = 4096;
/// Longest header/attribute value examined; longer input is ignored.
pub const MAX_POLICY_INPUT: usize = 4096;

/// The eight referrer policies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReferrerPolicy {
    /// `no-referrer`
    NoReferrer,
    /// `no-referrer-when-downgrade`
    NoReferrerWhenDowngrade,
    /// `same-origin`
    SameOrigin,
    /// `origin`
    Origin,
    /// `strict-origin`
    StrictOrigin,
    /// `origin-when-cross-origin`
    OriginWhenCrossOrigin,
    /// `strict-origin-when-cross-origin`
    StrictOriginWhenCrossOrigin,
    /// `unsafe-url`
    UnsafeUrl,
}

impl ReferrerPolicy {
    /// The default when nothing specifies one.
    pub const DEFAULT: ReferrerPolicy = ReferrerPolicy::StrictOriginWhenCrossOrigin;

    /// The token for this policy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoReferrer => "no-referrer",
            Self::NoReferrerWhenDowngrade => "no-referrer-when-downgrade",
            Self::SameOrigin => "same-origin",
            Self::Origin => "origin",
            Self::StrictOrigin => "strict-origin",
            Self::OriginWhenCrossOrigin => "origin-when-cross-origin",
            Self::StrictOriginWhenCrossOrigin => "strict-origin-when-cross-origin",
            Self::UnsafeUrl => "unsafe-url",
        }
    }

    /// Parse one token (ASCII case-insensitive, no surrounding whitespace).
    pub fn from_token(t: &str) -> Option<Self> {
        const ALL: [ReferrerPolicy; 8] = [
            ReferrerPolicy::NoReferrer,
            ReferrerPolicy::NoReferrerWhenDowngrade,
            ReferrerPolicy::SameOrigin,
            ReferrerPolicy::Origin,
            ReferrerPolicy::StrictOrigin,
            ReferrerPolicy::OriginWhenCrossOrigin,
            ReferrerPolicy::StrictOriginWhenCrossOrigin,
            ReferrerPolicy::UnsafeUrl,
        ];
        ALL.into_iter().find(|p| t.eq_ignore_ascii_case(p.as_str()))
    }
}

/// Parse a `Referrer-Policy` header value: comma-separated tokens, the last
/// *recognised* token wins, unknown tokens are skipped. `None` is the empty
/// policy (no valid token), which means "inherit/default".
pub fn parse_header(value: &str) -> Option<ReferrerPolicy> {
    trace::hit("referrer::parse_header");
    if value.len() > MAX_POLICY_INPUT {
        return None;
    }
    value
        .split(',')
        .filter_map(|t| {
            ReferrerPolicy::from_token(
                t.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')),
            )
        })
        .next_back()
}

/// Parse a `referrerpolicy` attribute or `<meta name=referrer content>`
/// value: one keyword, surrounding ASCII whitespace ignored. Anything else is
/// the empty policy (`None`); lists are *not* accepted here.
pub fn parse_attribute(value: &str) -> Option<ReferrerPolicy> {
    trace::hit("referrer::parse_attribute");
    if value.len() > MAX_POLICY_INPUT {
        return None;
    }
    ReferrerPolicy::from_token(
        value.trim_matches(|c| matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')),
    )
}

/// The policy that applies: the request's own (attribute) policy, else the
/// document's, else the default.
pub fn effective_policy(
    request_policy: Option<ReferrerPolicy>,
    document_policy: Option<ReferrerPolicy>,
) -> ReferrerPolicy {
    request_policy
        .or(document_policy)
        .unwrap_or(ReferrerPolicy::DEFAULT)
}

fn strip_for_referrer(source: &Url, origin_only: bool) -> Option<Url> {
    if !matches!(source.scheme(), "http" | "https" | "ws" | "wss" | "ftp") {
        return None;
    }
    let mut u = source.clone();
    u.strip_credentials();
    u.strip_fragment();
    if origin_only {
        u.set_origin_only();
    }
    Some(u)
}

/// Compute the `Referer` value to send, or `None` for no referrer.
///
/// `source` is the URL of the referring document (`None` if there is none);
/// `request_url` the URL being requested.
pub fn compute(source: Option<&Url>, policy: ReferrerPolicy, request_url: &Url) -> Option<String> {
    trace::hit("referrer::compute");
    if !matches!(request_url.scheme(), "http" | "https") {
        return None;
    }
    let source = source?;
    let full = strip_for_referrer(source, false)?;
    let origin_only = strip_for_referrer(source, true)?;
    let full = if full.serialize(true).len() > MAX_REFERRER_BYTES {
        origin_only.clone()
    } else {
        full
    };
    let same_origin = Origin::from_url(&full) == Origin::from_url(request_url)
        && !Origin::from_url(&full).is_opaque();
    let downgrade =
        is_url_potentially_trustworthy(&full) && !is_url_potentially_trustworthy(request_url);
    let out = match policy {
        ReferrerPolicy::NoReferrer => return None,
        ReferrerPolicy::Origin => origin_only,
        ReferrerPolicy::UnsafeUrl => full,
        ReferrerPolicy::StrictOrigin => {
            if downgrade {
                return None;
            }
            origin_only
        }
        ReferrerPolicy::StrictOriginWhenCrossOrigin => {
            if same_origin {
                full
            } else if downgrade {
                return None;
            } else {
                origin_only
            }
        }
        ReferrerPolicy::OriginWhenCrossOrigin => {
            if same_origin {
                full
            } else {
                origin_only
            }
        }
        ReferrerPolicy::SameOrigin => {
            if same_origin {
                full
            } else {
                return None;
            }
        }
        ReferrerPolicy::NoReferrerWhenDowngrade => {
            if downgrade {
                return None;
            }
            full
        }
    };
    Some(out.serialize(true))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReferrerPolicy as P;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn header_last_valid_wins() {
        assert_eq!(parse_header("no-referrer"), Some(P::NoReferrer));
        assert_eq!(parse_header("no-referrer, origin"), Some(P::Origin));
        assert_eq!(
            parse_header("origin, bogus"),
            Some(P::Origin),
            "unknown tokens skipped"
        );
        assert_eq!(parse_header("bogus"), None);
        assert_eq!(parse_header(""), None);
        assert_eq!(parse_header(" , ,"), None);
        assert_eq!(parse_header("ORIGIN"), Some(P::Origin));
        assert_eq!(
            parse_header("unsafe-url , same-origin ,"),
            Some(P::SameOrigin)
        );
        assert_eq!(parse_header("\"origin\""), None, "quoted is not a token");
        assert_eq!(parse_header("origin origin"), None);
        assert_eq!(
            parse_header(&"x,".repeat(MAX_POLICY_INPUT)),
            None,
            "oversized ignored"
        );
    }

    #[test]
    fn all_eight_tokens_round_trip() {
        for t in [
            "no-referrer",
            "no-referrer-when-downgrade",
            "same-origin",
            "origin",
            "strict-origin",
            "origin-when-cross-origin",
            "strict-origin-when-cross-origin",
            "unsafe-url",
        ] {
            assert_eq!(parse_header(t).map(P::as_str), Some(t));
            assert_eq!(parse_attribute(t).map(P::as_str), Some(t));
        }
    }

    #[test]
    fn attribute_is_a_single_keyword() {
        assert_eq!(parse_attribute("  Origin\n"), Some(P::Origin));
        assert_eq!(parse_attribute("origin, no-referrer"), None);
        assert_eq!(
            parse_attribute("never"),
            None,
            "legacy keywords not accepted"
        );
        assert_eq!(parse_attribute(""), None);
    }

    #[test]
    fn precedence_and_default() {
        assert_eq!(effective_policy(None, None), P::StrictOriginWhenCrossOrigin);
        assert_eq!(effective_policy(None, Some(P::Origin)), P::Origin);
        assert_eq!(
            effective_policy(Some(P::NoReferrer), Some(P::Origin)),
            P::NoReferrer
        );
    }

    const SRC: &str = "https://user:pw@a.example/dir/page?x=1#frag";

    fn r(policy: P, source: &str, target: &str) -> Option<String> {
        compute(Some(&u(source)), policy, &u(target))
    }

    #[test]
    fn strips_credentials_and_fragment() {
        assert_eq!(
            r(P::UnsafeUrl, SRC, "https://b.example/"),
            Some("https://a.example/dir/page?x=1".into())
        );
    }

    #[test]
    fn origin_only_form_has_trailing_slash() {
        assert_eq!(
            r(P::Origin, SRC, "https://b.example/"),
            Some("https://a.example/".into())
        );
        assert_eq!(
            r(P::Origin, "https://a.example:8443/p", "http://b.example/"),
            Some("https://a.example:8443/".into())
        );
    }

    #[test]
    fn matrix_https_to_https_same_origin() {
        let t = "https://a.example/other";
        let full = Some("https://a.example/dir/page?x=1".to_string());
        let orig = Some("https://a.example/".to_string());
        assert_eq!(r(P::NoReferrer, SRC, t), None);
        assert_eq!(r(P::NoReferrerWhenDowngrade, SRC, t), full);
        assert_eq!(r(P::SameOrigin, SRC, t), full);
        assert_eq!(r(P::Origin, SRC, t), orig);
        assert_eq!(r(P::StrictOrigin, SRC, t), orig);
        assert_eq!(r(P::OriginWhenCrossOrigin, SRC, t), full);
        assert_eq!(r(P::StrictOriginWhenCrossOrigin, SRC, t), full);
        assert_eq!(r(P::UnsafeUrl, SRC, t), full);
    }

    #[test]
    fn matrix_https_to_https_cross_origin() {
        let t = "https://b.example/";
        let full = Some("https://a.example/dir/page?x=1".to_string());
        let orig = Some("https://a.example/".to_string());
        assert_eq!(r(P::NoReferrer, SRC, t), None);
        assert_eq!(r(P::NoReferrerWhenDowngrade, SRC, t), full);
        assert_eq!(r(P::SameOrigin, SRC, t), None);
        assert_eq!(r(P::Origin, SRC, t), orig);
        assert_eq!(r(P::StrictOrigin, SRC, t), orig);
        assert_eq!(r(P::OriginWhenCrossOrigin, SRC, t), orig);
        assert_eq!(r(P::StrictOriginWhenCrossOrigin, SRC, t), orig);
        assert_eq!(r(P::UnsafeUrl, SRC, t), full);
    }

    #[test]
    fn matrix_https_to_http_downgrade() {
        let t = "http://b.example/";
        let full = Some("https://a.example/dir/page?x=1".to_string());
        let orig = Some("https://a.example/".to_string());
        assert_eq!(r(P::NoReferrer, SRC, t), None);
        assert_eq!(r(P::NoReferrerWhenDowngrade, SRC, t), None);
        assert_eq!(r(P::SameOrigin, SRC, t), None);
        assert_eq!(r(P::Origin, SRC, t), orig);
        assert_eq!(r(P::StrictOrigin, SRC, t), None);
        assert_eq!(r(P::OriginWhenCrossOrigin, SRC, t), orig);
        assert_eq!(r(P::StrictOriginWhenCrossOrigin, SRC, t), None);
        assert_eq!(r(P::UnsafeUrl, SRC, t), full);
    }

    #[test]
    fn matrix_http_to_https_and_http() {
        let src = "http://a.example/p?q";
        let full = Some("http://a.example/p?q".to_string());
        let orig = Some("http://a.example/".to_string());
        // Upgrade (http -> https) is not a downgrade.
        assert_eq!(r(P::StrictOrigin, src, "https://b.example/"), orig);
        assert_eq!(
            r(P::StrictOriginWhenCrossOrigin, src, "https://b.example/"),
            orig
        );
        assert_eq!(
            r(P::NoReferrerWhenDowngrade, src, "https://b.example/"),
            full
        );
        // http -> http.
        assert_eq!(r(P::StrictOrigin, src, "http://b.example/"), orig);
        assert_eq!(
            r(P::NoReferrerWhenDowngrade, src, "http://b.example/"),
            full
        );
        // Different scheme, same host: cross-origin.
        assert_eq!(r(P::SameOrigin, src, "https://a.example/p"), None);
    }

    #[test]
    fn localhost_targets_are_not_downgrades() {
        assert_eq!(
            r(P::StrictOrigin, SRC, "http://localhost:3000/"),
            Some("https://a.example/".into())
        );
        assert_eq!(
            r(P::NoReferrerWhenDowngrade, SRC, "http://127.0.0.1/"),
            Some("https://a.example/dir/page?x=1".into())
        );
    }

    #[test]
    fn long_referrer_falls_back_to_origin() {
        let long = format!("https://a.example/{}", "p".repeat(MAX_REFERRER_BYTES));
        assert_eq!(
            r(P::UnsafeUrl, &long, "https://b.example/"),
            Some("https://a.example/".into())
        );
        let ok = format!("https://a.example/{}", "p".repeat(100));
        assert_eq!(r(P::UnsafeUrl, &ok, "https://b.example/"), Some(ok.clone()));
    }

    #[test]
    fn local_schemes_and_files_never_referrer() {
        for src in [
            "about:blank",
            "data:text/html,x",
            "blob:https://a.example/i",
            "file:///tmp/x",
            "custom://a/",
        ] {
            assert_eq!(r(P::UnsafeUrl, src, "https://b.example/"), None, "{src}");
        }
        assert_eq!(compute(None, P::UnsafeUrl, &u("https://b.example/")), None);
        assert_eq!(r(P::UnsafeUrl, SRC, "data:text/plain,x"), None);
        assert_eq!(r(P::UnsafeUrl, SRC, "file:///x"), None);
    }

    #[test]
    fn source_default_port_and_case_same_origin() {
        assert_eq!(
            r(
                P::SameOrigin,
                "https://A.example:443/x",
                "https://a.example/y"
            ),
            Some("https://a.example/x".into())
        );
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let _ = parse_header("origin");
        let _ = parse_attribute("origin");
        let _ = compute(Some(&u(SRC)), P::Origin, &u("https://b.example/"));
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_input_never_panics() {
        let mut x: u64 = 0x0123_4567_89AB_CDEF;
        let alphabet = b"origin,- \t\"no-referrerunsafeurl\x00\xff";
        let pols = [
            P::NoReferrer,
            P::NoReferrerWhenDowngrade,
            P::SameOrigin,
            P::Origin,
            P::StrictOrigin,
            P::OriginWhenCrossOrigin,
            P::StrictOriginWhenCrossOrigin,
            P::UnsafeUrl,
        ];
        for i in 0..10_000usize {
            let mut s = String::new();
            for _ in 0..(x % 80) {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                s.push(alphabet[(x % alphabet.len() as u64) as usize] as char);
            }
            let _ = parse_header(&s);
            let _ = parse_attribute(&s);
            if let Some(src) = Url::parse(&format!("https://{s}")) {
                let _ = compute(Some(&src), pols[i % 8], &u("http://b.example/"));
            }
        }
    }
}
