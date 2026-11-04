//! ICO container: picks the best frame and decodes it.
//!
//! "Best" is the largest `width * height` from the directory (a width or
//! height byte of 0 means 256), then the highest bit depth, then the first.
//! Frames may be BMP (a DIB with doubled height, the colour bitmap followed by
//! a 1-bit AND mask) or PNG. Cursor files (`.cur`, type 2) are not accepted.
//!
//! The directory dimensions only choose a frame; the frame's own header is
//! authoritative for the decoded size. Every frame's data range is validated
//! against the file before it is read.

use crate::bmp::{decode_dib, decode_png_frame};
use crate::limits::Budget;
use crate::{DecodeError, Image, Limits, at, le16, le32};

const PNG_SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Decode the best frame of an ICO file.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    if data.len() < 6 {
        return Err(DecodeError::Truncated);
    }
    if le16(data, 0) != 0 || le16(data, 2) != 1 {
        return Err(DecodeError::Malformed("not an icon file"));
    }
    let count = usize::from(le16(data, 4));
    if count == 0 {
        return Err(DecodeError::Malformed("icon file without images"));
    }
    let dir = data.get(6..6 + count * 16).ok_or(DecodeError::Truncated)?;
    let mut best: Option<(u32, u16, usize)> = None; // (area, bpp, index)
    for (i, e) in dir.as_chunks::<16>().0.iter().enumerate() {
        let dim = |b: u8| if b == 0 { 256u32 } else { u32::from(b) };
        let area = dim(at(e, 0)) * dim(at(e, 1));
        let bpp = le16(e, 6);
        let better = match best {
            None => true,
            Some((a, b, _)) => area > a || (area == a && bpp > b),
        };
        if better {
            best = Some((area, bpp, i));
        }
    }
    let (_, _, idx) = best.ok_or(DecodeError::Malformed("no icon image"))?;
    let e = dir
        .get(idx * 16..idx * 16 + 16)
        .ok_or(DecodeError::Truncated)?;
    let (size, off) = (le32(e, 8) as usize, le32(e, 12) as usize);
    let end = off.checked_add(size).ok_or(DecodeError::Truncated)?;
    let frame = data.get(off..end).ok_or(DecodeError::Truncated)?;
    if frame.starts_with(&PNG_SIG) {
        return decode_png_frame(frame, limits);
    }
    let mut budget = Budget::new(limits);
    decode_dib(frame, None, true, &mut budget)
}
