#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use super::synthetic;
use super::*;

fn shaper() -> HarfrustShaper {
    HarfrustShaper::new(Arc::from(synthetic::font()), 0).expect("synthetic font loads")
}

fn run(text: &str, direction: Direction, features: &[Feature]) -> ShapedRun {
    shaper()
        .shape(&ShapeInput {
            text,
            script: Script::LATIN,
            language: Some("en"),
            direction,
            features,
        })
        .expect("shapes")
}

fn ids(r: &ShapedRun) -> Vec<u32> {
    r.glyphs.iter().map(|g| g.glyph_id).collect()
}
fn advances(r: &ShapedRun) -> Vec<i32> {
    r.glyphs.iter().map(|g| g.x_advance).collect()
}
fn clusters(r: &ShapedRun) -> Vec<u32> {
    r.glyphs.iter().map(|g| g.cluster).collect()
}

// Expectations below are hand-derived from the tables in `synthetic.rs`.

#[test]
fn units_per_em_is_the_fonts() {
    assert_eq!(shaper().units_per_em(), 1000);
}

#[test]
fn plain_latin_uses_hmtx_advances() {
    // space (250), A (600), no kern pair (space, A).
    let r = run(" A", Direction::LeftToRight, &[]);
    assert_eq!(ids(&r), [6, 1]);
    assert_eq!(advances(&r), [250, 600]);
    assert_eq!(clusters(&r), [0, 1]);
    assert_eq!(r.total_advance(), 850);
}

#[test]
fn kerning_pair_av_is_tightened_by_80() {
    let r = run("AV", Direction::LeftToRight, &[]);
    assert_eq!(ids(&r), [1, 2]);
    assert_eq!(advances(&r), [520, 600]);
    assert_eq!(clusters(&r), [0, 1]);
}

#[test]
fn kerning_can_be_switched_off() {
    let r = run(
        "AV",
        Direction::LeftToRight,
        &[Feature::global(*b"kern", 0)],
    );
    assert_eq!(advances(&r), [600, 600]);
}

#[test]
fn fi_ligature_forms_one_glyph() {
    let r = run("fi", Direction::LeftToRight, &[]);
    assert_eq!(ids(&r), [5]);
    assert_eq!(advances(&r), [500]);
    assert_eq!(clusters(&r), [0]);
}

#[test]
fn ligature_inside_a_longer_word_keeps_clusters() {
    // f, then f+i: clusters are byte offsets 0 and 1.
    let r = run("ffi", Direction::LeftToRight, &[]);
    assert_eq!(ids(&r), [3, 5]);
    assert_eq!(advances(&r), [300, 500]);
    assert_eq!(clusters(&r), [0, 1]);
}

#[test]
fn ligature_off_by_feature() {
    let r = run(
        "fi",
        Direction::LeftToRight,
        &[Feature::global(*b"liga", 0)],
    );
    assert_eq!(ids(&r), [3, 4]);
    assert_eq!(advances(&r), [300, 250]);
}

#[test]
fn feature_range_is_in_bytes() {
    // liga off only for byte range 0..1 (just the first f): the f+i pair
    // straddles the range end, so it still ligates only when it is not
    // fully inside; with the range covering `f` alone the ligature is
    // suppressed because its first component is disabled.
    let off = Feature {
        tag: *b"liga",
        value: 0,
        start: 0,
        end: 1,
    };
    let r = run("fi", Direction::LeftToRight, &[off]);
    assert_eq!(ids(&r), [3, 4]);
}

#[test]
fn unmapped_character_is_notdef() {
    let r = run("z", Direction::LeftToRight, &[]);
    assert_eq!(ids(&r), [0]);
    assert_eq!(advances(&r), [500]);
}

#[test]
fn multibyte_clusters_are_byte_offsets() {
    // U+00E9 is two bytes and unmapped; A follows at byte 2.
    let r = run("\u{e9}A", Direction::LeftToRight, &[]);
    assert_eq!(clusters(&r), [0, 2]);
}

#[test]
fn rtl_returns_visual_order_with_decreasing_clusters() {
    // Logical A V, visual V A. GPOS runs over the reversed (visual) buffer for
    // RTL, so the (A, V) pair does not match and nothing is kerned.
    let r = run("AV", Direction::RightToLeft, &[]);
    assert_eq!(ids(&r), [2, 1]);
    assert_eq!(clusters(&r), [1, 0]);
    assert_eq!(r.total_advance(), 600 + 600);
}

#[test]
fn empty_text_is_an_empty_run() {
    let r = run("", Direction::LeftToRight, &[]);
    assert!(r.glyphs.is_empty());
    assert_eq!(r.units_per_em, 1000);
}

#[test]
fn shaping_is_deterministic() {
    let a = run("AV fi A fi", Direction::LeftToRight, &[]);
    let b = run("AV fi A fi", Direction::LeftToRight, &[]);
    assert_eq!(a, b);
}

#[test]
fn run_over_the_limit_is_an_error_and_at_the_limit_is_not() {
    let s = shaper();
    let big = "A".repeat(MAX_RUN_BYTES + 1);
    let input = ShapeInput {
        text: &big,
        script: Script::LATIN,
        language: None,
        direction: Direction::LeftToRight,
        features: &[],
    };
    assert_eq!(s.shape(&input).unwrap_err(), ShapeError::RunTooLarge);
    let ok = "A".repeat(MAX_RUN_BYTES);
    let r = s
        .shape(&ShapeInput { text: &ok, ..input })
        .expect("at the limit");
    assert_eq!(r.glyphs.len(), MAX_RUN_BYTES);
}

#[test]
fn too_many_features_is_an_error() {
    let s = shaper();
    let f = vec![Feature::global(*b"liga", 1); MAX_FEATURES + 1];
    let err = s
        .shape(&ShapeInput {
            text: "fi",
            script: Script::LATIN,
            language: None,
            direction: Direction::LeftToRight,
            features: &f,
        })
        .unwrap_err();
    assert_eq!(err, ShapeError::TooManyFeatures);
}

#[test]
fn odd_script_and_language_tags_do_not_panic() {
    let s = shaper();
    for script in [[0u8; 4], *b"\xff\xff\xff\xff", *b"zzzz", *b"Zzzz"] {
        for lang in [
            None,
            Some(""),
            Some("x"),
            Some("\u{0}"),
            Some("en-US-x-private-very-long-tag"),
        ] {
            let _ = s.shape(&ShapeInput {
                text: "AV fi",
                script: Script(script),
                language: lang,
                direction: Direction::LeftToRight,
                features: &[],
            });
        }
    }
}

#[test]
fn au_conversion_rounds_half_away_from_zero() {
    // 16 px = 960 Au, upem 1000: 500 units are 480 Au exactly.
    assert_eq!(font_units_to_au(500, 1000, 960), 480);
    // 1 unit at 960 Au is 0.96 Au: rounds to 1.
    assert_eq!(font_units_to_au(1, 1000, 960), 1);
    assert_eq!(font_units_to_au(-1, 1000, 960), -1);
    // Exactly half: 1 * 500 / 1000 = 0.5 rounds away from zero.
    assert_eq!(font_units_to_au(1, 1000, 500), 1);
    assert_eq!(font_units_to_au(-1, 1000, 500), -1);
    // 0.49 stays 0.
    assert_eq!(font_units_to_au(1, 1000, 490), 0);
    assert_eq!(font_units_to_au(0, 1000, 960), 0);
}

#[test]
fn au_conversion_is_total() {
    assert_eq!(font_units_to_au(5, 0, 960), 0);
    assert_eq!(font_units_to_au(i32::MAX, 1, i32::MAX), i32::MAX);
    assert_eq!(font_units_to_au(i32::MIN, 1, i32::MAX), i32::MIN);
    assert_eq!(font_units_to_au(i32::MAX, 1000, i32::MIN), i32::MIN);
}

#[test]
fn au_conversion_of_a_shaped_run() {
    let r = run("AV", Direction::LeftToRight, &[]);
    // 16 px = 960 Au: 520 units -> 499.2 -> 499; 600 -> 576.
    let au: Vec<i32> = r
        .glyphs
        .iter()
        .map(|g| font_units_to_au(g.x_advance, r.units_per_em, 960))
        .collect();
    assert_eq!(au, [499, 576]);
}

// Hostile fonts.

#[test]
fn not_a_font_is_an_error() {
    for bytes in [
        &b""[..],
        b"abc",
        &[0u8; 64],
        b"OTTO",
        b"true",
        b"\0\x01\0\0",
    ] {
        assert!(HarfrustShaper::new(Arc::from(bytes), 0).is_err());
    }
}

#[test]
fn font_index_out_of_range_is_an_error() {
    assert!(HarfrustShaper::new(Arc::from(synthetic::font()), 7).is_err());
}

fn exercise(shaper: &HarfrustShaper) {
    for dir in [Direction::LeftToRight, Direction::RightToLeft] {
        let _ = shaper.shape(&ShapeInput {
            text: "AV fi ffi z\u{e9} \u{644}\u{627}",
            script: Script::LATIN,
            language: Some("en"),
            direction: dir,
            features: &[],
        });
    }
}

#[test]
fn truncating_the_font_at_every_byte_never_panics() {
    let font = synthetic::font();
    let mut accepted = 0usize;
    for len in 0..font.len() {
        let bytes: Arc<[u8]> = Arc::from(&font[..len]);
        if let Ok(s) = HarfrustShaper::new(bytes, 0) {
            accepted += 1;
            exercise(&s);
        }
    }
    // Only the full file is complete; a few truncations that only cut table
    // padding may still load. The point of the loop is that none panics.
    assert!(accepted < font.len());
}

#[test]
fn corrupting_every_byte_never_panics() {
    let font = synthetic::font();
    for i in 0..font.len() {
        for xor in [0xFFu8, 0x80, 0x01] {
            let mut bytes = font.clone();
            if let Some(b) = bytes.get_mut(i) {
                *b ^= xor;
            }
            if let Ok(s) = HarfrustShaper::new(Arc::from(bytes), 0) {
                exercise(&s);
            }
        }
    }
}

#[test]
fn random_garbage_fonts_never_panic() {
    // xorshift64*: deterministic, no dependency.
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = || {
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    };
    let font = synthetic::font();
    for _ in 0..2000 {
        let mut bytes = font.clone();
        let edits = 1 + (next() % 8) as usize;
        for _ in 0..edits {
            let i = (next() as usize) % bytes.len();
            if let Some(b) = bytes.get_mut(i) {
                *b = next() as u8;
            }
        }
        if let Ok(s) = HarfrustShaper::new(Arc::from(bytes), 0) {
            exercise(&s);
        }
    }
}

// Real fonts. Complex scripts need fonts that are not committed yet, so the
// parity test reads them from a directory named by an environment variable and
// says plainly when it skips.

const FONT_DIR_VAR: &str = "PELORUS_PARITY_FONTS";

fn parity_font(name: &str) -> Option<HarfrustShaper> {
    let dir = match std::env::var_os(FONT_DIR_VAR) {
        Some(d) => std::path::PathBuf::from(d),
        None => {
            eprintln!(
                "SKIP parity/{name}: {FONT_DIR_VAR} is not set (a directory holding latin.ttf, \
                 arabic.ttf, hebrew.ttf, devanagari.ttf)"
            );
            return None;
        }
    };
    let path = dir.join(format!("{name}.ttf"));
    match std::fs::read(&path) {
        Ok(bytes) => Some(HarfrustShaper::new(Arc::from(bytes), 0).expect("parity font loads")),
        Err(e) => {
            eprintln!("SKIP parity/{name}: cannot read {}: {e}", path.display());
            None
        }
    }
}

fn shape_with(s: &HarfrustShaper, text: &str, script: Script, dir: Direction) -> ShapedRun {
    s.shape(&ShapeInput {
        text,
        script,
        language: None,
        direction: dir,
        features: &[],
    })
    .expect("shapes")
}

/// Properties that must hold for any correct shaping of covered text, checked
/// for every parity string: nothing falls back to .notdef, clusters are
/// monotone in the run's direction, output is deterministic.
fn check_common(s: &HarfrustShaper, text: &str, script: Script, dir: Direction) -> ShapedRun {
    let a = shape_with(s, text, script, dir);
    assert!(!a.glyphs.is_empty(), "{text:?}: no glyphs");
    assert!(
        a.glyphs.iter().all(|g| g.glyph_id != 0),
        "{text:?}: .notdef in output"
    );
    assert!(a.glyphs.len() <= text.chars().count() + 1, "{text:?}");
    let cl = clusters(&a);
    let mono = match dir {
        Direction::LeftToRight => cl.windows(2).all(|w| w[0] <= w[1]),
        Direction::RightToLeft => cl.windows(2).all(|w| w[0] >= w[1]),
    };
    assert!(mono, "{text:?}: clusters not monotone: {cl:?}");
    assert_eq!(
        a,
        shape_with(s, text, script, dir),
        "{text:?}: not deterministic"
    );
    a
}

#[test]
fn parity_latin() {
    let Some(s) = parity_font("latin") else {
        return;
    };
    for t in [
        "Hello, world",
        "office affinity",
        "AVATAR To Wa",
        "Привет мир",
        "naïve café",
    ] {
        let script = if t.starts_with("Привет") {
            Script::CYRILLIC
        } else {
            Script::LATIN
        };
        check_common(&s, t, script, Direction::LeftToRight);
    }
}

#[test]
fn parity_hebrew() {
    let Some(s) = parity_font("hebrew") else {
        return;
    };
    for t in ["שלום", "עברית", "שָׁלוֹם"] {
        let r = check_common(&s, t, Script::HEBREW, Direction::RightToLeft);
        assert!(r.total_advance() > 0);
    }
    // Four letters, four glyphs, visual order is the reverse of logical.
    let r = shape_with(&s, "שלום", Script::HEBREW, Direction::RightToLeft);
    assert_eq!(r.glyphs.len(), 4);
    assert_eq!(clusters(&r), [6, 4, 2, 0]);
}

#[test]
fn parity_arabic() {
    let Some(s) = parity_font("arabic") else {
        return;
    };
    for t in ["مرحبا", "العربية", "بيت", "\u{644}\u{627}"] {
        check_common(&s, t, Script::ARABIC, Direction::RightToLeft);
    }
    // Lam + alef is a mandatory ligature: two characters, one glyph.
    let lam_alef = shape_with(&s, "\u{644}\u{627}", Script::ARABIC, Direction::RightToLeft);
    assert_eq!(lam_alef.glyphs.len(), 1);
    // Joining: the medial form of beh inside a word is not the isolated one.
    let alone = shape_with(&s, "\u{628}", Script::ARABIC, Direction::RightToLeft);
    let word = shape_with(
        &s,
        "\u{628}\u{628}\u{628}",
        Script::ARABIC,
        Direction::RightToLeft,
    );
    assert_eq!(word.glyphs.len(), 3);
    assert!(
        word.glyphs
            .iter()
            .all(|g| g.glyph_id != alone.glyphs[0].glyph_id)
    );
    // Initial and final forms differ from each other.
    assert_ne!(word.glyphs[0].glyph_id, word.glyphs[2].glyph_id);
}

#[test]
fn parity_devanagari() {
    let Some(s) = parity_font("devanagari") else {
        return;
    };
    for t in ["नमस्ते", "हिन्दी", "क्षत्रिय", "कि"] {
        check_common(&s, t, Script::DEVANAGARI, Direction::LeftToRight);
    }
    // Conjunct: ka + virama + ssa (three code points) must not stay three
    // separate glyphs plus a visible virama.
    let ksha = shape_with(&s, "क्ष", Script::DEVANAGARI, Direction::LeftToRight);
    assert!(
        ksha.glyphs.len() < 3,
        "no conjunct formed: {:?}",
        ids(&ksha)
    );
}

#[test]
fn ahem_when_the_bundled_font_is_present() {
    // tests/fonts/ahem is added by the font/raster branch. Until it is merged
    // this skips; the synthetic font above carries the deterministic cases.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fonts/ahem");
    let Ok(rd) = std::fs::read_dir(&root) else {
        eprintln!("SKIP ahem: {} does not exist yet", root.display());
        return;
    };
    let ttf = rd.filter_map(Result::ok).map(|e| e.path()).find(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("ttf"))
    });
    let Some(path) = ttf else {
        eprintln!("SKIP ahem: no .ttf in {}", root.display());
        return;
    };
    let s = HarfrustShaper::new(Arc::from(std::fs::read(path).unwrap()), 0).unwrap();
    let upem = s.units_per_em();
    let r = shape_with(&s, "Xp x", Script::LATIN, Direction::LeftToRight);
    assert_eq!(r.glyphs.len(), 4);
    // Ahem: every glyph, the space included, is one em wide.
    assert!(r.glyphs.iter().all(|g| g.x_advance == i32::from(upem)));
}

/// Regenerates the fuzz seed: `PELORUS_WRITE_FUZZ_SEED=<dir> cargo test -p
/// px-text write_fuzz_seed`. Does nothing otherwise. The seed is the synthetic
/// font framed the way `fuzz/fuzz_targets/shaper.rs` expects, so the fuzzer
/// starts from a font that loads.
#[test]
fn write_fuzz_seed() {
    let Some(dir) = std::env::var_os("PELORUS_WRITE_FUZZ_SEED") else {
        return;
    };
    let font = synthetic::font();
    for (name, control, text) in [
        ("ltr-kern-liga", 0u8, "AV fi ffi"),
        ("rtl-latin", 1u8, "AV fi"),
    ] {
        let mut seed = vec![control];
        seed.extend_from_slice(&(font.len() as u16).to_le_bytes());
        seed.extend_from_slice(&font);
        seed.extend_from_slice(text.as_bytes());
        std::fs::write(std::path::Path::new(&dir).join(name), seed).unwrap();
    }
}
