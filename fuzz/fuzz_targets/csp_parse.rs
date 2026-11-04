#![no_main]

//! Untrusted `Content-Security-Policy` header and `<meta>` bytes through the
//! CSP engine (Phase 12, §4.3, §10).
//!
//! Input layout: one control byte (policy kind, disposition, which URL to
//! test), then the rest as the serialized policy. Whatever the bytes, parsing
//! must return `Ok` or `Err` and never panic or exceed its documented limits;
//! a parsed header policy must be a fixed point of serialize-then-parse; and
//! every enforcement point must return a decision for every request kind.

use libfuzzer_sys::fuzz_target;
use px_websec::csp::{
    CspUrl, Disposition, EvalKind, InlineType, Origin, Policy, PolicySet, PolicySource,
    RequestKind, RequestOptions, check_base_uri, check_eval, check_form_action,
    check_frame_ancestors, check_inline, check_request, parse_policy_list,
};

const URLS: [&str; 6] = [
    "https://example.com/a/b.js",
    "http://sub.example.com:8080/x",
    "wss://example.com/socket",
    "data:text/plain,hi",
    "javascript:alert(1)",
    "https://[::1]:8443/p",
];

fuzz_target!(|data: &[u8]| {
    let Some((&control, bytes)) = data.split_first() else {
        return;
    };
    let disposition = if control & 1 == 0 {
        Disposition::Enforce
    } else {
        Disposition::Report
    };
    let origin = Origin::from_url_str("https://example.com/");
    let url = URLS
        .get(usize::from(control >> 1) % URLS.len())
        .and_then(|u| CspUrl::parse(u));
    let Some(url) = url else {
        return;
    };

    let _ = PolicySet::from_headers_fail_closed(&[bytes], &[bytes]);
    let _ = parse_policy_list(bytes, disposition);

    let policy = if control & 0x80 == 0 {
        Policy::parse(bytes, disposition, PolicySource::Header)
    } else {
        Policy::parse_meta(bytes)
    };
    let Ok(policy) = policy else {
        return;
    };

    if policy.source() == PolicySource::Header {
        let text = policy.serialize();
        let again = Policy::parse(text.as_bytes(), disposition, PolicySource::Header);
        assert_eq!(again.as_ref(), Ok(&policy));
    }

    for kind in RequestKind::ALL {
        let _ = check_request(&policy, kind, &url, &origin, &RequestOptions::default());
    }
    for ty in [
        InlineType::Script,
        InlineType::ScriptAttribute,
        InlineType::Style,
        InlineType::StyleAttribute,
        InlineType::Navigation,
    ] {
        let _ = check_inline(&policy, ty, Some("nonce"), "alert(1)");
    }
    let _ = check_eval(&policy, EvalKind::Eval);
    let _ = check_eval(&policy, EvalKind::Wasm);
    let _ = check_form_action(&policy, &url, &origin, 0);
    let _ = check_base_uri(&policy, &url, &origin);
    let _ = check_frame_ancestors(&policy, &url, std::slice::from_ref(&url));
    let _ = policy.sandbox_flags();
});
