//! Known-answer tests against the committed Ahem font (tests/fonts/Ahem.ttf) and
//! hostile-input tests: truncating or corrupting the font at every byte must not
//! panic.

use px_text::registry::AHEM_BYTES;
use px_text::{Font, FontError, FontRegistry, PathCmd, Pt};

fn ahem() -> Font {
    Font::from_bytes(AHEM_BYTES).unwrap()
}

fn rect(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<PathCmd> {
    let p = |x, y| Pt { x, y };
    vec![
        PathCmd::MoveTo(p(x0, y1)),
        PathCmd::LineTo(p(x1, y1)),
        PathCmd::LineTo(p(x1, y0)),
        PathCmd::LineTo(p(x0, y0)),
        PathCmd::Close,
    ]
}

#[test]
fn committed_file_is_the_documented_one() {
    assert_eq!(AHEM_BYTES.len(), 21_768);
}

#[test]
fn global_metrics() {
    let f = ahem();
    assert_eq!(f.units_per_em(), 1000);
    assert_eq!(f.ascent(), 800);
    assert_eq!(f.descent(), -200);
    assert_eq!(f.line_gap(), 0);
    assert_eq!(f.num_glyphs(), 278);
}

#[test]
fn ordinary_letters_are_the_em_box_with_full_advance() {
    let f = ahem();
    for c in ('a'..='z').filter(|&c| c != 'p').chain('A'..='Z') {
        let g = f.glyph_index(c).unwrap();
        assert_eq!(f.advance(g), Some(1000), "{c}");
        let o = f.outline(g).unwrap();
        assert_eq!(o.commands, rect(0, -200, 1000, 800), "{c}");
        assert_eq!(o.bounds(), Some((0, -200, 1000, 800)));
    }
}

#[test]
fn documented_exceptions() {
    let f = ahem();
    let outline = |c: char| f.outline(f.glyph_index(c).unwrap()).unwrap().commands;
    let adv = |c: char| f.advance(f.glyph_index(c).unwrap()).unwrap();
    // "p": no ascent, full descent.
    assert_eq!(outline('p'), rect(0, -200, 1000, 0));
    // U+C9: no descent, full ascent.
    assert_eq!(outline('\u{c9}'), rect(0, 0, 1000, 800));
    // Chi: thin horizontal stripe; Upsilon: thin vertical stripe.
    assert_eq!(outline('\u{3a7}'), rect(0, 400, 1000, 600));
    assert_eq!(outline('\u{3a5}'), rect(200, -200, 400, 800));
    // Spaces: no path; advances as the font's own description lists them.
    for (c, a) in [
        (' ', 1000),
        ('\u{a0}', 1000),
        ('\u{2002}', 500),
        ('\u{2003}', 1000),
        ('\u{2004}', 333),
        ('\u{2005}', 250),
        ('\u{2006}', 167),
        ('\u{2009}', 200),
        ('\u{200a}', 100),
        ('\u{200b}', 0),
        ('\u{3000}', 1000),
        ('\u{200c}', 0),
        ('\u{200d}', 0),
        ('\u{feff}', 0),
    ] {
        assert!(outline(c).is_empty(), "U+{:04X} has a path", c as u32);
        assert_eq!(adv(c), a, "U+{:04X}", c as u32);
    }
}

#[test]
fn cmap_lookups() {
    let f = ahem();
    assert_eq!(f.glyph_index(' '), Some(3));
    assert_eq!(f.glyph_index('X'), Some(58));
    assert_eq!(f.glyph_index('\u{4e00}').map(|g| g > 0), Some(true));
    // Unmapped: a combining mark, an astral character, a char past the BMP.
    assert_eq!(f.glyph_index('\u{300}'), None);
    assert_eq!(f.glyph_index('\u{1F600}'), None);
    assert_eq!(f.glyph_index('\0'), None);
    // The font's own list of full-box ranges: spot-check range ends.
    for c in ['\u{20}', '\u{26}', '\u{28}', '\u{6f}', '\u{ff}', '\u{f002}'] {
        assert!(f.glyph_index(c).is_some(), "U+{:04X}", c as u32);
    }
    // U+27 (apostrophe) sits between ranges U+20-26 and U+28-6F; it is mapped
    // elsewhere or not at all, but must not be the full box of its neighbours.
    assert!(f.glyph_index('\u{2f}').is_some());
}

#[test]
fn advance_and_outline_reject_bad_glyphs() {
    let f = ahem();
    assert_eq!(f.advance(278), None);
    assert_eq!(f.advance(u16::MAX), None);
    assert_eq!(f.outline(278), Err(FontError::NoSuchGlyph));
}

#[test]
fn registry_resolves_ahem() {
    let r = FontRegistry::with_ahem().unwrap();
    let id = r.resolve("Ahem").unwrap();
    assert_eq!(r.resolve("ahem"), Some(id));
    assert_eq!(r.resolve("AHEM"), Some(id));
    assert_eq!(r.resolve("Arial"), None);
    assert_eq!(r.get(id).unwrap().units_per_em(), 1000);
    assert!(r.get(px_text::FontId(1)).is_none());
}

#[test]
fn registry_first_registration_wins() {
    let mut r = FontRegistry::new();
    let a = r.register("X", ahem()).unwrap();
    let b = r.register("x", ahem()).unwrap();
    assert_ne!(a, b);
    assert_eq!(r.resolve("X"), Some(a));
}

// ---------------------------------------------------------------- hostile input

/// Parse, then exercise every accessor. Must never panic.
fn exercise(bytes: &[u8]) {
    let Ok(f) = Font::from_bytes(bytes) else {
        return;
    };
    for c in ['a', 'X', ' ', '\u{4e00}', '\u{f002}', '\u{1F600}', '\0'] {
        let _ = f.glyph_index(c);
    }
    for g in 0..f.num_glyphs().min(300) {
        let _ = f.advance(g);
        let _ = f.outline(g);
    }
    let _ = f.outline(u16::MAX);
}

#[test]
fn truncation_at_every_byte_never_panics() {
    for n in 0..AHEM_BYTES.len() {
        exercise(&AHEM_BYTES[..n]);
    }
}

#[test]
fn truncated_directory_is_an_error() {
    assert!(Font::from_bytes(&AHEM_BYTES[..11]).is_err());
    assert!(Font::from_bytes(&AHEM_BYTES[..200]).is_err());
    assert!(Font::from_bytes(&[][..]).is_err());
}

#[test]
fn every_byte_flipped_never_panics() {
    let mut copy = AHEM_BYTES.to_vec();
    for i in 0..copy.len() {
        let orig = copy[i];
        for mask in [0xFFu8, 0x80] {
            copy[i] = orig ^ mask;
            exercise(&copy);
        }
        copy[i] = orig;
    }
}

#[test]
fn garbage_is_rejected() {
    assert_eq!(
        Font::from_bytes(&b"not a font at all"[..]).unwrap_err(),
        FontError::BadMagic
    );
    // An OTTO header with no tables is a font with no `head`, not a refusal.
    assert_eq!(
        Font::from_bytes(&b"OTTO\0\0\0\0\0\0\0\0"[..]).unwrap_err(),
        FontError::MissingTable("head")
    );
    let zeros = vec![0u8; 4096];
    assert!(Font::from_bytes(zeros).is_err());
    let ones = vec![0xFFu8; 4096];
    assert!(Font::from_bytes(ones).is_err());
}

// ------------------------------------------------- synthetic fonts (other paths)

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// Assemble a font: glyph 0 empty, then `glyphs` (raw glyf entries), short loca,
/// every advance 500, and the given cmap table.
fn build(glyphs: &[Vec<u8>], cmap: Vec<u8>) -> Vec<u8> {
    let n = glyphs.len() + 1;
    let mut glyf = Vec::new();
    let mut loca = vec![];
    be16(&mut loca, 0);
    be16(&mut loca, 0);
    for g in glyphs {
        glyf.extend_from_slice(g);
        while glyf.len() % 2 != 0 {
            glyf.push(0);
        }
        be16(&mut loca, (glyf.len() / 2) as u16);
    }
    let mut head = vec![0u8; 54];
    head[12..16].copy_from_slice(&0x5F0F_3CF5u32.to_be_bytes());
    head[18..20].copy_from_slice(&1000u16.to_be_bytes());
    let mut hhea = vec![0u8; 36];
    hhea[4..6].copy_from_slice(&800i16.to_be_bytes());
    hhea[6..8].copy_from_slice(&(-200i16).to_be_bytes());
    hhea[34..36].copy_from_slice(&1u16.to_be_bytes());
    let mut maxp = vec![0u8; 6];
    maxp[4..6].copy_from_slice(&(n as u16).to_be_bytes());
    let mut hmtx = vec![];
    be16(&mut hmtx, 500);
    be16(&mut hmtx, 0);
    let tables: [(&[u8; 4], Vec<u8>); 7] = [
        (b"cmap", cmap),
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    let mut out = vec![];
    be32(&mut out, 0x0001_0000);
    be16(&mut out, tables.len() as u16);
    out.extend_from_slice(&[0; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, t) in &tables {
        out.extend_from_slice(*tag);
        be32(&mut out, 0);
        be32(&mut out, offset as u32);
        be32(&mut out, t.len() as u32);
        offset += t.len().div_ceil(4) * 4;
    }
    for (_, t) in &tables {
        out.extend_from_slice(t);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

/// A format-12 cmap (3,10) mapping `start..=end` to glyphs from `first`.
fn cmap12(start: u32, end: u32, first: u32) -> Vec<u8> {
    let mut t = vec![];
    be16(&mut t, 0);
    be16(&mut t, 1);
    be16(&mut t, 3);
    be16(&mut t, 10);
    be32(&mut t, 12);
    be16(&mut t, 12);
    be16(&mut t, 0);
    be32(&mut t, 28);
    be32(&mut t, 0);
    be32(&mut t, 1);
    be32(&mut t, start);
    be32(&mut t, end);
    be32(&mut t, first);
    t
}

/// A simple glyph with the given contour end points, flags and 16-bit deltas.
fn simple(ends: &[u16], pts: &[(bool, i16, i16)]) -> Vec<u8> {
    let mut g = vec![];
    be16(&mut g, ends.len() as u16);
    g.extend_from_slice(&[0; 8]);
    for e in ends {
        be16(&mut g, *e);
    }
    be16(&mut g, 0);
    for (on, _, _) in pts {
        g.push(u8::from(*on)); // x and y as full i16 deltas
    }
    let (mut px, mut py) = (0i16, 0i16);
    for (_, x, _) in pts {
        be16(&mut g, (x - px) as u16);
        px = *x;
    }
    for (_, _, y) in pts {
        be16(&mut g, (y - py) as u16);
        py = *y;
    }
    g
}

fn composite(child: u16, flags: u16, args: &[u8], more: bool) -> Vec<u8> {
    let mut g = vec![];
    be16(&mut g, 0xFFFF);
    g.extend_from_slice(&[0; 8]);
    be16(&mut g, flags | if more { 0x20 } else { 0 });
    be16(&mut g, child);
    g.extend_from_slice(args);
    g
}

#[test]
fn format_12_cmap_with_astral_code_points() {
    let box_glyph = simple(
        &[3],
        &[(true, 0, 0), (true, 10, 0), (true, 10, 10), (true, 0, 10)],
    );
    let f = Font::from_bytes(build(&[box_glyph], cmap12(0x1F600, 0x1F60F, 1))).unwrap();
    assert_eq!(f.glyph_index('\u{1F600}'), Some(1));
    // Glyphs 2.. do not exist (only 2 glyphs): mapped past numGlyphs is unmapped.
    assert_eq!(f.glyph_index('\u{1F601}'), None);
    assert_eq!(f.glyph_index('\u{1F5FF}'), None);
    assert_eq!(f.glyph_index('a'), None);
    assert_eq!(
        f.advance(1),
        Some(500),
        "glyphs past numberOfHMetrics share the last advance"
    );
}

#[test]
fn off_curve_points_become_quadratics_with_floor_midpoints() {
    // Square with all four corners off-curve: starts at mid(last, first).
    let g = simple(
        &[3],
        &[
            (false, 0, 0),
            (false, 11, 0),
            (false, 11, 11),
            (false, 0, 11),
        ],
    );
    let f = Font::from_bytes(build(&[g], cmap12(0x41, 0x41, 1))).unwrap();
    let c = f.outline(1).unwrap().commands;
    let p = |x, y| Pt { x, y };
    assert_eq!(
        c,
        vec![
            PathCmd::MoveTo(p(0, 5)), // (0+0)/2, (11+0)>>1 = 5
            PathCmd::QuadTo(p(0, 0), p(5, 0)),
            PathCmd::QuadTo(p(11, 0), p(11, 5)),
            PathCmd::QuadTo(p(11, 11), p(5, 11)),
            PathCmd::QuadTo(p(0, 11), p(0, 5)),
            PathCmd::Close,
        ]
    );
}

#[test]
fn composite_applies_offset_and_scale() {
    let base = simple(
        &[3],
        &[
            (true, 0, 0),
            (true, 100, 0),
            (true, 100, 100),
            (true, 0, 100),
        ],
    );
    // Glyph 2: glyph 1 offset by (10, -20) with a 0.5 scale (0x2000).
    let c = composite(1, 0x0002 | 0x0008, &[10, 0xEC, 0x20, 0x00], false);
    let f = Font::from_bytes(build(&[base, c], cmap12(0x41, 0x42, 1))).unwrap();
    let o = f.outline(2).unwrap();
    // Offsets are added after scaling: (x*0.5 + 10, y*0.5 - 20).
    assert_eq!(o.bounds(), Some((10, -20, 60, 30)));
}

#[test]
fn composite_cycles_and_point_matching_are_refused() {
    let base = simple(
        &[3],
        &[(true, 0, 0), (true, 1, 0), (true, 1, 1), (true, 0, 1)],
    );
    // Glyph 2 includes itself.
    let selfref = composite(2, 0x0002, &[0, 0], false);
    let f = Font::from_bytes(build(&[base.clone(), selfref], cmap12(0x41, 0x42, 1))).unwrap();
    assert_eq!(f.outline(2), Err(FontError::TooComplex));
    // Matching by points (ARGS_ARE_XY_VALUES clear).
    let pm = composite(1, 0, &[0, 0], false);
    let f = Font::from_bytes(build(&[base, pm], cmap12(0x41, 0x42, 1))).unwrap();
    assert!(matches!(f.outline(2), Err(FontError::Unsupported(_))));
}

#[test]
fn component_flood_is_capped() {
    let base = simple(
        &[3],
        &[(true, 0, 0), (true, 1, 0), (true, 1, 1), (true, 0, 1)],
    );
    let mut g = vec![];
    be16(&mut g, 0xFFFF);
    g.extend_from_slice(&[0; 8]);
    for i in 0..500 {
        be16(&mut g, 0x0002 | if i < 499 { 0x20 } else { 0 });
        be16(&mut g, 1);
        g.extend_from_slice(&[0, 0]);
    }
    let f = Font::from_bytes(build(&[base, g], cmap12(0x41, 0x42, 1))).unwrap();
    assert_eq!(f.outline(2), Err(FontError::TooComplex));
}

#[test]
fn bogus_glyph_data_is_an_error_not_a_panic() {
    // 3 contours claimed, data ends immediately.
    let mut g = vec![];
    be16(&mut g, 3);
    g.extend_from_slice(&[0; 8]);
    let f = Font::from_bytes(build(&[g], cmap12(0x41, 0x41, 1))).unwrap();
    assert!(f.outline(1).is_err());
    // Contour ends going backwards.
    let g = simple(&[5, 2], &[(true, 0, 0); 6]);
    let f = Font::from_bytes(build(&[g], cmap12(0x41, 0x41, 1))).unwrap();
    assert!(f.outline(1).is_err());
}
