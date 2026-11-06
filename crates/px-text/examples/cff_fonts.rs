//! Writes the tiny deterministic CFF/OpenType test fonts under `tests/fonts/`.
//!
//! ```text
//! cargo run -p px-text --example cff_fonts -- tests/fonts
//! ```
//!
//! A companion to `fixture_fonts.rs`: where that writes TrueType fonts, this writes
//! the same kind of shapes as Type 2 charstrings, with curves, so the CFF reader and
//! its cubic flattening have fixtures that need no tool outside this repository.
//! The bytes are a pure function of this file.
//!
//! | file | what |
//! |---|---|
//! | `PxCff-Regular.otf` | family Px Cff, hollow rounded bars, advance 600 |
//! | `PxCff-Bold.otf` | Px Cff (700), solid wider bars, advance 700 |
//! | `PxCff.ttc` | a collection: face 0 Regular, face 1 Bold |
//! | `PxCff-Regular.woff` | Regular in WOFF 1.0 (tables stored, not deflated) |
//! | `PxCff-Regular.woff2` | Regular in WOFF 2.0 (a Brotli stream of stored blocks) |
//!
//! Glyphs: `.notdef`, space, `A-Z`, `a-z`, `0-9`. Uppercase are rounded rectangles
//! (`hlineto`, `vlineto`, `hvcurveto`, `vhcurveto`) with a rounded hole, using a
//! global subroutine for the hints (`hstemhm`, `hintmask`); lowercase are solid
//! rounded bars with a round dot from `vhcurveto`/`hvcurveto`, the dot drawn by a
//! local subroutine; digits are plain bars whose edges use, by `c % 3`, `vvcurveto`;
//! `hflex` with `hhcurveto`; `rlinecurve` with `rcurveline`. Digits and the space
//! carry a width operand (the others use the Private DICT default), so both width
//! paths are covered.

use std::path::Path;

const UPEM: i16 = 1000;
const ASCENT: i16 = 800;
const DESCENT: i16 = -200;
const NOMINAL: i32 = 500;

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
fn u16_of(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(0)
}
fn u32_of(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(0)
}

// ---- Type 2 charstring assembly ------------------------------------------------

const HSTEMHM: u8 = 18;
const HINTMASK: u8 = 19;
const RMOVETO: u8 = 21;
const RLINETO: u8 = 5;
const HLINETO: u8 = 6;
const VLINETO: u8 = 7;
const RRCURVETO: u8 = 8;
const RCURVELINE: u8 = 24;
const RLINECURVE: u8 = 25;
const VVCURVETO: u8 = 26;
const HHCURVETO: u8 = 27;
const VHCURVETO: u8 = 30;
const HVCURVETO: u8 = 31;
const CALLSUBR: u8 = 10;
const CALLGSUBR: u8 = 29;
const RETURN: u8 = 11;
const ENDCHAR: u8 = 14;

#[derive(Default)]
struct Cs(Vec<u8>);

impl Cs {
    fn n(&mut self, v: i32) {
        match v {
            -107..=107 => self.0.push((v + 139) as u8),
            108..=1131 => {
                let w = v - 108;
                self.0.push((w / 256 + 247) as u8);
                self.0.push((w % 256) as u8);
            }
            -1131..=-108 => {
                let w = -v - 108;
                self.0.push((w / 256 + 251) as u8);
                self.0.push((w % 256) as u8);
            }
            _ => {
                self.0.push(28);
                self.0.extend_from_slice(&(v as i16).to_be_bytes());
            }
        }
    }
    fn op(&mut self, args: &[i32], op: u8) {
        for &a in args {
            self.n(a);
        }
        self.0.push(op);
    }
    fn esc(&mut self, args: &[i32], op: u8) {
        for &a in args {
            self.n(a);
        }
        self.0.extend_from_slice(&[12, op]);
    }
    fn line(&mut self, dx: i32, dy: i32) {
        if dy == 0 {
            self.op(&[dx], HLINETO);
        } else if dx == 0 {
            self.op(&[dy], VLINETO);
        } else {
            self.op(&[dx, dy], RLINETO);
        }
    }
    /// A cubic given as three relative vectors; picks the shortest encoding.
    fn curve(&mut self, d1: (i32, i32), d2: (i32, i32), d3: (i32, i32)) {
        if d1.1 == 0 && d3.0 == 0 {
            self.op(&[d1.0, d2.0, d2.1, d3.1], HVCURVETO);
        } else if d1.0 == 0 && d3.1 == 0 {
            self.op(&[d1.1, d2.0, d2.1, d3.0], VHCURVETO);
        } else {
            self.op(&[d1.0, d1.1, d2.0, d2.1, d3.0, d3.1], RRCURVETO);
        }
    }
    /// A quarter turn of radius `r` from unit direction `a` to unit direction `b`.
    fn corner(&mut self, r: i32, a: (i32, i32), b: (i32, i32)) {
        let k = r * 55 / 100;
        let c1 = (k * a.0, k * a.1);
        let p3 = (r * a.0 + r * b.0, r * a.1 + r * b.1);
        let c2 = (p3.0 - k * b.0, p3.1 - k * b.1);
        self.curve(c1, (c2.0 - c1.0, c2.1 - c1.1), (p3.0 - c2.0, p3.1 - c2.1));
    }
    /// A rounded rectangle, starting where the current point is: the bottom edge's
    /// left end (counter-clockwise) or the top edge's left end (clockwise).
    fn rounded_rect(&mut self, w: i32, h: i32, r: i32, ccw: bool) {
        let dirs: [(i32, i32); 4] = if ccw {
            [(1, 0), (0, 1), (-1, 0), (0, -1)]
        } else {
            [(1, 0), (0, -1), (-1, 0), (0, 1)]
        };
        for i in 0..4 {
            let a = dirs[i];
            let b = dirs[(i + 1) % 4];
            let len = if i % 2 == 0 { w - 2 * r } else { h - 2 * r };
            self.line(a.0 * len, a.1 * len);
            self.corner(r, a, b);
        }
    }
}

// ---- glyph designs -----------------------------------------------------------------

struct Style {
    family: &'static str,
    style: &'static str,
    bold: bool,
    /// Left and right edge of the bars.
    x0: i32,
    x1: i32,
    /// Advance of letters and `.notdef` (the Private DICT's defaultWidthX).
    advance: i32,
    digit_advance: i32,
    hole: bool,
}

struct GlyphOut {
    cs: Vec<u8>,
    advance: i32,
    lsb: i32,
}

fn bar_height(c: char) -> i32 {
    320 + i32::try_from(u32::from(c) % 7).unwrap_or(0) * 60
}

fn glyph(st: &Style, c: char) -> GlyphOut {
    let w = st.x1 - st.x0;
    let h = bar_height(c);
    let mut cs = Cs::default();
    let mut advance = st.advance;
    let lsb = st.x0;
    if c.is_ascii_uppercase() {
        cs.op(&[-107], CALLGSUBR); // hints
        cs.op(&[st.x0 + 80, 0], RMOVETO);
        cs.rounded_rect(w, h, 80, true);
        if st.hole {
            cs.op(&[50, h - 70], RMOVETO);
            cs.rounded_rect(w - 180, h - 140, 40, false);
        }
    } else if c.is_ascii_lowercase() {
        cs.op(&[st.x0 + 50, 0], RMOVETO);
        cs.rounded_rect(w, h, 50, true);
        // The dot: a local subroutine drawn from its rightmost point.
        cs.op(&[st.advance / 2 + 50 - (st.x0 + 50), h + 100], RMOVETO);
        cs.op(&[-107], CALLSUBR);
    } else if c.is_ascii_digit() {
        advance = st.digit_advance;
        cs.n(advance - NOMINAL);
        cs.op(&[st.x0, 0], RMOVETO);
        match u32::from(c) % 3 {
            0 => {
                cs.op(&[w], HLINETO);
                cs.op(&[h], VLINETO);
                cs.op(&[-w], HLINETO);
                // The left edge bulges outwards: dx1 dya dxb dyb dyc.
                cs.op(&[-20, -100, 20, -100, -(h - 200)], VVCURVETO);
            }
            1 => {
                let s = w / 6;
                cs.esc(&[s, s, 30, s, s, s, s], 34); // hflex
                cs.op(&[h], VLINETO);
                cs.op(&[-w / 3, -w / 3, 40, -w / 3], HHCURVETO);
                cs.op(&[-(h + 40)], VLINETO);
            }
            _ => {
                cs.op(&[w, 0, 0, 100, 0, 100, 0, h - 200], RLINECURVE);
                cs.op(
                    &[-w / 4, 20, -w / 4, 10, -w / 6, 0, -w / 3, -30],
                    RCURVELINE,
                );
            }
        }
    }
    cs.0.push(ENDCHAR);
    GlyphOut {
        cs: cs.0,
        advance,
        lsb,
    }
}

fn notdef(st: &Style) -> GlyphOut {
    let mut cs = Cs::default();
    cs.op(&[60, 0], RMOVETO);
    cs.rounded_rect(460, 700, 40, true);
    cs.op(&[40, 600], RMOVETO);
    cs.rounded_rect(300, 560, 20, false);
    cs.0.push(ENDCHAR);
    GlyphOut {
        cs: cs.0,
        advance: st.advance,
        lsb: 60,
    }
}

fn space() -> GlyphOut {
    let mut cs = Cs::default();
    cs.op(&[400 - NOMINAL], ENDCHAR);
    GlyphOut {
        cs: cs.0,
        advance: 400,
        lsb: 0,
    }
}

// ---- CFF table --------------------------------------------------------------------

fn index(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    be16(&mut out, u16_of(objects.len()));
    if objects.is_empty() {
        return out;
    }
    out.push(4);
    let mut at = 1u32;
    be32(&mut out, at);
    for o in objects {
        at += u32_of(o.len());
        be32(&mut out, at);
    }
    for o in objects {
        out.extend_from_slice(o);
    }
    out
}

fn num5(v: i32) -> Vec<u8> {
    let mut o = vec![29];
    o.extend_from_slice(&v.to_be_bytes());
    o
}

fn cff_table(st: &Style, name: &str, glyphs: &[GlyphOut]) -> Vec<u8> {
    // Global subroutine 0: the hints of an uppercase letter.
    let mut hints = Cs::default();
    hints.op(&[0, 70], HSTEMHM);
    hints.op(&[st.x0, 90], HINTMASK);
    hints.0.push(0xC0);
    hints.0.push(RETURN);
    // Local subroutine 0: a round dot of radius 50, counter-clockwise from its
    // rightmost point.
    let mut dot = Cs::default();
    for (a, b) in [
        ((0, 1), (-1, 0)),
        ((-1, 0), (0, -1)),
        ((0, -1), (1, 0)),
        ((1, 0), (0, 1)),
    ] {
        dot.corner(50, a, b);
    }
    dot.0.push(RETURN);

    let head = [1u8, 0, 4, 4];
    let names = index(&[name.as_bytes().to_vec()]);
    let strings = index(&[]);
    let gsubrs = index(&[hints.0]);
    let charstrings = index(&glyphs.iter().map(|g| g.cs.clone()).collect::<Vec<_>>());

    let mut private = num5(st.advance);
    private.push(20);
    private.extend(num5(NOMINAL));
    private.push(21);
    let subrs_off = private.len() + 6;
    private.extend(num5(i32::try_from(subrs_off).unwrap_or(0)));
    private.push(19);

    let top = |cs_off: usize, priv_off: usize| {
        let mut t = Vec::new();
        for v in [0, i32::from(DESCENT), st.digit_advance, 900] {
            t.extend(num5(v));
        }
        t.push(5); // FontBBox
        t.extend(num5(0));
        t.push(15); // charset: ISOAdobe
        t.extend(num5(i32::try_from(cs_off).unwrap_or(0)));
        t.push(17); // CharStrings
        t.extend(num5(i32::try_from(private.len()).unwrap_or(0)));
        t.extend(num5(i32::try_from(priv_off).unwrap_or(0)));
        t.push(18); // Private
        index(&[t])
    };
    let top_len = top(0, 0).len();
    let cs_off = head.len() + names.len() + top_len + strings.len() + gsubrs.len();
    let priv_off = cs_off + charstrings.len();
    let mut out = head.to_vec();
    out.extend(names);
    out.extend(top(cs_off, priv_off));
    out.extend(strings);
    out.extend(gsubrs);
    out.extend(charstrings);
    out.extend(private);
    out.extend(index(&[dot.0]));
    out
}

// ---- sfnt -------------------------------------------------------------------------

type Tables = Vec<([u8; 4], Vec<u8>)>;

fn tables(st: &Style) -> Tables {
    let mut glyphs = vec![notdef(st), space()];
    let mut chars: Vec<char> = ('A'..='Z').chain('a'..='z').chain('0'..='9').collect();
    glyphs.extend(chars.iter().map(|&c| glyph(st, c)));
    let n = u16_of(glyphs.len());
    let full = format!("{} {}", st.family, st.style);
    let cff = cff_table(st, &full.replace(' ', "-"), &glyphs);

    let mut hmtx = Vec::new();
    for g in &glyphs {
        be16(&mut hmtx, u16_of(usize::try_from(g.advance).unwrap_or(0)));
        bei16(&mut hmtx, i16::try_from(g.lsb).unwrap_or(0));
    }

    // cmap format 4, one segment per code point.
    chars.insert(0, ' ');
    let mut codes: Vec<(u16, u16)> = Vec::new();
    for (i, c) in chars.iter().enumerate() {
        codes.push((u16_of(u32::from(*c) as usize), u16_of(i + 1)));
    }
    codes.sort_unstable();
    let seg = codes.len() + 1;
    let mut sub = Vec::new();
    be16(&mut sub, 4);
    be16(&mut sub, u16_of(16 + seg * 8));
    be16(&mut sub, 0);
    be16(&mut sub, u16_of(seg * 2));
    let mut pow = 1usize;
    let mut log = 0u16;
    while pow * 2 <= seg {
        pow *= 2;
        log += 1;
    }
    be16(&mut sub, u16_of(pow * 2));
    be16(&mut sub, log);
    be16(&mut sub, u16_of(seg * 2 - pow * 2));
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
    be32(&mut head, 0); // checkSumAdjustment, patched by the caller
    be32(&mut head, 0x5F0F_3CF5);
    be16(&mut head, 0b11);
    be16(&mut head, UPEM as u16);
    head.extend_from_slice(&[0; 16]);
    for v in [
        0i16,
        DESCENT,
        i16::try_from(st.digit_advance).unwrap_or(0),
        900,
    ] {
        bei16(&mut head, v);
    }
    be16(&mut head, u16::from(st.bold));
    be16(&mut head, 8);
    bei16(&mut head, 2);
    bei16(&mut head, 0); // short loca: unused by CFF
    bei16(&mut head, 0);

    let mut hhea = Vec::new();
    be32(&mut hhea, 0x0001_0000);
    bei16(&mut hhea, ASCENT);
    bei16(&mut hhea, DESCENT);
    bei16(&mut hhea, 0);
    be16(
        &mut hhea,
        u16_of(usize::try_from(st.digit_advance).unwrap_or(0)),
    );
    hhea.extend_from_slice(&[0; 22]);
    be16(&mut hhea, n);

    // maxp version 0.5.
    let mut maxp = Vec::new();
    be32(&mut maxp, 0x0000_5000);
    be16(&mut maxp, n);

    let mut os2 = vec![0u8; 78];
    os2[4..6].copy_from_slice(&(if st.bold { 700u16 } else { 400 }).to_be_bytes());
    os2[62..64].copy_from_slice(&(if st.bold { 0x20u16 } else { 0x40 }).to_be_bytes());

    let mut post = Vec::new();
    be32(&mut post, 0x0003_0000);
    post.extend_from_slice(&[0; 28]);

    let strings: [(u16, String); 4] = [
        (1, st.family.to_string()),
        (2, st.style.to_string()),
        (4, full.clone()),
        (6, full.replace(' ', "-")),
    ];
    let mut name = Vec::new();
    be16(&mut name, 0);
    be16(&mut name, 4);
    be16(&mut name, u16_of(6 + 12 * 4));
    let mut pool = Vec::new();
    for (id, s) in &strings {
        let u = utf16(s);
        be16(&mut name, 3);
        be16(&mut name, 1);
        be16(&mut name, 0x409);
        be16(&mut name, *id);
        be16(&mut name, u16_of(u.len()));
        be16(&mut name, u16_of(pool.len()));
        pool.extend_from_slice(&u);
    }
    name.extend_from_slice(&pool);

    let mut t: Tables = vec![
        (*b"CFF ", cff),
        (*b"OS/2", os2),
        (*b"cmap", cmap),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"maxp", maxp),
        (*b"name", name),
        (*b"post", post),
    ];
    t.sort_by_key(|e| e.0);
    t
}

fn search_fields(out: &mut Vec<u8>, nt: u16) {
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow * 2 <= nt {
        pow *= 2;
        log += 1;
    }
    be16(out, pow * 16);
    be16(out, log);
    be16(out, nt * 16 - pow * 16);
}

/// An sfnt whose tables start `base` bytes into the file it will live in (0 for a
/// plain font); `head.checkSumAdjustment` is patched only for a plain font.
fn sfnt(tables: &Tables, base: usize) -> Vec<u8> {
    let nt = u16_of(tables.len());
    let mut out = Vec::new();
    out.extend_from_slice(b"OTTO");
    be16(&mut out, nt);
    search_fields(&mut out, nt);
    let mut off = base + 12 + 16 * tables.len();
    let mut head_at = 0usize;
    for (tag, data) in tables {
        out.extend_from_slice(tag);
        be32(&mut out, checksum(data));
        be32(&mut out, u32_of(off));
        be32(&mut out, u32_of(data.len()));
        if tag == b"head" {
            head_at = off - base;
        }
        off += data.len().div_ceil(4) * 4;
    }
    for (_, data) in tables {
        out.extend_from_slice(data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    if base == 0 {
        let adj = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
        out.splice(head_at + 8..head_at + 12, adj.to_be_bytes());
    }
    out
}

fn ttc(faces: &[Tables]) -> Vec<u8> {
    let header = 12 + 4 * faces.len();
    let mut bodies = Vec::new();
    let mut offsets = Vec::new();
    let mut at = header;
    for f in faces {
        offsets.push(at);
        let body = sfnt(f, at);
        at += body.len();
        bodies.push(body);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"ttcf");
    be32(&mut out, 0x0001_0000);
    be32(&mut out, u32_of(faces.len()));
    for o in &offsets {
        be32(&mut out, u32_of(*o));
    }
    for b in &bodies {
        out.extend_from_slice(b);
    }
    out
}

fn woff1(tables: &Tables) -> Vec<u8> {
    let nt = tables.len();
    let mut total = 12 + 16 * nt;
    for (_, d) in tables {
        total += d.len().div_ceil(4) * 4;
    }
    let dir_end = 44 + 20 * nt;
    let mut body_len = 0usize;
    for (_, d) in tables {
        body_len += d.len().div_ceil(4) * 4;
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"wOFF");
    out.extend_from_slice(b"OTTO");
    be32(&mut out, u32_of(dir_end + body_len));
    be16(&mut out, u16_of(nt));
    be16(&mut out, 0);
    be32(&mut out, u32_of(total));
    be16(&mut out, 1);
    be16(&mut out, 0);
    out.extend_from_slice(&[0; 20]);
    let mut off = dir_end;
    for (tag, d) in tables {
        out.extend_from_slice(tag);
        be32(&mut out, u32_of(off));
        be32(&mut out, u32_of(d.len()));
        be32(&mut out, u32_of(d.len()));
        be32(&mut out, checksum(d));
        off += d.len().div_ceil(4) * 4;
    }
    for (_, d) in tables {
        out.extend_from_slice(d);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    out
}

fn base128(mut v: u32, out: &mut Vec<u8>) {
    let mut groups = vec![(v & 0x7f) as u8];
    v >>= 7;
    while v > 0 {
        groups.push((v & 0x7f) as u8 | 0x80);
        v >>= 7;
    }
    groups.reverse();
    out.extend(groups);
}

/// A valid Brotli stream that stores `data` in uncompressed meta-blocks.
fn brotli_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, chunk) in data.chunks(65536).enumerate() {
        let m = u32_of(chunk.len() - 1);
        let bits = if i == 0 {
            // WBITS = 16 (one 0 bit), ISLAST 0, MNIBBLES 0 (four), MLEN-1, ISUNCOMPRESSED 1.
            (m << 4) | (1 << 20)
        } else {
            (m << 3) | (1 << 19)
        };
        out.extend_from_slice(&bits.to_le_bytes()[..3]);
        out.extend_from_slice(chunk);
    }
    if data.is_empty() {
        out.push(0x06);
    } else {
        out.push(0x03); // ISLAST, ISLASTEMPTY
    }
    out
}

fn woff2(tables: &Tables) -> Vec<u8> {
    let nt = tables.len();
    let mut total = 12 + 16 * nt;
    let mut raw = Vec::new();
    let mut dir = Vec::new();
    for (tag, d) in tables {
        total += d.len().div_ceil(4) * 4;
        dir.push(0x3f); // arbitrary tag, no transform
        dir.extend_from_slice(tag);
        base128(u32_of(d.len()), &mut dir);
        raw.extend_from_slice(d);
    }
    let comp = brotli_stored(&raw);
    let mut body = dir;
    body.extend_from_slice(&comp);
    while !(48 + body.len()).is_multiple_of(4) {
        body.push(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"wOF2");
    out.extend_from_slice(b"OTTO");
    be32(&mut out, u32_of(48 + body.len()));
    be16(&mut out, u16_of(nt));
    be16(&mut out, 0);
    be32(&mut out, u32_of(total));
    be32(&mut out, u32_of(comp.len()));
    be16(&mut out, 1);
    be16(&mut out, 0);
    out.extend_from_slice(&[0; 20]);
    out.extend_from_slice(&body);
    out
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let dir = Path::new(&dir);
    let regular = Style {
        family: "Px Cff",
        style: "Regular",
        bold: false,
        x0: 60,
        x1: 540,
        advance: 600,
        digit_advance: 650,
        hole: true,
    };
    let bold = Style {
        family: "Px Cff",
        style: "Bold",
        bold: true,
        x0: 40,
        x1: 640,
        advance: 700,
        digit_advance: 750,
        hole: false,
    };
    let rt = tables(&regular);
    let bt = tables(&bold);
    let files: [(&str, Vec<u8>); 5] = [
        ("PxCff-Regular.otf", sfnt(&rt, 0)),
        ("PxCff-Bold.otf", sfnt(&bt, 0)),
        ("PxCff.ttc", ttc(&[rt.clone(), bt])),
        ("PxCff-Regular.woff", woff1(&rt)),
        ("PxCff-Regular.woff2", woff2(&rt)),
    ];
    for (file, bytes) in &files {
        if let Err(e) = std::fs::write(dir.join(file), bytes) {
            eprintln!("{file}: {e}");
            std::process::exit(1);
        }
        println!("{file} {} bytes", bytes.len());
    }
}
