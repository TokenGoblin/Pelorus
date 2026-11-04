#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

fn opts() -> Options {
    Options::default()
}

fn offs(text: &str, o: &Options) -> Vec<usize> {
    opportunities(text, o).iter().map(|b| b.offset).collect()
}

fn offs_of(text: &str, o: &Options, kind: Kind) -> Vec<usize> {
    opportunities(text, o)
        .iter()
        .filter(|b| b.kind == kind)
        .map(|b| b.offset)
        .collect()
}

// Expectations are derived by hand from UAX #14 and CSS Text 3.

#[test]
fn empty_text_has_no_opportunities() {
    assert!(opportunities("", &opts()).is_empty());
}

#[test]
fn space_separated_words_break_after_the_space() {
    // LB18: break after spaces. LB7: never before them.
    assert_eq!(offs("hello world", &opts()), [6, 11]);
    assert_eq!(offs_of("hello world", &opts(), Kind::Soft), [6]);
    // The end of text is always the last (hard) break (LB3).
    assert_eq!(offs_of("hello world", &opts(), Kind::Hard), [11]);
}

#[test]
fn a_run_of_spaces_breaks_only_after_the_last() {
    assert_eq!(offs("a   b", &opts()), [4, 5]);
}

#[test]
fn hyphen_minus_breaks_after_it() {
    // LB21: BA/HY: no break before, a break after.
    assert_eq!(offs("a-b", &opts()), [2, 3]);
}

#[test]
fn no_break_around_a_no_break_space() {
    // LB12: GL x and x GL. "a NBSP b" is one unit, then a space, then c.
    let t = "a\u{a0}b c";
    assert_eq!(offs(t, &opts()), [5, 6]);
}

#[test]
fn no_break_before_closing_punctuation_or_exclamation() {
    // LB13: x CL, x EX (even after spaces). "a) b": no break at 1 or 2.
    assert_eq!(offs("a) b", &opts()), [3, 4]);
    // "a !b": the space does not make a break before '!'.
    assert_eq!(offs("a !b", &opts()), [3, 4]);
}

#[test]
fn cjk_breaks_between_every_ideograph() {
    // Four ideographs, three bytes each.
    assert_eq!(offs("漢字仮名", &opts()), [3, 6, 9, 12]);
}

#[test]
fn cjk_no_break_before_closing_corner_bracket() {
    // 字 」 字: LB13 x CL forbids a break before U+300D, LB: CL then ID may.
    let t = "字」字";
    assert_eq!(offs(t, &opts()), [6, 9]);
}

#[test]
fn cjk_no_break_before_ideographic_full_stop() {
    // U+3002 is class CL: no break before it.
    assert_eq!(offs("字。字", &opts()), [6, 9]);
}

#[test]
fn small_kana_follow_strictness() {
    // U+3041 small a is CJ: strict resolves it to NS (no break before it),
    // normal and loose to ID (break allowed). CSS Text 3, line-break.
    let t = "字ぁ";
    let strict = Options {
        line_break: LineBreak::Strict,
        ..opts()
    };
    let normal = Options {
        line_break: LineBreak::Normal,
        ..opts()
    };
    assert_eq!(offs(t, &strict), [6]);
    assert_eq!(offs(t, &normal), [3, 6]);
}

#[test]
fn word_break_break_all_breaks_between_letters() {
    let o = Options {
        word_break: WordBreak::BreakAll,
        ..opts()
    };
    assert_eq!(offs("hello", &o), [1, 2, 3, 4, 5]);
    // Spaces still behave: the break goes after the space, not before it.
    assert_eq!(offs("ab c", &o), [1, 3, 4]);
}

#[test]
fn word_break_keep_all_suppresses_cjk_breaks() {
    let keep = Options {
        word_break: WordBreak::KeepAll,
        ..opts()
    };
    // 漢字 space 仮名: only the space gives a break.
    let t = "漢字 仮名";
    assert_eq!(offs(t, &keep), [7, 13]);
    // Without keep-all each ideograph is a break (and the space's break).
    assert_eq!(offs(t, &opts()), [3, 7, 10, 13]);
}

#[test]
fn overflow_wrap_adds_emergency_breaks_at_grapheme_boundaries() {
    let any = Options {
        overflow_wrap: OverflowWrap::Anywhere,
        ..opts()
    };
    assert_eq!(offs_of("abcd", &any, Kind::Soft), Vec::<usize>::new());
    assert_eq!(offs_of("abcd", &any, Kind::Emergency), [1, 2, 3]);
    // A combining acute stays with its base: e + U+0301 is one cluster.
    assert_eq!(offs_of("e\u{301}x", &any, Kind::Emergency), [3]);
    // An emergency break is not added where a soft one already is.
    assert_eq!(offs_of("ab cd", &any, Kind::Soft), [3]);
    assert_eq!(offs_of("ab cd", &any, Kind::Emergency), [1, 2, 4]);
    // break-word has the same offsets; only min-content differs.
    let bw = Options {
        overflow_wrap: OverflowWrap::BreakWord,
        ..opts()
    };
    assert_eq!(offs("abcd", &bw), offs("abcd", &any));
    assert!(any.emergency_breaks_in_min_content());
    assert!(!bw.emergency_breaks_in_min_content());
}

#[test]
fn legacy_word_break_break_word_is_normal_plus_anywhere() {
    let o = Options {
        word_break: WordBreak::BreakWord,
        ..opts()
    };
    assert_eq!(offs_of("abcd", &o, Kind::Emergency), [1, 2, 3]);
    assert!(o.emergency_breaks_in_min_content());
}

#[test]
fn overflow_wrap_normal_has_no_emergency_breaks() {
    assert!(offs_of("abcd", &opts(), Kind::Emergency).is_empty());
}

#[test]
fn emergency_breaks_do_not_apply_when_not_wrapping() {
    for ws in [WhiteSpace::Nowrap, WhiteSpace::Pre] {
        let o = Options {
            overflow_wrap: OverflowWrap::Anywhere,
            white_space: ws,
            ..opts()
        };
        assert_eq!(offs("abc def", &o), [7], "{ws:?}");
    }
}

#[test]
fn line_break_anywhere_makes_every_cluster_a_soft_break() {
    let o = Options {
        line_break: LineBreak::Anywhere,
        ..opts()
    };
    assert_eq!(offs_of("abc", &o, Kind::Soft), [1, 2]);
    assert_eq!(offs("e\u{301}x", &o), [3, 4]);
}

#[test]
fn soft_hyphen_is_a_break_that_wants_a_hyphen() {
    // "co" SHY "op": c0 o1 SHY(2..4) o4 p5 -> break at 4.
    let t = "co\u{ad}op";
    let r = opportunities(t, &opts());
    assert_eq!(
        r,
        [
            Opportunity {
                offset: 4,
                kind: Kind::Soft,
                hyphen: true
            },
            Opportunity {
                offset: 6,
                kind: Kind::Hard,
                hyphen: false
            },
        ]
    );
}

#[test]
fn hyphens_none_ignores_soft_hyphens() {
    let o = Options {
        hyphens: Hyphens::None,
        ..opts()
    };
    assert_eq!(offs("co\u{ad}op", &o), [6]);
}

#[test]
fn newline_is_a_forced_break_only_when_white_space_preserves_it() {
    for ws in [
        WhiteSpace::Pre,
        WhiteSpace::PreWrap,
        WhiteSpace::PreLine,
        WhiteSpace::BreakSpaces,
    ] {
        let o = Options {
            white_space: ws,
            ..opts()
        };
        assert_eq!(offs_of("foo\nbar", &o, Kind::Hard), [4, 7], "{ws:?}");
    }
    // normal: the newline is a collapsible space: a soft opportunity after it.
    assert_eq!(offs_of("foo\nbar", &opts(), Kind::Hard), [7]);
    assert_eq!(offs_of("foo\nbar", &opts(), Kind::Soft), [4]);
    // nowrap: collapsed and never wrapped.
    let nowrap = Options {
        white_space: WhiteSpace::Nowrap,
        ..opts()
    };
    assert_eq!(offs("foo\nbar", &nowrap), [7]);
}

#[test]
fn crlf_is_one_forced_break_after_the_lf() {
    let o = Options {
        white_space: WhiteSpace::PreWrap,
        ..opts()
    };
    // a0 CR1 LF2 b3: no break between CR and LF (LB5).
    assert_eq!(offs("a\r\nb", &o), [3, 4]);
    // A lone CR is a break after itself.
    assert_eq!(offs_of("a\rb", &o, Kind::Hard), [2, 3]);
}

#[test]
fn separators_force_a_break_in_every_mode() {
    // U+2028 is three bytes: a0, LS 1..4, b4.
    for ws in [WhiteSpace::Normal, WhiteSpace::Nowrap, WhiteSpace::Pre] {
        let o = Options {
            white_space: ws,
            ..opts()
        };
        assert_eq!(offs_of("a\u{2028}b", &o, Kind::Hard), [4, 5], "{ws:?}");
    }
    // Form feed, one byte.
    assert_eq!(offs_of("a\u{c}b", &opts(), Kind::Hard), [2, 3]);
}

#[test]
fn pre_and_nowrap_have_no_soft_breaks() {
    for ws in [WhiteSpace::Pre, WhiteSpace::Nowrap] {
        let o = Options {
            white_space: ws,
            ..opts()
        };
        assert!(offs_of("a b-c 漢字", &o, Kind::Soft).is_empty(), "{ws:?}");
    }
}

#[test]
fn break_spaces_allows_a_break_after_every_preserved_space() {
    let o = Options {
        white_space: WhiteSpace::BreakSpaces,
        ..opts()
    };
    // a0 s1 s2 b3: normal has one break (3); break-spaces also breaks after the
    // first space (2).
    assert_eq!(offs("a  b", &o), [2, 3, 4]);
    assert_eq!(offs("a  b", &opts()), [3, 4]);
    // pre-wrap does not.
    let pw = Options {
        white_space: WhiteSpace::PreWrap,
        ..opts()
    };
    assert_eq!(offs("a  b", &pw), [3, 4]);
    // A trailing space adds no extra offset beyond the end.
    assert_eq!(offs("a ", &o), [2]);
}

#[test]
fn devanagari_breaks_only_at_spaces_never_inside_a_syllable() {
    // नमस्ते (6 code points, 18 bytes) space दुनिया (6 code points, 18 bytes).
    // Virama and vowel signs are combining (CM): no break before them (LB9),
    // and Devanagari letters are AL, so the only opportunity is after the space.
    let t = "नमस्ते दुनिया";
    assert_eq!(offs(t, &opts()), [19, 37]);
    // Under word-break: break-all it breaks between letters but still not
    // before a combining mark: न|म|स् |ते -> boundaries after each base+marks.
    let all = Options {
        word_break: WordBreak::BreakAll,
        ..opts()
    };
    let r = offs(t, &all);
    assert!(r.contains(&3) && r.contains(&19) && r.contains(&37));
    // No offset falls between स (bytes 6..9) and its virama (9..12), or
    // between त (12..15) and the vowel sign (15..18).
    assert!(!r.contains(&9) && !r.contains(&15), "{r:?}");
}

#[test]
fn thai_without_spaces_gets_interior_breaks_from_the_lstm() {
    // "สวัสดีครับ" has no spaces; UAX #14 alone would give one unbreakable run
    // (class SA). The compiled LSTM data adds word boundaries. Which ones is
    // the model's business; the contract is that there is at least one, and
    // that every offset is valid.
    let t = "สวัสดีครับผมชื่อสมชายยินดีที่ได้รู้จักครับ";
    let r = offs(t, &opts());
    assert!(r.len() > 1, "no interior break found in Thai: {r:?}");
    assert_eq!(r.last().copied(), Some(t.len()));
    assert!(r.iter().all(|&i| t.is_char_boundary(i)));
}

#[test]
fn offsets_are_valid_for_every_option_combination() {
    let samples = [
        "",
        "plain words here",
        "漢字かな交じり文、「引用」。",
        "a\r\nb\rc\nd\u{2029}e",
        "co\u{ad}operate  \t spaced",
        "e\u{301}\u{200d}x\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}!",
        "สวัสดีครับ",
        "\u{0}\u{7f}\u{fffd}\u{10ffff}",
    ];
    let lbs = [
        LineBreak::Auto,
        LineBreak::Loose,
        LineBreak::Normal,
        LineBreak::Strict,
        LineBreak::Anywhere,
    ];
    let wbs = [
        WordBreak::Normal,
        WordBreak::BreakAll,
        WordBreak::KeepAll,
        WordBreak::BreakWord,
    ];
    let ows = [
        OverflowWrap::Normal,
        OverflowWrap::BreakWord,
        OverflowWrap::Anywhere,
    ];
    let wss = [
        WhiteSpace::Normal,
        WhiteSpace::Nowrap,
        WhiteSpace::Pre,
        WhiteSpace::PreWrap,
        WhiteSpace::PreLine,
        WhiteSpace::BreakSpaces,
    ];
    for t in samples {
        for line_break in lbs {
            for word_break in wbs {
                for overflow_wrap in ows {
                    for white_space in wss {
                        for hyphens in [Hyphens::None, Hyphens::Manual] {
                            let o = Options {
                                line_break,
                                word_break,
                                overflow_wrap,
                                white_space,
                                hyphens,
                            };
                            check_invariants(t, &o);
                        }
                    }
                }
            }
        }
    }
}

fn check_invariants(t: &str, o: &Options) {
    let r = opportunities(t, o);
    if t.is_empty() {
        assert!(r.is_empty());
        return;
    }
    let mut prev = 0;
    for b in &r {
        assert!(b.offset > prev, "{t:?} {o:?}: not increasing: {r:?}");
        assert!(t.is_char_boundary(b.offset), "{t:?} {o:?}: {b:?}");
        prev = b.offset;
    }
    let last = r.last().expect("non-empty");
    assert_eq!(last.offset, t.len(), "{t:?} {o:?}");
    assert_eq!(last.kind, Kind::Hard);
}

#[test]
fn random_text_never_panics() {
    // xorshift64*, over code points including unassigned and noncharacters.
    let mut x: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    for round in 0..400 {
        let n = (next() % 40) as usize;
        let t: String = (0..n)
            .filter_map(|_| {
                let r = next();
                // Mix of ASCII, the BMP, and the full range.
                let cp = match r % 4 {
                    0 => (r >> 8) % 0x80,
                    1 => (r >> 8) % 0x3100,
                    2 => 0x0E00 + (r >> 8) % 0x80,
                    _ => (r >> 8) % 0x11_0000,
                };
                char::from_u32(cp as u32)
            })
            .collect();
        let o = Options {
            line_break: [
                LineBreak::Auto,
                LineBreak::Loose,
                LineBreak::Normal,
                LineBreak::Strict,
                LineBreak::Anywhere,
            ][round % 5],
            word_break: [WordBreak::Normal, WordBreak::BreakAll, WordBreak::KeepAll][round % 3],
            overflow_wrap: [OverflowWrap::Normal, OverflowWrap::Anywhere][round % 2],
            white_space: [
                WhiteSpace::Normal,
                WhiteSpace::PreWrap,
                WhiteSpace::BreakSpaces,
                WhiteSpace::Pre,
            ][round % 4],
            hyphens: Hyphens::Manual,
        };
        check_invariants(&t, &o);
    }
}
