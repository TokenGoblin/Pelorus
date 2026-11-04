//! Matcher behaviour through the public API. Expectations are derived by hand
//! from the Adblock Plus filter semantics documented in the crate.

use px_block::filter::{ResourceType, parse_list};
use px_block::matcher::{Matcher, Request, RuleId, Verdict, check_request, is_third_party};
use px_websec::origin::{Origin, Url};

fn psl(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    let n = if host.ends_with(".co.uk") { 3 } else { 2 };
    if labels.len() < n {
        return None;
    }
    Some(labels[labels.len() - n..].join("."))
}

fn matcher(list: &str) -> Matcher {
    Matcher::from_list("test", parse_list(list))
}

fn check(m: &Matcher, page: Option<&str>, url: &str, ty: ResourceType) -> Verdict {
    let u = Url::parse(url).unwrap();
    let src = page.map(|p| Origin::from_url(&Url::parse(p).unwrap()));
    let tp = is_third_party(&u, src.as_ref(), &psl);
    check_request(
        m,
        &Request {
            url: &u,
            source_origin: src.as_ref(),
            resource_type: ty,
            is_third_party: tp,
        },
    )
}

const BLOCK0: Verdict = Verdict::Block { rule_id: RuleId(0) };

#[test]
fn host_anchored_block() {
    let m = matcher("||ads.example.com^\n");
    let p = Some("https://news.org/");
    assert_eq!(check(&m, p, "https://ads.example.com/x.js", ResourceType::Script), BLOCK0);
    assert_eq!(check(&m, p, "https://sub.ads.example.com/", ResourceType::Image), BLOCK0);
    assert_eq!(check(&m, p, "https://example.com/", ResourceType::Image), Verdict::Allow);
    assert_eq!(check(&m, p, "https://xads.example.com/", ResourceType::Image), Verdict::Allow);
    assert_eq!(
        check(&m, p, "https://news.org/?u=https://ads.example.com/", ResourceType::Image),
        Verdict::Allow
    );
    assert_eq!(check(&m, p, "http://ads.example.com:8080/a", ResourceType::Other), BLOCK0);
}

#[test]
fn exception_overrides_block() {
    let m = matcher("||tracker.net^\n@@||tracker.net/ok.js^\n");
    let p = Some("https://a.com/");
    assert_eq!(check(&m, p, "https://tracker.net/a.js", ResourceType::Script), BLOCK0);
    assert_eq!(
        check(&m, p, "https://tracker.net/ok.js", ResourceType::Script),
        Verdict::AllowedByException { rule_id: RuleId(1) }
    );
}

#[test]
fn exception_with_nothing_to_except_still_reports_exception() {
    let m = matcher("@@||fine.net^\n");
    assert_eq!(
        check(&m, None, "https://fine.net/", ResourceType::Script),
        Verdict::AllowedByException { rule_id: RuleId(0) }
    );
}

#[test]
fn important_beats_exception_and_exception_beats_block() {
    // ids: 0 exception, 1 important block, 2 plain block
    let m = matcher("@@||t.com^\n||t.com^$important\n||t.com^\n");
    assert_eq!(
        check(&m, None, "https://t.com/", ResourceType::Image),
        Verdict::Block { rule_id: RuleId(1) }
    );
    let m = matcher("||t.com^\n@@||t.com^\n");
    assert_eq!(
        check(&m, None, "https://t.com/", ResourceType::Image),
        Verdict::AllowedByException { rule_id: RuleId(1) }
    );
}

#[test]
fn lowest_matching_id_is_reported() {
    let m = matcher("||a.com/x^\n||a.com^\n/x^\n");
    assert_eq!(check(&m, None, "https://a.com/x/y", ResourceType::Image), BLOCK0);
    assert_eq!(
        check(&m, None, "https://a.com/z", ResourceType::Image),
        Verdict::Block { rule_id: RuleId(1) }
    );
}

#[test]
fn third_party_option() {
    let m = matcher("||cdn.net^$third-party\n||own.org^$~third-party\n");
    // cdn.net requested by a.com: third-party -> blocked.
    assert_eq!(
        check(&m, Some("https://a.com/"), "https://cdn.net/x", ResourceType::Script),
        BLOCK0
    );
    // cdn.net asking for itself, and its own subdomain: first-party -> allowed.
    assert_eq!(
        check(&m, Some("https://cdn.net/"), "https://cdn.net/x", ResourceType::Script),
        Verdict::Allow
    );
    assert_eq!(
        check(&m, Some("https://www.cdn.net/"), "https://static.cdn.net/x", ResourceType::Script),
        Verdict::Allow
    );
    // ~third-party rule blocks only first-party.
    assert_eq!(
        check(&m, Some("https://own.org/"), "https://own.org/x", ResourceType::Script),
        Verdict::Block { rule_id: RuleId(1) }
    );
    assert_eq!(
        check(&m, Some("https://a.com/"), "https://own.org/x", ResourceType::Script),
        Verdict::Allow
    );
    // co.uk handled through the resolver: two sites under one public suffix.
    assert_eq!(
        check(&m, Some("https://a.co.uk/"), "https://cdn.net/x", ResourceType::Script),
        BLOCK0
    );
    let m2 = matcher("||b.co.uk^$third-party\n");
    assert_eq!(
        check(&m2, Some("https://a.co.uk/"), "https://b.co.uk/", ResourceType::Script),
        BLOCK0
    );
    assert_eq!(
        check(&m2, Some("https://www.b.co.uk/"), "https://b.co.uk/", ResourceType::Script),
        Verdict::Allow
    );
}

#[test]
fn opaque_and_missing_sources() {
    let u = Url::parse("https://cdn.net/x").unwrap();
    assert!(!is_third_party(&u, None, &psl));
    assert!(is_third_party(&u, Some(&Origin::new_opaque()), &psl));
}

#[test]
fn resource_types() {
    let m = matcher("||x.com^$script,image\n||y.com^$~image\n");
    let p = Some("https://a.com/");
    assert_eq!(check(&m, p, "https://x.com/a", ResourceType::Script), BLOCK0);
    assert_eq!(check(&m, p, "https://x.com/a", ResourceType::Image), BLOCK0);
    assert_eq!(check(&m, p, "https://x.com/a", ResourceType::Stylesheet), Verdict::Allow);
    assert_eq!(
        check(&m, p, "https://y.com/a", ResourceType::Font),
        Verdict::Block { rule_id: RuleId(1) }
    );
    assert_eq!(check(&m, p, "https://y.com/a", ResourceType::Image), Verdict::Allow);
    // Top-level navigations are never blocked.
    assert_eq!(check(&m, p, "https://y.com/a", ResourceType::Document), Verdict::Allow);
}

#[test]
fn domain_option() {
    let m = matcher("||x.com^$domain=a.com|~b.a.com\n");
    let r = |page: &str| check(&m, Some(page), "https://x.com/", ResourceType::Script);
    assert_eq!(r("https://a.com/"), BLOCK0);
    assert_eq!(r("https://www.a.com/"), BLOCK0);
    assert_eq!(r("https://b.a.com/"), Verdict::Allow);
    assert_eq!(r("https://q.b.a.com/"), Verdict::Allow);
    assert_eq!(r("https://other.org/"), Verdict::Allow);
    assert_eq!(r("https://nota.com/"), Verdict::Allow);
    assert_eq!(check(&m, None, "https://x.com/", ResourceType::Script), Verdict::Allow);
}

#[test]
fn case_sensitivity() {
    let m = matcher("/Banner/x^$match-case\n/AdS/y^\n");
    assert_eq!(check(&m, None, "https://a.com/Banner/x", ResourceType::Image), BLOCK0);
    assert_eq!(check(&m, None, "https://a.com/banner/x", ResourceType::Image), Verdict::Allow);
    assert_eq!(
        check(&m, None, "https://a.com/ADS/Y", ResourceType::Image),
        Verdict::Block { rule_id: RuleId(1) }
    );
    // The host is case-insensitive regardless.
    let m = matcher("||Ads.Example.com^\n");
    assert_eq!(check(&m, None, "https://ADS.example.COM/", ResourceType::Image), BLOCK0);
}

#[test]
fn separators_and_wildcards_end_to_end() {
    let m = matcher("/ad*banner^\n|https://t.example/p|\n.swf|\n");
    assert_eq!(check(&m, None, "https://a.com/ad/x/banner/1", ResourceType::Image), BLOCK0);
    assert_eq!(check(&m, None, "https://a.com/ad/x/banners", ResourceType::Image), Verdict::Allow);
    assert_eq!(
        check(&m, None, "https://t.example/p", ResourceType::Image),
        Verdict::Block { rule_id: RuleId(1) }
    );
    assert_eq!(check(&m, None, "https://t.example/p/", ResourceType::Image), Verdict::Allow);
    assert_eq!(
        check(&m, None, "https://a.com/m.swf", ResourceType::Other),
        Verdict::Block { rule_id: RuleId(2) }
    );
    assert_eq!(check(&m, None, "https://a.com/m.swf?x", ResourceType::Other), Verdict::Allow);
}

#[test]
fn untokenized_rules_still_match() {
    // Tokens next to `*` or at free edges are not index keys.
    let m = matcher("/ads*\n*$third-party,domain=a.com\n");
    assert_eq!(m.untokenized_len(), 2);
    assert_eq!(check(&m, None, "https://x.com/adserver", ResourceType::Image), BLOCK0);
    assert_eq!(
        check(&m, Some("https://a.com/"), "https://z.net/q", ResourceType::Image),
        Verdict::Block { rule_id: RuleId(1) }
    );
}

#[test]
fn query_and_userinfo_handling() {
    let m = matcher("||evil.com^\n");
    // Userinfo cannot smuggle a fake host into the matched string.
    assert_eq!(
        check(&m, None, "https://evil.com@good.org/", ResourceType::Image),
        Verdict::Allow
    );
    assert_eq!(
        check(&m, None, "https://good.org@evil.com/", ResourceType::Image),
        BLOCK0
    );
    // Query text is matched by plain patterns.
    let m = matcher("?affiliate=\n");
    assert_eq!(check(&m, None, "https://a.com/p?affiliate=7", ResourceType::Other), BLOCK0);
    // Fragments are not part of the request.
    let m = matcher("#frag\n");
    assert_eq!(check(&m, None, "https://a.com/p#frag", ResourceType::Other), Verdict::Allow);
}

#[test]
fn non_network_schemes_and_ip_hosts() {
    let m = matcher("||ads.example.com^\n||10.0.0.1^\n");
    assert_eq!(check(&m, None, "data:text/plain,ads.example.com", ResourceType::Image), Verdict::Allow);
    assert_eq!(check(&m, None, "https://10.0.0.1/x", ResourceType::Image), Verdict::Block { rule_id: RuleId(1) });
    assert_eq!(check(&m, None, "wss://ads.example.com/s", ResourceType::Other), BLOCK0);
}

#[test]
fn multiple_lists_keep_order_and_provenance() {
    use px_block::matcher::MatcherBuilder;
    let m = MatcherBuilder::new()
        .add_list("one", parse_list("! c\n||a.com^\n"))
        .add_list("two", parse_list("||b.com^\n"))
        .build();
    assert_eq!(m.len(), 2);
    let v = check(&m, None, "https://b.com/", ResourceType::Image);
    let Verdict::Block { rule_id } = v else {
        panic!("not blocked: {v:?}");
    };
    assert_eq!(rule_id, RuleId(1));
    let info = m.rule_info(rule_id).unwrap();
    assert_eq!((info.list, info.list_name, info.line, info.text), (1, "two", 1, "||b.com^"));
    assert_eq!(m.rule_info(RuleId(0)).unwrap().line, 2);
    assert!(m.rule_info(RuleId(2)).is_none());
}

#[test]
fn rebuild_is_deterministic() {
    let text: String = (0..500)
        .map(|i| format!("||h{}.t{}.com^\n/p{}/x{}^\n", i % 37, i % 11, i % 13, i))
        .collect();
    let a = matcher(&text);
    let b = matcher(&text);
    assert_eq!(a.bucket_count(), b.bucket_count());
    assert_eq!(a.max_bucket(), b.max_bucket());
    assert_eq!(a.untokenized_len(), b.untokenized_len());
    for i in 0..500 {
        let url = format!("https://h{}.t{}.com/p{}/x{}", i % 37, i % 11, i % 13, i);
        let u = Url::parse(&url).unwrap();
        let req = Request {
            url: &u,
            source_origin: None,
            resource_type: ResourceType::Image,
            is_third_party: false,
        };
        let va = check_request(&a, &req);
        assert_eq!(va, check_request(&b, &req));
        assert!(matches!(va, Verdict::Block { .. }));
    }
}

/// 100,000 synthetic rules, 10,000 lookups. Asserts results and index shape,
/// never time.
#[test]
fn scale_100k_rules() {
    let mut text = String::new();
    for i in 0..100_000u32 {
        match i % 4 {
            0 => text.push_str(&format!("||trk{i}.net{}.com^\n", i % 1000)),
            1 => text.push_str(&format!("/ads{i}/banner{}.gif\n", i % 97)),
            2 => text.push_str(&format!("||cdn{}.example^$third-party,script\n", i)),
            _ => text.push_str(&format!("@@||ok{i}.com^\n")),
        }
    }
    let list = parse_list(&text);
    assert_eq!(list.stats.accepted, 100_000);
    let m = Matcher::from_list("synthetic", list);
    assert_eq!(m.len(), 100_000);
    // Every rule has a unique number token: none should need the full scan.
    assert_eq!(m.untokenized_len(), 0);
    assert!(m.max_bucket() <= 4, "max bucket {}", m.max_bucket());

    let mut blocked = 0;
    let mut excepted = 0;
    let mut allowed = 0;
    for k in 0..10_000u32 {
        let i = k * 10 + (k % 4); // i % 4 == k % 4
        let url = match k % 5 {
            0 | 1 | 2 | 3 => match i % 4 {
                0 => format!("https://trk{i}.net{}.com/p", i % 1000),
                1 => format!("https://s.org/ads{i}/banner{}.gif", i % 97),
                2 => format!("https://cdn{i}.example/x.js"),
                _ => format!("https://ok{i}.com/"),
            },
            _ => format!("https://clean{k}.org/index.html"),
        };
        let u = Url::parse(&url).unwrap();
        let src = Origin::from_url(&Url::parse("https://page.test/").unwrap());
        let v = check_request(
            &m,
            &Request {
                url: &u,
                source_origin: Some(&src),
                resource_type: ResourceType::Script,
                is_third_party: true,
            },
        );
        match v {
            Verdict::Block { rule_id } => {
                blocked += 1;
                assert_eq!(rule_id.0, i, "{url}");
            }
            Verdict::AllowedByException { rule_id } => {
                excepted += 1;
                assert_eq!(rule_id.0, i, "{url}");
            }
            Verdict::Allow => allowed += 1,
        }
    }
    // k%5==4 -> 2000 clean URLs. Of the remaining 8000, i%4 is k%4 (k*10 is
    // even so i%4 = (2*(k%2)... ) computed below rather than guessed:
    let mut exp_block = 0;
    let mut exp_exc = 0;
    let mut exp_allow = 0;
    for k in 0..10_000u32 {
        if k % 5 == 4 {
            exp_allow += 1;
            continue;
        }
        let i = k * 10 + (k % 4);
        match i % 4 {
            3 => exp_exc += 1,
            _ => exp_block += 1,
        }
    }
    assert_eq!((blocked, excepted, allowed), (exp_block, exp_exc, exp_allow));
    assert!(exp_block > 0 && exp_exc > 0 && exp_allow > 0);
}
