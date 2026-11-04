//! PNG and APNG decoder.
//!
//! Supported: every colour type at every legal bit depth (1/2/4/8/16), Adam7
//! interlacing, `PLTE`, `tRNS` (palette, grey and RGB colour key), APNG
//! (`acTL`, `fcTL`, `fdAT`, dispose ops none/background/previous, blend ops
//! source/over). Ancillary chunks (`gAMA`, `cHRM`, `sRGB`, `iCCP`, text, ...)
//! are CRC-checked and ignored: no colour management is applied.
//!
//! Choices, all deterministic:
//! * 16-bit samples become 8-bit by taking the high byte.
//! * Every chunk CRC and the zlib Adler-32 are verified; a mismatch is an error.
//! * A palette index beyond the palette is an error.
//! * APNG compositing is in straight alpha with integer arithmetic (see
//!   [`blend_over`]). A file with `acTL` and one frame is returned as a still.
//!   If the first `fcTL` comes after the first `IDAT`, the default image is not
//!   part of the animation and is skipped (as the APNG specification says).
//!
//! Every size is derived from the header and checked against [`Limits`] before
//! any buffer is created; inflate never produces a byte beyond the computed
//! size, so a few bytes claiming a huge image cost nothing.

use crate::inflate::{crc32, zlib_decompress};
use crate::limits::{Budget, mul};
use crate::{Animation, DecodeError, Decoded, Frame, Image, Limits, at, be32, put};

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

#[derive(Clone, Copy)]
struct Header {
    width: u32,
    height: u32,
    depth: u8,
    colour: u8,
    interlace: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.colour {
            0 | 3 => 1,
            4 => 2,
            2 => 3,
            _ => 4,
        }
    }
    fn bits_per_pixel(&self) -> usize {
        self.channels() * usize::from(self.depth)
    }
}

#[derive(Clone, Copy, Default)]
struct FrameCtl {
    width: u32,
    height: u32,
    x: u32,
    y: u32,
    delay_ms: u32,
    dispose: u8,
    blend: u8,
}

struct FrameData<'a> {
    ctl: FrameCtl,
    chunks: Vec<&'a [u8]>,
}

/// Adam7: (x start, y start, x step, y step).
const ADAM7: [(usize, usize, usize, usize); 7] = [
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
];
const SINGLE: [(usize, usize, usize, usize); 1] = [(0, 0, 1, 1)];

fn pass_dim(total: usize, start: usize, step: usize) -> usize {
    if total <= start {
        0
    } else {
        (total - start).div_ceil(step)
    }
}

/// Decode a PNG or APNG.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded, DecodeError> {
    if data.len() < 8 {
        return Err(DecodeError::Truncated);
    }
    if data.get(..8) != Some(&SIGNATURE[..]) {
        return Err(DecodeError::Malformed("bad signature"));
    }
    let mut budget = Budget::new(limits);

    let mut pos = 8usize;
    let mut hdr: Option<Header> = None;
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<&[u8]> = Vec::new();
    let mut seen_idat = false;
    let mut idat_ended = false;
    let mut seen_end = false;
    // APNG
    let mut actl: Option<(u32, u32)> = None;
    let mut next_seq: u32 = 0;
    let mut pending_ctl: Option<FrameCtl> = None;
    let mut default_ctl: Option<FrameCtl> = None;
    let mut frames: Vec<FrameData> = Vec::new();

    while pos < data.len() {
        let head = data.get(pos..pos + 8).ok_or(DecodeError::Truncated)?;
        let len = be32(head, 0) as usize;
        let kind = [at(head, 4), at(head, 5), at(head, 6), at(head, 7)];
        let body_start = pos + 8;
        let body_end = body_start.checked_add(len).ok_or(DecodeError::Truncated)?;
        let end = body_end.checked_add(4).ok_or(DecodeError::Truncated)?;
        if end > data.len() {
            return Err(DecodeError::Truncated);
        }
        let body = data
            .get(body_start..body_end)
            .ok_or(DecodeError::Truncated)?;
        let crc_covered = data.get(pos + 4..body_end).ok_or(DecodeError::Truncated)?;
        if crc32(crc_covered) != be32(data, body_end) {
            return Err(DecodeError::Malformed("chunk CRC mismatch"));
        }
        pos = end;
        if hdr.is_none() && &kind != b"IHDR" {
            return Err(DecodeError::Malformed("IHDR must be first"));
        }
        match &kind {
            b"IHDR" => {
                if hdr.is_some() || body.len() != 13 {
                    return Err(DecodeError::Malformed("bad IHDR"));
                }
                let (w, h) = (be32(body, 0), be32(body, 4));
                budget.check_dims(u64::from(w), u64::from(h))?;
                let (depth, colour) = (at(body, 8), at(body, 9));
                let ok = match colour {
                    0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
                    3 => matches!(depth, 1 | 2 | 4 | 8),
                    2 | 4 | 6 => matches!(depth, 8 | 16),
                    _ => return Err(DecodeError::Malformed("bad colour type")),
                };
                if !ok {
                    return Err(DecodeError::Malformed("bad bit depth for colour type"));
                }
                if at(body, 10) != 0 || at(body, 11) != 0 {
                    return Err(DecodeError::Malformed("bad compression or filter method"));
                }
                let interlace = match at(body, 12) {
                    0 => false,
                    1 => true,
                    _ => return Err(DecodeError::Malformed("bad interlace method")),
                };
                hdr = Some(Header {
                    width: w,
                    height: h,
                    depth,
                    colour,
                    interlace,
                });
            }
            b"PLTE" => {
                if body.is_empty()
                    || !body.len().is_multiple_of(3)
                    || body.len() / 3 > 256
                    || seen_idat
                {
                    return Err(DecodeError::Malformed("bad PLTE"));
                }
                palette = body.as_chunks::<3>().0.to_vec();
            }
            b"tRNS" => {
                if body.len() > 256 || seen_idat {
                    return Err(DecodeError::Malformed("bad tRNS"));
                }
                trns = body.to_vec();
            }
            b"acTL" => {
                if seen_idat || actl.is_some() || body.len() != 8 {
                    return Err(DecodeError::Malformed("bad acTL"));
                }
                let (n, plays) = (be32(body, 0), be32(body, 4));
                if n == 0 {
                    return Err(DecodeError::Malformed("acTL with zero frames"));
                }
                budget.check_frame_count(usize::try_from(n - 1).unwrap_or(usize::MAX))?;
                actl = Some((n, plays));
            }
            b"fcTL" => {
                if actl.is_none() || body.len() != 26 {
                    return Err(DecodeError::Malformed("bad fcTL"));
                }
                if be32(body, 0) != next_seq {
                    return Err(DecodeError::Malformed("APNG sequence number"));
                }
                next_seq = next_seq.wrapping_add(1);
                let den = u64::from(crate::be16(body, 22));
                let num = u64::from(crate::be16(body, 20));
                let den = if den == 0 { 100 } else { den };
                let ctl = FrameCtl {
                    width: be32(body, 4),
                    height: be32(body, 8),
                    x: be32(body, 12),
                    y: be32(body, 16),
                    delay_ms: u32::try_from(num * 1000 / den).unwrap_or(u32::MAX),
                    dispose: at(body, 24),
                    blend: at(body, 25),
                };
                if ctl.dispose > 2 || ctl.blend > 1 {
                    return Err(DecodeError::Malformed("bad fcTL ops"));
                }
                let h = hdr.ok_or(DecodeError::Malformed("IHDR must be first"))?;
                let fits = u64::from(ctl.x) + u64::from(ctl.width) <= u64::from(h.width)
                    && u64::from(ctl.y) + u64::from(ctl.height) <= u64::from(h.height);
                if ctl.width == 0 || ctl.height == 0 || !fits {
                    return Err(DecodeError::Malformed("APNG frame outside canvas"));
                }
                if pending_ctl.is_some() {
                    return Err(DecodeError::Malformed("fcTL without frame data"));
                }
                pending_ctl = Some(ctl);
            }
            b"fdAT" => {
                if body.len() < 4 || actl.is_none() {
                    return Err(DecodeError::Malformed("bad fdAT"));
                }
                if be32(body, 0) != next_seq {
                    return Err(DecodeError::Malformed("APNG sequence number"));
                }
                next_seq = next_seq.wrapping_add(1);
                let payload = body.get(4..).unwrap_or(&[]);
                match (pending_ctl.take(), frames.last_mut()) {
                    (Some(ctl), _) => {
                        budget.check_frame_count(frames.len())?;
                        frames.push(FrameData {
                            ctl,
                            chunks: vec![payload],
                        });
                    }
                    (None, Some(f)) => f.chunks.push(payload),
                    (None, None) => return Err(DecodeError::Malformed("fdAT before fcTL")),
                }
            }
            b"IDAT" => {
                if idat_ended {
                    return Err(DecodeError::Malformed("IDAT chunks not consecutive"));
                }
                if !seen_idat {
                    // An fcTL before the first IDAT makes the default image frame 0.
                    default_ctl = pending_ctl.take();
                }
                seen_idat = true;
                idat.push(body);
            }
            b"IEND" => {
                seen_end = true;
                break;
            }
            _ => {
                if at(&kind, 0) & 0x20 == 0 {
                    return Err(DecodeError::Unsupported("unknown critical chunk"));
                }
            }
        }
        if seen_idat && &kind != b"IDAT" {
            idat_ended = true;
        }
    }
    if !seen_end {
        return Err(DecodeError::Truncated);
    }
    let hdr = hdr.ok_or(DecodeError::Truncated)?;
    if !seen_idat {
        return Err(DecodeError::Malformed("no IDAT"));
    }
    if pending_ctl.is_some() {
        return Err(DecodeError::Malformed("fcTL without frame data"));
    }
    if hdr.colour == 3 && palette.is_empty() {
        return Err(DecodeError::Malformed("palette image without PLTE"));
    }
    let ctx = Ctx {
        hdr,
        palette: &palette,
        trns: &trns,
    };

    // Single image (no animation, or an animation of one frame).
    let (num_frames, plays) = actl.unwrap_or((1, 1));
    let (w, h) = (hdr.width, hdr.height);
    if actl.is_none() || (num_frames == 1 && default_ctl.is_none() && frames.is_empty()) {
        let rgba = decode_pixels(&ctx, w, h, &idat, &mut budget)?;
        return Ok(Decoded::Still(Image {
            width: w,
            height: h,
            rgba,
        }));
    }

    // Animation: the frame list is [default image if it has an fcTL] + fdAT frames.
    let mut all: Vec<(FrameCtl, Vec<&[u8]>)> = Vec::new();
    if let Some(c) = default_ctl {
        all.push((c, idat.clone()));
    }
    for f in frames {
        all.push((f.ctl, f.chunks));
    }
    if all.len() != usize::try_from(num_frames).unwrap_or(usize::MAX) {
        return Err(DecodeError::Malformed("APNG frame count mismatch"));
    }
    let first = all
        .first()
        .map(|(c, _)| *c)
        .ok_or(DecodeError::Malformed("no frames"))?;
    if first.x != 0 || first.y != 0 || first.width != w || first.height != h {
        return Err(DecodeError::Malformed(
            "first APNG frame must fill the canvas",
        ));
    }
    if all.len() == 1 {
        let rgba = decode_pixels(&ctx, w, h, &idat_or(&all), &mut budget)?;
        return Ok(Decoded::Still(Image {
            width: w,
            height: h,
            rgba,
        }));
    }

    let canvas_bytes = mul(budget.check_dims(u64::from(w), u64::from(h))?, 4)?;
    let mut canvas = budget.zeroed(canvas_bytes)?;
    let mut saved = budget.zeroed(canvas_bytes)?;
    let mut out_frames: Vec<Frame> = Vec::new();
    let mut prev: Option<FrameCtl> = None;
    for (ctl, chunks) in &all {
        // Dispose of the previous frame first.
        if let Some(p) = prev {
            match p.dispose {
                1 => clear_rect(&mut canvas, w, &p),
                2 => canvas.copy_from_slice_checked(&saved),
                _ => {}
            }
        }
        if ctl.dispose == 2 {
            saved.copy_from_slice_checked(&canvas);
        }
        let px = decode_pixels(&ctx, ctl.width, ctl.height, chunks, &mut budget)?;
        draw(&mut canvas, w, ctl, &px);
        budget.release(px.len());
        drop(px);
        budget.check_frame_count(out_frames.len())?;
        let mut copy = budget.with_capacity(canvas.len())?;
        copy.extend_from_slice(&canvas);
        out_frames.push(Frame {
            image: Image {
                width: w,
                height: h,
                rgba: copy,
            },
            delay_ms: ctl.delay_ms,
        });
        prev = Some(*ctl);
    }
    Ok(Decoded::Animated(Animation {
        width: w,
        height: h,
        frames: out_frames,
        loop_count: plays,
    }))
}

fn idat_or<'a>(all: &[(FrameCtl, Vec<&'a [u8]>)]) -> Vec<&'a [u8]> {
    all.first().map(|(_, c)| c.clone()).unwrap_or_default()
}

trait CopyChecked {
    fn copy_from_slice_checked(&mut self, src: &[u8]);
}
impl CopyChecked for Vec<u8> {
    fn copy_from_slice_checked(&mut self, src: &[u8]) {
        if self.len() == src.len() {
            self.clear();
            self.extend_from_slice(src);
        }
    }
}

fn clear_rect(canvas: &mut [u8], cw: u32, c: &FrameCtl) {
    for row in 0..c.height as usize {
        let start = ((c.y as usize + row) * cw as usize + c.x as usize) * 4;
        let len = c.width as usize * 4;
        if let Some(s) = canvas.get_mut(start..).and_then(|s| s.get_mut(..len)) {
            s.fill(0);
        }
    }
}

fn draw(canvas: &mut [u8], cw: u32, c: &FrameCtl, px: &[u8]) {
    let fw = c.width as usize;
    for (row, src) in px.chunks_exact(fw * 4).enumerate() {
        let start = ((c.y as usize + row) * cw as usize + c.x as usize) * 4;
        let Some(dst) = canvas.get_mut(start..).and_then(|s| s.get_mut(..fw * 4)) else {
            continue;
        };
        if c.blend == 0 {
            dst.copy_from_slice(src);
        } else {
            let (dp, sp) = (dst.as_chunks_mut::<4>().0, src.as_chunks::<4>().0);
            for (d, s) in dp.iter_mut().zip(sp) {
                let r = blend_over(
                    [at(s, 0), at(s, 1), at(s, 2), at(s, 3)],
                    [at(d, 0), at(d, 1), at(d, 2), at(d, 3)],
                );
                d.copy_from_slice(&r);
            }
        }
    }
}

/// Source-over in straight alpha, integer, rounding half up:
/// `a = as*255 + ad*(255-as)`, `c = (cs*as*255 + cd*ad*(255-as)) / a`,
/// output alpha `(a + 127) / 255`. Opaque or fully transparent sources are
/// exact (copy / keep).
pub fn blend_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
    let (sa, da) = (u32::from(src[3]), u32::from(dst[3]));
    if sa == 255 {
        return src;
    }
    if sa == 0 {
        return dst;
    }
    let a = sa * 255 + da * (255 - sa);
    if a == 0 {
        return [0; 4];
    }
    let ch = |s: u8, d: u8| -> u8 {
        let num = u32::from(s) * sa * 255 + u32::from(d) * da * (255 - sa);
        ((num + a / 2) / a).min(255) as u8
    };
    [
        ch(src[0], dst[0]),
        ch(src[1], dst[1]),
        ch(src[2], dst[2]),
        ((a + 127) / 255).min(255) as u8,
    ]
}

struct Ctx<'a> {
    hdr: Header,
    palette: &'a [[u8; 3]],
    trns: &'a [u8],
}

/// Inflate, unfilter, de-interlace and expand one image or frame of `w` x `h`
/// to RGBA8. The caller has validated the canvas; `w` x `h` is re-checked.
fn decode_pixels(
    ctx: &Ctx,
    w: u32,
    h: u32,
    chunks: &[&[u8]],
    budget: &mut Budget,
) -> Result<Vec<u8>, DecodeError> {
    let px = budget.check_dims(u64::from(w), u64::from(h))?;
    let (wu, hu) = (w as usize, h as usize);
    let bpp_bits = ctx.hdr.bits_per_pixel();
    let passes: &[(usize, usize, usize, usize)] = if ctx.hdr.interlace { &ADAM7 } else { &SINGLE };
    // Inflated size, with checked arithmetic.
    let mut expect: usize = 0;
    for &(x0, y0, dx, dy) in passes {
        let (pw, ph) = (pass_dim(wu, x0, dx), pass_dim(hu, y0, dy));
        if pw == 0 || ph == 0 {
            continue;
        }
        let rb = mul(pw, bpp_bits)?.div_ceil(8);
        expect = expect
            .checked_add(mul(rb + 1, ph)?)
            .ok_or(DecodeError::LimitExceeded("size"))?;
    }
    let out_bytes = mul(px, 4)?;
    budget.charge(expect)?;
    // Concatenate the zlib stream (bounded by the input size).
    let total: usize = chunks.iter().map(|c| c.len()).sum();
    let mut z = Vec::new();
    z.try_reserve_exact(total)
        .map_err(|_| DecodeError::LimitExceeded("allocation"))?;
    for c in chunks {
        z.extend_from_slice(c);
    }
    let mut raw = zlib_decompress(&z, expect)?;
    drop(z);
    if raw.len() != expect {
        return Err(DecodeError::Malformed("image data length mismatch"));
    }
    let mut out = budget.zeroed(out_bytes)?;

    let bpp = (bpp_bits / 8).max(1);
    let mut offset = 0usize;
    let mut prev: Vec<u8> = Vec::new();
    for &(x0, y0, dx, dy) in passes {
        let (pw, ph) = (pass_dim(wu, x0, dx), pass_dim(hu, y0, dy));
        if pw == 0 || ph == 0 {
            continue;
        }
        let rb = pw * bpp_bits / 8 + usize::from(!(pw * bpp_bits).is_multiple_of(8));
        let size = (rb + 1) * ph;
        let pass_buf = raw
            .get_mut(offset..offset + size)
            .ok_or(DecodeError::Malformed("image data length mismatch"))?;
        offset += size;
        prev.clear();
        prev.resize(rb, 0);
        for (ry, row) in pass_buf.chunks_exact_mut(rb + 1).enumerate() {
            let Some((ft, data)) = row.split_first_mut() else {
                continue;
            };
            unfilter(*ft, bpp, data, &prev)?;
            prev.clear();
            prev.extend_from_slice(data);
            let y = y0 + ry * dy;
            for rx in 0..pw {
                let x = x0 + rx * dx;
                let px = pixel(ctx, data, rx)?;
                let base = (y * wu + x) * 4;
                if let Some(o) = out.get_mut(base..).and_then(|s| s.get_mut(..4)) {
                    o.copy_from_slice(&px);
                }
            }
        }
    }
    budget.release(expect);
    Ok(out)
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i32::from(a), i32::from(b), i32::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn unfilter(ft: u8, bpp: usize, cur: &mut [u8], prev: &[u8]) -> Result<(), DecodeError> {
    if ft > 4 {
        return Err(DecodeError::Malformed("bad filter type"));
    }
    for i in 0..cur.len() {
        let left = i.checked_sub(bpp);
        let a = left.map_or(0, |j| at(cur, j));
        let b = at(prev, i);
        let c = left.map_or(0, |j| at(prev, j));
        let add = match ft {
            0 => 0,
            1 => a,
            2 => b,
            3 => ((u16::from(a) + u16::from(b)) / 2) as u8,
            _ => paeth(a, b, c),
        };
        let v = at(cur, i).wrapping_add(add);
        put(cur, i, v);
    }
    Ok(())
}

/// Sample `idx` (counted in samples across the row) of the given depth.
fn sample(row: &[u8], depth: u8, idx: usize) -> u16 {
    match depth {
        8 => u16::from(at(row, idx)),
        16 => u16::from_be_bytes([at(row, idx * 2), at(row, idx * 2 + 1)]),
        d => {
            let bit = idx * usize::from(d);
            let byte = at(row, bit / 8);
            let shift = 8 - usize::from(d) - (bit % 8);
            u16::from((byte >> shift) & ((1u8 << d) - 1))
        }
    }
}

/// Scale a sample of `depth` bits to 8 bits (high byte for 16).
fn to8(v: u16, depth: u8) -> u8 {
    match depth {
        16 => (v >> 8) as u8,
        8 => v as u8,
        d => ((u32::from(v) * 255) / ((1u32 << d) - 1)) as u8,
    }
}

fn pixel(ctx: &Ctx, row: &[u8], x: usize) -> Result<[u8; 4], DecodeError> {
    let d = ctx.hdr.depth;
    let key = |i: usize| -> u16 { crate::be16(ctx.trns, i * 2) };
    Ok(match ctx.hdr.colour {
        0 => {
            let v = sample(row, d, x);
            let g = to8(v, d);
            let a = if ctx.trns.len() >= 2 && key(0) == v {
                0
            } else {
                255
            };
            [g, g, g, a]
        }
        2 => {
            let (r, g, b) = (
                sample(row, d, x * 3),
                sample(row, d, x * 3 + 1),
                sample(row, d, x * 3 + 2),
            );
            let a = if ctx.trns.len() >= 6 && key(0) == r && key(1) == g && key(2) == b {
                0
            } else {
                255
            };
            [to8(r, d), to8(g, d), to8(b, d), a]
        }
        3 => {
            let i = usize::from(sample(row, d, x));
            let c = ctx
                .palette
                .get(i)
                .ok_or(DecodeError::Malformed("palette index out of range"))?;
            let a = ctx.trns.get(i).copied().unwrap_or(255);
            [at(c, 0), at(c, 1), at(c, 2), a]
        }
        4 => {
            let g = to8(sample(row, d, x * 2), d);
            [g, g, g, to8(sample(row, d, x * 2 + 1), d)]
        }
        _ => [
            to8(sample(row, d, x * 4), d),
            to8(sample(row, d, x * 4 + 1), d),
            to8(sample(row, d, x * 4 + 2), d),
            to8(sample(row, d, x * 4 + 3), d),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_over_is_exact_at_extremes_and_hand_derived_midpoint() {
        assert_eq!(blend_over([9, 8, 7, 255], [1, 2, 3, 4]), [9, 8, 7, 255]);
        assert_eq!(blend_over([9, 8, 7, 0], [1, 2, 3, 4]), [1, 2, 3, 4]);
        // 50% (128/255) red over opaque blue:
        // a = 128*255 + 255*127 = 65025; r = 255*128*255/65025 = 128 (exact),
        // b = 255*255*127/65025 = 127; alpha = 255.
        assert_eq!(
            blend_over([255, 0, 0, 128], [0, 0, 255, 255]),
            [128, 0, 127, 255]
        );
    }

    #[test]
    fn pass_dims_match_the_adam7_table() {
        // 8x8 image: pass sizes 1x1, 1x1, 2x1, 2x2, 4x2, 4x4, 8x4.
        let dims: Vec<(usize, usize)> = ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| (pass_dim(8, x0, dx), pass_dim(8, y0, dy)))
            .collect();
        assert_eq!(
            dims,
            vec![(1, 1), (1, 1), (2, 1), (2, 2), (4, 2), (4, 4), (8, 4)]
        );
        // 1x1: only pass 1 has pixels.
        let n: usize = ADAM7
            .iter()
            .map(|&(x0, y0, dx, dy)| pass_dim(1, x0, dx) * pass_dim(1, y0, dy))
            .sum();
        assert_eq!(n, 1);
    }

    #[test]
    fn sample_unpacks_sub_byte_depths_msb_first() {
        assert_eq!(sample(&[0b1011_0010], 1, 0), 1);
        assert_eq!(sample(&[0b1011_0010], 1, 1), 0);
        assert_eq!(sample(&[0b1011_0010], 2, 1), 0b11);
        assert_eq!(sample(&[0b1011_0010], 4, 1), 0b0010);
        assert_eq!(to8(0b11, 2), 255);
        assert_eq!(to8(0b01, 2), 85);
        assert_eq!(to8(0xABCD, 16), 0xAB);
    }
}
