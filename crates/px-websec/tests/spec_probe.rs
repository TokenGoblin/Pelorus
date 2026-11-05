//! Second-tranche adversarial probes: parsing edge cases and matching rules
//! checked against CSP3 / HTML / Fetch / Mixed Content / Referrer Policy.

use px_websec::csp::{
    CspUrl, Disposition, InlineType, Origin, Policy, PolicySet, PolicySource, RequestKind,
    RequestOptions, SandboxAllow, check_inline, check_request, parse_policy_list,
};
use px_websec::frame::{self, XfoVerdict};
use px_websec::mixed::{self, Context, ContextNode};
use px_websec::origin::{Origin as WOrigin, Url as WUrl};
use px_websec::referrer::{self, ReferrerPolicy};

macro_rules! pol {
    ($s:expr) => {
        Policy::parse($s.as_bytes(), Disposition::Enforce, PolicySource::Header).unwrap()
    };
}
macro_rules! cu {
    ($s:expr) => {
        CspUrl::parse($s).unwrap()
    };
}

fn blocked(policy: &str, kind: RequestKind, url: &str) -> bool {
    let p = pol!(policy);
    let o = Origin::from_url_str("https://page.test/");
    check_request(&p, kind, &cu!(url), &o, &RequestOptions::default()).is_blocked()
}

#[test]
fn directive_names_and_keywords_case_insensitive() {
    assert!(blocked(
        "IMG-SRC 'NONE'",
        RequestKind::Image,
        "https://a.test/x.png"
    ));
    assert!(!blocked(
        "img-src HTTPS://A.TEST",
        RequestKind::Image,
        "https://a.test/x.png"
    ));
    assert!(!blocked(
        "img-src 'SELF'",
        RequestKind::Image,
        "https://page.test/x.png"
    ));
    assert!(!blocked(
        "img-src HTTPS:",
        RequestKind::Image,
        "https://a.test/x.png"
    ));
}

#[test]
fn duplicate_directive_first_wins_even_across_case() {
    assert!(blocked(
        "img-src 'none'; IMG-SRC *",
        RequestKind::Image,
        "https://a.test/x.png"
    ));
    // But separate policies in one list are independent and both apply.
    let ps = PolicySet::from_headers(&[b"img-src *, img-src 'none'"], &[]).unwrap();
    let o = Origin::from_url_str("https://page.test/");
    let v = ps.check_request(
        RequestKind::Image,
        &cu!("https://a.test/x.png"),
        &o,
        &RequestOptions::default(),
    );
    assert!(v.is_blocked());
}

#[test]
fn control_characters_are_not_separators() {
    // VT, NUL, DEL, NBSP inside a token do not split it.
    let p = pol!("img-src a.test\x0bb.test");
    assert_eq!(p.directive("img-src").unwrap().values, ["a.test\x0bb.test"]);
    assert!(blocked(
        "img-src a.test\x0bb.test",
        RequestKind::Image,
        "https://a.test/"
    ));
    assert!(!blocked(
        "img-src 'none'\0 *",
        RequestKind::Image,
        "https://x.test/"
    ));
    // a NUL in the *name* makes it an unknown (skipped) directive, so the
    // real one after it still works.
    assert!(blocked(
        "img\0-src *; img-src 'none'",
        RequestKind::Image,
        "https://x.test/"
    ));
    // CR/LF/TAB/FF are whitespace.
    assert!(blocked(
        "img-src\r\n\t\x0c'none'",
        RequestKind::Image,
        "https://x.test/"
    ));
}

#[test]
fn comma_in_header_splits_policies_each_enforced() {
    let v = parse_policy_list(b"img-src a.test,img-src b.test", Disposition::Enforce).unwrap();
    assert_eq!(v.len(), 2);
}

#[test]
fn wildcard_port_scheme_rules() {
    // bare * does not match data:/blob:/javascript:
    assert!(blocked(
        "img-src *",
        RequestKind::Image,
        "data:image/png;base64,AA"
    ));
    assert!(blocked(
        "img-src *",
        RequestKind::Image,
        "blob:https://page.test/u"
    ));
    assert!(!blocked(
        "img-src *",
        RequestKind::Image,
        "https://x.test:8443/a"
    ));
    // *.example.com excludes the apex and IP literals, and requires a dot
    assert!(blocked(
        "img-src *.example.com",
        RequestKind::Image,
        "https://example.com/"
    ));
    assert!(blocked(
        "img-src *.example.com",
        RequestKind::Image,
        "https://evilexample.com/"
    ));
    assert!(blocked(
        "img-src *.example.com",
        RequestKind::Image,
        "https://example.com.evil.test/"
    ));
    assert!(!blocked(
        "img-src *.example.com",
        RequestKind::Image,
        "https://a.b.example.com/"
    ));
    // port rules
    assert!(blocked(
        "img-src example.com",
        RequestKind::Image,
        "https://example.com:8443/"
    ));
    assert!(!blocked(
        "img-src example.com:*",
        RequestKind::Image,
        "https://example.com:8443/"
    ));
    assert!(!blocked(
        "img-src example.com:8443",
        RequestKind::Image,
        "https://example.com:8443/"
    ));
    assert!(blocked(
        "img-src example.com:8443",
        RequestKind::Image,
        "https://example.com/"
    ));
    assert!(!blocked(
        "img-src example.com:443",
        RequestKind::Image,
        "https://example.com/"
    ));
    // scheme-less host from an https page does not match http
    assert!(blocked(
        "img-src example.com",
        RequestKind::Image,
        "http://example.com/"
    ));
    // http: source upgrades to https, https: source does not match http
    assert!(!blocked(
        "img-src http://example.com",
        RequestKind::Image,
        "https://example.com/"
    ));
    assert!(blocked(
        "img-src https://example.com",
        RequestKind::Image,
        "http://example.com/"
    ));
    assert!(blocked(
        "img-src https:",
        RequestKind::Image,
        "http://example.com/"
    ));
    // path-part: exact vs prefix, case-sensitive, percent-decoded
    assert!(!blocked(
        "img-src example.com/a/",
        RequestKind::Image,
        "https://example.com/a/b"
    ));
    assert!(blocked(
        "img-src example.com/a/",
        RequestKind::Image,
        "https://example.com/A/b"
    ));
    assert!(blocked(
        "img-src example.com/a",
        RequestKind::Image,
        "https://example.com/a/b"
    ));
    assert!(blocked(
        "img-src example.com/a/",
        RequestKind::Image,
        "https://example.com/a/../b"
    ));
    assert!(!blocked(
        "img-src example.com/a%2Fb",
        RequestKind::Image,
        "https://example.com/a%2fb"
    ));
    assert!(blocked(
        "img-src example.com/a/b",
        RequestKind::Image,
        "https://example.com/a%2fb"
    ));
    // userinfo / backslash tricks in the URL do not change the host
    assert!(blocked(
        "img-src good.test",
        RequestKind::Image,
        "https://good.test@evil.test/"
    ));
    assert!(blocked(
        "img-src good.test",
        RequestKind::Image,
        "https://evil.test\\@good.test/"
    ));
}

#[test]
fn self_semantics() {
    let o = Origin::from_url_str("http://page.test/");
    let p = pol!("img-src 'self'");
    let chk = |u: &str| {
        check_request(
            &p,
            RequestKind::Image,
            &cu!(u),
            &o,
            &RequestOptions::default(),
        )
        .is_blocked()
    };
    assert!(!chk("http://page.test/x"));
    assert!(!chk("https://page.test/x"));
    assert!(!chk("http://page.test:80/x"));
    assert!(chk("http://page.test:8080/x"));
    assert!(chk("https://page.test:8443/x"));
    assert!(chk("http://sub.page.test/x"));
    assert!(chk("data:,x"));
    // opaque origin: 'self' matches nothing
    let p2 = pol!("img-src 'self'");
    let opaque = Origin::Opaque;
    assert!(
        check_request(
            &p2,
            RequestKind::Image,
            &cu!("https://page.test/"),
            &opaque,
            &RequestOptions::default()
        )
        .is_blocked()
    );
    // https page: http is not allowed by 'self'
    let o2 = Origin::from_url_str("https://page.test/");
    assert!(
        check_request(
            &p2,
            RequestKind::Image,
            &cu!("http://page.test/"),
            &o2,
            &RequestOptions::default()
        )
        .is_blocked()
    );
}

#[test]
fn nonce_hash_rules() {
    let p = pol!("script-src 'nonce-AbC' 'unsafe-inline'");
    assert!(!check_inline(&p, InlineType::Script, Some("AbC"), "x").is_blocked());
    assert!(check_inline(&p, InlineType::Script, Some("abc"), "x").is_blocked());
    assert!(check_inline(&p, InlineType::Script, Some(""), "x").is_blocked());
    // nonce does not apply to attributes
    assert!(check_inline(&p, InlineType::ScriptAttribute, Some("AbC"), "x").is_blocked());
    // empty nonce source `'nonce-'` is not a nonce: unsafe-inline stays
    let q = pol!("script-src 'nonce-' 'unsafe-inline'");
    assert!(!check_inline(&q, InlineType::Script, None, "x").is_blocked());
    // hash of "" (sha256) allows inline with that text only
    let h = pol!("script-src 'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='");
    assert!(!check_inline(&h, InlineType::Script, None, "").is_blocked());
    assert!(check_inline(&h, InlineType::Script, None, " ").is_blocked());
    // strict-dynamic + unsafe-inline: inline script blocked
    let sd = pol!("script-src 'strict-dynamic' 'unsafe-inline'");
    assert!(check_inline(&sd, InlineType::Script, None, "x").is_blocked());
}

#[test]
fn default_src_fallback_and_missing_directives() {
    assert!(blocked(
        "default-src 'none'",
        RequestKind::Worker,
        "https://page.test/w.js"
    ));
    assert!(!blocked(
        "default-src 'none'; worker-src 'self'",
        RequestKind::Worker,
        "https://page.test/w.js"
    ));
    // child-src governs workers and frames when specific ones are absent
    assert!(!blocked(
        "default-src 'none'; child-src 'self'",
        RequestKind::Iframe,
        "https://page.test/f"
    ));
    assert!(!blocked(
        "default-src 'none'; child-src 'self'",
        RequestKind::Worker,
        "https://page.test/w.js"
    ));
    // script-src sits between child-src and default-src for workers
    assert!(!blocked(
        "default-src 'none'; script-src 'self'",
        RequestKind::Worker,
        "https://page.test/w.js"
    ));
    // empty directive value == 'none'
    assert!(blocked(
        "img-src",
        RequestKind::Image,
        "https://page.test/a"
    ));
    // frame-src does not fall back to script-src
    assert!(!blocked(
        "script-src 'none'",
        RequestKind::Iframe,
        "https://a.test/"
    ));
}

macro_rules! node {
    ($s:expr) => {{
        let url = WUrl::parse($s).unwrap();
        ContextNode {
            origin: WOrigin::from_url(&url),
            url,
        }
    }};
}

#[test]
fn mixed_content_edge_urls() {
    let ctx = Context {
        document: node!("https://a.test/"),
        ancestors: vec![],
    };
    let chk = |k, u: &str| mixed::check_request(&ctx, k, &WUrl::parse(u).unwrap());
    use mixed::{RequestKind as K, Verdict};
    assert_eq!(chk(K::Script, "http://b.test/x.js"), Verdict::Block);
    assert_eq!(chk(K::Script, "https://b.test/x.js"), Verdict::Allow);
    assert_eq!(chk(K::Script, "http://localhost/x.js"), Verdict::Allow);
    assert_eq!(
        chk(K::Script, "http://localhost.evil.test/x.js"),
        Verdict::Block
    );
    assert_eq!(chk(K::WebSocket, "ws://b.test/"), Verdict::Block);
    assert_eq!(chk(K::Fetch, "http://127.0.0.1.evil.test/"), Verdict::Block);
    // blob: of an insecure origin must not be trusted
    assert_eq!(chk(K::Script, "blob:http://evil.test/uuid"), Verdict::Block);
    assert_eq!(chk(K::Script, "blob:https://b.test/uuid"), Verdict::Allow);
    assert_eq!(
        chk(K::Script, "filesystem:http://evil.test/temporary/x"),
        Verdict::Block
    );
    assert!(matches!(
        chk(K::Image, "http://b.test/i.png"),
        Verdict::Upgrade(_)
    ));
    assert_eq!(chk(K::ImageSet, "http://b.test/i.png"), Verdict::Block);
    // http document, no ancestors secure: nothing to prohibit
    let insecure = Context {
        document: node!("http://a.test/"),
        ancestors: vec![],
    };
    assert_eq!(
        mixed::check_request(
            &insecure,
            K::Script,
            &WUrl::parse("http://b.test/").unwrap()
        ),
        Verdict::Allow
    );
    // insecure frame under a secure parent is still prohibited
    let nested = Context {
        document: node!("http://a.test/"),
        ancestors: vec![node!("https://top.test/")],
    };
    assert_eq!(
        mixed::check_request(&nested, K::Script, &WUrl::parse("http://b.test/").unwrap()),
        Verdict::Block
    );
}

#[test]
fn xfo_edge_cases() {
    let o = |s: &str| WOrigin::from_url(&WUrl::parse(s).unwrap());
    let resp = o("https://a.test/");
    let anc = [o("https://evil.test/")];
    let x = |v: &[&str]| frame::check_xfo(v, &resp, &anc);
    assert_eq!(x(&["DENY"]), XfoVerdict::Blocked);
    assert_eq!(x(&["deny, deny"]), XfoVerdict::Blocked);
    assert_eq!(x(&["sameorigin", "deny"]), XfoVerdict::Blocked);
    assert_eq!(
        x(&["deny", "allow-from https://evil.test"]),
        XfoVerdict::Blocked
    );
    assert_eq!(x(&["DENY,"]), XfoVerdict::Blocked);
    assert_eq!(x(&["\tDENY \r\n"]), XfoVerdict::Blocked);
    assert_eq!(x(&["allow-from https://evil.test"]), XfoVerdict::Allowed);
    assert_eq!(x(&["\"deny\""]), XfoVerdict::Allowed);
    assert_eq!(x(&["deny\x0b"]), XfoVerdict::Allowed);
    assert_eq!(x(&[]), XfoVerdict::Allowed);
    assert_eq!(x(&[""]), XfoVerdict::Allowed);
    // top-level navigations are unaffected
    assert_eq!(frame::check_xfo(&["deny"], &resp, &[]), XfoVerdict::Allowed);
}

#[test]
fn sandbox_attribute_edge_cases() {
    let f = |s: &str| frame::parse_sandbox_attribute(Some(s)).unwrap();
    assert!(frame::parse_sandbox_attribute(None).is_none());
    assert!(!f("").allows_scripts());
    assert!(f("ALLOW-SCRIPTS").allows_scripts());
    assert!(f("\tallow-scripts\n").allows_scripts());
    assert!(f("allow-scripts\x0c").allows_scripts());
    // vertical tab, NBSP and comma are not separators
    assert!(!f("allow-scripts\x0b").allows_scripts());
    assert!(!f("allow-scripts,allow-forms").allows_scripts());
    assert!(!f("allow-scripts\u{a0}allow-forms").allows_scripts());
    assert!(!f("allow-scriptsx").allows_scripts());
    assert!(f("bogus allow-forms").allows(SandboxAllow::Forms));
    // a truncated oversized value only ever loses permissions
    let big = format!("{} allow-scripts", "x ".repeat(40_000));
    assert!(!f(&big).allows_scripts());
    // multibyte char straddling the cut must not panic nor grant anything
    let mb = format!("{}\u{e9}allow-scripts", "a".repeat(16_383));
    assert!(!f(&mb).allows_scripts());
}

#[test]
fn referrer_header_edge_cases() {
    use ReferrerPolicy as P;
    assert_eq!(
        referrer::parse_header("no-referrer,unsafe-url"),
        Some(P::UnsafeUrl)
    );
    assert_eq!(
        referrer::parse_header("unsafe-url,bogus"),
        Some(P::UnsafeUrl)
    );
    assert_eq!(referrer::parse_header("unsafe-url\x0b"), None);
    assert_eq!(referrer::parse_header("no-referrer\0"), None);
    let u = |s: &str| WUrl::parse(s).unwrap();
    // fragment and credentials never leak
    assert_eq!(
        referrer::compute(
            Some(&u("https://user:pw@a.test/p?q=1#frag")),
            P::UnsafeUrl,
            &u("https://b.test/")
        )
        .as_deref(),
        Some("https://a.test/p?q=1")
    );
    // downgrade
    assert_eq!(
        referrer::compute(
            Some(&u("https://a.test/p")),
            P::StrictOriginWhenCrossOrigin,
            &u("http://b.test/")
        ),
        None
    );
    assert_eq!(
        referrer::compute(
            Some(&u("https://a.test/p")),
            P::NoReferrerWhenDowngrade,
            &u("http://localhost/")
        )
        .as_deref(),
        Some("https://a.test/p")
    );
    // oversize falls back to origin
    let long = format!("https://a.test/{}", "p".repeat(5000));
    assert_eq!(
        referrer::compute(Some(&u(&long)), P::UnsafeUrl, &u("https://b.test/")).as_deref(),
        Some("https://a.test/")
    );
    // data:/about: sources never leak
    assert_eq!(
        referrer::compute(
            Some(&u("data:text/html,x")),
            P::UnsafeUrl,
            &u("https://b.test/")
        ),
        None
    );
}
