//! WOFF 2.0 (W3C Recommendation, RFC 8618 transforms) to a plain sfnt.
//!
//! The file is a 48-byte header, a table directory with variable-length sizes,
//! and one Brotli stream holding every table's bytes in directory order. Two
//! tables may be stored transformed: `glyf` (with `loca`, rebuilt from it) and
//! `hmtx`. This module undoes both so the output is an ordinary sfnt.
//!
//! Hostile-input discipline: every length is read through [`Reader`], which only
//! hands out bytes that exist; the sum of declared table sizes is capped by
//! [`crate::woff::MAX_SFNT_BYTES`] before the Brotli stream is asked for that
//! many bytes (and Brotli itself stops at the cap); a glyph's point count, the
//! number of glyphs and the size of every stream are checked against the
//! transformed table that carries them; arithmetic on coordinates is checked.

use crate::sfnt::{FontError, u16_at};
use crate::woff::{MAX_SFNT_BYTES, MAX_TABLES, OutTable, brotli_bounded, build_sfnt, checksum};

fn bad(why: &'static str) -> FontError {
    FontError::Malformed(why)
}

/// The 63 table tags a one-byte directory index can name (WOFF2 section 5.1).
const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

/// Most glyphs a font may have (`maxp.numGlyphs` is a `u16`).
const MAX_GLYPHS: usize = 65535;

/// A bounds-checked cursor.
struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Self {
        Reader { d, p: 0 }
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], FontError> {
        let end = self.p.checked_add(n).ok_or(FontError::Truncated)?;
        let s = self.d.get(self.p..end).ok_or(FontError::Truncated)?;
        self.p = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, FontError> {
        Ok(self.bytes(1)?.first().copied().unwrap_or(0))
    }

    fn u16(&mut self) -> Result<u16, FontError> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ]))
    }

    fn i16(&mut self) -> Result<i16, FontError> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, FontError> {
        let hi = u32::from(self.u16()?);
        let lo = u32::from(self.u16()?);
        Ok((hi << 16) | lo)
    }

    /// `UIntBase128`: up to five bytes, no leading zero byte, must fit `u32`.
    fn base128(&mut self) -> Result<u32, FontError> {
        let mut acc: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return Err(bad("UIntBase128 leading zero"));
            }
            if acc > (u32::MAX >> 7) {
                return Err(bad("UIntBase128 overflow"));
            }
            acc = (acc << 7) | u32::from(b & 0x7f);
            if b & 0x80 == 0 {
                return Ok(acc);
            }
        }
        Err(bad("UIntBase128 too long"))
    }

    /// `255UInt16`.
    fn u255_16(&mut self) -> Result<u16, FontError> {
        match self.u8()? {
            253 => self.u16(),
            254 => Ok(506 + u16::from(self.u8()?)),
            255 => Ok(253 + u16::from(self.u8()?)),
            b => Ok(u16::from(b)),
        }
    }
}

struct Entry {
    tag: [u8; 4],
    transformed: bool,
    orig_len: usize,
    stored_len: usize,
}

pub(crate) fn decode(data: &[u8]) -> Result<Vec<u8>, FontError> {
    let mut h = Reader::new(data);
    h.u32()?; // signature, checked by the sniffer
    let flavor = h.u32()?;
    let length = h.u32()? as usize;
    let num_tables = usize::from(h.u16()?);
    h.u16()?; // reserved
    let total_sfnt = h.u32()? as usize;
    let total_comp = h.u32()? as usize;
    // major, minor version; meta and private block offsets: ignored.
    h.bytes(4 + 20)?;
    if length != data.len() {
        return Err(bad("WOFF2 length does not match the file"));
    }
    if num_tables == 0 || num_tables > MAX_TABLES {
        return Err(bad("WOFF2 table count"));
    }
    if total_sfnt > MAX_SFNT_BYTES {
        return Err(bad("WOFF2 sfnt size too large"));
    }
    match flavor {
        0x0001_0000 | 0x7472_7565 => {}
        0x4F54_544F => return Err(FontError::Unsupported("CFF outlines")),
        0x7474_6366 => return Err(FontError::Unsupported("font collection")),
        _ => return Err(FontError::BadMagic),
    }

    let mut entries: Vec<Entry> = Vec::with_capacity(num_tables);
    let mut stored_total: usize = 0;
    let mut orig_total: usize = 0;
    for _ in 0..num_tables {
        let flags = h.u8()?;
        let tag: [u8; 4] = if flags & 0x3f == 0x3f {
            h.bytes(4)?.try_into().map_err(|_| FontError::Truncated)?
        } else {
            KNOWN_TAGS
                .get(usize::from(flags & 0x3f))
                .map(|t| **t)
                .ok_or(bad("table tag index"))?
        };
        let version = flags >> 6;
        let orig_len = h.base128()? as usize;
        let transformed = if &tag == b"glyf" || &tag == b"loca" {
            version == 0
        } else {
            version != 0
        };
        let stored_len = if transformed {
            h.base128()? as usize
        } else {
            orig_len
        };
        if transformed && &tag == b"loca" && stored_len != 0 {
            return Err(bad("transformed loca must be empty"));
        }
        if transformed && !matches!(&tag, b"glyf" | b"loca" | b"hmtx") {
            return Err(FontError::Unsupported("unknown WOFF2 table transform"));
        }
        if transformed && &tag == b"hmtx" && version != 1 {
            return Err(FontError::Unsupported("unknown hmtx transform"));
        }
        stored_total = stored_total
            .checked_add(stored_len)
            .ok_or(bad("WOFF2 sizes overflow"))?;
        orig_total = orig_total
            .checked_add(orig_len)
            .ok_or(bad("WOFF2 sizes overflow"))?;
        if stored_total > MAX_SFNT_BYTES || orig_total > MAX_SFNT_BYTES {
            return Err(bad("WOFF2 tables too large"));
        }
        if entries.iter().any(|e| e.tag == tag) {
            return Err(bad("duplicate WOFF2 table"));
        }
        entries.push(Entry {
            tag,
            transformed,
            orig_len,
            stored_len,
        });
    }
    let compressed = h.bytes(total_comp)?;
    let raw = brotli_bounded(compressed, stored_total)?;
    if raw.len() != stored_total {
        return Err(bad("WOFF2 Brotli output has the wrong size"));
    }

    // Slice the decompressed block into tables.
    let mut at = 0usize;
    let mut slices: Vec<&[u8]> = Vec::with_capacity(entries.len());
    for e in &entries {
        let end = at + e.stored_len;
        slices.push(raw.get(at..end).ok_or(FontError::Truncated)?);
        at = end;
    }
    let find = |tag: &[u8; 4]| entries.iter().position(|e| &e.tag == tag);

    let mut glyf_out: Option<Glyf> = None;
    if let Some(gi) = find(b"glyf")
        && entries.get(gi).is_some_and(|e| e.transformed)
    {
        let li = find(b"loca").ok_or(bad("transformed glyf without loca"))?;
        if !entries.get(li).is_some_and(|e| e.transformed) {
            return Err(bad("transformed glyf with an untransformed loca"));
        }
        glyf_out = Some(reconstruct_glyf(slices.get(gi).copied().unwrap_or(&[]))?);
    } else if find(b"loca").is_some_and(|li| entries.get(li).is_some_and(|e| e.transformed)) {
        return Err(bad("transformed loca without a transformed glyf"));
    }

    let mut tables = Vec::with_capacity(entries.len());
    for (e, body) in entries.iter().zip(&slices) {
        let data: Vec<u8> = match (&e.tag, e.transformed) {
            (b"glyf", true) => glyf_out
                .as_ref()
                .map(|g| g.glyf.clone())
                .unwrap_or_default(),
            (b"loca", true) => glyf_out
                .as_ref()
                .map(|g| g.loca.clone())
                .unwrap_or_default(),
            (b"hmtx", true) => {
                let g = glyf_out
                    .as_ref()
                    .ok_or(bad("transformed hmtx needs a transformed glyf"))?;
                let hi = find(b"hhea").ok_or(bad("transformed hmtx without hhea"))?;
                let hhea = slices.get(hi).copied().unwrap_or(&[]);
                let nh = usize::from(u16_at(hhea, 34).ok_or(bad("hhea too short"))?);
                rebuild_hmtx(body, g, nh)?
            }
            _ => {
                if body.len() != e.orig_len {
                    return Err(bad("WOFF2 table length mismatch"));
                }
                let mut v = body.to_vec();
                if &e.tag == b"head"
                    && let Some(g) = &glyf_out
                    && let Some(slot) = v.get_mut(50..52)
                {
                    slot.copy_from_slice(&g.index_format.to_be_bytes());
                }
                v
            }
        };
        tables.push(OutTable {
            tag: e.tag,
            checksum: checksum(&data),
            data,
        });
    }
    build_sfnt(flavor, tables)
}

struct Glyf {
    glyf: Vec<u8>,
    loca: Vec<u8>,
    index_format: u16,
    x_min: Vec<i16>,
}

/// Decode one `(dx, dy)` coordinate triplet (RFC 8618 section 5.2).
fn triplet(flag: u8, g: &mut Reader<'_>) -> Result<(i32, i32), FontError> {
    let sign = |bit: u8, v: i32| if bit & 1 != 0 { v } else { -v };
    let f = i32::from(flag);
    Ok(if flag < 10 {
        let b0 = i32::from(g.u8()?);
        (0, sign(flag, ((f & 14) << 7) + b0))
    } else if flag < 20 {
        let b0 = i32::from(g.u8()?);
        (sign(flag, (((f - 10) & 14) << 7) + b0), 0)
    } else if flag < 84 {
        let b0 = f - 20;
        let b1 = i32::from(g.u8()?);
        (
            sign(flag, 1 + (b0 & 0x30) + (b1 >> 4)),
            sign(flag >> 1, 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f)),
        )
    } else if flag < 120 {
        let b0 = f - 84;
        let b1 = i32::from(g.u8()?);
        let b2 = i32::from(g.u8()?);
        (
            sign(flag, 1 + ((b0 / 12) << 8) + b1),
            sign(flag >> 1, 1 + (((b0 % 12) >> 2) << 8) + b2),
        )
    } else if flag < 124 {
        let b1 = i32::from(g.u8()?);
        let b2 = i32::from(g.u8()?);
        let b3 = i32::from(g.u8()?);
        (
            sign(flag, (b1 << 4) + (b2 >> 4)),
            sign(flag >> 1, ((b2 & 0x0f) << 8) + b3),
        )
    } else {
        let b1 = i32::from(g.u8()?);
        let b2 = i32::from(g.u8()?);
        let b3 = i32::from(g.u8()?);
        let b4 = i32::from(g.u8()?);
        (sign(flag, (b1 << 8) + b2), sign(flag >> 1, (b3 << 8) + b4))
    })
}

fn push_u16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn push_i16(v: &mut Vec<u8>, x: i16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn reconstruct_glyf(t: &[u8]) -> Result<Glyf, FontError> {
    let mut r = Reader::new(t);
    if r.u16()? != 0 {
        return Err(FontError::Unsupported("glyf transform version"));
    }
    let option_flags = r.u16()?;
    let num_glyphs = usize::from(r.u16()?);
    let index_format = r.u16()?;
    if index_format > 1 {
        return Err(bad("indexFormat"));
    }
    if num_glyphs > MAX_GLYPHS || num_glyphs == 0 {
        return Err(bad("glyph count"));
    }
    let mut sizes = [0usize; 7];
    for s in &mut sizes {
        *s = r.u32()? as usize;
    }
    let mut stream = |n: usize| r.bytes(n);
    let mut n_contour = Reader::new(stream(sizes[0])?);
    let mut n_points = Reader::new(stream(sizes[1])?);
    let mut flags_s = Reader::new(stream(sizes[2])?);
    let mut glyph_s = Reader::new(stream(sizes[3])?);
    let mut comp_s = Reader::new(stream(sizes[4])?);
    let bbox_all = stream(sizes[5])?;
    let mut instr_s = Reader::new(stream(sizes[6])?);
    let overlap: &[u8] = if option_flags & 1 != 0 {
        r.bytes(num_glyphs.div_ceil(8))?
    } else {
        &[]
    };
    let bitmap_len = 4 * num_glyphs.div_ceil(32);
    let bbox_bitmap = bbox_all.get(..bitmap_len).ok_or(bad("bbox bitmap"))?;
    let mut bbox_s = Reader::new(bbox_all.get(bitmap_len..).unwrap_or(&[]));

    let mut glyf: Vec<u8> = Vec::new();
    let mut offsets: Vec<u32> = Vec::with_capacity(num_glyphs + 1);
    let mut x_min: Vec<i16> = Vec::with_capacity(num_glyphs);
    for i in 0..num_glyphs {
        offsets.push(u32::try_from(glyf.len()).map_err(|_| bad("glyf too large"))?);
        let nc = n_contour.i16()?;
        let has_bbox = bbox_bitmap
            .get(i >> 3)
            .is_some_and(|b| b & (0x80 >> (i & 7)) != 0);
        if nc == 0 {
            if has_bbox {
                return Err(bad("empty glyph with a bbox"));
            }
            x_min.push(0);
            continue;
        }
        let start = glyf.len();
        if nc > 0 {
            let nc = usize::try_from(nc).map_err(|_| bad("contours"))?;
            let mut end_pts: Vec<u16> = Vec::with_capacity(nc);
            let mut total: usize = 0;
            for _ in 0..nc {
                let np = usize::from(n_points.u255_16()?);
                total += np;
                if total == 0 || total > 0xFFFF {
                    return Err(bad("point count"));
                }
                end_pts.push(u16::try_from(total - 1).map_err(|_| bad("point count"))?);
            }
            let flags = flags_s.bytes(total)?;
            let mut pts: Vec<(i32, i32, bool)> = Vec::with_capacity(total);
            let (mut x, mut y) = (0i32, 0i32);
            for &f in flags {
                let (dx, dy) = triplet(f & 0x7f, &mut glyph_s)?;
                x += dx;
                y += dy;
                if i16::try_from(x).is_err() || i16::try_from(y).is_err() {
                    return Err(bad("coordinate out of range"));
                }
                pts.push((x, y, f & 0x80 == 0));
            }
            let instr_len = usize::from(glyph_s.u255_16()?);
            let instr = instr_s.bytes(instr_len)?;
            let (bx0, by0, bx1, by1) = if has_bbox {
                (bbox_s.i16()?, bbox_s.i16()?, bbox_s.i16()?, bbox_s.i16()?)
            } else {
                let xs = pts.iter().map(|p| p.0);
                let ys = pts.iter().map(|p| p.1);
                let c = |v: i32| i16::try_from(v).unwrap_or(0);
                (
                    c(xs.clone().min().unwrap_or(0)),
                    c(ys.clone().min().unwrap_or(0)),
                    c(xs.max().unwrap_or(0)),
                    c(ys.max().unwrap_or(0)),
                )
            };
            push_i16(&mut glyf, i16::try_from(nc).map_err(|_| bad("contours"))?);
            for v in [bx0, by0, bx1, by1] {
                push_i16(&mut glyf, v);
            }
            for e in &end_pts {
                push_u16(&mut glyf, *e);
            }
            push_u16(
                &mut glyf,
                u16::try_from(instr_len).map_err(|_| bad("instructions"))?,
            );
            glyf.extend_from_slice(instr);
            let set_overlap = overlap
                .get(i >> 3)
                .is_some_and(|b| b & (0x80 >> (i & 7)) != 0);
            let mut xs: Vec<u8> = Vec::new();
            let mut ys: Vec<u8> = Vec::new();
            let (mut px, mut py) = (0i32, 0i32);
            for (n, &(x, y, on)) in pts.iter().enumerate() {
                let mut fl: u8 = u8::from(on);
                if n == 0 && set_overlap {
                    fl |= 0x40;
                }
                let (dx, dy) = (x - px, y - py);
                px = x;
                py = y;
                if dx == 0 {
                    fl |= 0x10;
                } else if dx.abs() < 256 {
                    fl |= 0x02 | if dx > 0 { 0x10 } else { 0 };
                    xs.push(dx.unsigned_abs() as u8);
                } else {
                    push_i16(&mut xs, i16::try_from(dx).map_err(|_| bad("delta"))?);
                }
                if dy == 0 {
                    fl |= 0x20;
                } else if dy.abs() < 256 {
                    fl |= 0x04 | if dy > 0 { 0x20 } else { 0 };
                    ys.push(dy.unsigned_abs() as u8);
                } else {
                    push_i16(&mut ys, i16::try_from(dy).map_err(|_| bad("delta"))?);
                }
                glyf.push(fl);
            }
            glyf.extend_from_slice(&xs);
            glyf.extend_from_slice(&ys);
            x_min.push(bx0);
        } else if nc == -1 {
            if !has_bbox {
                return Err(bad("composite glyph without a bbox"));
            }
            let bbox = [bbox_s.i16()?, bbox_s.i16()?, bbox_s.i16()?, bbox_s.i16()?];
            let comp_start = comp_s.p;
            let mut have_instr = false;
            loop {
                let fl = comp_s.u16()?;
                comp_s.u16()?; // glyph index
                comp_s.bytes(if fl & 0x0001 != 0 { 4 } else { 2 })?;
                if fl & 0x0008 != 0 {
                    comp_s.bytes(2)?;
                } else if fl & 0x0040 != 0 {
                    comp_s.bytes(4)?;
                } else if fl & 0x0080 != 0 {
                    comp_s.bytes(8)?;
                }
                have_instr |= fl & 0x0100 != 0;
                if fl & 0x0020 == 0 {
                    break;
                }
            }
            let comps = comp_s
                .d
                .get(comp_start..comp_s.p)
                .ok_or(FontError::Truncated)?;
            push_i16(&mut glyf, -1);
            for v in bbox {
                push_i16(&mut glyf, v);
            }
            glyf.extend_from_slice(comps);
            if have_instr {
                let n = usize::from(glyph_s.u255_16()?);
                push_u16(&mut glyf, n as u16);
                glyf.extend_from_slice(instr_s.bytes(n)?);
            }
            x_min.push(bbox[0]);
        } else {
            return Err(bad("numberOfContours"));
        }
        while !(glyf.len() - start).is_multiple_of(4) {
            glyf.push(0);
        }
        if glyf.len() > MAX_SFNT_BYTES {
            return Err(bad("glyf too large"));
        }
    }
    offsets.push(u32::try_from(glyf.len()).map_err(|_| bad("glyf too large"))?);
    let mut loca = Vec::with_capacity(offsets.len() * 4);
    for o in &offsets {
        if index_format == 0 {
            push_u16(
                &mut loca,
                u16::try_from(o / 2).map_err(|_| bad("loca offset too large for short format"))?,
            );
        } else {
            loca.extend_from_slice(&o.to_be_bytes());
        }
    }
    Ok(Glyf {
        glyf,
        loca,
        index_format,
        x_min,
    })
}

fn rebuild_hmtx(t: &[u8], g: &Glyf, num_hmetrics: usize) -> Result<Vec<u8>, FontError> {
    let num_glyphs = g.x_min.len();
    let mut r = Reader::new(t);
    let flags = r.u8()?;
    if flags & 0xFC != 0 {
        return Err(bad("hmtx transform flags"));
    }
    if num_hmetrics == 0 || num_hmetrics > num_glyphs {
        return Err(bad("numberOfHMetrics"));
    }
    let mut adv = Vec::with_capacity(num_hmetrics);
    for _ in 0..num_hmetrics {
        adv.push(r.u16()?);
    }
    let mut lsb: Vec<i16> = Vec::with_capacity(num_glyphs);
    if flags & 1 == 0 {
        for _ in 0..num_hmetrics {
            lsb.push(r.i16()?);
        }
    } else {
        lsb.extend(g.x_min.iter().take(num_hmetrics));
    }
    if flags & 2 == 0 {
        for _ in num_hmetrics..num_glyphs {
            lsb.push(r.i16()?);
        }
    } else {
        lsb.extend(g.x_min.iter().skip(num_hmetrics));
    }
    let mut out = Vec::with_capacity(num_hmetrics * 4 + (num_glyphs - num_hmetrics) * 2);
    for (i, l) in lsb.iter().enumerate() {
        if let Some(a) = adv.get(i) {
            push_u16(&mut out, *a);
        }
        push_i16(&mut out, *l);
    }
    Ok(out)
}
