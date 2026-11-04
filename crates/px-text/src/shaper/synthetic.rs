//! A tiny hand-built TrueType-flavoured font for deterministic shaper tests.
//!
//! Tables: head, hhea, maxp, hmtx, cmap (format 12), GSUB (one `liga`
//! lookup), GPOS (one `kern` lookup). No outlines: shaping needs none.
//!
//! Glyphs (units per em 1000):
//!
//! | id | glyph | advance | code point |
//! |----|-------|---------|------------|
//! | 0  | .notdef | 500   |            |
//! | 1  | A     | 600     | U+0041     |
//! | 2  | V     | 600     | U+0056     |
//! | 3  | f     | 300     | U+0066     |
//! | 4  | i     | 250     | U+0069     |
//! | 5  | fi    | 500     | (ligature) |
//! | 6  | space | 250     | U+0020     |
//!
//! GSUB `liga`: `f i` becomes glyph 5. GPOS `kern`: the pair `A V` has an
//! x-advance adjustment of -80 on `A`.
#![allow(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub const UNITS_PER_EM: u16 = 1000;

fn u16be(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn i16be(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn u32be(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn head() -> Vec<u8> {
    let mut v = Vec::new();
    u32be(&mut v, 0x0001_0000); // version
    u32be(&mut v, 0x0001_0000); // fontRevision
    u32be(&mut v, 0); // checkSumAdjustment
    u32be(&mut v, 0x5F0F_3CF5); // magic
    u16be(&mut v, 0); // flags
    u16be(&mut v, UNITS_PER_EM);
    v.extend_from_slice(&[0; 8]); // created
    v.extend_from_slice(&[0; 8]); // modified
    i16be(&mut v, 0);
    i16be(&mut v, -200);
    i16be(&mut v, 1000);
    i16be(&mut v, 800);
    u16be(&mut v, 0); // macStyle
    u16be(&mut v, 8); // lowestRecPPEM
    i16be(&mut v, 2); // fontDirectionHint
    i16be(&mut v, 0); // indexToLocFormat
    i16be(&mut v, 0); // glyphDataFormat
    v
}

const ADVANCES: [u16; 7] = [500, 600, 600, 300, 250, 500, 250];

fn hhea() -> Vec<u8> {
    let mut v = Vec::new();
    u32be(&mut v, 0x0001_0000);
    i16be(&mut v, 800); // ascender
    i16be(&mut v, -200); // descender
    i16be(&mut v, 0); // lineGap
    u16be(&mut v, 600); // advanceWidthMax
    i16be(&mut v, 0);
    i16be(&mut v, 0);
    i16be(&mut v, 600);
    i16be(&mut v, 1);
    i16be(&mut v, 0);
    i16be(&mut v, 0);
    v.extend_from_slice(&[0; 8]); // reserved
    i16be(&mut v, 0); // metricDataFormat
    u16be(&mut v, ADVANCES.len() as u16);
    v
}

fn maxp() -> Vec<u8> {
    let mut v = Vec::new();
    u32be(&mut v, 0x0000_5000); // version 0.5
    u16be(&mut v, ADVANCES.len() as u16);
    v
}

fn hmtx() -> Vec<u8> {
    let mut v = Vec::new();
    for a in ADVANCES {
        u16be(&mut v, a);
        i16be(&mut v, 0);
    }
    v
}

fn cmap() -> Vec<u8> {
    let groups: [(u32, u32); 5] = [(0x20, 6), (0x41, 1), (0x56, 2), (0x66, 3), (0x69, 4)];
    let mut v = Vec::new();
    u16be(&mut v, 0); // version
    u16be(&mut v, 1); // numTables
    u16be(&mut v, 3); // platform
    u16be(&mut v, 10); // encoding: Unicode full repertoire
    u32be(&mut v, 12); // offset
    u16be(&mut v, 12); // format
    u16be(&mut v, 0);
    u32be(&mut v, 16 + 12 * groups.len() as u32); // length
    u32be(&mut v, 0); // language
    u32be(&mut v, groups.len() as u32);
    for (c, g) in groups {
        u32be(&mut v, c);
        u32be(&mut v, c);
        u32be(&mut v, g);
    }
    v
}

/// GSUB/GPOS shell: one DFLT script, one feature, one lookup. `lookup` is the
/// whole Lookup table (type, flags, subtable offsets and subtables).
fn layout_table(feature_tag: &[u8; 4], lookup: &[u8]) -> Vec<u8> {
    // ScriptList: count, record(tag, offset=8), Script(defaultLangSys=4, 0
    // langsys), LangSys(0, 0xFFFF, 1, [0]).
    let mut script_list = Vec::new();
    u16be(&mut script_list, 1);
    script_list.extend_from_slice(b"DFLT");
    u16be(&mut script_list, 8);
    u16be(&mut script_list, 4);
    u16be(&mut script_list, 0);
    u16be(&mut script_list, 0);
    u16be(&mut script_list, 0xFFFF);
    u16be(&mut script_list, 1);
    u16be(&mut script_list, 0);

    // FeatureList: count, record(tag, offset=8), Feature(0 params, 1 lookup, [0]).
    let mut feature_list = Vec::new();
    u16be(&mut feature_list, 1);
    feature_list.extend_from_slice(feature_tag);
    u16be(&mut feature_list, 8);
    u16be(&mut feature_list, 0);
    u16be(&mut feature_list, 1);
    u16be(&mut feature_list, 0);

    // LookupList: count, offset=4, then the lookup.
    let mut lookup_list = Vec::new();
    u16be(&mut lookup_list, 1);
    u16be(&mut lookup_list, 4);
    lookup_list.extend_from_slice(lookup);

    let mut v = Vec::new();
    u16be(&mut v, 1); // major
    u16be(&mut v, 0); // minor
    let script_off = 10u16;
    let feature_off = script_off + script_list.len() as u16;
    let lookup_off = feature_off + feature_list.len() as u16;
    u16be(&mut v, script_off);
    u16be(&mut v, feature_off);
    u16be(&mut v, lookup_off);
    v.extend_from_slice(&script_list);
    v.extend_from_slice(&feature_list);
    v.extend_from_slice(&lookup_list);
    v
}

fn gsub() -> Vec<u8> {
    // Lookup: type 4 (ligature), flags 0, 1 subtable at offset 8.
    let mut lookup = Vec::new();
    u16be(&mut lookup, 4);
    u16be(&mut lookup, 0);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 8);
    // LigatureSubstFormat1: format, coverage@18, 1 set @8.
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 18);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 8);
    // LigatureSet@8: 1 ligature at +4.
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 4);
    // Ligature@12: glyph 5, 2 components, [4].
    u16be(&mut lookup, 5);
    u16be(&mut lookup, 2);
    u16be(&mut lookup, 4);
    // Coverage@18: format 1, 1 glyph, [3].
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 3);
    layout_table(b"liga", &lookup)
}

fn gpos() -> Vec<u8> {
    // Lookup: type 2 (pair), flags 0, 1 subtable at offset 8.
    let mut lookup = Vec::new();
    u16be(&mut lookup, 2);
    u16be(&mut lookup, 0);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 8);
    // PairPosFormat1 (12-byte header): format, coverage@18, valueFormat1 =
    // XAdvance, vf2 = 0, 1 pair set @12.
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 18);
    u16be(&mut lookup, 0x0004);
    u16be(&mut lookup, 0);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 12);
    // PairSet@12: 1 record: second glyph V (2), xAdvance -80.
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 2);
    i16be(&mut lookup, -80);
    // Coverage@18: [A = 1].
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 1);
    u16be(&mut lookup, 1);
    layout_table(b"kern", &lookup)
}

/// The whole font file.
pub fn font() -> Vec<u8> {
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"GPOS", gpos()),
        (b"GSUB", gsub()),
        (b"cmap", cmap()),
        (b"head", head()),
        (b"hhea", hhea()),
        (b"hmtx", hmtx()),
        (b"maxp", maxp()),
    ];
    tables.sort_by_key(|(tag, _)| **tag);
    let n = tables.len();
    let mut out = Vec::new();
    u32be(&mut out, 0x0001_0000);
    u16be(&mut out, n as u16);
    // searchRange, entrySelector, rangeShift for n = 7: 4 * 16, 2, 7 * 16 - 64.
    u16be(&mut out, 64);
    u16be(&mut out, 2);
    u16be(&mut out, 48);
    let mut offset = 12 + 16 * n;
    let mut bodies = Vec::new();
    for (tag, body) in &tables {
        out.extend_from_slice(*tag);
        u32be(&mut out, 0); // checksum: not verified by the reader
        u32be(&mut out, offset as u32);
        u32be(&mut out, body.len() as u32);
        let mut padded = body.clone();
        while padded.len() % 4 != 0 {
            padded.push(0);
        }
        offset += padded.len();
        bodies.push(padded);
    }
    for b in bodies {
        out.extend_from_slice(&b);
    }
    out
}
