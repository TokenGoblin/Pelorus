#![no_main]

//! Untrusted filter-list text through the parser, the matcher index and the
//! mitigations (Phase 19, §4.3, §10).
//!
//! Input layout: one control byte (resource type, third-party flag, which
//! URL to request), then the rest as the list text. Whatever the bytes,
//! parsing must return and account for every line exactly once, the index
//! must build, and `check_request` must terminate with a verdict that names a
//! rule that exists. The same bytes are also tried as a CNAME chain element,
//! a tracking-parameter name list and a raw pattern.

use libfuzzer_sys::fuzz_target;
use px_block::cname::check_cname_chain;
use px_block::filter::{ResourceType, parse_list_bytes};
use px_block::glob::Pattern;
use px_block::matcher::{Matcher, Request, Verdict, check_request};
use px_block::params::{TrackingParams, strip_tracking_params};
use px_websec::origin::{Origin, Url};

const URLS: [&str; 6] = [
    "https://ads.example.com/banner/x.js?utm_source=a&id=1",
    "http://sub.example.com:8080/x#frag",
    "wss://example.com/socket",
    "ftp://example.com/file.swf",
    "https://10.0.0.1/a",
    "https://[::1]:8443/p",
];

const TYPES: [ResourceType; 10] = [
    ResourceType::Script,
    ResourceType::Image,
    ResourceType::Stylesheet,
    ResourceType::XmlHttpRequest,
    ResourceType::Subdocument,
    ResourceType::Ping,
    ResourceType::Media,
    ResourceType::Font,
    ResourceType::Other,
    ResourceType::Document,
];

fn psl(host: &str) -> Option<String> {
    let labels: Vec<&str> = host.split('.').collect();
    let n = labels.len();
    (n >= 2).then(|| labels[n - 2..].join("."))
}

fuzz_target!(|data: &[u8]| {
    let Some((&control, bytes)) = data.split_first() else {
        return;
    };
    let list = parse_list_bytes(bytes);
    let s = &list.stats;
    assert_eq!(
        s.lines,
        s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
    );
    assert_eq!(s.accepted as usize, list.rules.len());

    let matcher = Matcher::from_list("fuzz", list);
    let Some(url) = URLS
        .get(usize::from(control >> 4) % URLS.len())
        .and_then(|u| Url::parse(u))
    else {
        return;
    };
    let source = Origin::from_url(&Url::parse("https://page.test/").unwrap_or(url.clone()));
    let ty = TYPES[usize::from(control & 0x0f) % TYPES.len()];
    let req = Request {
        url: &url,
        source_origin: Some(&source),
        resource_type: ty,
        is_third_party: control & 0x08 != 0,
    };
    if let Verdict::Block { rule_id } | Verdict::AllowedByException { rule_id } =
        check_request(&matcher, &req)
    {
        assert!(matcher.rule(rule_id).is_some());
    }

    // The same bytes as a DNS name chain, a parameter name and a pattern.
    let text = String::from_utf8_lossy(bytes);
    let chain: Vec<&str> = text.split('\n').take(20).collect();
    let _ = check_cname_chain(&matcher, &req, &chain, &psl);

    let mut params = TrackingParams::default();
    let _ = params.add_exact(&text);
    let _ = params.add_prefix(&text);
    if let Some(u) = Url::parse(&format!("https://a.test/?{text}")) {
        if let Some(r) = strip_tracking_params(&u, &params) {
            // Idempotent and still valid.
            assert!(strip_tracking_params(&r.url, &params).is_none());
        }
    }
    if let Ok(p) = Pattern::from_text(&text, control & 1 == 0) {
        let _ = p.matches(text.as_bytes(), Some((0, text.len())));
        let _ = p.index_tokens();
    }
});
