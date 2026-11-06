//! Writes the tiny deterministic test fonts under `tests/fonts/` (`Px*.ttf`).
//!
//! ```text
//! cargo run -p px-text --example fixture_fonts -- tests/fonts
//! ```
//!
//! The fonts exist so the web-font and generic-family render baselines have faces
//! whose shapes differ from Noto Sans and from each other at a glance, and so
//! tests can ship a real, valid TrueType font in a few kilobytes. Every glyph is
//! a polygon of straight on-curve segments; `A-Z a-z 0-9` map to bars whose height
//! depends on the character, so a line of text reads as a skyline and different
//! fonts are visibly different. The bytes are a pure function of this file.
//!
//! | file | family | look |
//! |---|---|---|
//! | `PxText-Regular.ttf` | Px Text | hollow bars, advance 600 |
//! | `PxText-Bold.ttf` | Px Text (700) | solid wide bars, advance 700 |
//! | `PxText-Italic.ttf` | Px Text (italic) | slanted hollow bars |
//! | `PxSerif-Regular.ttf` | Px Serif | solid bars with slab feet and caps |
//! | `PxMono-Regular.ttf` | Px Mono | thin solid bars, advance 500 |
//! | `PxIcons-Regular.ttf` | Px Icons | private-use icons U+E000..U+E004 |

use std::path::Path;

const UPEM: i16 = 1000;
const ASCENT: i16 = 800;
const DESCENT: i16 = -200;

type Contour = Vec<(i16, i16)>;

struct Glyph {
    advance: u16,
    contours: Vec<Contour>,
}

fn rect(x0: i16, y0: i16, x1: i16, y1: i16) -> Contour {
    // Clockwise in a y-up system: the TrueType outer direction.
    vec![(x0, y0), (x0, y1), (x1, y1), (x1, y0)]
}

fn rect_ccw(x0: i16, y0: i16, x1: i16, y1: i16) -> Contour {
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

#[derive(Clone, Copy)]
enum Look {
    Hollow,
    Solid,
    Slanted,
    Serif,
    Thin,
}

fn bar(look: Look, c: char) -> Glyph {
    let h = 320 + i16::try_from(u32::from(c) % 7).unwrap_or(0) * 60;
    let upper = c.is_ascii_uppercase();
    match look {
        Look::Hollow => Glyph {
            advance: 600,
            contours: vec![rect(60, 0, 520, h), rect_ccw(130, 70, 450, h - 70)],
        },
        Look::Solid => {
            let mut contours = vec![rect(40, 0, 640, h)];
            if upper {
                contours.push(rect(40, h + 40, 640, h + 120));
            }
            Glyph {
                advance: 700,
                contours,
            }
        }
        Look::Slanted => {
            let s = 120;
            let outer = vec![(60, 0), (60 + s, h), (520 + s, h), (520, 0)];
            let inner = vec![(130, 70), (450, 70), (450 + s, h - 70), (130 + s, h - 70)];
            Glyph {
                advance: 600,
                contours: vec![outer, inner],
            }
        }
        Look::Serif => {
            let mut contours = vec![
                rect(250, 60, 400, h - 60),
                rect(100, 0, 550, 60),
                rect(100, h - 60, 550, h),
            ];
            if upper {
                contours.push(rect(250, h + 40, 400, h + 120));
            }
            Glyph {
                advance: 650,
                contours,
            }
        }
        Look::Thin => Glyph {
            advance: 500,
            contours: vec![rect(190, 0, 310, h)],
        },
    }
}

fn icon(n: u32) -> Glyph {
    let contours = match n {
        // Diamond.
        0 => vec![vec![(360, 0), (0, 380), (360, 760), (720, 380)]],
        // Triangle.
        1 => vec![vec![(50, 0), (50, 760), (600, 380)]],
        // Plus.
        2 => vec![vec![
            (200, 0),
            (200, 280),
            (0, 280),
            (0, 480),
            (200, 480),
            (200, 760),
            (400, 760),
            (400, 480),
            (600, 480),
            (600, 280),
            (400, 280),
            (400, 0),
        ]],
        // Ring (square with a square hole).
        3 => vec![rect(0, 0, 640, 700), rect_ccw(170, 170, 470, 530)],
        // Stair.
        _ => vec![vec![
            (0, 0),
            (0, 200),
            (200, 200),
            (200, 400),
            (400, 400),
            (400, 600),
            (640, 600),
            (640, 0),
        ]],
    };
    Glyph {
        advance: 720,
        contours,
    }
}

fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn bei16(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn checksum(d: &[u8]) -> u32 {
    let mut s = 0u32;
    for c in d.chunks(4) {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        s = s.wrapping_add(u32::from_be_bytes(w));
    }
    s
}

fn utf16(s: &str) -> Vec<u8> {
    s.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

struct Spec {
    family: &'static str,
    style: &'static str,
    mac_style: u16,
    glyphs: Vec<(char, Glyph)>,
}

fn build(spec: &Spec) -> Vec<u8> {
    // Glyph 0 is .notdef, glyph 1 the space; the rest follow `spec.glyphs`.
    let notdef = Glyph {
        advance: 600,
        contours: vec![rect(60, 0, 520, 700), rect_ccw(120, 60, 460, 640)],
    };
    let space = Glyph {
        advance: 400,
        contours: vec![],
    };
    let mut all: Vec<&Glyph> = vec![&notdef, &space];
    all.extend(spec.glyphs.iter().map(|(_, g)| g));
    let n = u16::try_from(all.len()).unwrap_or(0);

    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    let (mut bx0, mut by0, mut bx1, mut by1) = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    let mut max_pts = 0usize;
    let mut max_contours = 0usize;
    let mut adv_max = 0u16;
    for g in &all {
        be32(&mut loca, u32::try_from(glyf.len()).unwrap_or(0));
        adv_max = adv_max.max(g.advance);
        if g.contours.is_empty() {
            continue;
        }
        let pts: Vec<(i16, i16)> = g.contours.iter().flatten().copied().collect();
        max_pts = max_pts.max(pts.len());
        max_contours = max_contours.max(g.contours.len());
        let x0 = pts.iter().map(|p| p.0).min().unwrap_or(0);
        let x1 = pts.iter().map(|p| p.0).max().unwrap_or(0);
        let y0 = pts.iter().map(|p| p.1).min().unwrap_or(0);
        let y1 = pts.iter().map(|p| p.1).max().unwrap_or(0);
        bx0 = bx0.min(x0);
        by0 = by0.min(y0);
        bx1 = bx1.max(x1);
        by1 = by1.max(y1);
        bei16(&mut glyf, i16::try_from(g.contours.len()).unwrap_or(0));
        for v in [x0, y0, x1, y1] {
            bei16(&mut glyf, v);
        }
        let mut end = 0u16;
        for c in &g.contours {
            end += u16::try_from(c.len()).unwrap_or(0);
            be16(&mut glyf, end - 1);
        }
        be16(&mut glyf, 0); // no instructions
        glyf.extend(std::iter::repeat_n(0x01u8, pts.len())); // on-curve, i16 deltas
        let (mut px, mut py) = (0i16, 0i16);
        for p in &pts {
            bei16(&mut glyf, p.0 - px);
            px = p.0;
        }
        for p in &pts {
            bei16(&mut glyf, p.1 - py);
            py = p.1;
        }
        while !glyf.len().is_multiple_of(4) {
            glyf.push(0);
        }
    }
    be32(&mut loca, u32::try_from(glyf.len()).unwrap_or(0));

    let mut hmtx = Vec::new();
    for g in &all {
        be16(&mut hmtx, g.advance);
        let lsb = g.contours.iter().flatten().map(|p| p.0).min().unwrap_or(0);
        bei16(&mut hmtx, lsb);
    }

    // cmap format 4, one segment per code point.
    let mut codes: Vec<(u16, u16)> = vec![(0x20, 1)];
    for (i, (c, _)) in spec.glyphs.iter().enumerate() {
        codes.push((
            u16::try_from(u32::from(*c)).unwrap_or(0),
            u16::try_from(i + 2).unwrap_or(0),
        ));
    }
    codes.sort_unstable();
    let seg = codes.len() + 1;
    let mut sub = Vec::new();
    be16(&mut sub, 4);
    be16(&mut sub, u16::try_from(16 + seg * 8).unwrap_or(0));
    be16(&mut sub, 0);
    be16(&mut sub, u16::try_from(seg * 2).unwrap_or(0));
    let mut pow = 1usize;
    let mut log = 0u16;
    while pow * 2 <= seg {
        pow *= 2;
        log += 1;
    }
    be16(&mut sub, u16::try_from(pow * 2).unwrap_or(0));
    be16(&mut sub, log);
    be16(&mut sub, u16::try_from(seg * 2 - pow * 2).unwrap_or(0));
    for (c, _) in &codes {
        be16(&mut sub, *c);
    }
    be16(&mut sub, 0xFFFF);
    be16(&mut sub, 0);
    for (c, _) in &codes {
        be16(&mut sub, *c);
    }
    be16(&mut sub, 0xFFFF);
    for (c, g) in &codes {
        be16(&mut sub, g.wrapping_sub(*c));
    }
    be16(&mut sub, 1);
    for _ in 0..seg {
        be16(&mut sub, 0);
    }
    let mut cmap = Vec::new();
    be16(&mut cmap, 0);
    be16(&mut cmap, 1);
    be16(&mut cmap, 3);
    be16(&mut cmap, 1);
    be32(&mut cmap, 12);
    cmap.extend_from_slice(&sub);

    let mut head = Vec::new();
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0x0001_0000);
    be32(&mut head, 0); // checkSumAdjustment, patched below
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0b11); // baseline at 0, lsb at 0
    be16(&mut head, UPEM as u16);
    head.extend_from_slice(&[0; 16]);
    for v in [bx0, by0, bx1, by1] {
        bei16(&mut head, v);
    }
    be16(&mut head, spec.mac_style);
    be16(&mut head, 8);
    bei16(&mut head, 2);
    bei16(&mut head, 1); // long loca
    bei16(&mut head, 0);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    bei16(&mut hhea, ASCENT);
    bei16(&mut hhea, DESCENT);
    bei16(&mut hhea, 0);
    be16(&mut hhea, adv_max);
    hhea.extend_from_slice(&[0; 22]);
    be16(&mut hhea, n);

    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0001_0000);
    be16(&mut maxp, n);
    for v in [max_pts, max_contours, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0] {
        be16(&mut maxp, u16::try_from(v).unwrap_or(0));
    }

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    post.extend_from_slice(&[0; 28]);

    let full = format!("{} {}", spec.family, spec.style);
    let strings: [(u16, String); 4] = [
        (1, spec.family.to_string()),
        (2, spec.style.to_string()),
        (4, full.clone()),
        (6, full.replace(' ', "-")),
    ];
    let mut name = Vec::new();
    be16(&mut name, 0);
    be16(&mut name, 4);
    be16(&mut name, u16::try_from(6 + 12 * 4).unwrap_or(0));
    let mut pool = Vec::new();
    for (id, s) in &strings {
        let u = utf16(s);
        be16(&mut name, 3);
        be16(&mut name, 1);
        be16(&mut name, 0x409);
        be16(&mut name, *id);
        be16(&mut name, u16::try_from(u.len()).unwrap_or(0));
        be16(&mut name, u16::try_from(pool.len()).unwrap_or(0));
        pool.extend_from_slice(&u);
    }
    name.extend_from_slice(&pool);

    let mut tables: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"maxp", maxp),
        (*b"name", name),
        (*b"post", post),
    ];
    tables.sort_by_key(|t| t.0);
    let nt = u16::try_from(tables.len()).unwrap_or(0);
    let mut out = Vec::new();
    be32(&mut out, 0x0001_0000);
    be16(&mut out, nt);
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= nt {
        pow *= 2;
        log += 1;
    }
    be16(&mut out, pow * 16);
    be16(&mut out, log);
    be16(&mut out, nt * 16 - pow * 16);
    let mut off = 12 + 16 * tables.len();
    let mut head_at = 0usize;
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        be32(&mut out, checksum(data));
        be32(&mut out, u32::try_from(off).unwrap_or(0));
        be32(&mut out, u32::try_from(data.len()).unwrap_or(0));
        if tag == b"head" {
            head_at = off;
        }
        off += data.len().div_ceil(4) * 4;
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    out.splice(head_at + 8..head_at + 12, adj.to_be_bytes());
    out
}

fn letters(look: Look) -> Vec<(char, Glyph)> {
    ('A'..='Z')
        .chain('a'..='z')
        .chain('0'..='9')
        .map(|c| (c, bar(look, c)))
        .collect()
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let dir = Path::new(&dir);
    let specs = [
        (
            "PxText-Regular.ttf",
            Spec {
                family: "Px Text",
                style: "Regular",
                mac_style: 0,
                glyphs: letters(Look::Hollow),
            },
        ),
        (
            "PxText-Bold.ttf",
            Spec {
                family: "Px Text",
                style: "Bold",
                mac_style: 1,
                glyphs: letters(Look::Solid),
            },
        ),
        (
            "PxText-Italic.ttf",
            Spec {
                family: "Px Text",
                style: "Italic",
                mac_style: 2,
                glyphs: letters(Look::Slanted),
            },
        ),
        (
            "PxSerif-Regular.ttf",
            Spec {
                family: "Px Serif",
                style: "Regular",
                mac_style: 0,
                glyphs: letters(Look::Serif),
            },
        ),
        (
            "PxMono-Regular.ttf",
            Spec {
                family: "Px Mono",
                style: "Regular",
                mac_style: 0,
                glyphs: letters(Look::Thin),
            },
        ),
        (
            "PxIcons-Regular.ttf",
            Spec {
                family: "Px Icons",
                style: "Regular",
                mac_style: 0,
                glyphs: (0..5u32)
                    .filter_map(|i| char::from_u32(0xE000 + i).map(|c| (c, icon(i))))
                    .collect(),
            },
        ),
    ];
    for (file, spec) in &specs {
        let bytes = build(spec);
        if let Err(e) = std::fs::write(dir.join(file), &bytes) {
            eprintln!("{file}: {e}");
            std::process::exit(1);
        }
        println!("{file} {} bytes", bytes.len());
    }
}
