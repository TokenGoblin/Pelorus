//! The bundled Noto faces: shaping, structure and fallback.
//!
//! # How the expected values were derived
//!
//! Nothing here is a number copied from the shaper's own output. For each string:
//!
//! 1. the **nominal** glyphs come from the font's `cmap` through px-text's own
//!    reader ([`Font::glyph_index`]);
//! 2. what a correct shaper must turn them into is read from the font itself:
//!    **glyph names** from the `post` table through `skrifa` (an independent
//!    parser from the one `harfrust` shapes with), for example `uni0633.init`
//!    for a seen in initial position or `k_ssa-deva` for the ka-virama-ssa
//!    conjunct. A name states the form the font's designers gave that glyph, so
//!    "the shaper applied the joining forms" is checked against the font's own
//!    labelling, not against a second shaper;
//! 3. counts and cluster maps follow from the Unicode model: Arabic and Hebrew
//!    clusters are one per character and, being right to left, come back in
//!    decreasing order; a Devanagari conjunct merges its three characters into
//!    one cluster.
//!
//! **No HarfBuzz reference tool is installed here** (`hb-shape`, `uharfbuzz` and
//! `hb-view` are absent), so no agreement with HarfBuzz is claimed: `harfrust` is
//! a HarfBuzz port, and what is verified is that its output matches the font's own
//! glyph naming, not that it matches HarfBuzz byte for byte.

use px_text::registry::{
    BUNDLED_FAMILIES, FALLBACK_ORDER, NOTO_SANS_ARABIC_BYTES, NOTO_SANS_BYTES,
    NOTO_SANS_DEVANAGARI_BYTES, NOTO_SANS_HEBREW_BYTES,
};
use px_text::shaper::{Direction, HarfrustShaper, ShapeInput, ShapedRun, Shaper, Script};
use px_text::{Font, FontId, FontRegistry};
use skrifa::MetadataProvider;

struct Face {
    font: Font,
    shaper: HarfrustShaper,
    names: skrifa::FontRef<'static>,
}

fn face(bytes: &'static [u8]) -> Face {
    let font = Font::from_bytes(bytes).expect("font");
    let shaper = HarfrustShaper::new(font.bytes().clone(), 0).expect("shaper");
    let names = skrifa::FontRef::new(bytes).expect("skrifa");
    Face {
        font,
        shaper,
        names,
    }
}

impl Face {
    fn shape(&self, text: &str, script: Script, dir: Direction) -> ShapedRun {
        self.shaper
            .shape(&ShapeInput {
                text,
                script,
                language: None,
                direction: dir,
                features: &[],
            })
            .expect("shape")
    }

    fn name(&self, gid: u32) -> String {
        self.names
            .glyph_names()
            .get(skrifa::GlyphId::new(gid))
            .map(|n| n.to_string())
            .unwrap_or_default()
    }

    fn nominal(&self, text: &str) -> Vec<u32> {
        text.chars()
            .map(|c| u32::from(self.font.glyph_index(c).expect("mapped")))
            .collect()
    }

    fn names_of(&self, run: &ShapedRun) -> Vec<String> {
        run.glyphs.iter().map(|g| self.name(g.glyph_id)).collect()
    }
}

fn ids(r: &ShapedRun) -> Vec<u32> {
    r.glyphs.iter().map(|g| g.glyph_id).collect()
}
fn clusters(r: &ShapedRun) -> Vec<u32> {
    r.glyphs.iter().map(|g| g.cluster).collect()
}

const LTR: Direction = Direction::LeftToRight;
const RTL: Direction = Direction::RightToLeft;

#[test]
fn committed_files_are_the_documented_ones() {
    // Sizes from tests/fonts/PROVENANCE; the SHA-256s there are the real pin.
    assert_eq!(NOTO_SANS_BYTES.len(), 431_364);
    assert_eq!(NOTO_SANS_HEBREW_BYTES.len(), 26_860);
    assert_eq!(NOTO_SANS_ARABIC_BYTES.len(), 234_892);
    assert_eq!(NOTO_SANS_DEVANAGARI_BYTES.len(), 243_520);
    let total = NOTO_SANS_BYTES.len()
        + NOTO_SANS_HEBREW_BYTES.len()
        + NOTO_SANS_ARABIC_BYTES.len()
        + NOTO_SANS_DEVANAGARI_BYTES.len();
    assert!(total < 2_500_000, "bundled Noto bytes: {total}");
}

#[test]
fn latin_is_plain_one_glyph_per_character() {
    let f = face(NOTO_SANS_BYTES);
    let text = "Hello, world 123";
    let r = f.shape(text, Script::LATIN, LTR);
    assert_eq!(r.glyphs.len(), text.chars().count());
    assert_eq!(clusters(&r), (0..16).collect::<Vec<u32>>());
    assert!(r.glyphs.iter().all(|g| g.glyph_id != 0), "no .notdef");
    // `Hello`: no ligature (no `fi`), so ids are the nominal ones.
    let hello = f.shape("Hello", Script::LATIN, LTR);
    assert_eq!(ids(&hello), f.nominal("Hello"));
    assert_eq!(f.names_of(&hello), ["H", "e", "l", "l", "o"]);
    assert_eq!(f.font.units_per_em(), 1000);
}

#[test]
fn cyrillic_is_plain_with_two_byte_clusters() {
    let f = face(NOTO_SANS_BYTES);
    let text = "Привет мир";
    let r = f.shape(text, Script::CYRILLIC, LTR);
    assert_eq!(r.glyphs.len(), 10);
    // Each Cyrillic letter is two UTF-8 bytes; the space is one.
    assert_eq!(clusters(&r), [0, 2, 4, 6, 8, 10, 12, 13, 15, 17]);
    assert!(r.glyphs.iter().all(|g| g.glyph_id != 0));
    let word = f.shape("Привет", Script::CYRILLIC, LTR);
    assert_eq!(ids(&word), f.nominal("Привет"));
    assert_eq!(
        f.names_of(&word),
        ["uni041F", "uni0440", "uni0438", "uni0432", "uni0435", "uni0442"]
    );
}

#[test]
fn hebrew_is_reversed_and_unjoined() {
    let f = face(NOTO_SANS_HEBREW_BYTES);
    let r = f.shape("שלום", Script::HEBREW, RTL);
    assert_eq!(r.glyphs.len(), 4);
    // Visual order, left to right: the end of the logical text first.
    let mut want = f.nominal("שלום");
    want.reverse();
    assert_eq!(ids(&r), want);
    assert_eq!(clusters(&r), [6, 4, 2, 0]);
    assert_eq!(f.names_of(&r), ["uni05DD", "uni05D5", "uni05DC", "uni05E9"]);
}

#[test]
fn hebrew_points_are_zero_advance_marks_attached_to_the_base() {
    let f = face(NOTO_SANS_HEBREW_BYTES);
    // SHIN + QAMATS + SHIN DOT.
    let text = "\u{5E9}\u{5B8}\u{5C1}";
    let r = f.shape(text, Script::HEBREW, RTL);
    assert_eq!(r.glyphs.len(), 3, "base and two marks, nothing merged");
    assert!(r.glyphs.iter().all(|g| g.glyph_id != 0), "no .notdef");
    let mut names = f.names_of(&r);
    names.sort();
    assert_eq!(names, ["uni05B8", "uni05C1", "uni05E9"]);
    // All three share the base's cluster.
    assert_eq!(clusters(&r), [0, 0, 0]);
    let base = r
        .glyphs
        .iter()
        .find(|g| f.name(g.glyph_id) == "uni05E9")
        .expect("base");
    assert_eq!(
        i64::from(base.x_advance),
        i64::from(f.font.advance(base.glyph_id as u16).expect("advance"))
    );
    for g in r.glyphs.iter().filter(|g| g.glyph_id != base.glyph_id) {
        assert_eq!(g.x_advance, 0, "a point advances nothing");
        // Positioned by mark attachment (GPOS): not left at the pen origin.
        assert!(g.x_offset != 0 || g.y_offset != 0, "{}", f.name(g.glyph_id));
    }
    // The run's width is the base's width.
    assert_eq!(r.total_advance(), i64::from(base.x_advance));
}

#[test]
fn arabic_joining_forms_are_applied() {
    let f = face(NOTO_SANS_ARABIC_BYTES);
    // SEEN LAM ALEF MEEM.
    let text = "سلام";
    let nominal = f.nominal(text);
    let r = f.shape(text, Script::ARABIC, RTL);
    assert_eq!(r.glyphs.len(), 4, "no ligature merges these four");
    assert_eq!(clusters(&r), [6, 4, 2, 0]);
    // Visual order: meem, alef, lam, seen. Joining: seen is initial, lam medial
    // (it joins both sides), alef final (right joining only), meem isolated
    // (alef does not join to its left, so nothing joins it).
    assert_eq!(
        f.names_of(&r),
        [
            "uni0645",
            "uni0627.fina.rlig.2",
            "uni0644.medi.rlig",
            "uni0633.init"
        ]
    );
    let shaped = ids(&r);
    // Three of the four differ from the nominal cmap glyphs; the isolated meem
    // is its nominal glyph.
    assert_eq!(shaped[0], nominal[3]);
    assert_ne!(shaped[1], nominal[2]);
    assert_ne!(shaped[2], nominal[1]);
    assert_ne!(shaped[3], nominal[0]);
    assert!(shaped.iter().all(|&g| g != 0));
}

#[test]
fn arabic_lam_alef_forms_a_two_glyph_pair_with_contextual_forms() {
    let f = face(NOTO_SANS_ARABIC_BYTES);
    let r = f.shape("لا", Script::ARABIC, RTL);
    // The font does not fuse lam-alef into one glyph; it substitutes the
    // `rlig` pair `uni0627.fina.rlig` and `uni0644.init.rlig`.
    assert_eq!(r.glyphs.len(), 2);
    assert_eq!(f.names_of(&r), ["uni0627.fina.rlig", "uni0644.init.rlig"]);
    assert_eq!(clusters(&r), [2, 0]);
}

#[test]
fn devanagari_conjunct_is_one_glyph() {
    let f = face(NOTO_SANS_DEVANAGARI_BYTES);
    // KA, VIRAMA, SSA: the font defines the ligature `k_ssa-deva`.
    let r = f.shape("क्ष", Script::DEVANAGARI, LTR);
    assert_eq!(r.glyphs.len(), 1);
    assert_eq!(f.names_of(&r), ["k_ssa-deva"]);
    assert_eq!(clusters(&r), [0]);
    assert!(r.glyphs.iter().all(|g| g.glyph_id != 0));
    // Nominally three glyphs, all of which are different from the result.
    assert_eq!(f.nominal("क्ष").len(), 3);
}

#[test]
fn devanagari_short_i_matra_is_reordered_before_its_base() {
    let f = face(NOTO_SANS_DEVANAGARI_BYTES);
    // HA, I-MATRA, NA, VIRAMA, DA, II-MATRA: six characters.
    let text = "हिन्दी";
    let r = f.shape(text, Script::DEVANAGARI, LTR);
    assert_eq!(r.glyphs.len(), 5);
    // The font's own names: the short i matra variant first (reordering), then
    // the base ha, the half-form of na (virama folded into it), da, long ii.
    assert_eq!(
        f.names_of(&r),
        ["iMatra-deva.03", "ha-deva", "n-deva", "da-deva", "iiMatra-deva"]
    );
    // Clusters are byte offsets (each character is 3 bytes): the matra shares
    // ha's cluster 0, the half-na starts at byte 6, da and long ii at 12.
    assert_eq!(clusters(&r), [0, 0, 6, 12, 12]);
    assert!(r.glyphs.iter().all(|g| g.glyph_id != 0));
}

#[test]
fn no_notdef_for_any_character_of_the_test_strings() {
    for (bytes, script, dir, text) in [
        (NOTO_SANS_BYTES, Script::LATIN, LTR, "The quick brown fox, 0123456789."),
        (NOTO_SANS_BYTES, Script::CYRILLIC, LTR, "Съешь ещё этих мягких французских булок"),
        (NOTO_SANS_HEBREW_BYTES, Script::HEBREW, RTL, "שלום עולם"),
        (NOTO_SANS_ARABIC_BYTES, Script::ARABIC, RTL, "مرحبا بالعالم"),
        (NOTO_SANS_DEVANAGARI_BYTES, Script::DEVANAGARI, LTR, "नमस्ते दुनिया"),
    ] {
        let f = face(bytes);
        let r = f.shape(text, script, dir);
        assert!(!r.glyphs.is_empty());
        assert!(
            r.glyphs.iter().all(|g| g.glyph_id != 0),
            "{text}: {:?}",
            f.names_of(&r)
        );
    }
}

#[test]
fn shaping_is_deterministic_across_shapers() {
    let a = face(NOTO_SANS_ARABIC_BYTES);
    let b = face(NOTO_SANS_ARABIC_BYTES);
    assert_eq!(
        a.shape("مرحبا بالعالم", Script::ARABIC, RTL),
        b.shape("مرحبا بالعالم", Script::ARABIC, RTL)
    );
}

// ---- registry and fallback -------------------------------------------------

fn registry() -> FontRegistry {
    FontRegistry::with_bundled().expect("bundled")
}

#[test]
fn bundled_registry_has_the_documented_ids_and_names() {
    let r = registry();
    assert_eq!(r.len(), BUNDLED_FAMILIES.len());
    assert_eq!(r.families().collect::<Vec<_>>(), BUNDLED_FAMILIES);
    for (i, name) in BUNDLED_FAMILIES.iter().enumerate() {
        assert_eq!(r.resolve(name), Some(FontId(i as u32)));
        assert_eq!(r.family(FontId(i as u32)), Some(*name));
    }
    // Ahem stays id 0 so layouts that default to FontId(0) are unchanged.
    assert_eq!(r.resolve("ahem"), Some(FontId(0)));
    assert!(!FALLBACK_ORDER.contains(&"Ahem"));
}

#[test]
fn fallback_picks_the_script_face() {
    let r = registry();
    let id = |n: &str| r.resolve(n).expect(n);
    assert_eq!(r.fallback_for('a', &[]), Some(id("Noto Sans")));
    assert_eq!(r.fallback_for('Ж', &[]), Some(id("Noto Sans")));
    assert_eq!(r.fallback_for('ש', &[]), Some(id("Noto Sans Hebrew")));
    assert_eq!(r.fallback_for('س', &[]), Some(id("Noto Sans Arabic")));
    assert_eq!(r.fallback_for('क', &[]), Some(id("Noto Sans Devanagari")));
}

#[test]
fn shared_codepoints_go_to_noto_sans_first() {
    let r = registry();
    let sans = r.resolve("Noto Sans");
    for c in [' ', '0', '9', '.', ',', '-', '(', ')', ':'] {
        assert_eq!(r.fallback_for(c, &[]), sans, "{c:?}");
    }
    // If a later face maps them too, the order, not coverage, decides.
}

#[test]
fn listed_families_come_before_the_fallback_order() {
    let r = registry();
    // Hebrew listed first and covering a digit: it wins over Noto Sans.
    let hebrew = r.resolve("Noto Sans Hebrew").expect("hebrew");
    for c in ['0', ' '] {
        if r.covers(hebrew, c) {
            assert_eq!(r.fallback_for(c, &["Noto Sans Hebrew"]), Some(hebrew));
        }
    }
    // A listed family that does not cover the character is skipped; unknown names
    // are skipped.
    assert_eq!(
        r.fallback_for('ש', &["No Such Family", "Noto Sans"]),
        Some(hebrew)
    );
    // Ahem is used only when named, and then only for what it maps.
    let ahem = r.resolve("Ahem").expect("ahem");
    assert_eq!(r.fallback_for('a', &["Ahem"]), Some(ahem));
    assert_ne!(r.fallback_for('a', &[]), Some(ahem));
}

#[test]
fn unmapped_characters_have_no_fallback_yet() {
    // CJK-ready: when a CJK face is appended to the registry and to
    // FALLBACK_ORDER this returns it; until then it is honestly `None`.
    let r = registry();
    assert_eq!(r.fallback_for('日', &[]), None);
    assert_eq!(r.fallback_for('\u{10FFFF}', &[]), None);
}

#[test]
fn fallback_is_stable_across_calls_and_registries() {
    let a = registry();
    let b = registry();
    let text = "aЖשسक0 .-日";
    let first: Vec<_> = text.chars().map(|c| a.fallback_for(c, &[])).collect();
    let second: Vec<_> = text.chars().map(|c| a.fallback_for(c, &[])).collect();
    let other: Vec<_> = text.chars().map(|c| b.fallback_for(c, &[])).collect();
    assert_eq!(first, second);
    assert_eq!(first, other);
}

#[test]
fn the_registry_never_contains_a_system_face() {
    // The deterministic corpus resolves fonts through this registry alone, and it
    // is exactly the committed set: no name outside BUNDLED_FAMILIES.
    let r = registry();
    assert_eq!(r.families().count(), 5);
    assert!(r.families().all(|n| BUNDLED_FAMILIES.contains(&n)));
    // `FontRegistry` has no constructor taking a `SystemFonts` or a path, and
    // `register` takes parsed `Font`s whose bytes the caller supplies.
}
