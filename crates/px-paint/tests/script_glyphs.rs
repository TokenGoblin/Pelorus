//! One glyph per gate script, from the bundled Noto faces, through px-text's
//! reader and px-paint's integer rasterizer: non-empty, deterministic, and pinned
//! by a hash of the mask.
//!
//! The pinned values are FNV-1a 64 over `left, top, width, height` (little-endian
//! `i32`/`u32`) and the coverage bytes at 32 px. They are not derived by hand --
//! a glyph's exact coverage cannot be -- they pin today's output so a change in
//! the reader, the rasterizer or the font is noticed, and a reviewer must re-bless
//! them knowing why. Integer-only rasterization is what makes them the same on
//! every machine (the claim the cross-OS job will test on Windows).

use px_paint::glyph::{Mask, PathCmd, Pt, Scale, rasterize_glyph};
use px_text::registry::{
    NOTO_SANS_ARABIC_BYTES, NOTO_SANS_BYTES, NOTO_SANS_DEVANAGARI_BYTES, NOTO_SANS_HEBREW_BYTES,
};
use px_text::{Font, PathCmd as TextCmd};

fn fnv(mask: &Mask) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |b: u8| {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    for v in [mask.left, mask.top] {
        v.to_le_bytes().into_iter().for_each(&mut eat);
    }
    for v in [mask.width, mask.height] {
        v.to_le_bytes().into_iter().for_each(&mut eat);
    }
    mask.data.iter().copied().for_each(eat);
    h
}

fn mask_of(bytes: &[u8], c: char) -> Mask {
    let font = Font::from_bytes(bytes).expect("font");
    let gid = font.glyph_index(c).expect("mapped");
    let outline = font.outline(gid).expect("outline");
    let p = |p: px_text::Pt| Pt { x: p.x, y: p.y };
    let cmds: Vec<PathCmd> = outline
        .commands
        .iter()
        .map(|c| match *c {
            TextCmd::MoveTo(a) => PathCmd::MoveTo(p(a)),
            TextCmd::LineTo(a) => PathCmd::LineTo(p(a)),
            TextCmd::QuadTo(a, b) => PathCmd::QuadTo(p(a), p(b)),
            TextCmd::Close => PathCmd::Close,
        })
        .collect();
    rasterize_glyph(
        &cmds,
        Scale {
            size64: 32 * 64,
            units_per_em: u32::from(font.units_per_em()),
        },
        (0, 0),
    )
    .expect("mask")
}

#[test]
fn one_glyph_per_script_is_non_empty_deterministic_and_pinned() {
    let cases: [(&str, &[u8], char, u64); 5] = [
        ("Latin A", NOTO_SANS_BYTES, 'A', PIN_LATIN),
        ("Cyrillic Zhe", NOTO_SANS_BYTES, 'Ж', PIN_CYRILLIC),
        ("Hebrew Shin", NOTO_SANS_HEBREW_BYTES, 'ש', PIN_HEBREW),
        ("Arabic Seen", NOTO_SANS_ARABIC_BYTES, 'س', PIN_ARABIC),
        (
            "Devanagari Ka",
            NOTO_SANS_DEVANAGARI_BYTES,
            'क',
            PIN_DEVANAGARI,
        ),
    ];
    let mut report = String::new();
    let mut bad = false;
    for (name, bytes, c, pin) in cases {
        let m = mask_of(bytes, c);
        assert!(
            m.width > 4 && m.height > 4,
            "{name}: {}x{}",
            m.width,
            m.height
        );
        let ink: u64 = m.data.iter().map(|&b| u64::from(b)).sum();
        assert!(ink > 255 * 20, "{name}: ink {ink}");
        assert!(m.data.contains(&255), "{name}: no fully covered pixel");
        assert!(
            m.data.contains(&0),
            "{name}: no empty pixel (a filled box?)"
        );
        let again = mask_of(bytes, c);
        assert_eq!(m, again, "{name}: not deterministic");
        let h = fnv(&m);
        report.push_str(&format!("{name}: {h:#018x} ({}x{})\n", m.width, m.height));
        bad |= h != pin;
    }
    assert!(!bad, "pinned hashes differ, actual:\n{report}");
}

const PIN_LATIN: u64 = 0x3538_c24f_18e3_0920;
const PIN_CYRILLIC: u64 = 0x8e0b_b91e_0983_ff00;
const PIN_HEBREW: u64 = 0xc894_f44e_e085_10d8;
const PIN_ARABIC: u64 = 0x7336_f4a5_ad58_1245;
const PIN_DEVANAGARI: u64 = 0x960f_109e_18b6_dc4f;
