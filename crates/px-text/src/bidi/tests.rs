#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::*;

const LRE: char = '\u{202a}';
const RLE: char = '\u{202b}';
const PDF: char = '\u{202c}';
const RLI: char = '\u{2067}';
const PDI: char = '\u{2069}';

fn is_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

/// Visual rendering of the whole text as one line, directional controls
/// dropped (BidiCharacterTest omits characters removed by X9 the same way).
fn visual(text: &str, base: BaseDirection) -> String {
    let r = analyze(text, base);
    r.visual_string(0..text.len())
        .expect("a whole single paragraph is a valid line")
        .chars()
        .filter(|c| !is_control(*c))
        .collect()
}

fn levels_without_controls(text: &str, base: BaseDirection) -> Vec<u8> {
    let r = analyze(text, base);
    text.chars()
        .zip(r.char_levels())
        .filter(|(c, _)| !is_control(*c))
        .map(|(_, l)| l)
        .collect()
}

// Expectations are derived by hand from UAX #9; each comment gives the rules.

#[test]
fn pure_ltr_text_has_one_level_zero_run() {
    let r = analyze("hello world", BaseDirection::Ltr);
    assert!(!r.has_rtl());
    assert_eq!(
        r.runs(),
        [Run {
            range: 0..11,
            level: 0
        }]
    );
    assert_eq!(
        r.paragraphs(),
        [Paragraph {
            range: 0..11,
            level: 0
        }]
    );
}

#[test]
fn empty_text_has_no_runs_and_an_empty_line() {
    let r = analyze("", BaseDirection::Ltr);
    assert!(r.runs().is_empty());
    assert_eq!(r.reorder_line(0..0), Some(Vec::new()));
}

#[test]
fn hebrew_inside_latin_in_an_ltr_paragraph() {
    // The spaces between L and R take the embedding direction (N2): level 0.
    let t = "abc אבג def";
    assert_eq!(
        levels_without_controls(t, BaseDirection::Ltr),
        [0, 0, 0, 0, 1, 1, 1, 0, 0, 0, 0]
    );
    assert_eq!(visual(t, BaseDirection::Ltr), "abc גבא def");
}

#[test]
fn latin_inside_hebrew_in_an_rtl_paragraph() {
    // Levels 1 1 1 1 2 2 2 1 1 1 1; reverse everything, then the level-2 run
    // back: "והד abc גבא".
    let t = "אבג abc דהו";
    assert_eq!(
        levels_without_controls(t, BaseDirection::Rtl),
        [1, 1, 1, 1, 2, 2, 2, 1, 1, 1, 1]
    );
    assert_eq!(visual(t, BaseDirection::Rtl), "והד abc גבא");
}

#[test]
fn european_digits_after_hebrew_keep_their_order() {
    // W7 leaves EN as EN after R; I1 raises EN at level 0 to 2; the space
    // between R and EN is R (N1). L2: reverse "123" then everything.
    let t = "אבג 123";
    assert_eq!(
        levels_without_controls(t, BaseDirection::Ltr),
        [1, 1, 1, 1, 2, 2, 2]
    );
    assert_eq!(visual(t, BaseDirection::Ltr), "123 גבא");
}

#[test]
fn arabic_indic_digits_keep_their_order() {
    // AL letters, then AN digits (level 2 in an LTR paragraph), space is R by
    // N1 (AN counts as R).
    let t = "\u{627}\u{628}\u{62c} \u{661}\u{662}\u{663}";
    assert_eq!(
        levels_without_controls(t, BaseDirection::Ltr),
        [1, 1, 1, 1, 2, 2, 2]
    );
    assert_eq!(
        visual(t, BaseDirection::Ltr),
        "\u{661}\u{662}\u{663} \u{62c}\u{628}\u{627}"
    );
}

#[test]
fn explicit_rle_and_nested_lre() {
    // RLE א ב SP LRE c d PDF SP ג ד PDF, LTR paragraph. The embeddings
    // are level runs of their own: "אב " (1), "cd" (2), " גד" (1). The space
    // at each side of "cd" resolves to the embedding direction (N2): 1.
    // Reverse "cd" at level 2, then the whole line at level >= 1.
    let t = format!("{RLE}אב {LRE}cd{PDF} גד{PDF}");
    assert_eq!(
        levels_without_controls(&t, BaseDirection::Ltr),
        [1, 1, 1, 2, 2, 1, 1, 1]
    );
    assert_eq!(visual(&t, BaseDirection::Ltr), "דג cd בא");
}

#[test]
fn isolate_does_not_affect_the_surrounding_run() {
    // a SP RLI א ב SP c PDI SP d: the isolate is one neutral to the outside,
    // between L and L, so level 0. Inside: א ב 1, space N2 -> 1, c L at odd -> 2.
    let t = format!("a {RLI}אב c{PDI} d");
    assert_eq!(
        levels_without_controls(&t, BaseDirection::Ltr),
        [0, 0, 1, 1, 1, 2, 0, 0]
    );
    assert_eq!(visual(&t, BaseDirection::Ltr), "a c בא d");
}

#[test]
fn parentheses_around_rtl_text_are_mirrored_in_an_rtl_paragraph() {
    // All four characters are level 1: reversed ")בא(" and mirrored, which
    // reads "(בא)".
    let t = "(אב)";
    let r = analyze(t, BaseDirection::Rtl);
    assert_eq!(r.char_levels(), [1, 1, 1, 1]);
    assert_eq!(r.visual_string(0..t.len()).unwrap(), "(בא)");
}

#[test]
fn parentheses_around_rtl_text_in_an_ltr_paragraph_stay_put() {
    // The brackets resolve to the embedding direction (N0/N2): level 0, left
    // alone and not mirrored; only the letters reverse.
    let t = "(אב)";
    assert_eq!(levels_without_controls(t, BaseDirection::Ltr), [0, 1, 1, 0]);
    assert_eq!(visual(t, BaseDirection::Ltr), "(בא)");
}

#[test]
fn paired_brackets_take_the_embedding_direction_when_it_matches_their_content() {
    // N0 on UAX #9's own example: RTL paragraph, "smith (fabrikam אבג) דהו".
    // The pair encloses an R character, matching the embedding direction, so
    // both brackets become R (level 1). Without N0, "(" would sit between two
    // L runs and resolve to L (level 2).
    let t = "smith (fabrikam אבג) דהו";
    let mut expect = vec![2; 5]; // smith
    expect.push(1); // space
    expect.push(1); // (
    expect.extend([2; 8]); // fabrikam
    expect.push(1); // space
    expect.extend([1; 3]); // אבג
    expect.push(1); // )
    expect.push(1); // space
    expect.extend([1; 3]); // דהו
    assert_eq!(levels_without_controls(t, BaseDirection::Rtl), expect);
}

#[test]
fn mirror_pairs() {
    assert_eq!(mirror('('), Some(')'));
    assert_eq!(mirror(')'), Some('('));
    assert_eq!(mirror('['), Some(']'));
    assert_eq!(mirror('<'), Some('>'));
    assert_eq!(mirror('\u{ab}'), Some('\u{bb}'));
    assert_eq!(mirror('a'), None);
    assert_eq!(mirror('א'), None);
}

#[test]
fn auto_base_direction_follows_the_first_strong_character() {
    // P2/P3: 123 is not strong; the first strong is Hebrew.
    let r = analyze("123 אבג", BaseDirection::Auto);
    assert_eq!(r.paragraphs()[0].level, 1);
    let r = analyze("123 abc", BaseDirection::Auto);
    assert_eq!(r.paragraphs()[0].level, 0);
    // No strong character at all: left to right.
    let r = analyze("123 !?", BaseDirection::Auto);
    assert_eq!(r.paragraphs()[0].level, 0);
}

#[test]
fn paragraph_separators_start_new_paragraphs_with_their_own_direction() {
    let t = "abc\nאבג\ndef";
    let r = analyze(t, BaseDirection::Auto);
    let p = r.paragraphs();
    assert_eq!(p.len(), 3);
    assert_eq!(
        p[0],
        Paragraph {
            range: 0..4,
            level: 0
        }
    );
    // "אבג" is 6 bytes plus the newline.
    assert_eq!(
        p[1],
        Paragraph {
            range: 4..11,
            level: 1
        }
    );
    assert_eq!(
        p[2],
        Paragraph {
            range: 11..14,
            level: 0
        }
    );
    // Runs never cross a paragraph.
    for run in r.runs() {
        assert!(
            p.iter()
                .any(|q| q.range.start <= run.range.start && run.range.end <= q.range.end)
        );
    }
}

#[test]
fn trailing_space_of_a_line_takes_the_paragraph_level() {
    // "אב גד" in an LTR paragraph. Unbroken, the space is between two R: level
    // 1. Break after the space (bytes 0..5): L1 resets it to level 0, so it
    // is drawn after "בא", not before.
    let t = "אב גד";
    let r = analyze(t, BaseDirection::Ltr);
    assert_eq!(r.char_levels(), [1, 1, 1, 1, 1]);
    assert_eq!(r.visual_string(0..5).unwrap(), "בא ");
    assert_eq!(
        r.reorder_line(0..5).unwrap(),
        [
            Run {
                range: 0..4,
                level: 1
            },
            Run {
                range: 4..5,
                level: 0
            }
        ]
    );
    // The unbroken line puts the space inside the RTL run.
    assert_eq!(r.visual_string(0..t.len()).unwrap(), "דג בא");
}

#[test]
fn lines_reorder_independently_after_a_break() {
    // RTL paragraph "abc אבג def" broken after the first space: the second
    // line is "אבג def".
    let t = "abc אבג def";
    let r = analyze(t, BaseDirection::Rtl);
    let first = r.visual_string(0..4).unwrap();
    let second = r.visual_string(4..t.len()).unwrap();
    // Line 2: levels 1 1 1 1 2 2 2 -> reversed whole then "def" back.
    assert_eq!(second, "def גבא");
    // Line 1: "abc" at 2, trailing space at paragraph level 1: the space goes
    // to the left (RTL paragraph).
    assert_eq!(first, " abc");
}

#[test]
fn visual_runs_cover_the_line_exactly_once() {
    let t = "abc אבג 123 def ١٢٣";
    let r = analyze(t, BaseDirection::Auto);
    for (a, b) in [(0, t.len()), (0, 8), (4, 14), (7, t.len())] {
        if !t.is_char_boundary(a) || !t.is_char_boundary(b) {
            continue;
        }
        let runs = r.reorder_line(a..b).unwrap();
        let mut covered: Vec<Range<usize>> = runs.iter().map(|x| x.range.clone()).collect();
        covered.sort_by_key(|x| x.start);
        let mut at = a;
        for c in covered {
            assert_eq!(c.start, at);
            at = c.end;
        }
        assert_eq!(at, b);
    }
}

#[test]
fn invalid_lines_are_none_not_panics() {
    let t = "אבג\nabc";
    let r = analyze(t, BaseDirection::Ltr);
    // Past the end, reversed, inside a code point, across two paragraphs.
    assert_eq!(r.reorder_line(0..100), None);
    let (start, end) = (5, 2);
    assert_eq!(r.reorder_line(start..end), None);
    assert_eq!(r.reorder_line(1..3), None);
    assert_eq!(r.reorder_line(0..t.len()), None);
    assert_eq!(r.visual_string(0..100), None);
    assert!(r.reorder_line(0..7).is_some());
}

#[test]
fn free_function_matches_the_method() {
    let r = analyze("abc אבג", BaseDirection::Ltr);
    assert_eq!(reorder_line(&r, 0..11), r.reorder_line(0..11));
}

#[test]
fn deep_embedding_nesting_is_clamped_not_fatal() {
    // 200 nested RLE/LRE pairs exceed the 125-level limit (X1-X8 overflow
    // counters), then 200 PDFs.
    let mut t = String::new();
    for i in 0..200 {
        t.push(if i % 2 == 0 { RLE } else { LRE });
    }
    t.push_str("a\u{5d0}b");
    for _ in 0..200 {
        t.push(PDF);
    }
    let r = analyze(&t, BaseDirection::Ltr);
    assert!(r.char_levels().iter().all(|&l| l <= 126));
    let _ = r.reorder_line(0..t.len()).unwrap();
    let _ = r.visual_string(0..t.len()).unwrap();
}

#[test]
fn unmatched_controls_are_fine() {
    for t in [
        format!("{PDF}{PDF}{PDI}a"),
        format!("{RLI}{RLI}{RLI}אבג"),
        format!("a{RLE}{LRE}{PDF}"),
        "\u{2068}x".to_string(),
        "\u{202e}abc\u{202d}def".to_string(),
    ] {
        for base in [BaseDirection::Ltr, BaseDirection::Rtl, BaseDirection::Auto] {
            let r = analyze(&t, base);
            let _ = r.runs();
            let _ = r.visual_string(0..t.len());
        }
    }
}

fn xorshift(seed: u64) -> impl FnMut() -> u64 {
    let mut x = seed;
    move || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Code points weighted toward the classes that matter: Hebrew, Arabic,
/// digits, directional controls, brackets, separators, plus the full range
/// (unassigned, noncharacters, surrogates are skipped by `from_u32`).
fn random_char(next: &mut impl FnMut() -> u64) -> char {
    let r = next();
    let cp = match r % 8 {
        0 => 0x0590 + (r >> 8) % 0x100, // Hebrew block
        1 => 0x0600 + (r >> 8) % 0x100, // Arabic block
        2 => 0x20 + (r >> 8) % 0x60,    // printable ASCII
        3 => 0x2000 + (r >> 8) % 0x70,  // general punctuation, controls
        4 => (r >> 8) % 0x11_0000,
        5 => 0xFFFE + (r >> 8) % 2 + ((r >> 16) % 17) * 0x1_0000, // noncharacters
        6 => 0x0A + (r >> 8) % 4,                                 // newlines
        _ => (r >> 8) % 0x3000,
    };
    char::from_u32(cp as u32).unwrap_or('\u{fffd}')
}

#[test]
fn random_text_never_panics_and_keeps_its_invariants() {
    let mut next = xorshift(0x9E37_79B9_7F4A_7C15);
    for round in 0..600 {
        let n = (next() % 60) as usize;
        let t: String = (0..n).map(|_| random_char(&mut next)).collect();
        let base = [BaseDirection::Ltr, BaseDirection::Rtl, BaseDirection::Auto][round % 3];
        let r = analyze(&t, base);

        // Runs partition the text.
        let runs = r.runs();
        let mut at = 0;
        for run in &runs {
            assert_eq!(run.range.start, at, "{t:?}");
            assert!(run.range.end > run.range.start);
            assert!(run.level <= 126);
            at = run.range.end;
        }
        assert_eq!(at, t.len(), "{t:?}");
        assert_eq!(r.char_levels().len(), t.chars().count());

        // Every paragraph is a valid line and its visual runs cover it once.
        for p in r.paragraphs() {
            let runs = r
                .reorder_line(p.range.clone())
                .expect("paragraph is a line");
            let mut spans: Vec<_> = runs.iter().map(|x| x.range.clone()).collect();
            spans.sort_by_key(|s| s.start);
            let mut at = p.range.start;
            for s in spans {
                assert_eq!(s.start, at, "{t:?}");
                at = s.end;
            }
            assert_eq!(at, p.range.end, "{t:?}");
            let s = r.visual_string(p.range.clone()).expect("string");
            assert_eq!(s.chars().count(), t[p.range.clone()].chars().count());
        }

        // Random sub-lines inside the first paragraph.
        if let Some(p) = r.paragraphs().first() {
            for _ in 0..4 {
                let a = (next() as usize) % (p.range.end + 1);
                let b = (next() as usize) % (p.range.end + 1);
                let (a, b) = if a <= b { (a, b) } else { (b, a) };
                let _ = r.reorder_line(a..b);
                let _ = r.visual_string(a..b);
            }
        }
    }
}
