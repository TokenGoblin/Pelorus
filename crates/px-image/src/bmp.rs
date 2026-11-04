//! BMP decoder (also the DIB decoder used by ICO).
//!
//! Supported: 1, 4, 8, 16, 24 and 32 bits per pixel; palettes; `BI_RGB`,
//! `BI_RLE8`, `BI_RLE4`, `BI_BITFIELDS` and `BI_ALPHABITFIELDS`; bottom-up and
//! top-down (negative height) rows; core (12 byte, OS/2), INFO (40), V2/V3
//! (52/56), V4 (108) and V5 (124) headers. Embedded JPEG/PNG (`BI_JPEG`,
//! `BI_PNG`), OS/2 2.x headers and RLE with a top-down row order are
//! `Unsupported` or malformed.
//!
//! Choices, all deterministic:
//! * Channels from bit masks are scaled to 8 bits as `v * 255 / max`.
//! * 32-bit `BI_RGB` and 32-bit masks without an alpha mask: if any pixel has
//!   a non-zero alpha byte, the alpha byte is used, otherwise the image is
//!   opaque. With an alpha mask (`BI_BITFIELDS` with A, V4/V5) the mask is used.
//! * RLE pixels never written (skipped by a delta or an early end of line) are
//!   transparent black. A run that would leave the image is an error.
//! * A palette index beyond the palette is opaque black.
//! * The pixel data must lie inside the file: a header claiming more rows than
//!   the file holds is `Truncated` before any output is allocated (RLE aside,
//!   where the pixel and byte caps in [`Limits`] bound the output).

use crate::limits::{Budget, mul};
use crate::{DecodeError, Image, Limits, at, le16, le32, png, put};

const BI_RGB: u32 = 0;
const BI_RLE8: u32 = 1;
const BI_RLE4: u32 = 2;
const BI_BITFIELDS: u32 = 3;
const BI_ALPHABITFIELDS: u32 = 6;

/// Decode a BMP file.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    if data.get(..2) != Some(b"BM".as_slice()) {
        return Err(DecodeError::Malformed("bad signature"));
    }
    if data.len() < 26 {
        return Err(DecodeError::Truncated);
    }
    let mut budget = Budget::new(limits);
    let off = le32(data, 10) as usize;
    let dib = data.get(14..).ok_or(DecodeError::Truncated)?;
    decode_dib(dib, Some(off.saturating_sub(14)), false, &mut budget)
}

struct Dib {
    w: usize,
    h: usize,
    top_down: bool,
    bpp: u16,
    compression: u32,
    masks: [u32; 4],
    palette: Vec<[u8; 3]>,
    /// Offset of the pixel data within the DIB slice.
    data_off: usize,
}

fn parse_dib(dib: &[u8], pixel_off: Option<usize>, ico: bool) -> Result<Dib, DecodeError> {
    let hs = le32(dib, 0) as usize;
    let (w, h_raw, bpp, compression, clr_used, entry) = match hs {
        12 => (
            i64::from(le16(dib, 4)),
            i64::from(le16(dib, 6)),
            le16(dib, 10),
            BI_RGB,
            0usize,
            3usize,
        ),
        40 | 52 | 56 | 108 | 124 => (
            i64::from(le32(dib, 4) as i32),
            i64::from(le32(dib, 8) as i32),
            le16(dib, 14),
            le32(dib, 16),
            le32(dib, 32) as usize,
            4,
        ),
        _ => return Err(DecodeError::Unsupported("BMP header size")),
    };
    if dib.len() < hs {
        return Err(DecodeError::Truncated);
    }
    if hs >= 16 && le16(dib, 12) != 1 {
        return Err(DecodeError::Malformed("BMP planes != 1"));
    }
    if !matches!(bpp, 1 | 4 | 8 | 16 | 24 | 32) {
        return Err(DecodeError::Malformed("bad BMP bit depth"));
    }
    match compression {
        BI_RGB | BI_BITFIELDS | BI_ALPHABITFIELDS => {}
        BI_RLE8 if bpp == 8 => {}
        BI_RLE4 if bpp == 4 => {}
        4 | 5 => return Err(DecodeError::Unsupported("JPEG/PNG compressed BMP")),
        _ => return Err(DecodeError::Malformed("bad BMP compression")),
    }
    let top_down = h_raw < 0;
    if w <= 0 || h_raw == 0 {
        return Err(DecodeError::BadDimensions);
    }
    if top_down && matches!(compression, BI_RLE4 | BI_RLE8) {
        return Err(DecodeError::Malformed("top-down RLE"));
    }
    let mut h = h_raw.unsigned_abs() as usize;
    if ico {
        h /= 2;
        if h == 0 {
            return Err(DecodeError::BadDimensions);
        }
    }
    // Bit masks.
    let uses_masks = matches!(compression, BI_BITFIELDS | BI_ALPHABITFIELDS);
    let mut masks = match bpp {
        16 => [0x7C00, 0x03E0, 0x001F, 0],
        32 => [0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0],
        _ => [0; 4],
    };
    let mut after = hs;
    if uses_masks && (bpp == 16 || bpp == 32) {
        if hs >= 52 {
            masks[0] = le32(dib, 40);
            masks[1] = le32(dib, 44);
            masks[2] = le32(dib, 48);
            if hs >= 56 {
                masks[3] = le32(dib, 52);
            }
        } else {
            let n = if compression == BI_ALPHABITFIELDS {
                4
            } else {
                3
            };
            if dib.len() < hs + 4 * n {
                return Err(DecodeError::Truncated);
            }
            for (i, m) in masks.iter_mut().enumerate().take(n) {
                *m = le32(dib, hs + 4 * i);
            }
            after = hs + 4 * n;
        }
    }
    // Palette.
    let mut palette = Vec::new();
    if bpp <= 8 {
        let max = 1usize << bpp;
        let n = if clr_used == 0 {
            max
        } else {
            clr_used.min(max)
        };
        let avail = match pixel_off {
            Some(o) => o.saturating_sub(after),
            None => dib.len().saturating_sub(after),
        };
        let n = n.min(avail / entry);
        let raw = dib
            .get(after..after + n * entry)
            .ok_or(DecodeError::Truncated)?;
        for e in raw.chunks_exact(entry) {
            palette.push([at(e, 2), at(e, 1), at(e, 0)]);
        }
        after += n * entry;
    }
    let data_off = pixel_off.unwrap_or(after);
    if data_off > dib.len() {
        return Err(DecodeError::Truncated);
    }
    Ok(Dib {
        w: w as usize,
        h,
        top_down,
        bpp,
        compression,
        masks,
        palette,
        data_off,
    })
}

/// Decode a DIB (BMP without its file header). `ico` halves the height and
/// applies the AND mask that follows the colour bitmap.
pub(crate) fn decode_dib(
    dib: &[u8],
    pixel_off: Option<usize>,
    ico: bool,
    budget: &mut Budget,
) -> Result<Image, DecodeError> {
    let d = parse_dib(dib, pixel_off, ico)?;
    let px = budget.check_dims(d.w as u64, d.h as u64)?;
    let out_bytes = mul(px, 4)?;
    let pixels = dib.get(d.data_off..).ok_or(DecodeError::Truncated)?;
    let stride = (d.w * usize::from(d.bpp)).div_ceil(32) * 4;
    let mut rgba;
    let mut color_end = 0usize;
    if matches!(d.compression, BI_RLE4 | BI_RLE8) {
        rgba = budget.zeroed(out_bytes)?;
        decode_rle(&d, pixels, &mut rgba)?;
    } else {
        // The rows must be present in the file before anything is allocated.
        let need = mul(stride, d.h)?;
        if pixels.len() < need {
            return Err(DecodeError::Truncated);
        }
        color_end = need;
        rgba = budget.zeroed(out_bytes)?;
        for r in 0..d.h {
            let y = if d.top_down { r } else { d.h - 1 - r };
            let row = pixels
                .get(r * stride..(r + 1) * stride)
                .ok_or(DecodeError::Truncated)?;
            let dst = rgba
                .get_mut(y * d.w * 4..(y + 1) * d.w * 4)
                .ok_or(DecodeError::Truncated)?;
            decode_row(&d, row, dst);
        }
    }
    // 32-bit: alpha heuristic.
    let has_alpha_mask = d.masks[3] != 0;
    let mut alpha_used = false;
    if d.bpp == 32 {
        if has_alpha_mask {
            alpha_used = true;
        } else {
            alpha_used = rgba.as_chunks::<4>().0.iter().any(|p| at(p, 3) != 0);
            // decode_row stored the raw fourth byte as alpha; fix up if unused.
        }
        if !alpha_used {
            for p in rgba.as_chunks_mut::<4>().0.iter_mut() {
                put(p, 3, 255);
            }
        }
    }
    if ico && !alpha_used {
        // AND mask: one bit per pixel, 1 = transparent, rows padded to 4 bytes.
        let mstride = d.w.div_ceil(32) * 4;
        let mask = pixels.get(color_end..).unwrap_or(&[]);
        if mask.len() >= mul(mstride, d.h)? {
            for r in 0..d.h {
                let y = if d.top_down { r } else { d.h - 1 - r };
                for x in 0..d.w {
                    let bit = at(mask, r * mstride + x / 8) >> (7 - (x % 8)) & 1;
                    if bit == 1 {
                        put(&mut rgba, (y * d.w + x) * 4 + 3, 0);
                    }
                }
            }
        }
    }
    Ok(Image {
        width: d.w as u32,
        height: d.h as u32,
        rgba,
    })
}

fn scale(v: u32, bits: u32) -> u8 {
    if bits == 0 {
        return 0;
    }
    let max = (1u64 << bits.min(32)) - 1;
    ((u64::from(v) * 255) / max).min(255) as u8
}

fn field(px: u32, mask: u32) -> Option<u8> {
    if mask == 0 {
        return None;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).trailing_ones();
    Some(scale((px & mask) >> shift, bits))
}

fn decode_row(d: &Dib, row: &[u8], dst: &mut [u8]) {
    for (x, out) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let px: [u8; 4] = match d.bpp {
            1 | 4 | 8 => {
                let bits = usize::from(d.bpp);
                let bit = x * bits;
                let b = at(row, bit / 8);
                let idx = usize::from((b >> (8 - bits - (bit % 8))) & ((1u16 << bits) - 1) as u8);
                let c = d.palette.get(idx).copied().unwrap_or([0, 0, 0]);
                [c[0], c[1], c[2], 255]
            }
            16 => {
                let v = u32::from(le16(row, x * 2));
                [
                    field(v, d.masks[0]).unwrap_or(0),
                    field(v, d.masks[1]).unwrap_or(0),
                    field(v, d.masks[2]).unwrap_or(0),
                    field(v, d.masks[3]).unwrap_or(255),
                ]
            }
            24 => [at(row, x * 3 + 2), at(row, x * 3 + 1), at(row, x * 3), 255],
            _ => {
                let v = le32(row, x * 4);
                if d.compression == BI_RGB {
                    [
                        at(row, x * 4 + 2),
                        at(row, x * 4 + 1),
                        at(row, x * 4),
                        at(row, x * 4 + 3),
                    ]
                } else {
                    [
                        field(v, d.masks[0]).unwrap_or(0),
                        field(v, d.masks[1]).unwrap_or(0),
                        field(v, d.masks[2]).unwrap_or(0),
                        field(v, d.masks[3]).unwrap_or(0),
                    ]
                }
            }
        };
        out.copy_from_slice(&px);
    }
}

/// RLE4 / RLE8. `rgba` is zeroed (transparent) and bottom-up: row 0 of the
/// stream is the bottom row of the image.
fn decode_rle(d: &Dib, data: &[u8], rgba: &mut [u8]) -> Result<(), DecodeError> {
    let four = d.compression == BI_RLE4;
    let (mut x, mut r) = (0usize, 0usize); // r counts stream rows from the bottom
    let mut i = 0usize;
    let set = |x: usize, r: usize, idx: usize, rgba: &mut [u8]| -> Result<(), DecodeError> {
        if x >= d.w || r >= d.h {
            return Err(DecodeError::Malformed("RLE pixel outside image"));
        }
        let y = d.h - 1 - r;
        let c = d.palette.get(idx).copied().unwrap_or([0, 0, 0]);
        let base = (y * d.w + x) * 4;
        if let Some(o) = rgba.get_mut(base..).and_then(|s| s.get_mut(..4)) {
            o.copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
        Ok(())
    };
    while let Some(&n) = data.get(i) {
        let Some(&v) = data.get(i + 1) else {
            return Err(DecodeError::Truncated);
        };
        i += 2;
        if n > 0 {
            // Encoded run of n pixels.
            for k in 0..usize::from(n) {
                let idx = if four {
                    usize::from(if k % 2 == 0 { v >> 4 } else { v & 15 })
                } else {
                    usize::from(v)
                };
                set(x, r, idx, rgba)?;
                x += 1;
            }
            continue;
        }
        match v {
            0 => {
                x = 0;
                r += 1;
            }
            1 => return Ok(()),
            2 => {
                let (Some(&dx), Some(&dy)) = (data.get(i), data.get(i + 1)) else {
                    return Err(DecodeError::Truncated);
                };
                i += 2;
                x += usize::from(dx);
                r += usize::from(dy);
            }
            n => {
                // Absolute run of n pixels, padded to a 16-bit boundary.
                let n = usize::from(n);
                let nbytes = if four { n.div_ceil(2) } else { n };
                let run = data.get(i..i + nbytes).ok_or(DecodeError::Truncated)?;
                for k in 0..n {
                    let idx = if four {
                        let b = at(run, k / 2);
                        usize::from(if k % 2 == 0 { b >> 4 } else { b & 15 })
                    } else {
                        usize::from(at(run, k))
                    };
                    set(x, r, idx, rgba)?;
                    x += 1;
                }
                i += nbytes + (nbytes % 2);
            }
        }
    }
    Ok(())
}

/// Used by ICO for PNG-encoded frames.
pub(crate) fn decode_png_frame(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    match png::decode(data, limits)? {
        crate::Decoded::Still(i) => Ok(i),
        crate::Decoded::Animated(a) => a
            .frames
            .into_iter()
            .next()
            .map(|f| f.image)
            .ok_or(DecodeError::Malformed("empty animation")),
    }
}
