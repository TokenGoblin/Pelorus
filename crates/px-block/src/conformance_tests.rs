//! Conformance to the Adblock Plus filter-syntax documentation.
//!
//! The ABP documentation examples below are reproduced from the published
//! "Writing Adblock Plus filters" text as remembered by the author: the
//! upstream repository could not be cloned from the build sandbox (no GitHub
//! access for that repo), so these were NOT diffed against a fresh copy of the
//! docs or of `adblockpluscore`'s test fixtures. The lists in the second half
//! are EasyList-*style* samples written by hand; no real list is vendored.

use crate::filter::{ResourceType, parse_list};
use crate::matcher::{Matcher, Request, RuleId, Verdict, check_request, is_third_party};
use px_websec::origin::{Origin, Url};

fn psl(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    let n = if host.ends_with(".co.uk") { 3 } else { 2 };
    (labels.len() >= n).then(|| labels[labels.len() - n..].join("."))
}

fn verdict(m: &Matcher, page: Option<&str>, url: &str, ty: ResourceType) -> Verdict {
    let u = Url::parse(url).unwrap();
    let src = page.map(|p| Origin::from_url(&Url::parse(p).unwrap()));
    check_request(
        m,
        &Request {
            url: &u,
            source_origin: src.as_ref(),
            resource_type: ty,
            is_third_party: is_third_party(&u, src.as_ref(), &psl),
        },
    )
}

/// Assert `filter` blocks every URL in `yes` and none in `no`.
fn doc_example(filter: &str, yes: &[&str], no: &[&str]) {
    let m = Matcher::from_list("doc", parse_list(filter));
    assert_eq!(m.len(), 1, "{filter} did not parse as one rule");
    for u in yes {
        assert!(
            matches!(
                verdict(&m, None, u, ResourceType::Other),
                Verdict::Block { .. }
            ),
            "{filter} should block {u}"
        );
    }
    for u in no {
        assert_eq!(
            verdict(&m, None, u, ResourceType::Other),
            Verdict::Allow,
            "{filter} should not block {u}"
        );
    }
}

#[test]
fn abp_doc_basic_filter_rules() {
    // "Basic filter rules": substring match anywhere in the address.
    doc_example(
        "/banner/*/img^",
        &[
            "http://example.com/banner/foo/img",
            "http://example.com/banner/foo/bar/img?param",
            "http://example.com/banner//img/foo",
        ],
        &[
            "http://example.com/banner/img",
            "http://example.com/banner/foo/imgraph",
            "http://example.com/banner/foo/img.gif",
        ],
    );
}

#[test]
fn abp_doc_separator_examples() {
    // "Separator placeholders": `http://example.com^`.
    doc_example(
        "http://example.com^",
        &[
            "http://example.com/",
            "http://example.com:8000/",
            "http://example.com",
        ],
        &["http://example.com.ar/", "http://example.com-foo/"],
    );
}

#[test]
fn abp_doc_domain_name_anchor() {
    // "Specifying filter rules for a domain name": `||ads.example.com^`.
    doc_example(
        "||ads.example.com^",
        &[
            "http://ads.example.com/foo.gif",
            "http://server1.ads.example.com/foo.gif",
            "https://ads.example.com:8000/",
        ],
        &[
            "http://ads.example.com.ua/foo.gif",
            "http://example.com/redirect/http://ads.example.com/",
        ],
    );
}

#[test]
fn abp_doc_anchors() {
    // "Anchors": `|http://baddomain.example/`.
    doc_example(
        "|http://baddomain.example/",
        &["http://baddomain.example/banner.gif"],
        &["http://gooddomain.example/analyze?http://baddomain.example"],
    );
    // `swf|`.
    doc_example(
        "swf|",
        &["http://example.com/annoyingflash.swf"],
        &["http://example.com/swf/index.html"],
    );
    // Both anchors: `|http://example.com/|`.
    doc_example(
        "|http://example.com/|",
        &["http://example.com/"],
        &[
            "http://example.com/foo.gif",
            "http://example.info/redirect/http://example.com/",
        ],
    );
}

#[test]
fn abp_doc_exceptions() {
    // "Exception rules": `@@||example.com/ads/good.gif` style.
    let m = Matcher::from_list(
        "doc",
        parse_list("||example.com/ads^\n@@||example.com/ads/good.gif\n"),
    );
    assert_eq!(
        verdict(
            &m,
            None,
            "http://example.com/ads/bad.gif",
            ResourceType::Image
        ),
        Verdict::Block { rule_id: RuleId(0) }
    );
    assert_eq!(
        verdict(
            &m,
            None,
            "http://example.com/ads/good.gif",
            ResourceType::Image
        ),
        Verdict::AllowedByException { rule_id: RuleId(1) }
    );
}

#[test]
fn abp_doc_filter_options() {
    // `*/ads/*$script,domain=example.com|foo.example.com` (documented shape).
    let m = Matcher::from_list(
        "doc",
        parse_list("/ads/$script,domain=example.com|foo.example.com\n"),
    );
    // ("/ads/" alone is a regex per ABP; with options it is still a path
    //  pattern only when it does not end in a slash.)
    assert_eq!(m.len(), 0);
    assert_eq!(m.untokenized_len(), 0);

    let m = Matcher::from_list(
        "doc",
        parse_list("*/ads/*$script,domain=example.com|~foo.example.com\n"),
    );
    let page = Some("http://example.com/");
    assert!(matches!(
        verdict(&m, page, "http://x.net/ads/a.js", ResourceType::Script),
        Verdict::Block { .. }
    ));
    assert_eq!(
        verdict(&m, page, "http://x.net/ads/a.png", ResourceType::Image),
        Verdict::Allow
    );
    assert_eq!(
        verdict(
            &m,
            Some("http://foo.example.com/"),
            "http://x.net/ads/a.js",
            ResourceType::Script
        ),
        Verdict::Allow
    );
    assert_eq!(
        verdict(
            &m,
            Some("http://other.org/"),
            "http://x.net/ads/a.js",
            ResourceType::Script
        ),
        Verdict::Allow
    );
}

#[test]
fn abp_doc_third_party_and_match_case() {
    let m = Matcher::from_list("doc", parse_list("||ads.example.com^$third-party\n"));
    assert!(matches!(
        verdict(
            &m,
            Some("http://news.org/"),
            "http://ads.example.com/a",
            ResourceType::Image
        ),
        Verdict::Block { .. }
    ));
    assert_eq!(
        verdict(
            &m,
            Some("http://example.com/"),
            "http://ads.example.com/a",
            ResourceType::Image
        ),
        Verdict::Allow
    );
    // match-case: "Banner" differs from "banner".
    doc_example(
        "/BannerAd.gif$match-case",
        &["http://x.com/BannerAd.gif"],
        &["http://x.com/bannerad.gif"],
    );
}

#[test]
fn abp_doc_comments_and_regex() {
    let l = parse_list("[Adblock Plus 2.0]\n! Checksum: abc\n! Title: x\n/banner\\d+/\n");
    assert_eq!(l.stats.headers, 1);
    assert_eq!(l.stats.comments, 2);
    assert_eq!(l.stats.regex_rejected(), 1);
    assert_eq!(l.rules.len(), 0);
}

const EASYLIST_STYLE: &str = "\
[Adblock Plus 2.0]
! Title: Hand-written EasyList-style sample (not a real list)
! Expires: 4 days
-ad-banner-
-advertisement.
/adframe.
/ads/banner*
||adserver.example^$third-party
||doubleclick-like.example^
||track.example/pixel.gif?
||metrics.example^$image,ping,xmlhttprequest
@@||adserver.example/allowed/
@@||example.org^$script,domain=example.org
|http://ads.legacy.example/
.swf?ad=
&adtype=
##.ad-banner
example.org###ad
~example.org##.sponsored
example.org#?#div:has-text(Sponsored)
##+js(set-constant, adblock, false)
/\\/ad[0-9]+\\.js/
||ads.example^$redirect=1x1.gif
||cdn.example^$important,third-party
@@||cdn.example^$domain=trusted.example
";

fn easylist() -> Matcher {
    Matcher::from_list("easylist-style", parse_list(EASYLIST_STYLE))
}

#[test]
fn easylist_style_counts() {
    let l = parse_list(EASYLIST_STYLE);
    let s = &l.stats;
    assert_eq!(s.headers, 1);
    assert_eq!(s.comments, 2);
    assert_eq!(s.cosmetic_skipped, 5);
    assert_eq!(s.scriptlet_skipped, 1);
    assert_eq!(s.regex_rejected(), 1);
    assert_eq!(s.rejected_total(), 2); // regex + $redirect
    assert_eq!(s.accepted, 15);
    assert_eq!(s.exceptions, 3);
    assert_eq!(
        s.lines,
        s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
    );
}

#[test]
fn easylist_style_verdicts() {
    let m = easylist();
    let page = Some("https://news.org/");
    let v = |url: &str, ty| verdict(&m, page, url, ty);
    let blocked = |x: Verdict| matches!(x, Verdict::Block { .. });

    // Plain substring filters with no anchors.
    assert!(blocked(v(
        "https://x.net/a-ad-banner-1.png",
        ResourceType::Image
    )));
    assert!(blocked(v(
        "https://x.net/img/adframe.html",
        ResourceType::Subdocument
    )));
    assert!(blocked(v(
        "https://x.net/ads/banner300.gif",
        ResourceType::Image
    )));
    assert_eq!(
        v("https://x.net/ads/other.gif", ResourceType::Image),
        Verdict::Allow
    );
    // `&adtype=` and `.swf?ad=` query filters.
    assert!(blocked(v(
        "https://x.net/p?a=1&adtype=3",
        ResourceType::Other
    )));
    assert!(blocked(v("https://x.net/m.swf?ad=1", ResourceType::Other)));
    // Third-party-only host rule.
    assert!(blocked(v(
        "https://adserver.example/x.js",
        ResourceType::Script
    )));
    assert_eq!(
        verdict(
            &m,
            Some("https://adserver.example/"),
            "https://adserver.example/x.js",
            ResourceType::Script
        ),
        Verdict::Allow
    );
    // Exception carves out a path.
    assert!(matches!(
        v(
            "https://adserver.example/allowed/a.js",
            ResourceType::Script
        ),
        Verdict::AllowedByException { .. }
    ));
    // Pixel with query.
    assert!(blocked(v(
        "https://track.example/pixel.gif?id=9",
        ResourceType::Image
    )));
    assert_eq!(
        v("https://track.example/pixel.gif", ResourceType::Image),
        Verdict::Allow
    );
    // Type-restricted.
    assert!(blocked(v("https://metrics.example/c", ResourceType::Ping)));
    assert_eq!(
        v("https://metrics.example/c", ResourceType::Script),
        Verdict::Allow
    );
    // Start anchor.
    assert!(blocked(v(
        "http://ads.legacy.example/b.png",
        ResourceType::Image
    )));
    assert_eq!(
        v(
            "https://x.net/?r=http://ads.legacy.example/",
            ResourceType::Image
        ),
        Verdict::Allow
    );
    // Rejected lines contribute nothing.
    assert_eq!(
        v("https://ads.example/x.gif", ResourceType::Image),
        Verdict::Allow
    );
    assert_eq!(
        v("https://x.net/ad123.js", ResourceType::Script),
        Verdict::Allow
    );
    // Important beats a domain-scoped exception only where the exception
    // does not apply; here the exception applies on trusted.example but an
    // `$important` block cannot be overridden by any exception.
    assert!(blocked(verdict(
        &m,
        Some("https://trusted.example/"),
        "https://cdn.example/lib.js",
        ResourceType::Script
    )));
    assert!(blocked(v(
        "https://cdn.example/lib.js",
        ResourceType::Script
    )));
    assert_eq!(
        verdict(
            &m,
            Some("https://example.org/"),
            "https://example.org/a.js",
            ResourceType::Script
        ),
        Verdict::AllowedByException { rule_id: RuleId(9) }
    );
}

#[test]
fn rule_ids_follow_accepted_rule_order() {
    let m = easylist();
    assert_eq!(m.len(), 15);
    let info = m.rule_info(RuleId(0)).unwrap();
    assert_eq!(info.text, "-ad-banner-");
    assert_eq!(info.line, 4);
    let last = m.rule_info(RuleId(14)).unwrap();
    assert_eq!(last.text, "@@||cdn.example^$domain=trusted.example");
}
