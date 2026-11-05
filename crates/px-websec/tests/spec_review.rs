//! Adversarial-review regression tests (docs/security/websec-review.md).
//!
//! Each test names the finding it pins. Helpers are macros so that the
//! workspace's test-only `unwrap` allowance applies at the expansion site.

use px_websec::coop::{self, Coep, CorpVerdict};
use px_websec::cors::{self, CorsRequest, CredentialsMode, RequestMode, Tainting};
use px_websec::csp::check::Blocked;
use px_websec::csp::{
    BlockedResource, CspUrl, Disposition, InlineType, Policy, PolicySet, PolicySource, Violation,
    ViolationContext, check_inline,
};
use px_websec::origin::{Origin as WOrigin, Url as WUrl};
use px_websec::referrer::{self, ReferrerPolicy};
use px_websec::sri::{self, SriError};

macro_rules! enforce {
    ($s:expr) => {
        Policy::parse($s.as_bytes(), Disposition::Enforce, PolicySource::Header).unwrap()
    };
}
macro_rules! cu {
    ($s:expr) => {
        CspUrl::parse($s).unwrap()
    };
}
macro_rules! creq {
    ($m:expr, $o:expr, $u:expr, $mode:expr) => {
        CorsRequest {
            method: $m.to_string(),
            url: WUrl::parse($u).unwrap(),
            origin: WOrigin::from_url(&WUrl::parse($o).unwrap()),
            headers: vec![],
            credentials: CredentialsMode::SameOrigin,
            mode: $mode,
            use_cors_preflight_flag: false,
            origin_tainted: false,
        }
    };
}

// F-CSP-1: a grammatically valid hash-source of the wrong length still
// counts as a hash-source, so it disables 'unsafe-inline' (CSP3 6.7.3.3).
#[test]
fn malformed_hash_source_still_disables_unsafe_inline() {
    for h in [
        "'sha256-AAAA'",
        "'sha256-A'",
        "'sha512-short'",
        "'sha384-+/+/'",
    ] {
        let p = enforce!(format!("script-src 'unsafe-inline' {h}"));
        assert!(
            check_inline(&p, InlineType::Script, None, "alert(1)").is_blocked(),
            "{h}"
        );
    }
    // A token that is not hash-source grammar at all stays ignored.
    let p = enforce!("script-src 'unsafe-inline' 'sha256-!!!'");
    assert!(!check_inline(&p, InlineType::Script, None, "alert(1)").is_blocked());
}

// F-CSP-2: frame-ancestors in a report-only header is reported, never
// enforced.
#[test]
fn report_only_frame_ancestors_reports_but_does_not_block() {
    let set = PolicySet::from_headers(&[], &[b"frame-ancestors 'none'"]).unwrap();
    let v = set.check_frame_ancestors(&cu!("https://a.test/"), &[cu!("https://evil.test/")]);
    assert!(!v.is_blocked());
    assert_eq!(v.reports.len(), 1);
}

// F-CSP-3: meta delivery still never carries frame-ancestors.
#[test]
fn meta_frame_ancestors_dropped() {
    let p = Policy::parse_meta(b"frame-ancestors 'none'").unwrap();
    assert!(!p.has("frame-ancestors"));
}

// F-SRI-1: a token with a recognised algorithm but an undecodable or
// wrong-length digest is still integrity metadata; it can never match, so
// the load is refused (SRI "parse metadata" / "does response match").
#[test]
fn sri_bad_digest_is_enforced_and_fails() {
    for v in ["sha256-AAAA", "sha256-!!!!", "sha384-abc", "sha256-TR=="] {
        let md = sri::parse(v).unwrap();
        if v == "sha256-!!!!" {
            // not valid metadata (grammar), skipped, but a token was present
            assert!(!md.is_enforced(), "{v}");
            continue;
        }
        assert!(md.is_enforced(), "{v}");
        assert_eq!(
            sri::verify(&md, Tainting::Basic, b"body"),
            Err(SriError::Mismatch),
            "{v}"
        );
    }
}

// F-SRI-2: unrecognised-only metadata on an opaque response is refused:
// eligibility is checked before the "empty set => true" step.
#[test]
fn sri_unknown_algorithm_on_opaque_response_is_refused() {
    let md = sri::parse("md5-abcdef").unwrap();
    assert_eq!(
        sri::verify(&md, Tainting::Opaque, b"x"),
        Err(SriError::NotEligible)
    );
    assert_eq!(sri::verify(&md, Tainting::Basic, b"x"), Ok(()));
    // Truly empty metadata is "no metadata": proceeds even when opaque.
    let none = sri::parse("   ").unwrap();
    assert_eq!(sri::verify(&none, Tainting::Opaque, b"x"), Ok(()));
}

// F-CORS-1: Range safelisting is exact: `bytes=` is case-sensitive and
// numbers are plain digits.
#[test]
fn range_safelist_is_strict() {
    let ok = |v: &str| cors::is_safelisted_request_header("Range", v.as_bytes());
    assert!(ok("bytes=0-"));
    assert!(ok("bytes=0-10"));
    assert!(!ok("BYTES=0-"));
    assert!(!ok("bytes=0-+5"));
    assert!(!ok("bytes=-5"));
    assert!(!ok("bytes=10-5"));
    assert!(!ok("bytes=0-1,3-4"));
    assert!(!ok("bytes= 0-1"));
}

// F-CORS-2: Fetch "append a request Origin header" serialises `null` for
// non-CORS, non-GET/HEAD requests under privacy-preserving referrer policies.
#[test]
fn origin_header_value_follows_referrer_policy() {
    use ReferrerPolicy as P;
    let r = creq!("POST", "https://a.test/", "http://b.test/", RequestMode::NoCors);
    let v = |p| cors::origin_header_value(&r, Tainting::Opaque, p);
    assert_eq!(v(P::NoReferrer).as_deref(), Some("null"));
    assert_eq!(v(P::StrictOrigin).as_deref(), Some("null"));
    assert_eq!(v(P::StrictOriginWhenCrossOrigin).as_deref(), Some("null"));
    assert_eq!(v(P::NoReferrerWhenDowngrade).as_deref(), Some("null"));
    assert_eq!(v(P::SameOrigin).as_deref(), Some("null"));
    assert_eq!(v(P::UnsafeUrl).as_deref(), Some("https://a.test"));
    assert_eq!(v(P::Origin).as_deref(), Some("https://a.test"));
    // CORS-mode requests always carry the real origin.
    let c = creq!("POST", "https://a.test/", "http://b.test/", RequestMode::Cors);
    assert_eq!(
        cors::origin_header_value(&c, Tainting::Cors, P::NoReferrer).as_deref(),
        Some("https://a.test")
    );
    // GET same-origin: no header.
    let g = creq!("GET", "https://a.test/", "https://a.test/x", RequestMode::Cors);
    assert_eq!(cors::origin_header_value(&g, Tainting::Basic, P::UnsafeUrl), None);
    // https -> https same-site no-cors POST under strict-origin keeps origin.
    let s = creq!("POST", "https://a.test/", "https://b.test/", RequestMode::NoCors);
    assert_eq!(
        cors::origin_header_value(&s, Tainting::Opaque, P::StrictOrigin).as_deref(),
        Some("https://a.test")
    );
}

// F-REF-1: legacy <meta name=referrer> keywords.
#[test]
fn meta_referrer_legacy_keywords() {
    use ReferrerPolicy as P;
    let m = referrer::parse_meta_content;
    assert_eq!(m("never"), Some(P::NoReferrer));
    assert_eq!(m("NEVER"), Some(P::NoReferrer));
    assert_eq!(m("always"), Some(P::UnsafeUrl));
    assert_eq!(m("origin-when-crossorigin"), Some(P::OriginWhenCrossOrigin));
    assert_eq!(m("default"), Some(P::NoReferrerWhenDowngrade));
    assert_eq!(m("no-referrer"), Some(P::NoReferrer));
    assert_eq!(m("bogus"), None);
    // The attribute form does not accept legacy keywords.
    assert_eq!(referrer::parse_attribute("never"), None);
}

// F-COEP-1: a nested navigation under a COEP parent must itself opt in.
#[test]
fn nested_navigation_must_opt_in_to_coep() {
    use Coep::*;
    let c = coop::coep_navigation_check;
    assert_eq!(c(RequireCorp, UnsafeNone), CorpVerdict::Blocked);
    assert_eq!(c(Credentialless, UnsafeNone), CorpVerdict::Blocked);
    assert_eq!(c(RequireCorp, RequireCorp), CorpVerdict::Allowed);
    assert_eq!(c(RequireCorp, Credentialless), CorpVerdict::Allowed);
    assert_eq!(c(UnsafeNone, UnsafeNone), CorpVerdict::Allowed);
    assert_eq!(c(UnsafeNone, RequireCorp), CorpVerdict::Allowed);
}

// F-COEP-2: SF parameters: a later non-string report-to replaces the
// earlier string; a malformed bare parameter value is a parse failure.
#[test]
fn coep_params_follow_structured_fields() {
    let p = coop::parse_coep(Some("require-corp; report-to=\"a\"; report-to=b"));
    assert_eq!(p.value, Coep::RequireCorp);
    assert_eq!(p.report_to, None);
    assert_eq!(
        coop::parse_coep(Some("require-corp; x=1x")).value,
        Coep::UnsafeNone
    );
    assert_eq!(
        coop::parse_coep(Some("require-corp; x=1.5; y=?1; z=tok")).value,
        Coep::RequireCorp
    );
    assert_eq!(
        coop::parse_coep(Some("require-corp; x=?2")).value,
        Coep::UnsafeNone
    );
}

// F-REPORT-1: cross-origin-redirect blocked-uri is an origin serialisation
// (default port omitted).
#[test]
fn redirect_blocked_uri_omits_default_port() {
    let doc = cu!("https://a.test/");
    let pol = enforce!("img-src 'none'");
    let b = Blocked {
        effective_directive: "img-src",
        violated_directive: "img-src",
    };
    for (target, want) in [
        ("https://b.test/secret/path", "https://b.test"),
        ("https://b.test:8443/x", "https://b.test:8443"),
        ("http://b.test:80/x", "http://b.test"),
    ] {
        let tgt = cu!(target);
        let ctx = ViolationContext {
            document_url: &doc,
            referrer: None,
            blocked: BlockedResource::UrlAfterCrossOriginRedirect(&tgt),
            status_code: 200,
            position: None,
            sample: None,
        };
        assert_eq!(Violation::new(&pol, b, &ctx).blocked_url, want);
    }
}
