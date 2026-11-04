//! JPEG decoder: baseline, extended-sequential (8-bit) and progressive
//! Huffman, 1 (grey) or 3 (YCbCr or RGB) components, any integral chroma
//! subsampling, restart intervals.
//!
//! Rejected as `Unsupported`, never decoded wrongly: arithmetic coding,
//! 12-bit precision, lossless, hierarchical/differential frames, four
//! components (CMYK/YCCK), two components, fractional sampling ratios.
//! EXIF, ICC, JFIF thumbnails and all other metadata are skipped (orientation
//! is *not* applied). Multi-frame (DNL) files are rejected.
//!
//! # Determinism (no floating point anywhere)
//!
//! * **IDCT**: the libjpeg "islow" algorithm (Loeffler, Ligtenberg and
//!   Moschytz; 13-bit fixed-point constants, 2 extra bits between passes),
//!   in `i64`. Output is the level-shifted, clamped 8-bit sample.
//! * **Upsampling**: libjpeg's "fancy" triangle filter for 2:1 horizontal
//!   (h2v1), 2:1 both (h2v2) and 1:2 vertical (h1v2), with replicated edges
//!   at the true component boundary; plain replication for any other integral
//!   ratio, and for h2v1/h2v2 components three samples wide or less.
//! * **Colour**: YCbCr to RGB with libjpeg's 16-bit fixed-point tables
//!   (`R = Y + (91881 Cr' + 32768 >> 16)`, `B = Y + (116130 Cb' + 32768 >> 16)`,
//!   `G = Y + (-22554 Cb' - 46802 Cr' + 32768 >> 16)`, `Cb' = Cb - 128`).
//!   Three-component files are YCbCr unless an Adobe marker says transform 0,
//!   or (without JFIF) the component ids are `R`, `G`, `B`.
//!
//! Checked bit-for-bit against libjpeg-turbo (via Pillow) in tests/jpeg.rs.
//!
//! Bounds: pixels, coefficient and plane memory are charged to the
//! [`Budget`] from the frame header before anything is allocated; a progressive
//! stream may have at most [`MAX_SCANS`] scans; entropy data that ends early
//! is an error, not zero-filled.

use crate::limits::{Budget, mul};
use crate::{DecodeError, Image, Limits, at, be16};

/// Most scans accepted in one file (bounds CPU spent per input byte).
pub const MAX_SCANS: usize = 128;

#[rustfmt::skip]
const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27,
    20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58,
    59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

fn zz(k: usize) -> usize {
    usize::from(ZIGZAG.get(k).copied().unwrap_or(0))
}

// ------------------------------------------------------------------ Huffman

struct Huff {
    mincode: [i32; 17],
    maxcode: [i32; 17],
    valptr: [i32; 17],
    vals: Vec<u8>,
}

impl Huff {
    fn new(counts: &[u8], vals: &[u8]) -> Result<Huff, DecodeError> {
        let mut h = Huff {
            mincode: [0; 17],
            maxcode: [-1; 17],
            valptr: [0; 17],
            vals: vals.to_vec(),
        };
        let (mut code, mut k) = (0i32, 0i32);
        for l in 1..=16usize {
            let n = i32::from(at(counts, l - 1));
            if let Some(x) = h.valptr.get_mut(l) {
                *x = k;
            }
            if let Some(x) = h.mincode.get_mut(l) {
                *x = code;
            }
            code += n;
            k += n;
            if n > 0
                && let Some(x) = h.maxcode.get_mut(l)
            {
                *x = code - 1;
            }
            if code > (1 << l) {
                return Err(DecodeError::Malformed("over-subscribed JPEG Huffman table"));
            }
            code <<= 1;
        }
        Ok(h)
    }
}

// ------------------------------------------------------------------ bit reader

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    acc: u32,
    n: u32,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Bits {
            d,
            pos,
            acc: 0,
            n: 0,
        }
    }
    fn bit(&mut self) -> Result<u32, DecodeError> {
        if self.n == 0 {
            let b = self
                .d
                .get(self.pos)
                .copied()
                .ok_or(DecodeError::Truncated)?;
            if b == 0xFF {
                match self.d.get(self.pos + 1).copied() {
                    Some(0) => self.pos += 2,
                    // A marker (or fill): no more entropy data in this segment.
                    _ => return Err(DecodeError::Truncated),
                }
            } else {
                self.pos += 1;
            }
            self.acc = u32::from(b);
            self.n = 8;
        }
        self.n -= 1;
        Ok((self.acc >> self.n) & 1)
    }
    fn receive(&mut self, n: u32) -> Result<i32, DecodeError> {
        let mut v = 0i32;
        for _ in 0..n {
            v = (v << 1) | self.bit()? as i32;
        }
        Ok(v)
    }
    fn receive_extend(&mut self, s: u32) -> Result<i32, DecodeError> {
        if s == 0 {
            return Ok(0);
        }
        let v = self.receive(s)?;
        Ok(if v < (1 << (s - 1)) {
            v - (1 << s) + 1
        } else {
            v
        })
    }
    fn decode(&mut self, h: &Huff) -> Result<u8, DecodeError> {
        let mut code = 0i32;
        for l in 1..=16usize {
            code = (code << 1) | self.bit()? as i32;
            let max = h.maxcode.get(l).copied().unwrap_or(-1);
            if max >= 0 && code <= max {
                let idx = h.valptr.get(l).copied().unwrap_or(0) + code
                    - h.mincode.get(l).copied().unwrap_or(0);
                return usize::try_from(idx)
                    .ok()
                    .and_then(|i| h.vals.get(i))
                    .copied()
                    .ok_or(DecodeError::Malformed("bad JPEG Huffman symbol"));
            }
        }
        Err(DecodeError::Malformed("invalid JPEG Huffman code"))
    }
    /// Discard partial byte; expect `FF D0+n` next.
    fn restart(&mut self, n: u8) -> Result<(), DecodeError> {
        self.n = 0;
        self.acc = 0;
        if self.d.get(self.pos) == Some(&0xFF)
            && self.d.get(self.pos + 1) == Some(&(0xD0 + (n & 7)))
        {
            self.pos += 2;
            Ok(())
        } else {
            Err(DecodeError::Malformed("missing JPEG restart marker"))
        }
    }
}

// ------------------------------------------------------------------ frame

struct Comp {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// True (unpadded) component size in samples.
    cw: usize,
    ch: usize,
    /// Padded size in blocks.
    bw: usize,
    bh: usize,
    coefs: Vec<i16>,
    quant: Option<[u16; 64]>,
}

struct Frame {
    width: usize,
    height: usize,
    progressive: bool,
    hmax: usize,
    vmax: usize,
    mcus_x: usize,
    mcus_y: usize,
    comps: Vec<Comp>,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Baseline,
    DcFirst,
    DcRefine,
    AcFirst,
    AcRefine,
}

struct ScanComp {
    ci: usize,
    td: usize,
    ta: usize,
}

struct Tables {
    dc: [Option<Huff>; 4],
    ac: [Option<Huff>; 4],
    quant: [Option<[u16; 64]>; 4],
}

fn clamp16(v: i32) -> i16 {
    v.clamp(i32::from(i16::MIN), i32::from(i16::MAX)) as i16
}

/// Decode a JPEG to RGBA8.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    let mut budget = Budget::new(limits);
    if data.get(..2) != Some(&[0xFF, 0xD8][..]) {
        return Err(DecodeError::Malformed("missing SOI"));
    }
    let mut tabs = Tables {
        dc: [None, None, None, None],
        ac: [None, None, None, None],
        quant: [None, None, None, None],
    };
    let mut frame: Option<Frame> = None;
    let mut restart_interval = 0usize;
    let mut saw_jfif = false;
    let mut adobe: Option<u8> = None;
    let mut scans = 0usize;
    let mut pos = 2usize;
    let mut done = false;

    while !done {
        // Find the next marker: skip to FF, then past any fill FFs.
        while data.get(pos) != Some(&0xFF) {
            if pos >= data.len() {
                return Err(DecodeError::Truncated);
            }
            pos += 1;
        }
        while data.get(pos) == Some(&0xFF) {
            pos += 1;
        }
        let marker = data.get(pos).copied().ok_or(DecodeError::Truncated)?;
        pos += 1;
        match marker {
            0x00 | 0x01 | 0xD0..=0xD7 => continue,
            0xD8 => return Err(DecodeError::Malformed("nested SOI")),
            0xD9 => {
                done = true;
                continue;
            }
            _ => {}
        }
        let len = usize::from(be16(data, pos));
        if len < 2 {
            return Err(DecodeError::Malformed("bad segment length"));
        }
        let seg = data.get(pos + 2..pos + len).ok_or(DecodeError::Truncated)?;
        pos += len;
        match marker {
            0xC0..=0xC2 => {
                if frame.is_some() {
                    return Err(DecodeError::Unsupported("multiple frames"));
                }
                frame = Some(parse_sof(seg, marker == 0xC2, &mut budget)?);
            }
            0xC3 => return Err(DecodeError::Unsupported("lossless JPEG")),
            0xC5..=0xC7 | 0xCD..=0xCF => {
                return Err(DecodeError::Unsupported("hierarchical JPEG"));
            }
            0xC9..=0xCB => return Err(DecodeError::Unsupported("arithmetic-coded JPEG")),
            0xCC => return Err(DecodeError::Unsupported("arithmetic conditioning")),
            0xC4 => parse_dht(seg, &mut tabs)?,
            0xDB => parse_dqt(seg, &mut tabs)?,
            0xDD => {
                if seg.len() != 2 {
                    return Err(DecodeError::Malformed("bad DRI"));
                }
                restart_interval = usize::from(be16(seg, 0));
            }
            0xE0 => {
                if seg.starts_with(b"JFIF\0") {
                    saw_jfif = true;
                }
            }
            0xEE => {
                if seg.len() >= 12 && seg.starts_with(b"Adobe") {
                    adobe = Some(at(seg, 11));
                }
            }
            0xDA => {
                let f = frame
                    .as_mut()
                    .ok_or(DecodeError::Malformed("SOS before SOF"))?;
                scans += 1;
                if scans > MAX_SCANS {
                    return Err(DecodeError::LimitExceeded("scans"));
                }
                pos = decode_scan(f, &tabs, seg, data, pos, restart_interval)?;
            }
            0xDC => return Err(DecodeError::Unsupported("DNL")),
            _ => {} // APPn, COM, others: skipped
        }
    }
    let mut f = frame.ok_or(DecodeError::Malformed("no frame"))?;
    if scans == 0 {
        return Err(DecodeError::Malformed("no scan"));
    }
    finish(&mut f, saw_jfif, adobe, &mut budget)
}

fn parse_sof(seg: &[u8], progressive: bool, budget: &mut Budget) -> Result<Frame, DecodeError> {
    if seg.len() < 6 {
        return Err(DecodeError::Truncated);
    }
    if at(seg, 0) != 8 {
        return Err(DecodeError::Unsupported("JPEG precision other than 8 bits"));
    }
    let height = usize::from(be16(seg, 1));
    let width = usize::from(be16(seg, 3));
    let n = usize::from(at(seg, 5));
    budget.check_dims(width as u64, height as u64)?;
    match n {
        1 | 3 => {}
        4 => return Err(DecodeError::Unsupported("CMYK/YCCK JPEG")),
        _ => return Err(DecodeError::Unsupported("JPEG component count")),
    }
    if seg.len() != 6 + 3 * n {
        return Err(DecodeError::Malformed("bad SOF length"));
    }
    let mut raw = Vec::new();
    for i in 0..n {
        let b = 6 + 3 * i;
        let (hv, tq) = (at(seg, b + 1), at(seg, b + 2));
        let (h, v) = (usize::from(hv >> 4), usize::from(hv & 15));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 {
            return Err(DecodeError::Malformed("bad component sampling"));
        }
        let id = at(seg, b);
        if raw.iter().any(|&(i2, _, _, _)| i2 == id) {
            return Err(DecodeError::Malformed("duplicate component id"));
        }
        raw.push((id, h, v, usize::from(tq)));
    }
    let hmax = raw.iter().map(|c| c.1).max().unwrap_or(1);
    let vmax = raw.iter().map(|c| c.2).max().unwrap_or(1);
    if raw.iter().any(|c| hmax % c.1 != 0 || vmax % c.2 != 0) {
        return Err(DecodeError::Unsupported("fractional chroma sampling"));
    }
    let mcus_x = width.div_ceil(8 * hmax);
    let mcus_y = height.div_ceil(8 * vmax);
    let mut comps = Vec::new();
    for (id, h, v, tq) in raw {
        let (bw, bh) = (mul(mcus_x, h)?, mul(mcus_y, v)?);
        let coefs = budget.zeroed_of::<i16>(mul(mul(bw, bh)?, 64)?)?;
        comps.push(Comp {
            id,
            h,
            v,
            tq,
            cw: (width * h).div_ceil(hmax),
            ch: (height * v).div_ceil(vmax),
            bw,
            bh,
            coefs,
            quant: None,
        });
    }
    Ok(Frame {
        width,
        height,
        progressive,
        hmax,
        vmax,
        mcus_x,
        mcus_y,
        comps,
    })
}

fn parse_dht(seg: &[u8], tabs: &mut Tables) -> Result<(), DecodeError> {
    let mut p = 0;
    while p < seg.len() {
        let tc_th = at(seg, p);
        let counts = seg.get(p + 1..p + 17).ok_or(DecodeError::Truncated)?;
        let total: usize = counts.iter().map(|&c| usize::from(c)).sum();
        if total > 256 {
            return Err(DecodeError::Malformed("too many Huffman symbols"));
        }
        let vals = seg
            .get(p + 17..p + 17 + total)
            .ok_or(DecodeError::Truncated)?;
        p += 17 + total;
        let th = usize::from(tc_th & 15);
        if th > 3 || tc_th >> 4 > 1 {
            return Err(DecodeError::Malformed("bad Huffman table id"));
        }
        let h = Huff::new(counts, vals)?;
        let slot = if tc_th >> 4 == 0 {
            tabs.dc.get_mut(th)
        } else {
            tabs.ac.get_mut(th)
        };
        if let Some(s) = slot {
            *s = Some(h);
        }
    }
    Ok(())
}

fn parse_dqt(seg: &[u8], tabs: &mut Tables) -> Result<(), DecodeError> {
    let mut p = 0;
    while p < seg.len() {
        let pq_tq = at(seg, p);
        p += 1;
        let (wide, tq) = (pq_tq >> 4, usize::from(pq_tq & 15));
        if wide > 1 || tq > 3 {
            return Err(DecodeError::Malformed("bad DQT"));
        }
        let mut q = [0u16; 64];
        for k in 0..64 {
            let v = if wide == 1 {
                let v = be16(seg, p);
                p += 2;
                v
            } else {
                let v = u16::from(at(seg, p));
                p += 1;
                v
            };
            if p > seg.len() {
                return Err(DecodeError::Truncated);
            }
            if let Some(slot) = q.get_mut(zz(k)) {
                *slot = v;
            }
        }
        if let Some(s) = tabs.quant.get_mut(tq) {
            *s = Some(q);
        }
    }
    Ok(())
}

fn decode_scan(
    f: &mut Frame,
    tabs: &Tables,
    seg: &[u8],
    data: &[u8],
    start: usize,
    restart_interval: usize,
) -> Result<usize, DecodeError> {
    let ns = usize::from(at(seg, 0));
    if !(1..=4).contains(&ns) || seg.len() != 1 + 2 * ns + 3 {
        return Err(DecodeError::Malformed("bad SOS"));
    }
    let mut sc: Vec<ScanComp> = Vec::new();
    for i in 0..ns {
        let (cid, t) = (at(seg, 1 + 2 * i), at(seg, 2 + 2 * i));
        let ci = f
            .comps
            .iter()
            .position(|c| c.id == cid)
            .ok_or(DecodeError::Malformed("scan references unknown component"))?;
        if sc.iter().any(|s| s.ci == ci) {
            return Err(DecodeError::Malformed("component repeated in scan"));
        }
        let (td, ta) = (usize::from(t >> 4), usize::from(t & 15));
        if td > 3 || ta > 3 {
            return Err(DecodeError::Malformed("bad Huffman selector"));
        }
        sc.push(ScanComp { ci, td, ta });
    }
    let (ss, se, ahal) = (
        usize::from(at(seg, 1 + 2 * ns)),
        usize::from(at(seg, 2 + 2 * ns)),
        at(seg, 3 + 2 * ns),
    );
    let (ah, al) = (u32::from(ahal >> 4), u32::from(ahal & 15));
    let mode = if !f.progressive {
        Mode::Baseline
    } else if ss == 0 {
        if se != 0 {
            return Err(DecodeError::Malformed("progressive DC scan with Se != 0"));
        }
        if ah == 0 {
            Mode::DcFirst
        } else {
            Mode::DcRefine
        }
    } else {
        if se < ss || se > 63 || ns != 1 {
            return Err(DecodeError::Malformed("bad progressive AC scan"));
        }
        if ah == 0 {
            Mode::AcFirst
        } else {
            Mode::AcRefine
        }
    };
    if f.progressive && (al > 13 || (ah != 0 && ah != al + 1)) {
        return Err(DecodeError::Malformed("bad successive approximation"));
    }
    if ns > 1 {
        let blocks: usize = sc
            .iter()
            .map(|s| f.comps.get(s.ci).map_or(0, |c| c.h * c.v))
            .sum();
        if blocks > 10 {
            return Err(DecodeError::Malformed("too many blocks per MCU"));
        }
    }
    // Latch quantisation tables on a component's first scan.
    for s in &sc {
        let comp = f
            .comps
            .get_mut(s.ci)
            .ok_or(DecodeError::Malformed("component"))?;
        if comp.quant.is_none() {
            let q = tabs
                .quant
                .get(comp.tq)
                .and_then(|q| q.as_ref())
                .ok_or(DecodeError::Malformed("missing quantisation table"))?;
            comp.quant = Some(*q);
        }
    }
    // Validate the Huffman tables the mode needs.
    let need_dc = matches!(mode, Mode::Baseline | Mode::DcFirst);
    let need_ac = matches!(mode, Mode::Baseline | Mode::AcFirst | Mode::AcRefine);
    for s in &sc {
        if need_dc && tabs.dc.get(s.td).is_none_or(|t| t.is_none()) {
            return Err(DecodeError::Malformed("missing DC Huffman table"));
        }
        if need_ac && tabs.ac.get(s.ta).is_none_or(|t| t.is_none()) {
            return Err(DecodeError::Malformed("missing AC Huffman table"));
        }
    }

    let mut bits = Bits::new(data, start);
    let mut preds = vec![0i32; sc.len()];
    let mut eobrun = 0u32;
    let single = sc.len() == 1;
    let (mcus_x, mcus_y) = (f.mcus_x, f.mcus_y);
    let (per_row, total) = if single {
        let c = f
            .comps
            .get(sc.first().map_or(0, |s| s.ci))
            .ok_or(DecodeError::Malformed("component"))?;
        let bwr = c.cw.div_ceil(8);
        (bwr, mul(bwr, c.ch.div_ceil(8))?)
    } else {
        (mcus_x, mul(mcus_x, mcus_y)?)
    };
    let mut next_rst = 0u8;
    let p = ScanParams { mode, ss, se, al };
    for mcu in 0..total {
        if restart_interval > 0 && mcu > 0 && mcu % restart_interval == 0 {
            bits.restart(next_rst)?;
            next_rst = next_rst.wrapping_add(1) & 7;
            preds.iter_mut().for_each(|x| *x = 0);
            eobrun = 0;
        }
        let (mx, my) = (mcu % per_row, mcu / per_row);
        for (si, s) in sc.iter().enumerate() {
            let (cw_h, cw_v) = if single {
                (1, 1)
            } else {
                f.comps.get(s.ci).map_or((1, 1), |c| (c.h, c.v))
            };
            let comp = f
                .comps
                .get_mut(s.ci)
                .ok_or(DecodeError::Malformed("component"))?;
            for vy in 0..cw_v {
                for hx in 0..cw_h {
                    let (bx, by) = (mx * cw_h + hx, my * cw_v + vy);
                    let off = (by * comp.bw + bx) * 64;
                    let block = comp
                        .coefs
                        .get_mut(off..off + 64)
                        .ok_or(DecodeError::Malformed("block outside component"))?;
                    let dc = tabs.dc.get(s.td).and_then(|t| t.as_ref());
                    let ac = tabs.ac.get(s.ta).and_then(|t| t.as_ref());
                    let pred = preds.get_mut(si).ok_or(DecodeError::Malformed("pred"))?;
                    decode_block(&mut bits, &p, block, dc, ac, pred, &mut eobrun)?;
                }
            }
        }
    }
    // Skip to the next marker.
    let mut q = bits.pos;
    loop {
        match (data.get(q), data.get(q + 1)) {
            (Some(&0xFF), Some(&m)) if m != 0 && !(0xD0..=0xD7).contains(&m) && m != 0xFF => {
                return Ok(q);
            }
            (Some(_), Some(_)) => q += 1,
            _ => return Err(DecodeError::Truncated),
        }
    }
}

struct ScanParams {
    mode: Mode,
    ss: usize,
    se: usize,
    al: u32,
}

fn decode_block(
    b: &mut Bits,
    p: &ScanParams,
    block: &mut [i16],
    dc: Option<&Huff>,
    ac: Option<&Huff>,
    pred: &mut i32,
    eobrun: &mut u32,
) -> Result<(), DecodeError> {
    let missing = DecodeError::Malformed("missing Huffman table");
    match p.mode {
        Mode::Baseline | Mode::DcFirst => {
            let t = u32::from(b.decode(dc.ok_or(missing.clone())?)?);
            if t > 15 {
                return Err(DecodeError::Malformed("bad DC magnitude"));
            }
            let diff = b.receive_extend(t)?;
            *pred = pred.wrapping_add(diff);
            let v = if p.mode == Mode::DcFirst {
                pred.wrapping_shl(p.al)
            } else {
                *pred
            };
            if let Some(x) = block.get_mut(0) {
                *x = clamp16(v);
            }
            if p.mode == Mode::Baseline {
                let ac = ac.ok_or(missing)?;
                let mut k = 1usize;
                while k < 64 {
                    let rs = b.decode(ac)?;
                    let (r, s) = (usize::from(rs >> 4), u32::from(rs & 15));
                    if s == 0 {
                        if r == 15 {
                            k += 16;
                            continue;
                        }
                        break;
                    }
                    k += r;
                    if k > 63 {
                        return Err(DecodeError::Malformed("AC coefficient index overrun"));
                    }
                    let v = b.receive_extend(s)?;
                    if let Some(x) = block.get_mut(zz(k)) {
                        *x = clamp16(v);
                    }
                    k += 1;
                }
            }
        }
        Mode::DcRefine => {
            if b.bit()? == 1
                && let Some(x) = block.get_mut(0)
            {
                *x |= 1i16.wrapping_shl(p.al);
            }
        }
        Mode::AcFirst => {
            if *eobrun > 0 {
                *eobrun -= 1;
                return Ok(());
            }
            let ac = ac.ok_or(missing)?;
            let mut k = p.ss;
            while k <= p.se {
                let rs = b.decode(ac)?;
                let (r, s) = (u32::from(rs >> 4), u32::from(rs & 15));
                if s == 0 {
                    if r < 15 {
                        *eobrun = (1u32 << r) - 1;
                        if r > 0 {
                            *eobrun += b.receive(r)? as u32;
                        }
                        break;
                    }
                    k += 16;
                    continue;
                }
                k += r as usize;
                if k > p.se {
                    return Err(DecodeError::Malformed("AC coefficient index overrun"));
                }
                let v = b.receive_extend(s)?.wrapping_shl(p.al);
                if let Some(x) = block.get_mut(zz(k)) {
                    *x = clamp16(v);
                }
                k += 1;
            }
        }
        Mode::AcRefine => {
            let ac = ac.ok_or(missing)?;
            let p1 = 1i32 << p.al;
            let m1 = -1i32 << p.al;
            let mut k = p.ss;
            if *eobrun == 0 {
                while k <= p.se {
                    let rs = b.decode(ac)?;
                    let (mut r, s) = (i32::from(rs >> 4), u32::from(rs & 15));
                    let mut value = 0i32;
                    if s != 0 {
                        if s != 1 {
                            return Err(DecodeError::Malformed("bad refinement magnitude"));
                        }
                        value = if b.bit()? == 1 { p1 } else { m1 };
                    } else if r != 15 {
                        *eobrun = 1u32 << r;
                        if r > 0 {
                            *eobrun += b.receive(r as u32)? as u32;
                        }
                        break;
                    }
                    // Advance over nonzero coefficients (refining them) and r zeros.
                    while k <= p.se {
                        let idx = zz(k);
                        let cur = i32::from(block.get(idx).copied().unwrap_or(0));
                        if cur != 0 {
                            if b.bit()? == 1 && cur & p1 == 0 {
                                let nv = if cur >= 0 { cur + p1 } else { cur + m1 };
                                if let Some(x) = block.get_mut(idx) {
                                    *x = clamp16(nv);
                                }
                            }
                        } else {
                            r -= 1;
                            if r < 0 {
                                break;
                            }
                        }
                        k += 1;
                    }
                    if value != 0 {
                        if k > p.se {
                            return Err(DecodeError::Malformed("refinement index overrun"));
                        }
                        if let Some(x) = block.get_mut(zz(k)) {
                            *x = clamp16(value);
                        }
                    }
                    k += 1;
                }
            }
            if *eobrun > 0 {
                while k <= p.se {
                    let idx = zz(k);
                    let cur = i32::from(block.get(idx).copied().unwrap_or(0));
                    if cur != 0 && b.bit()? == 1 && cur & p1 == 0 {
                        let nv = if cur >= 0 { cur + p1 } else { cur + m1 };
                        if let Some(x) = block.get_mut(idx) {
                            *x = clamp16(nv);
                        }
                    }
                    k += 1;
                }
                *eobrun -= 1;
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ IDCT

const CONST_BITS: u32 = 13;
const PASS1_BITS: u32 = 2;
const F_0_298: i64 = 2446;
const F_0_390: i64 = 3196;
const F_0_541: i64 = 4433;
const F_0_765: i64 = 6270;
const F_0_899: i64 = 7373;
const F_1_175: i64 = 9633;
const F_1_501: i64 = 12299;
const F_1_847: i64 = 15137;
const F_1_961: i64 = 16069;
const F_2_053: i64 = 16819;
const F_2_562: i64 = 20995;
const F_3_072: i64 = 25172;

fn descale(x: i64, n: u32) -> i64 {
    (x + (1i64 << (n - 1))) >> n
}

/// One 1-D islow butterfly over eight inputs; returns the eight outputs
/// (before descaling) in order 0..7.
fn idct_1d(i: [i64; 8]) -> [i64; 8] {
    let z1 = (i[2] + i[6]) * F_0_541;
    let tmp2 = z1 + i[6] * -F_1_847;
    let tmp3 = z1 + i[2] * F_0_765;
    let tmp0 = (i[0] + i[4]) << CONST_BITS;
    let tmp1 = (i[0] - i[4]) << CONST_BITS;
    let (t10, t13) = (tmp0 + tmp3, tmp0 - tmp3);
    let (t11, t12) = (tmp1 + tmp2, tmp1 - tmp2);

    let (mut o0, mut o1, mut o2, mut o3) = (i[7], i[5], i[3], i[1]);
    let z1 = o0 + o3;
    let z2 = o1 + o2;
    let z3 = o0 + o2;
    let z4 = o1 + o3;
    let z5 = (z3 + z4) * F_1_175;
    o0 *= F_0_298;
    o1 *= F_2_053;
    o2 *= F_3_072;
    o3 *= F_1_501;
    let z1 = z1 * -F_0_899;
    let z2 = z2 * -F_2_562;
    let z3 = z3 * -F_1_961 + z5;
    let z4 = z4 * -F_0_390 + z5;
    o0 += z1 + z3;
    o1 += z2 + z4;
    o2 += z2 + z3;
    o3 += z1 + z4;
    [
        t10 + o3,
        t11 + o2,
        t12 + o1,
        t13 + o0,
        t13 - o0,
        t12 - o1,
        t11 - o2,
        t10 - o3,
    ]
}

/// Dequantise and inverse-transform one block (natural order coefficients) to
/// level-shifted, clamped samples.
fn idct_block(coefs: &[i16], q: &[u16; 64], out: &mut [u8; 64]) {
    let mut ws = [0i64; 64];
    for col in 0..8 {
        let g = |row: usize| -> i64 {
            let k = row * 8 + col;
            i64::from(coefs.get(k).copied().unwrap_or(0))
                * i64::from(q.get(k).copied().unwrap_or(0))
        };
        let o = idct_1d([g(0), g(1), g(2), g(3), g(4), g(5), g(6), g(7)]);
        for (row, v) in o.iter().enumerate() {
            if let Some(w) = ws.get_mut(row * 8 + col) {
                *w = descale(*v, CONST_BITS - PASS1_BITS);
            }
        }
    }
    for row in 0..8 {
        let g = |c: usize| ws.get(row * 8 + c).copied().unwrap_or(0);
        let o = idct_1d([g(0), g(1), g(2), g(3), g(4), g(5), g(6), g(7)]);
        for (c, v) in o.iter().enumerate() {
            let s = descale(*v, CONST_BITS + PASS1_BITS + 3) + 128;
            if let Some(px) = out.get_mut(row * 8 + c) {
                *px = s.clamp(0, 255) as u8;
            }
        }
    }
}

// ------------------------------------------------------------------ output

struct Plane {
    data: Vec<u8>,
    stride: usize,
    cw: usize,
    ch: usize,
    hs: usize,
    vs: usize,
}

impl Plane {
    fn row(&self, y: usize) -> &[u8] {
        let y = y.min(self.ch.saturating_sub(1));
        self.data
            .get(y * self.stride..)
            .and_then(|s| s.get(..self.stride))
            .unwrap_or(&[])
    }

    /// Fill `out` (cleared first) with the `w` full-resolution samples of row `y`.
    fn upsampled_row(&self, y: usize, w: usize, out: &mut Vec<u8>) {
        out.clear();
        match (self.hs, self.vs) {
            (1, 1) => out.extend(self.row(y).iter().take(w)),
            (2, 1) if self.cw > 2 => {
                let a = self.row(y);
                for x in 0..w {
                    let i = x / 2;
                    let c = u32::from(at(a, i));
                    let v = if x % 2 == 0 {
                        if i == 0 {
                            c
                        } else {
                            (3 * c + u32::from(at(a, i - 1)) + 1) >> 2
                        }
                    } else if i + 1 >= self.cw {
                        c
                    } else {
                        (3 * c + u32::from(at(a, i + 1)) + 2) >> 2
                    };
                    out.push(v as u8);
                }
            }
            (1, 2) => {
                let (inrow, v) = (y / 2, y % 2);
                let near = self.row(inrow);
                let far = if v == 0 {
                    self.row(inrow.saturating_sub(1))
                } else {
                    self.row(inrow + 1)
                };
                let bias = if v == 0 { 1 } else { 2 };
                for x in 0..w {
                    let s = 3 * u32::from(at(near, x)) + u32::from(at(far, x)) + bias;
                    out.push((s >> 2) as u8);
                }
            }
            (2, 2) if self.cw > 2 => {
                let (inrow, v) = (y / 2, y % 2);
                let near = self.row(inrow);
                let far = if v == 0 {
                    self.row(inrow.saturating_sub(1))
                } else {
                    self.row(inrow + 1)
                };
                let col = |i: usize| 3 * u32::from(at(near, i)) + u32::from(at(far, i));
                for x in 0..w {
                    let i = x / 2;
                    let this = col(i);
                    let s = if x % 2 == 0 {
                        if i == 0 {
                            (this * 4 + 8) >> 4
                        } else {
                            (this * 3 + col(i - 1) + 8) >> 4
                        }
                    } else if i + 1 >= self.cw {
                        (this * 4 + 7) >> 4
                    } else {
                        (this * 3 + col(i + 1) + 7) >> 4
                    };
                    out.push(s as u8);
                }
            }
            (hs, vs) => {
                let src = self.row(y / vs);
                for x in 0..w {
                    out.push(at(src, x / hs));
                }
            }
        }
    }
}

fn finish(
    f: &mut Frame,
    saw_jfif: bool,
    adobe: Option<u8>,
    budget: &mut Budget,
) -> Result<Image, DecodeError> {
    // IDCT every component into a plane, releasing its coefficients.
    let mut planes: Vec<Plane> = Vec::new();
    for c in &mut f.comps {
        let q = c
            .quant
            .ok_or(DecodeError::Malformed("component never scanned"))?;
        let stride = c.bw * 8;
        let mut data = budget.zeroed(mul(stride, c.bh * 8)?)?;
        let mut blk = [0u8; 64];
        for by in 0..c.bh {
            for bx in 0..c.bw {
                let off = (by * c.bw + bx) * 64;
                if let Some(co) = c.coefs.get(off..off + 64) {
                    idct_block(co, &q, &mut blk);
                }
                for (r, row) in blk.as_chunks::<8>().0.iter().enumerate() {
                    let o = (by * 8 + r) * stride + bx * 8;
                    if let Some(d) = data.get_mut(o..o + 8) {
                        d.copy_from_slice(row);
                    }
                }
            }
        }
        budget.release(c.coefs.len() * 2);
        c.coefs = Vec::new();
        planes.push(Plane {
            data,
            stride,
            cw: c.cw,
            ch: c.ch,
            hs: f.hmax / c.h,
            vs: f.vmax / c.v,
        });
    }
    let (w, h) = (f.width, f.height);
    let mut rgba = budget.with_capacity(mul(mul(w, h)?, 4)?)?;
    let ycc = match (f.comps.len(), adobe) {
        (3, _) if saw_jfif => true,
        (3, Some(t)) => t != 0,
        (3, None) => {
            !(f.comps.first().map(|c| c.id) == Some(b'R')
                && f.comps.get(1).map(|c| c.id) == Some(b'G')
                && f.comps.get(2).map(|c| c.id) == Some(b'B'))
        }
        _ => false,
    };
    let mut rows: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    for y in 0..h {
        for (p, r) in planes.iter().zip(rows.iter_mut()) {
            p.upsampled_row(y, w, r);
        }
        let [r0, r1, r2] = &rows;
        if planes.len() == 1 {
            for x in 0..w {
                let g = at(r0, x);
                rgba.extend_from_slice(&[g, g, g, 255]);
            }
        } else {
            for x in 0..w {
                let (a, b, c) = (at(r0, x), at(r1, x), at(r2, x));
                let px = if ycc { ycc_to_rgb(a, b, c) } else { [a, b, c] };
                rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }
        }
    }
    Ok(Image {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

/// libjpeg's fixed-point YCbCr to RGB (16 fractional bits).
fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> [u8; 3] {
    let (y, cb, cr) = (i32::from(y), i32::from(cb) - 128, i32::from(cr) - 128);
    let r = y + ((91881 * cr + 32768) >> 16);
    let b = y + ((116130 * cb + 32768) >> 16);
    let g = y + ((-22554 * cb - 46802 * cr + 32768) >> 16);
    [
        r.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        b.clamp(0, 255) as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_block_is_flat() {
        // DC 80 with quant 1: ws = 80<<13 >> 11 = 320; out = (320 + ... ) -> 80/8 = 10 + 128.
        let mut c = [0i16; 64];
        c[0] = 80;
        let q = [1u16; 64];
        let mut out = [0u8; 64];
        idct_block(&c, &q, &mut out);
        assert!(out.iter().all(|&v| v == 138), "{out:?}");
    }

    #[test]
    fn dc_clamps() {
        let mut c = [0i16; 64];
        c[0] = 2000;
        let q = [1u16; 64];
        let mut out = [0u8; 64];
        idct_block(&c, &q, &mut out);
        assert!(out.iter().all(|&v| v == 255));
        c[0] = -2000;
        idct_block(&c, &q, &mut out);
        assert!(out.iter().all(|&v| v == 0));
    }

    #[test]
    fn colour_conversion_known_values() {
        // Neutral chroma leaves grey untouched.
        assert_eq!(ycc_to_rgb(77, 128, 128), [77, 77, 77]);
        // Cr' = 127: R = 255 + (91881*127+32768 >> 16) = clamp; hand check G:
        // G = 0 + ((-46802*127 + 32768) >> 16) = (-5943854+32768)>>16 = -91 -> 0.
        // R = (91881*127 + 32768) >> 16 = 178, B = 0.
        assert_eq!(ycc_to_rgb(0, 128, 255), [178, 0, 0]);
    }

    #[test]
    fn huffman_rejects_oversubscribed() {
        let mut counts = [0u8; 16];
        counts[0] = 3; // three 1-bit codes
        assert!(Huff::new(&counts, &[0, 1, 2]).is_err());
        counts[0] = 2;
        assert!(Huff::new(&counts, &[0, 1]).is_ok());
    }
}
