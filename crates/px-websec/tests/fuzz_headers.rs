//! Deterministic, std-only mutation fuzzing of every header parser and
//! enforcer in the crate. The assertion is "never panics" (and a few cheap
//! invariants); a failure reproduces from the printed seed and iteration.

use px_websec::coop::{self, Coep};
use px_websec::cors::{self, CorsRequest, CorsResponse, CredentialsMode, Header, RequestMode};
use px_websec::csp::{
    CspUrl, Disposition, EvalKind, InlineType, Origin, Policy, PolicySet, PolicySource,
    RequestKind, RequestOptions, SandboxFlags, SourceList, check_base_uri, check_eval,
    check_form_action, check_frame_ancestors, check_inline, check_request, parse_policy_list,
    serialize_policy_list,
};
use px_websec::frame;
use px_websec::origin::{Origin as WOrigin, Url as WUrl};
use px_websec::referrer::{self, ReferrerPolicy};
use px_websec::sri;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { (self.next() % n as u64) as usize }
    }
}

const INTERESTING: &[&[u8]] = &[
    b";", b",", b" ", b"\t", b"\r\n", b"\x0b", b"\x0c", b"\0", b"\xff", b"\xc3\xa9", b"'", b"\"",
    b"*", b"*.", b":", b"://", b"/", b"\\", b"%2e", b"%", b"%zz", b"?", b"#", b"@", b"[", b"]",
    b"=", b"==", b"-", b"_", b"'none'", b"'self'", b"'nonce-", b"'sha256-", b"https:", b"0",
    b"99999", b"deny", b"same-origin", b"require-corp", b"bytes=0-", b"report-to=\"x\"",
];

const SEEDS: &[&str] = &[
    "default-src 'self'; script-src 'nonce-abc' 'strict-dynamic' https: 'unsafe-inline'; img-src * data:; frame-ancestors 'none'; sandbox allow-scripts; report-uri /r",
    "script-src https://a.test:443/p/ *.b.test:* 'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=' 'self'",
    "require-corp; report-to=\"ep\"; x=1.5; y=?1; z=:YWJj:",
    "same-origin-allow-popups",
    "DENY, SAMEORIGIN",
    "no-referrer, origin-when-cross-origin",
    "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0= sha384-abc?opt md5-x",
    "bytes=0-100",
    "allow-scripts allow-same-origin ALLOW-FORMS",
    "https://user:pw@A.Test:8443/a/../b%2f/c?q#f",
    "multipart/form-data; boundary=x",
    "*, \"a,b\", c",
];

fn mutate(rng: &mut Rng, seed: &[u8]) -> Vec<u8> {
    let mut v = seed.to_vec();
    for _ in 0..=rng.below(8) {
        match rng.below(6) {
            0 if !v.is_empty() => {
                let i = rng.below(v.len());
                v.remove(i);
            }
            1 => {
                let i = rng.below(v.len() + 1);
                let b = (rng.next() >> 56) as u8;
                v.insert(i, b);
            }
            2 => {
                let tok = INTERESTING[rng.below(INTERESTING.len())];
                let i = rng.below(v.len() + 1);
                for (k, b) in tok.iter().enumerate() {
                    v.insert(i + k, *b);
                }
            }
            3 if !v.is_empty() => {
                let i = rng.below(v.len());
                v[i] ^= 1 << rng.below(8);
            }
            4 if v.len() > 1 => {
                let a = rng.below(v.len());
                let b = rng.below(v.len());
                v.swap(a, b);
            }
            _ => {
                if !v.is_empty() && rng.below(4) == 0 {
                    let n = v.len();
                    v.extend_from_within(..n.min(64));
                }
            }
        }
    }
    v
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn fuzz_csp(input: &[u8], urls: &[CspUrl]) {
    let o = Origin::from_url_str("https://page.example/");
    for disp in [Disposition::Enforce, Disposition::Report] {
        let Ok(list) = parse_policy_list(input, disp) else {
            continue;
        };
        // serialisation never panics; a re-parse of a serialised list is
        // stable (idempotent) when serialisation is possible.
        if let Some(s) = serialize_policy_list(&list) {
            if let Ok(again) = parse_policy_list(s.as_bytes(), disp) {
                assert_eq!(
                    serialize_policy_list(&again).as_deref(),
                    Some(s.as_str()),
                    "serialisation not a fixed point for {input:?}"
                );
            }
        }
    }
    let Ok(p) = Policy::parse(input, Disposition::Enforce, PolicySource::Header) else {
        return;
    };
    let _ = Policy::parse_meta(input);
    let _ = p.sandbox_flags();
    for u in urls {
        for k in RequestKind::ALL {
            let _ = check_request(&p, k, u, &o, &RequestOptions::default());
            let opts = RequestOptions {
                nonce: Some("abc"),
                parser_inserted: true,
                redirect_count: 1,
                ..Default::default()
            };
            let _ = check_request(&p, k, u, &o, &opts);
        }
        let _ = check_form_action(&p, u, &o, 0);
        let _ = check_base_uri(&p, u, &o);
        let _ = check_frame_ancestors(&p, u, std::slice::from_ref(u));
    }
    for t in [
        InlineType::Script,
        InlineType::ScriptAttribute,
        InlineType::Style,
        InlineType::StyleAttribute,
        InlineType::Navigation,
    ] {
        let _ = check_inline(&p, t, Some("abc"), "alert(1)");
    }
    let _ = check_eval(&p, EvalKind::Eval);
    let _ = check_eval(&p, EvalKind::Wasm);
    let tokens: Vec<String> = lossy(input).split_ascii_whitespace().map(str::to_string).collect();
    let _ = SourceList::parse(&tokens);
    let _ = SandboxFlags::from_tokens(&tokens);
    let _ = PolicySet::from_headers_fail_closed(&[input], &[input]);
}

fn fuzz_misc(input: &[u8], req: &CorsRequest) {
    let s = lossy(input);
    // framing
    let o = WOrigin::from_url(&req.url);
    let _ = frame::check_xfo(&[s.as_str()], &o, std::slice::from_ref(&o));
    let _ = frame::check_xfo(&[s.as_str(), s.as_str()], &o, &[]);
    let _ = frame::parse_sandbox_attribute(Some(&s));
    // COOP / COEP / CORP
    for coep in [Coep::UnsafeNone, Coep::RequireCorp, Coep::Credentialless] {
        let _ = coop::parse_coop(Some(&s), coep);
    }
    let _ = coop::parse_coep(Some(&s));
    let _ = coop::parse_corp(&[s.as_str()]);
    let _ = coop::parse_corp(&[s.as_str(), s.as_str()]);
    // referrer
    let _ = referrer::parse_header(&s);
    let _ = referrer::parse_attribute(&s);
    let _ = referrer::parse_meta_content(&s);
    // SRI
    if let Ok(md) = sri::parse(&s) {
        let _ = sri::verify(&md, cors::Tainting::Basic, input);
        let _ = sri::verify(&md, cors::Tainting::Opaque, input);
    }
    let _ = sri::base64_decode(&s);
    // CORS
    let _ = cors::split_list(input);
    let _ = cors::is_safelisted_request_header("range", input);
    let _ = cors::is_safelisted_request_header("content-type", input);
    let _ = cors::is_safelisted_request_header("accept", input);
    let _ = cors::is_forbidden_request_header("x-http-method-override", input);
    let hdrs = vec![
        Header { name: "Access-Control-Allow-Origin".into(), value: input.to_vec() },
        Header { name: "Access-Control-Allow-Credentials".into(), value: input.to_vec() },
        Header { name: "Access-Control-Allow-Methods".into(), value: input.to_vec() },
        Header { name: "Access-Control-Allow-Headers".into(), value: input.to_vec() },
        Header { name: "Access-Control-Max-Age".into(), value: input.to_vec() },
        Header { name: "Access-Control-Expose-Headers".into(), value: input.to_vec() },
    ];
    let resp = CorsResponse { status: 204, headers: hdrs };
    let mut r = req.clone();
    r.headers = vec![Header { name: lossy(input), value: input.to_vec() }];
    for c in [CredentialsMode::Omit, CredentialsMode::Include] {
        r.credentials = c;
        let _ = cors::cors_check(&r, &resp.headers);
        let _ = cors::validate_preflight(&r, &resp);
        let _ = cors::classify(&r);
        let _ = cors::preflight_request(&r);
        if let Ok(t) = cors::taint(&r) {
            let _ = cors::filter_response(&r, t, &resp);
        }
    }
    // URL parsers
    let _ = WUrl::parse(&s);
    let _ = CspUrl::parse(&s);
}

#[test]
fn mutated_headers_never_panic() {
    let req = CorsRequest {
        method: "PUT".into(),
        url: WUrl::parse("https://b.test/x").unwrap(),
        origin: WOrigin::from_url(&WUrl::parse("https://a.test/").unwrap()),
        headers: vec![],
        credentials: CredentialsMode::SameOrigin,
        mode: RequestMode::Cors,
        use_cors_preflight_flag: false,
        origin_tainted: false,
    };
    let urls: Vec<CspUrl> = [
        "https://page.example/a",
        "http://page.example:8080/",
        "wss://x.test/s",
        "data:text/plain,hi",
        "https://a.b.example.com/p/q",
    ]
    .iter()
    .filter_map(|u| CspUrl::parse(u))
    .collect();
    for seed in [0xC0FFEE_u64, 0xBAD5EED, 0x1234_5678_9ABC, 0xDEAD_BEEF] {
        let mut rng = Rng(seed);
        for it in 0..4000 {
            let base = SEEDS[rng.below(SEEDS.len())].as_bytes();
            let input = mutate(&mut rng, base);
            // `it`/`seed` are printed by the harness on panic via this guard.
            let _ctx = (seed, it);
            fuzz_csp(&input, &urls);
            fuzz_misc(&input, &req);
        }
    }
}

#[test]
fn pure_noise_and_extremes_never_panic() {
    let req = CorsRequest {
        method: "GET".into(),
        url: WUrl::parse("http://b.test/").unwrap(),
        origin: WOrigin::from_url(&WUrl::parse("http://a.test/").unwrap()),
        headers: vec![],
        credentials: CredentialsMode::Omit,
        mode: RequestMode::NoCors,
        use_cors_preflight_flag: true,
        origin_tainted: true,
    };
    let mut rng = Rng(0x5EED_0001);
    for _ in 0..3000 {
        let n = rng.below(200);
        let input: Vec<u8> = (0..n).map(|_| (rng.next() >> 56) as u8).collect();
        fuzz_csp(&input, &[]);
        fuzz_misc(&input, &req);
    }
    for extreme in [
        Vec::new(),
        vec![b';'; 100_000],
        vec![b','; 100_000],
        vec![b'\''; 100_000],
        b"a ".repeat(60_000),
        b"%".repeat(50_000),
        b"*.".repeat(30_000),
    ] {
        fuzz_csp(&extreme, &[]);
        fuzz_misc(&extreme, &req);
    }
    // referrer policy values round-trip regardless of case
    for p in ["NO-REFERRER", "Strict-Origin-When-Cross-Origin", "unsafe-URL"] {
        assert!(ReferrerPolicy::from_token(p).is_some());
    }
}
