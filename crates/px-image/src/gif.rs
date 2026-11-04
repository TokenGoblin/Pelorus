//! GIF decoder (87a and 89a).
//!
//! Supported: global and local colour tables, interlaced frames, transparency
//! through the Graphic Control Extension, disposal methods 0-3 (4-7 are
//! treated as 0), per-frame delays, the NETSCAPE2.0 / ANIMEXTS1.0 loop
//! extension. Comment, plain-text and unknown extensions are skipped.
//!
//! Compositing: the canvas starts fully transparent. "Restore to background"
//! (disposal 2) clears the frame rectangle to transparent black, as browsers
//! do (not to the logical screen's background colour). Each returned frame is
//! the canvas right after that frame is drawn, before its own disposal.
//!
//! Strictness: a frame that does not lie inside the logical screen is an error,
//! as is LZW data that ends before filling the frame, a code that is not in
//! the table, or a first code that is not a literal. A palette index beyond the
//! colour table renders as opaque black. A stream that ends cleanly on a block
//! boundary after at least one complete frame, without the trailer, is
//! accepted. Image data longer than the frame is ignored.
//!
//! LZW tables are fixed at 4096 entries; once full no entry is added and the
//! code size stays 12 until a clear code, so a hostile stream cannot grow
//! memory or loop. Decoded pixels are capped at the frame's `width * height`.

use crate::limits::{Budget, mul};
use crate::{Animation, DecodeError, Decoded, Frame, Image, Limits, at, le16, put};

struct Reader<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Result<u8, DecodeError> {
        let b = self
            .d
            .get(self.pos)
            .copied()
            .ok_or(DecodeError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated)?;
        let s = self.d.get(self.pos..end).ok_or(DecodeError::Truncated)?;
        self.pos = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        let s = self.take(2)?;
        Ok(le16(s, 0))
    }
    /// Concatenate sub-blocks up to the zero terminator.
    fn sub_blocks(&mut self, out: &mut Vec<u8>) -> Result<(), DecodeError> {
        loop {
            let n = usize::from(self.u8()?);
            if n == 0 {
                return Ok(());
            }
            out.extend_from_slice(self.take(n)?);
        }
    }
    fn at_end(&self) -> bool {
        self.pos >= self.d.len()
    }
}

#[derive(Clone, Copy, Default)]
struct Gce {
    disposal: u8,
    transparent: Option<u8>,
    delay_cs: u16,
}

type Palette = Vec<[u8; 3]>;

fn read_palette(r: &mut Reader, size_bits: u8) -> Result<Palette, DecodeError> {
    let n = 1usize << (usize::from(size_bits & 7) + 1);
    let raw = r.take(n * 3)?;
    Ok(raw.as_chunks::<3>().0.to_vec())
}

/// Decode a GIF.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded, DecodeError> {
    let mut budget = Budget::new(limits);
    let mut r = Reader { d: data, pos: 0 };
    let magic = r.take(6)?;
    if magic != b"GIF87a" && magic != b"GIF89a" {
        return Err(DecodeError::Malformed("bad signature"));
    }
    let (cw, ch) = (r.u16()?, r.u16()?);
    let packed = r.u8()?;
    let _background = r.u8()?;
    let _aspect = r.u8()?;
    let px = budget.check_dims(u64::from(cw), u64::from(ch))?;
    let global: Option<Palette> = if packed & 0x80 != 0 {
        Some(read_palette(&mut r, packed)?)
    } else {
        None
    };
    let canvas_bytes = mul(px, 4)?;

    let mut canvas: Vec<u8> = Vec::new();
    let mut saved: Vec<u8> = Vec::new();
    let mut frames: Vec<Frame> = Vec::new();
    let mut gce = Gce::default();
    let mut loop_ext: Option<u16> = None;
    let mut prev: Option<(Gce, [usize; 4])> = None; // disposal info of the previous frame
    let mut scratch: Vec<u8> = Vec::new();

    loop {
        if r.at_end() {
            if frames.is_empty() {
                return Err(DecodeError::Truncated);
            }
            break;
        }
        match r.u8()? {
            0x3B => break,
            0x21 => {
                let label = r.u8()?;
                scratch.clear();
                if label == 0xF9 {
                    r.sub_blocks(&mut scratch)?;
                    if scratch.len() >= 4 {
                        let p = at(&scratch, 0);
                        gce = Gce {
                            disposal: (p >> 2) & 7,
                            transparent: (p & 1 != 0).then(|| at(&scratch, 3)),
                            delay_cs: le16(&scratch, 1),
                        };
                    }
                } else if label == 0xFF {
                    // First sub-block is the application id, later ones its data.
                    let n = usize::from(r.u8()?);
                    let id = r.take(n)?;
                    let is_loop = id == b"NETSCAPE2.0" || id == b"ANIMEXTS1.0";
                    r.sub_blocks(&mut scratch)?;
                    if is_loop && at(&scratch, 0) == 1 && scratch.len() >= 3 {
                        loop_ext = Some(le16(&scratch, 1));
                    }
                } else {
                    r.sub_blocks(&mut scratch)?;
                }
            }
            0x2C => {
                let (left, top) = (usize::from(r.u16()?), usize::from(r.u16()?));
                let (fw, fh) = (usize::from(r.u16()?), usize::from(r.u16()?));
                let fp = r.u8()?;
                let local = if fp & 0x80 != 0 {
                    Some(read_palette(&mut r, fp)?)
                } else {
                    None
                };
                let min_code = r.u8()?;
                scratch.clear();
                r.sub_blocks(&mut scratch)?;
                if fw == 0 || fh == 0 {
                    return Err(DecodeError::BadDimensions);
                }
                if left + fw > usize::from(cw) || top + fh > usize::from(ch) {
                    return Err(DecodeError::Malformed("frame outside logical screen"));
                }
                let palette = local
                    .as_ref()
                    .or(global.as_ref())
                    .ok_or(DecodeError::Malformed("no colour table"))?;
                budget.check_frame_count(frames.len())?;
                if canvas.is_empty() {
                    canvas = budget.zeroed(canvas_bytes)?;
                }
                // Dispose of the previous frame.
                if let Some((pg, rect)) = prev {
                    match pg.disposal {
                        2 => clear_rect(&mut canvas, usize::from(cw), rect),
                        3 if saved.len() == canvas.len() => canvas.copy_from_slice(&saved),
                        _ => {}
                    }
                }
                if gce.disposal == 3 {
                    if saved.is_empty() {
                        saved = budget.zeroed(canvas_bytes)?;
                    }
                    saved.copy_from_slice(&canvas);
                }
                let npx = mul(fw, fh)?;
                budget.charge(npx)?;
                let indices = lzw_decode(&scratch, min_code, npx)?;
                draw(
                    &mut canvas,
                    usize::from(cw),
                    [left, top, fw, fh],
                    &indices,
                    palette,
                    gce.transparent,
                    fp & 0x40 != 0,
                );
                budget.release(npx);
                drop(indices);
                let mut copy = budget.with_capacity(canvas_bytes)?;
                copy.extend_from_slice(&canvas);
                frames.push(Frame {
                    image: Image {
                        width: u32::from(cw),
                        height: u32::from(ch),
                        rgba: copy,
                    },
                    delay_ms: u32::from(gce.delay_cs) * 10,
                });
                prev = Some((gce, [left, top, fw, fh]));
                gce = Gce::default();
            }
            _ => return Err(DecodeError::Malformed("unknown block")),
        }
    }
    let mut it = frames.into_iter();
    match (it.next(), it.next()) {
        (None, _) => Err(DecodeError::Malformed("no image")),
        (Some(f), None) => Ok(Decoded::Still(f.image)),
        (Some(a), Some(b)) => {
            let mut v = vec![a, b];
            v.extend(it);
            Ok(Decoded::Animated(Animation {
                width: u32::from(cw),
                height: u32::from(ch),
                frames: v,
                loop_count: match loop_ext {
                    None => 1,
                    Some(0) => 0,
                    Some(n) => u32::from(n) + 1,
                },
            }))
        }
    }
}

fn clear_rect(canvas: &mut [u8], cw: usize, rect: [usize; 4]) {
    let [left, top, fw, fh] = rect;
    for y in top..top + fh {
        let start = (y * cw + left) * 4;
        if let Some(s) = canvas.get_mut(start..).and_then(|s| s.get_mut(..fw * 4)) {
            s.fill(0);
        }
    }
}

/// Source row (in decoded order) to destination row for an interlaced frame.
fn interlaced_row(seq: usize, h: usize) -> usize {
    let mut n = seq;
    for (start, step) in [(0usize, 8usize), (4, 8), (2, 4), (1, 2)] {
        let rows = if h > start {
            (h - start).div_ceil(step)
        } else {
            0
        };
        if n < rows {
            return start + n * step;
        }
        n -= rows;
    }
    0
}

fn draw(
    canvas: &mut [u8],
    cw: usize,
    rect: [usize; 4],
    indices: &[u8],
    palette: &Palette,
    transparent: Option<u8>,
    interlaced: bool,
) {
    let [left, top, fw, fh] = rect;
    for (seq, row) in indices.chunks_exact(fw).enumerate() {
        let y = top
            + if interlaced {
                interlaced_row(seq, fh)
            } else {
                seq
            };
        for (x, &idx) in row.iter().enumerate() {
            if transparent == Some(idx) {
                continue;
            }
            let c = palette.get(usize::from(idx)).copied().unwrap_or([0, 0, 0]);
            let base = (y * cw + left + x) * 4;
            if let Some(o) = canvas.get_mut(base..).and_then(|s| s.get_mut(..4)) {
                o.copy_from_slice(&[at(&c, 0), at(&c, 1), at(&c, 2), 255]);
            }
        }
    }
}

/// Variable-width LZW as used by GIF (LSB-first packing, deferred clear). The
/// result has exactly `target` indices.
fn lzw_decode(data: &[u8], min_code: u8, target: usize) -> Result<Vec<u8>, DecodeError> {
    if !(1..=8).contains(&min_code) {
        return Err(DecodeError::Malformed("bad LZW minimum code size"));
    }
    let mut out: Vec<u8> = Vec::new();
    out.try_reserve_exact(target)
        .map_err(|_| DecodeError::LimitExceeded("allocation"))?;
    let clear: u16 = 1 << min_code;
    let eoi = clear + 1;
    let mut prefix = [0u16; 4096];
    let mut suffix = [0u8; 4096];
    let mut first = [0u8; 4096];
    for i in 0..usize::from(clear) {
        put(&mut suffix, i, i as u8);
        put(&mut first, i, i as u8);
    }
    let mut next: u16 = clear + 2;
    let mut size: u32 = u32::from(min_code) + 1;
    let mut prev: Option<u16> = None;
    let mut acc: u32 = 0;
    let mut nbits: u32 = 0;
    let mut stack: Vec<u8> = Vec::with_capacity(4097);
    let mut bytes = data.iter();
    while out.len() < target {
        while nbits < size {
            let Some(&b) = bytes.next() else {
                return Err(DecodeError::Malformed(
                    "LZW data ended before the frame was full",
                ));
            };
            acc |= u32::from(b) << nbits;
            nbits += 8;
        }
        let code = (acc & ((1 << size) - 1)) as u16;
        acc >>= size;
        nbits -= size;
        if code == clear {
            next = clear + 2;
            size = u32::from(min_code) + 1;
            prev = None;
            continue;
        }
        if code == eoi {
            return Err(DecodeError::Malformed(
                "LZW data ended before the frame was full",
            ));
        }
        let Some(p) = prev else {
            if code >= clear {
                return Err(DecodeError::Malformed("LZW first code is not a literal"));
            }
            out.push(code as u8);
            prev = Some(code);
            continue;
        };
        // The string for `code` (or prev + its first byte for the KwKwK case).
        stack.clear();
        let mut c;
        if code < next {
            c = code;
        } else if code == next {
            stack.push(at(&first, usize::from(p)));
            c = p;
        } else {
            return Err(DecodeError::Malformed("LZW code not in table"));
        }
        let mut steps = 0;
        while c >= clear {
            stack.push(at(&suffix, usize::from(c)));
            c = prefix.get(usize::from(c)).copied().unwrap_or(0);
            steps += 1;
            if steps > 4096 {
                return Err(DecodeError::Malformed("LZW chain too long"));
            }
        }
        stack.push(c as u8);
        let first_byte = stack.last().copied().unwrap_or(0);
        let room = target - out.len();
        out.extend(stack.iter().rev().take(room));
        if next < 4096 {
            let n = usize::from(next);
            if let Some(x) = prefix.get_mut(n) {
                *x = p;
            }
            put(&mut suffix, n, first_byte);
            let pf = at(&first, usize::from(p));
            put(&mut first, n, pf);
            next += 1;
            if u32::from(next) == (1 << size) && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interlace_row_order_for_10_rows() {
        // Pass 1: rows 0,8. Pass 2: 4. Pass 3: 2,6. Pass 4: 1,3,5,7,9.
        let order: Vec<usize> = (0..10).map(|i| interlaced_row(i, 10)).collect();
        assert_eq!(order, vec![0, 8, 4, 2, 6, 1, 3, 5, 7, 9]);
    }

    #[test]
    fn lzw_hand_derived() {
        // min code size 2: clear=4 eoi=5, codes are 3 bits wide.
        // Stream: clear(4) 1 1 5(eoi)? Build: codes [4,1,1,5] LSB-first 3 bits:
        // bits: 100 001 001 101 -> bytes: 0b01_001_100 = 0x4C, 0b1_101_00_1.. computed below.
        let codes = [4u32, 1, 1, 5];
        let mut acc = 0u32;
        let mut n = 0;
        let mut bytes = vec![];
        for c in codes {
            acc |= c << n;
            n += 3;
            while n >= 8 {
                bytes.push((acc & 0xFF) as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            bytes.push(acc as u8);
        }
        // Reaching EOI before target pixels is an error; 2 pixels are available.
        assert_eq!(lzw_decode(&bytes, 2, 2).unwrap(), vec![1, 1]);
        assert!(lzw_decode(&bytes, 2, 3).is_err());
    }

    #[test]
    fn lzw_kwkwk_case() {
        // min 2. codes: clear, 0, 6 (== next, string = prev+first(prev) = 0,0), eoi.
        // Entry 6 is the first free code (clear=4, eoi=5, next=6).
        let codes = [4u32, 0, 6, 5];
        let mut acc = 0u32;
        let mut n = 0;
        let mut bytes = vec![];
        for c in codes {
            acc |= c << n;
            n += 3;
            while n >= 8 {
                bytes.push((acc & 0xFF) as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        if n > 0 {
            bytes.push(acc as u8);
        }
        assert_eq!(lzw_decode(&bytes, 2, 3).unwrap(), vec![0, 0, 0]);
    }
}
