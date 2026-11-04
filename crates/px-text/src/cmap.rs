//! `cmap`: code point to glyph id, formats 4 (BMP) and 12 (full range).
//!
//! The subtable is chosen once, in [`Cmap::parse`], by this preference order:
//! Windows Unicode full (3,10), Unicode full (0,4 / 0,6), Windows BMP (3,1),
//! Unicode BMP (0,0..=3). Any other platform or format is ignored. Lookups return
//! `None` for glyph 0 (`.notdef`), because "not mapped" is what the caller needs.

use crate::sfnt::{FontError, Span, u16_at, u32_at};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Four,
    Twelve,
}

/// The chosen subtable of a font's `cmap`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cmap {
    /// Absolute span of the chosen subtable within the font file.
    sub: Span,
    format: Format,
}

impl Cmap {
    /// Pick and validate a subtable of the `cmap` table at `table` in `data`.
    pub(crate) fn parse(data: &[u8], table: Span) -> Result<Cmap, FontError> {
        let t = table.slice(data);
        let count = usize::from(u16_at(t, 2).ok_or(FontError::Truncated)?);
        let mut best: Option<(u8, Span, Format)> = None;
        for i in 0..count {
            let rec = 4 + i * 8;
            let platform = u16_at(t, rec).ok_or(FontError::Truncated)?;
            let encoding = u16_at(t, rec + 2).ok_or(FontError::Truncated)?;
            let offset = u32_at(t, rec + 4).ok_or(FontError::Truncated)? as usize;
            let Some(format_tag) = u16_at(t, offset) else {
                continue;
            };
            let format = match format_tag {
                4 => Format::Four,
                12 => Format::Twelve,
                _ => continue,
            };
            let rank = match (platform, encoding, format) {
                (3, 10, Format::Twelve) => 4,
                (0, 4 | 6, Format::Twelve) => 4,
                (3, 1, Format::Four) => 3,
                (0, 0..=3, Format::Four) => 2,
                (0, _, Format::Twelve) => 2,
                _ => continue,
            };
            let Some(sub) = validate(t, offset, format) else {
                continue;
            };
            if best.is_none_or(|(r, _, _)| rank > r) {
                let abs = Span {
                    offset: table.offset + sub.offset,
                    len: sub.len,
                };
                best = Some((rank, abs, format));
            }
        }
        let (_, sub, format) = best.ok_or(FontError::Malformed("no usable cmap subtable"))?;
        Ok(Cmap { sub, format })
    }

    /// The glyph for `c`, or `None` when unmapped (or mapped to `.notdef`).
    pub(crate) fn glyph(&self, data: &[u8], c: char) -> Option<u16> {
        let sub = self.sub.slice(data);
        let cp = c as u32;
        let g = match self.format {
            Format::Four => lookup4(sub, cp),
            Format::Twelve => lookup12(sub, cp),
        }?;
        (g != 0).then_some(g)
    }
}

/// Check that a subtable's own header fits inside `t` and return its span
/// (relative to `t`).
fn validate(t: &[u8], offset: usize, format: Format) -> Option<Span> {
    match format {
        Format::Four => {
            let len = usize::from(u16_at(t, offset.checked_add(2)?)?);
            let seg_x2 = usize::from(u16_at(t, offset.checked_add(6)?)?);
            // header 14, endCode, reservedPad 2, startCode, idDelta, idRangeOffset.
            let need = 16usize.checked_add(seg_x2.checked_mul(4)?)?;
            let len = len.max(need);
            let end = offset.checked_add(len)?;
            (end <= t.len() && seg_x2 % 2 == 0).then_some(Span { offset, len })
        }
        Format::Twelve => {
            let groups = u32_at(t, offset.checked_add(12)?)? as usize;
            let len = 16usize.checked_add(groups.checked_mul(12)?)?;
            let end = offset.checked_add(len)?;
            (end <= t.len()).then_some(Span { offset, len })
        }
    }
}

/// Format 4 lookup. Binary search over `endCode` (which the spec sorts; a font that
/// is unsorted gets a wrong answer, never a panic).
fn lookup4(s: &[u8], cp: u32) -> Option<u16> {
    let cp = u16::try_from(cp).ok()?;
    let seg_count = usize::from(u16_at(s, 6)?) / 2;
    let end_codes = 14;
    let start_codes = end_codes + seg_count * 2 + 2;
    let id_deltas = start_codes + seg_count * 2;
    let id_range_offsets = id_deltas + seg_count * 2;
    let (mut lo, mut hi) = (0usize, seg_count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if u16_at(s, end_codes + mid * 2)? < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo >= seg_count {
        return None;
    }
    let start = u16_at(s, start_codes + lo * 2)?;
    if cp < start {
        return None;
    }
    let delta = u16_at(s, id_deltas + lo * 2)?;
    let range_pos = id_range_offsets + lo * 2;
    let range = usize::from(u16_at(s, range_pos)?);
    if range == 0 {
        return Some(cp.wrapping_add(delta));
    }
    let pos = range_pos
        .checked_add(range)?
        .checked_add(usize::from(cp - start) * 2)?;
    let g = u16_at(s, pos)?;
    if g == 0 {
        return Some(0);
    }
    Some(g.wrapping_add(delta))
}

/// Format 12 lookup: binary search over the sequential map groups.
fn lookup12(s: &[u8], cp: u32) -> Option<u16> {
    let groups = u32_at(s, 12)? as usize;
    let (mut lo, mut hi) = (0usize, groups);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let rec = 16 + mid * 12;
        if u32_at(s, rec + 4)? < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo >= groups {
        return None;
    }
    let rec = 16 + lo * 12;
    let start = u32_at(s, rec)?;
    if cp < start {
        return None;
    }
    let first = u32_at(s, rec + 8)?;
    let g = first.checked_add(cp - start)?;
    u16::try_from(g).ok()
}
