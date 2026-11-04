//! Hostile-input tests for the CSP engine.
//!
//! Everything here drives the public API the way an attacker-controlled
//! header or URL would. Generative tests use a hand-written xorshift PRNG
//! with fixed seeds, so a failure reproduces from the seed and no dependency
//! is added. Helpers avoid unwrap/expect/panic/indexing (clippy's test
//! allowances cover `#[test]` bodies, not helper functions).

use std::time::{Duration, Instant};

use px_websec::csp::policy::{MAX_POLICIES, MAX_POLICY_BYTES};
use px_websec::csp::{
    CspUrl, Decision, Directive, Disposition, EvalKind, InlineType, Origin, ParseError, Policy,
    PolicySet, PolicySource, RequestKind, RequestOptions, SandboxFlags, SourceList, check_base_uri,
    check_eval, check_form_action, check_frame_ancestors, check_inline, check_request,
    parse_policy_list, serialize_policy_list,
};

// ---------------------------------------------------------------- helpers

/// xorshift64*: deterministic, dependency-free.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            return 0;
        }
        (self.next_u64() % n as u64) as usize
    }
    fn byte(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        let i = self.below(xs.len());
        xs.get(i).copied().unwrap_or("")
    }
}

fn enforce(input: &[u8]) -> Option<Policy> {
    Policy::parse(input, Disposition::Enforce, PolicySource::Header).ok()
}

fn url(s: &str) -> Option<CspUrl> {
    CspUrl::parse(s)
}

fn page() -> Origin {
    Origin::from_url_str("https://page.example/")
}

/// Run every enforcement point against a policy; the result is irrelevant,
/// the point is that none of it panics.
fn exercise(p: &Policy, u: &CspUrl) {
    let o = page();
    for k in RequestKind::ALL {
        let _ = check_request(p, k, u, &o, &RequestOptions::default());
        let opts = RequestOptions {
            nonce: Some("abc"),
            parser_inserted: true,
            redirect_count: 1,
            ..Default::default()
        };
        let _ = check_request(p, k, u, &o, &opts);
    }
    for t in [
        InlineType::Script,
        InlineType::ScriptAttribute,
        InlineType::Style,
        InlineType::StyleAttribute,
        InlineType::Navigation,
    ] {
        let _ = check_inline(p, t, Some("abc"), "alert(1)");
    }
    let _ = check_eval(p, EvalKind::Eval);
    let _ = check_eval(p, EvalKind::Wasm);
    let _ = check_form_action(p, u, &o, 0);
    let _ = check_base_uri(p, u, &o);
    let _ = check_frame_ancestors(p, u, std::slice::from_ref(u));
    let _ = p.sandbox_flags();
    let _ = p.serialize();
}

// ------------------------------------------------------------ size bounds

/// Documented bound: a policy of up to `MAX_POLICY_BYTES` (1 MiB) is parsed,
/// and parsing plus a full matching pass is linear. The wall-clock limit is
/// deliberately 100x looser than a release build needs so a slow CI machine
/// does not flake, yet far below what a quadratic implementation would take
/// (100,000 tokens squared is 10^10 operations).
const TIME_BOUND: Duration = Duration::from_secs(20);

#[test]
fn policy_with_100_000_tokens_and_one_megabyte_is_handled_within_the_bound() {
    let mut s = String::from("script-src");
    let mut i = 0u32;
    // ~10 bytes per token, 100,000 tokens, ~1,000,000 bytes.
    while i < 100_000 {
        s.push_str(&format!(" h{i:05}.tt"));
        i += 1;
    }
    assert!(
        s.len() >= 1_000_000 && s.len() <= MAX_POLICY_BYTES,
        "{}",
        s.len()
    );

    let t = Instant::now();
    let p = enforce(s.as_bytes());
    let p = match p {
        Some(p) => p,
        None => panic!("1 MB policy must parse"),
    };
    let d = p.directive("script-src");
    assert_eq!(d.map(|d| d.values.len()), Some(100_000));

    // The last token matches, a token that is not listed does not.
    let hit = url("https://h99999.tt/x.js");
    let miss = url("https://h100000.tt/x.js");
    let (Some(hit), Some(miss)) = (hit, miss) else {
        panic!("urls")
    };
    let o = Origin::from_url_str("https://h00000.tt/");
    let ok = |u: &CspUrl| check_request(&p, RequestKind::Script, u, &o, &RequestOptions::default());
    assert_eq!(ok(&hit), Decision::Allowed);
    assert!(ok(&miss).is_blocked());
    // round trip stays within the bound too
    let ser = p.serialize();
    assert_eq!(enforce(ser.as_bytes()), Some(p));
    assert!(t.elapsed() < TIME_BOUND, "took {:?}", t.elapsed());
}

#[test]
fn one_megabyte_shapes_all_stay_linear() {
    let n = MAX_POLICY_BYTES;
    let shapes: Vec<(&str, Vec<u8>)> = vec![
        ("semicolons", vec![b';'; n]),
        ("commas", vec![b','; n]),
        ("spaces", vec![b' '; n]),
        ("one token", vec![b'a'; n]),
        ("a;b;c...", "a;".repeat(n / 2).into_bytes()),
        ("distinct directives", {
            let mut v = Vec::new();
            let mut i = 0u32;
            while v.len() + 12 < n {
                v.extend_from_slice(format!("d{i}-x v;").as_bytes());
                i += 1;
            }
            v
        }),
        ("many quotes", "'".repeat(n).into_bytes()),
        (
            "many nonces",
            "script-src 'nonce-a' ".repeat(n / 21).into_bytes(),
        ),
        ("many policies in a list", "a b,".repeat(n / 4).into_bytes()),
        ("nul bytes", vec![0u8; n]),
        ("0xff bytes", vec![0xffu8; n]),
    ];
    for (name, bytes) in shapes {
        let t = Instant::now();
        let single = Policy::parse(&bytes, Disposition::Enforce, PolicySource::Header);
        let list = parse_policy_list(&bytes, Disposition::Enforce);
        let meta = Policy::parse_meta(&bytes);
        assert!(single.is_ok(), "{name}");
        assert!(meta.is_ok(), "{name}");
        match name {
            "many policies in a list" => {
                assert_eq!(list, Err(ParseError::TooManyPolicies), "{name}")
            }
            _ => assert!(list.is_ok(), "{name}"),
        }
        if let (Ok(p), Some(u)) = (single, url("https://a.test/")) {
            exercise(&p, &u);
        }
        assert!(t.elapsed() < TIME_BOUND, "{name} took {:?}", t.elapsed());
    }
}

#[test]
fn over_the_limit_is_an_error_never_a_truncation() {
    let mut v = b"script-src 'none';".to_vec();
    v.resize(MAX_POLICY_BYTES + 1, b'a');
    assert_eq!(
        Policy::parse(&v, Disposition::Enforce, PolicySource::Header),
        Err(ParseError::TooLarge)
    );
    assert_eq!(Policy::parse_meta(&v), Err(ParseError::TooLarge));
    let set = PolicySet::from_headers_fail_closed(&[&v], &[]);
    let Some(u) = url("https://a.test/x") else {
        panic!("url")
    };
    assert!(
        set.check_request(RequestKind::Image, &u, &page(), &RequestOptions::default())
            .is_blocked()
    );
    // exactly at the limit still parses
    v.truncate(MAX_POLICY_BYTES);
    assert!(Policy::parse(&v, Disposition::Enforce, PolicySource::Header).is_ok());
}

#[test]
fn policy_count_bound_across_headers() {
    let one = "a b,".repeat(MAX_POLICIES);
    let r = PolicySet::from_headers(&[one.as_bytes(), b"c d"], &[]);
    assert_eq!(r, Err(ParseError::TooManyPolicies));
    let r = PolicySet::from_headers(&[one.as_bytes()], &[]);
    assert_eq!(r.map(|s| s.policies().len()), Ok(MAX_POLICIES));
}

// -------------------------------------------------------- hostile content

#[test]
fn hostile_bytes_unicode_and_nuls() {
    let u = match url("https://a.test/x") {
        Some(u) => u,
        None => panic!("url"),
    };
    let cases: Vec<Vec<u8>> = vec![
        b"\0".to_vec(),
        b"script-src\0'self'".to_vec(),
        b"script-src 'self'\0; img-src *".to_vec(),
        b"\0script-src 'none'".to_vec(),
        "script-src 'none'\u{feff}; img-src *".as_bytes().to_vec(),
        "\u{feff}script-src 'none'".as_bytes().to_vec(),
        "script-src \u{202e}'self'\u{202c}".as_bytes().to_vec(),
        "script-src ｈｔｔｐｓ://ａ.test".as_bytes().to_vec(),
        "scr\u{131}pt-src 'none'".as_bytes().to_vec(),
        "SCRİPT-SRC 'none'".as_bytes().to_vec(),
        vec![0xff, 0xfe, 0xfd, b';', 0xc0, 0x80, b' ', 0xed, 0xa0, 0x80],
        b"script-src \x0b'none'".to_vec(),
        b"script-src\x0c'none'".to_vec(),
        b"script-src%20'none'".to_vec(),
        b"script-src 'none'\r\nSet-Cookie: x=1".to_vec(),
        b";;;;;;,,,,,,;;;;".to_vec(),
        b"script-src "
            .iter()
            .chain(std::iter::repeat_n(&b'\'', 1000))
            .copied()
            .collect(),
        (0u8..=255).collect(),
        (0u8..=255).rev().collect(),
    ];
    for c in &cases {
        for disp in [Disposition::Enforce, Disposition::Report] {
            if let Ok(p) = Policy::parse(c, disp, PolicySource::Header) {
                exercise(&p, &u);
                assert!(p.serialize().is_ascii(), "serialization stays ASCII");
            }
            let _ = parse_policy_list(c, disp);
        }
        if let Ok(p) = Policy::parse_meta(c) {
            exercise(&p, &u);
        }
    }
}

#[test]
fn non_ascii_in_a_directive_never_widens_or_narrows_neighbours() {
    // A non-ASCII token is skipped whole, the rest of the policy applies.
    let Some(p) = enforce("img-src 'none'; script-src caf\u{e9}.test; font-src 'none'".as_bytes())
    else {
        panic!("parse")
    };
    assert!(p.has("img-src") && p.has("font-src") && !p.has("script-src"));
    // a lookalike directive name is NOT script-src
    let Some(p) = enforce("scr\u{131}pt-src *; default-src 'none'".as_bytes()) else {
        panic!("parse")
    };
    let Some(u) = url("https://evil.test/a.js") else {
        panic!("url")
    };
    assert!(
        check_request(
            &p,
            RequestKind::Script,
            &u,
            &page(),
            &RequestOptions::default()
        )
        .is_blocked(),
        "the skipped lookalike must not be treated as script-src"
    );
}

#[test]
fn injection_in_values_cannot_forge_directives() {
    // Newlines are whitespace, `;` still splits: nothing smuggles a second
    // directive in a way the parser does not see.
    let Some(p) = enforce(b"script-src 'self'\nimg-src *") else {
        panic!("parse")
    };
    assert_eq!(p.directive("script-src").map(|d| d.values.len()), Some(3));
    assert!(!p.has("img-src"));
}

#[test]
fn attacker_cannot_use_duplicates_to_replace_the_first_directive() {
    let Some(p) = enforce(b"script-src 'none'; script-src *; SCRIPT-SRC *; Script-Src https:")
    else {
        panic!("parse")
    };
    let Some(u) = url("https://evil.test/a.js") else {
        panic!("url")
    };
    assert!(
        check_request(
            &p,
            RequestKind::Script,
            &u,
            &page(),
            &RequestOptions::default()
        )
        .is_blocked()
    );
}

#[test]
fn hostile_urls_do_not_panic_and_unparseable_urls_are_none() {
    let mut cases: Vec<String> = vec![
        "https://".into(),
        "https://:".into(),
        "https://@".into(),
        "https://@:80".into(),
        "https://[".into(),
        "https://[]/".into(),
        "https://[::1]:99999/".into(),
        "https://a.test:00000000000000080/".into(),
        "https://a.test/%".into(),
        "https://a.test/%zz%".into(),
        format!("https://a.test/{}", "../".repeat(100_000)),
        format!("https://{}.test/", "a".repeat(100_000)),
        format!("https://a.test/{}", "/".repeat(100_000)),
        "javascript:".into(),
        "data:".into(),
        ":".into(),
        "::::".into(),
        "https:\\\\a.test\\".into(),
    ];
    cases.push((0u8..=127).map(char::from).collect());
    for c in &cases {
        if let Some(u) = CspUrl::parse(c) {
            let p = enforce(b"default-src 'self' https://a.test/ *.test; frame-ancestors a.test");
            if let Some(p) = p {
                exercise(&p, &u);
            }
        }
    }
    assert!(CspUrl::parse("https://a.test:00000000000000080/").is_none());
}

// ------------------------------------------------------- generative tests

const NAMES: &[&str] = &[
    "default-src",
    "script-src",
    "script-src-elem",
    "style-src",
    "img-src",
    "connect-src",
    "worker-src",
    "child-src",
    "frame-src",
    "form-action",
    "base-uri",
    "frame-ancestors",
    "sandbox",
    "upgrade-insecure-requests",
    "block-all-mixed-content",
    "report-uri",
    "unknown-thing",
    "x",
];

const VALUES: &[&str] = &[
    "'self'",
    "'none'",
    "'unsafe-inline'",
    "'unsafe-eval'",
    "'strict-dynamic'",
    "'unsafe-hashes'",
    "'wasm-unsafe-eval'",
    "'report-sample'",
    "'nonce-AbC123=='",
    "'sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='",
    "'sha384-cb00753f45'",
    "https:",
    "http:",
    "data:",
    "blob:",
    "ws:",
    "*",
    "*.example.com",
    "example.com",
    "https://example.com:8443/a/b/",
    "http://*.example.com:*",
    "example.com/path/file.js",
    "[::1]",
    "caf\u{e9}.test",
    "'bogus'",
    "%",
    "\\",
    "allow-scripts",
    "allow-same-origin",
    "/report",
];

fn random_policy_text(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..rng.below(8) {
        s.push_str(rng.pick(NAMES));
        for _ in 0..rng.below(6) {
            s.push(' ');
            s.push_str(rng.pick(VALUES));
        }
        s.push_str(rng.pick(&[";", "; ", " ;", ";;", "\t;\n"]));
    }
    s
}

fn random_bytes_text(rng: &mut Rng) -> Vec<u8> {
    let alphabet: &[u8] = b"abcsrc-defaultSELF'*.:/;,  \t\n\0%\xc3\xa9\xff09nonce-sha256=+_";
    let n = rng.below(300);
    (0..n)
        .map(|_| {
            if rng.below(10) == 0 {
                rng.byte()
            } else {
                alphabet
                    .get(rng.below(alphabet.len()))
                    .copied()
                    .unwrap_or(b' ')
            }
        })
        .collect()
}

#[test]
fn fuzz_loop_parse_and_enforce_never_panic() {
    let mut rng = Rng::new(0x00C5_9001);
    let urls: Vec<CspUrl> = [
        "https://example.com/a/b/c.js",
        "http://sub.example.com:8080/x",
        "wss://example.com/socket",
        "data:text/plain,hi",
        "javascript:alert(1)",
        "https://[::1]/",
        "https://127.0.0.1:8443/p",
        "blob:https://example.com/uuid",
    ]
    .iter()
    .filter_map(|s| CspUrl::parse(s))
    .collect();
    assert_eq!(urls.len(), 8);
    for i in 0..4_000 {
        let bytes = if i % 2 == 0 {
            random_policy_text(&mut rng).into_bytes()
        } else {
            random_bytes_text(&mut rng)
        };
        for disp in [Disposition::Enforce, Disposition::Report] {
            if let Ok(p) = Policy::parse(&bytes, disp, PolicySource::Header) {
                for u in &urls {
                    exercise(&p, u);
                }
            }
        }
        let _ = parse_policy_list(&bytes, Disposition::Enforce);
        if let Ok(p) = Policy::parse_meta(&bytes) {
            for u in &urls {
                exercise(&p, u);
            }
        }
        // source lists straight from random tokens
        let text = String::from_utf8_lossy(&bytes).into_owned();
        let toks: Vec<&str> = text.split_ascii_whitespace().collect();
        let l = SourceList::parse(&toks);
        for u in &urls {
            let _ = l.matches_url(u, &page(), (i % 3) as u32);
        }
        let _ = l.hash_matches_text(&text);
        let _ = l.nonce_matches(&text);
        let _ = SandboxFlags::from_tokens(&toks);
        let _ = CspUrl::parse(&text);
    }
}

/// Idempotence: whatever bytes come in, parse -> serialize -> parse is a
/// fixed point.
#[test]
fn fuzz_loop_parse_serialize_parse_is_a_fixed_point() {
    let mut rng = Rng::new(0xDEAD_BEEF);
    for i in 0..4_000 {
        let bytes = if i % 3 == 0 {
            random_bytes_text(&mut rng)
        } else {
            random_policy_text(&mut rng).into_bytes()
        };
        // a single policy: commas are ordinary token bytes here.
        let Ok(p) = Policy::parse(&bytes, Disposition::Enforce, PolicySource::Header) else {
            continue;
        };
        let text = p.serialize();
        assert!(text.is_ascii());
        assert_eq!(
            Policy::parse(text.as_bytes(), Disposition::Enforce, PolicySource::Header).as_ref(),
            Ok(&p),
            "round trip failed for {text:?}"
        );
    }
}

/// Generated policies round-trip exactly, as a list too.
#[test]
fn fuzz_loop_generated_policies_round_trip() {
    let mut rng = Rng::new(0x1234_5678_9ABC_DEF1);
    for _ in 0..3_000 {
        let mut policies = Vec::new();
        for _ in 0..(1 + rng.below(4)) {
            let mut dirs = Vec::new();
            for _ in 0..rng.below(7) {
                let name = rng.pick(NAMES).to_string();
                let values: Vec<String> = (0..rng.below(6))
                    .map(|_| rng.pick(VALUES).to_string())
                    .filter(|v| v.is_ascii() && !v.contains(','))
                    .collect();
                dirs.push(Directive { name, values });
            }
            let disp = if rng.below(4) == 0 {
                Disposition::Report
            } else {
                Disposition::Enforce
            };
            let p = Policy::from_directives(dirs, disp, PolicySource::Header);
            if !p.is_empty() {
                policies.push(p);
            }
        }
        // single-policy round trip
        for p in &policies {
            let text = p.serialize();
            let back = Policy::parse(text.as_bytes(), p.disposition(), PolicySource::Header);
            assert_eq!(back.as_ref(), Ok(p), "{text:?}");
        }
        // list round trip (dispositions are carried by the header, so only
        // check enforce policies in one list)
        let enforced: Vec<Policy> = policies
            .into_iter()
            .filter(|p| p.disposition() == Disposition::Enforce)
            .collect();
        if let Some(text) = serialize_policy_list(&enforced) {
            let back = parse_policy_list(text.as_bytes(), Disposition::Enforce);
            assert_eq!(back.as_ref(), Ok(&enforced), "{text:?}");
        }
    }
}

/// Matching never panics on random hosts, ports and paths, and a source
/// built from a URL always matches that URL (reflexivity), unless the URL is
/// an IP literal wildcard case which this test avoids.
#[test]
fn fuzz_loop_source_matching_reflexive_and_total() {
    let mut rng = Rng::new(0xFACE_FEED);
    let hosts = [
        "a.test",
        "b.a.test",
        "example.com",
        "x-y.z.example.org",
        "127.0.0.1",
    ];
    let paths = [
        "",
        "/",
        "/a",
        "/a/",
        "/a/b.js",
        "/%7Euser/x",
        "/a%2Fb",
        "/A",
    ];
    let ports = ["", ":8080", ":443", ":80"];
    for _ in 0..5_000 {
        let scheme = rng.pick(&["http", "https", "ws", "wss"]);
        let host = rng.pick(&hosts);
        let port = rng.pick(&ports);
        let path = rng.pick(&paths);
        let text = format!("{scheme}://{host}{port}{path}");
        let Some(u) = CspUrl::parse(&text) else {
            continue;
        };
        let l = SourceList::parse(&[text.as_str()]);
        // A source written with the URL's own scheme, host, port and an
        // exact (non-slash-terminated) path matches that URL.
        let exact = path.is_empty() || !path.ends_with('/') || path == "/";
        if exact {
            let origin = Origin::Opaque;
            assert!(
                l.matches_url(&u, &origin, 0),
                "{text} should match itself (u={u:?})"
            );
        }
        // and never matches a different host
        let other = format!("{scheme}://other-{host}{port}{path}");
        if let Some(o) = CspUrl::parse(&other) {
            assert!(!l.matches_url(&o, &Origin::Opaque, 0), "{text} vs {other}");
        }
    }
}
