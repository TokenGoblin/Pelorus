//! Hostile input, a differential check of the glob matcher against an
//! exhaustive reference, and an xorshift fuzz loop.

use crate::filter::{MAX_LINE_BYTES, RejectReason, ResourceType, parse_list, parse_list_bytes};
use crate::glob::{Elem, Pattern, StartAnchor, is_separator};
use crate::matcher::{Matcher, Request, Verdict, check_request};
use px_websec::origin::{Origin, Url};

struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn one_megabyte_line() {
    let line = "a".repeat(1 << 20);
    let l = parse_list(&line);
    assert_eq!(l.stats.rejected_for(RejectReason::LineTooLong), 1);
    assert!(l.rules.is_empty());
    // With a newline-terminated valid rule after it, parsing continues.
    let l = parse_list(&format!("{line}\n||ok.example^\n"));
    assert_eq!(l.stats.rejected_for(RejectReason::LineTooLong), 1);
    assert_eq!(l.stats.accepted, 1);
}

#[test]
fn million_stars() {
    let l = parse_list(&"*".repeat(1_000_000));
    assert_eq!(l.stats.rejected_for(RejectReason::LineTooLong), 1);
    // Within the line cap: 4000 stars around a literal collapse to one.
    let rule = format!("{}x{}", "*".repeat(2000), "*".repeat(2000));
    let l = parse_list(&rule);
    assert_eq!(l.stats.rejected_for(RejectReason::PatternTooLong), 1);
    // 500 stars: accepted, collapsed, and matching terminates.
    let rule = format!("{}x{}", "*".repeat(250), "*".repeat(250));
    let l = parse_list(&rule);
    assert_eq!(l.rules.len(), 1);
    assert_eq!(l.rules[0].pattern.elems(), &[Elem::Lit(b'x')][..]);
    // Alternating star/literal at the cap, against a long URL.
    let rule = format!("a{}", "*a".repeat(255));
    let l = parse_list(&rule);
    let m = Matcher::from_list("h", l);
    let url = Url::parse(&format!("https://h.example/{}", "a".repeat(8000))).unwrap();
    let v = check_request(
        &m,
        &Request {
            url: &url,
            source_origin: None,
            resource_type: ResourceType::Other,
            is_third_party: false,
        },
    );
    assert!(matches!(v, Verdict::Block { .. }));
}

#[test]
fn nuls_controls_and_unicode() {
    let text = "||a.com/\0^\n\0\n\u{0}||x.com^\n||\u{e9}xample.com^\n\
                ||\u{1F600}.example^\n\u{202e}||rtl.example^\n\
                \u{feff}||bom.example^\n||a.com^$script\0\n\
                ||a.com^$domain=\u{e9}.com\n\t\u{b}\u{c}\n";
    let l = parse_list(text);
    let s = &l.stats;
    assert_eq!(
        s.lines,
        s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
    );
    // The NUL-bearing patterns are rejected, not truncated at the NUL.
    assert!(s.rejected_for(RejectReason::InvalidCharacters) >= 2);
    // Non-ASCII hosts are accepted as opaque literal bytes and match
    // nothing an ASCII-only URL can contain.
    let m = Matcher::from_list("u", l);
    let u = Url::parse("https://xample.com/").unwrap();
    assert_eq!(
        check_request(
            &m,
            &Request {
                url: &u,
                source_origin: None,
                resource_type: ResourceType::Other,
                is_third_party: false
            }
        ),
        Verdict::Allow
    );
}

#[test]
fn invalid_utf8_and_binary() {
    let mut bytes = Vec::new();
    for i in 0..4096u32 {
        bytes.push((i * 7 + 3) as u8);
        if i % 61 == 0 {
            bytes.push(b'\n');
        }
    }
    let l = parse_list_bytes(&bytes);
    let s = &l.stats;
    assert_eq!(
        s.lines,
        s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
    );
    let _ = Matcher::from_list("bin", l);
}

#[test]
fn many_options_and_domains_are_bounded() {
    let opts = "script,".repeat(700);
    let l = parse_list(&format!("||a.com^${opts}"));
    assert_eq!(l.stats.rejected_for(RejectReason::LineTooLong), 1);
    let opts = "script,".repeat(100);
    assert_eq!(parse_list(&format!("||a.com^${opts}")).stats.accepted, 1);
    let domains: Vec<String> = (0..300).map(|i| format!("d{i}.com")).collect();
    let l = parse_list(&format!("||a.com^$domain={}", domains.join("|")));
    // 300 * ~7 bytes < 4096: reaches the per-rule domain cap.
    assert_eq!(l.stats.rejected_for(RejectReason::TooManyDomains), 1);
}

#[test]
fn long_url_against_adversarial_patterns() {
    let url = format!("https://h.example/{}", "a".repeat(8000));
    let u = Url::parse(&url).unwrap();
    let rules = [
        format!("{}b", "a".repeat(400)),
        format!("{}b", "a*".repeat(200)),
        format!("||h.example/{}b", "a^".repeat(200)),
        format!("|{}|", "a".repeat(400)),
    ]
    .join("\n");
    let l = parse_list(&rules);
    assert_eq!(l.rules.len(), 4);
    let m = Matcher::from_list("adv", l);
    let v = check_request(
        &m,
        &Request {
            url: &u,
            source_origin: None,
            resource_type: ResourceType::Other,
            is_third_party: false,
        },
    );
    assert_eq!(v, Verdict::Allow);
}

// ---- differential test of Pattern::matches against exhaustive search ----

fn reference(p: &Pattern, text: &[u8], host: (usize, usize)) -> bool {
    let elems = p.elems();
    let n = elems.len();
    let len = text.len();
    // memo[i][j]: Some(result)
    let mut memo: Vec<Option<bool>> = vec![None; (n + 1) * (len + 1)];
    fn go(
        i: usize,
        j: usize,
        elems: &[Elem],
        text: &[u8],
        end: bool,
        memo: &mut Vec<Option<bool>>,
    ) -> bool {
        let w = text.len() + 1;
        if let Some(r) = memo[i * w + j] {
            return r;
        }
        let r = if i == elems.len() {
            !end || j == text.len()
        } else {
            match elems[i] {
                Elem::Star => {
                    go(i + 1, j, elems, text, end, memo)
                        || (j < text.len() && go(i, j + 1, elems, text, end, memo))
                }
                Elem::Lit(b) => {
                    j < text.len() && text[j] == b && go(i + 1, j + 1, elems, text, end, memo)
                }
                Elem::Sep => {
                    (j < text.len()
                        && is_separator(text[j])
                        && go(i + 1, j + 1, elems, text, end, memo))
                        || (j == text.len() && go(i + 1, j, elems, text, end, memo))
                }
            }
        };
        memo[i * w + j] = Some(r);
        r
    }
    let starts: Vec<usize> = match p.start_anchor() {
        StartAnchor::Anywhere => (0..=len).collect(),
        StartAnchor::Start => vec![0],
        StartAnchor::Host => {
            let mut v = vec![host.0];
            for k in host.0..host.1 {
                if text[k] == b'.' {
                    v.push(k + 1);
                }
            }
            v
        }
    };
    starts
        .into_iter()
        .any(|s| go(0, s, elems, text, p.end_anchored(), &mut memo))
}

#[test]
fn glob_agrees_with_exhaustive_reference() {
    let mut rng = Xorshift(0x9E37_79B9_7F4A_7C15);
    let pat_alpha: &[u8] = b"ab./^*|";
    let txt_alpha: &[u8] = b"ab./?-";
    let mut checked = 0u32;
    let mut matched = 0u32;
    for _ in 0..60_000 {
        let plen = 1 + rng.below(8);
        let mut ptxt = String::new();
        if rng.below(4) == 0 {
            ptxt.push_str(if rng.below(2) == 0 { "||" } else { "|" });
        }
        for _ in 0..plen {
            ptxt.push(pat_alpha[rng.below(pat_alpha.len())] as char);
        }
        // '|' inside the body is a literal; only the edges are anchors.
        let Ok(p) = Pattern::from_text(&ptxt, false) else {
            continue;
        };
        let tlen = rng.below(14);
        let text: Vec<u8> = (0..tlen)
            .map(|_| txt_alpha[rng.below(txt_alpha.len())])
            .collect();
        let hs = rng.below(tlen + 1);
        let he = hs + rng.below(tlen + 1 - hs);
        let want = reference(&p, &text, (hs, he));
        let got = p.matches(&text, Some((hs, he)));
        assert_eq!(
            got,
            want,
            "pattern {ptxt:?} text {:?} host {:?}",
            String::from_utf8_lossy(&text),
            (hs, he)
        );
        checked += 1;
        matched += u32::from(want);
    }
    // The generator must exercise both outcomes substantially.
    assert!(checked > 40_000, "checked {checked}");
    assert!(
        matched > 2_000 && matched < checked - 2_000,
        "matched {matched}"
    );
}

// ---- xorshift fuzz: parse never panics, matching terminates ----

#[test]
fn fuzz_parse_and_match_never_panic() {
    let mut rng = Xorshift(0xDEAD_BEEF_CAFE_F00D);
    let atoms: &[&str] = &[
        "||",
        "|",
        "@@",
        "*",
        "^",
        "$",
        ",",
        "=",
        "~",
        "/",
        ".",
        "-",
        ":",
        "?",
        "&",
        "#",
        "##",
        "#@#",
        "$$",
        "!",
        "[",
        "]",
        "\n",
        "\n",
        "\r\n",
        " ",
        "\0",
        "\u{e9}",
        "\u{1F600}",
        "third-party",
        "script",
        "image",
        "domain=",
        "a.com",
        "b.a.com",
        "important",
        "match-case",
        "redirect=x",
        "xhr",
        "|~",
        "ads",
        "banner",
        "example",
        "com",
        "http://",
        "https://",
        "%20",
        "+js(",
        "#?#",
        "{",
        "}",
        "(",
        ")",
        "\\",
        "0",
        "9",
        "_",
    ];
    let urls: Vec<Url> = [
        "https://ads.example.com/banner/x.js?a=1&b=2",
        "http://a.com/",
        "https://b.a.com:8443/ads/ads/ads?x=%20#f",
        "wss://example.com/socket",
        "ftp://example.com/file.swf",
        "https://10.0.0.1/a",
        "https://[::1]/a",
    ]
    .iter()
    .map(|u| Url::parse(u).unwrap())
    .collect();
    let src = Origin::from_url(&Url::parse("https://a.com/").unwrap());
    let types = [
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
    let mut total_rules = 0usize;
    for round in 0..1500 {
        let mut text = String::new();
        let atoms_n = 1 + rng.below(120);
        for _ in 0..atoms_n {
            text.push_str(atoms[rng.below(atoms.len())]);
        }
        // Occasionally push a long run of one atom to stress caps.
        if round % 97 == 0 {
            let a = atoms[rng.below(atoms.len())];
            text.push_str(&a.repeat(MAX_LINE_BYTES / a.len().max(1) + 3));
        }
        let l = parse_list(&text);
        let s = &l.stats;
        assert_eq!(
            s.lines,
            s.blank + s.comments + s.headers + s.cosmetic_skipped + s.accepted + s.rejected_total()
        );
        assert_eq!(s.accepted as usize, l.rules.len());
        total_rules += l.rules.len();
        let m = Matcher::from_list("fuzz", l);
        for u in &urls {
            for ty in types {
                for tp in [false, true] {
                    let v = check_request(
                        &m,
                        &Request {
                            url: u,
                            source_origin: Some(&src),
                            resource_type: ty,
                            is_third_party: tp,
                        },
                    );
                    // Any rule id reported must exist.
                    if let Verdict::Block { rule_id } | Verdict::AllowedByException { rule_id } = v
                    {
                        assert!(m.rule(rule_id).is_some());
                    }
                }
            }
        }
        // Raw bytes too.
        let raw: Vec<u8> = (0..rng.below(300)).map(|_| rng.next() as u8).collect();
        let _ = parse_list_bytes(&raw);
    }
    assert!(
        total_rules > 500,
        "generator produced only {total_rules} rules"
    );
}

#[test]
fn fuzz_pattern_text_directly() {
    let mut rng = Xorshift(0x1234_5678_9ABC_DEF1);
    let alpha: &[u8] = b"|*^.ab/ \0\xc3\xa9%-";
    for _ in 0..20_000 {
        let n = rng.below(40);
        let bytes: Vec<u8> = (0..n).map(|_| alpha[rng.below(alpha.len())]).collect();
        let s = String::from_utf8_lossy(&bytes).into_owned();
        if let Ok(p) = Pattern::from_text(&s, rng.below(2) == 0) {
            let text: Vec<u8> = (0..rng.below(60))
                .map(|_| alpha[rng.below(alpha.len())])
                .collect();
            let _ = p.matches(&text, Some((0, text.len())));
            let _ = p.matches(&text, None);
            let _ = p.index_tokens();
        }
    }
}
