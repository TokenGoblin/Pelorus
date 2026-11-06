//! Web font containers: raw sfnt, WOFF 1.0 and WOFF 2.0, unwrapped to a plain
//! sfnt that [`crate::font::Font::from_bytes`] and the shaper read.
//!
//! A font from a web page is hostile input. Every size in a container header is
//! checked against a fixed cap *before* it sizes an allocation, every offset is
//! checked against the file, and the decompressors are bounded (zlib through
//! `px_image::inflate`, Brotli through [`crate::brotli`]), so a font cannot make
//! this module allocate more than [`MAX_SFNT_BYTES`] or panic. Nothing here
//! executes anything: hinting programs are carried as opaque bytes and the
//! rasterizer never runs them.
//!
//! Supported: sfnt with TrueType outlines (`glyf`), WOFF 1.0, WOFF 2.0 including
//! the `glyf`/`loca` and `hmtx` transforms. Not supported: CFF outlines
//! (`OTTO`; [`crate::font::Font`] reads `glyf` only), font collections.

use crate::brotli;
use crate::font::Font;
use crate::sfnt::{FontError, u16_at, u32_at};
use crate::woff2;

/// Most bytes of font file accepted (compressed or not).
pub const MAX_FILE_BYTES: usize = 8 * 1024 * 1024;
/// Most bytes of unwrapped sfnt produced.
pub const MAX_SFNT_BYTES: usize = 32 * 1024 * 1024;
/// Most tables in one font.
pub const MAX_TABLES: usize = 64;

const SIG_WOFF: u32 = 0x774F_4646;
const SIG_WOFF2: u32 = 0x774F_4632;
const SIG_TRUETYPE: u32 = 0x0001_0000;
const SIG_TRUE: u32 = 0x7472_7565;

/// Which container the bytes start with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Container {
    Sfnt,
    Woff,
    Woff2,
}

/// Sniff the container from the first four bytes.
#[must_use]
pub fn sniff(data: &[u8]) -> Option<Container> {
    match u32_at(data, 0)? {
        SIG_WOFF => Some(Container::Woff),
        SIG_WOFF2 => Some(Container::Woff2),
        SIG_TRUETYPE | SIG_TRUE | 0x4F54_544F | 0x7474_6366 => Some(Container::Sfnt),
        _ => None,
    }
}

/// Unwrap any supported container to a plain sfnt. Raw sfnt data is returned
/// unchanged (and still has to pass `Font::from_bytes`).
pub fn to_sfnt(data: &[u8]) -> Result<Vec<u8>, FontError> {
    if data.len() > MAX_FILE_BYTES {
        return Err(FontError::Malformed("font file too large"));
    }
    match sniff(data) {
        Some(Container::Sfnt) => Ok(data.to_vec()),
        Some(Container::Woff) => woff1(data),
        Some(Container::Woff2) => woff2::decode(data),
        None => Err(FontError::BadMagic),
    }
}

/// One table to write into a rebuilt sfnt.
pub(crate) struct OutTable {
    pub tag: [u8; 4],
    pub checksum: u32,
    pub data: Vec<u8>,
}

/// Build an sfnt from tables (sorted by tag here, 4-byte aligned).
pub(crate) fn build_sfnt(flavor: u32, mut tables: Vec<OutTable>) -> Result<Vec<u8>, FontError> {
    tables.sort_by_key(|t| t.tag);
    let n = tables.len();
    if n == 0 || n > MAX_TABLES {
        return Err(FontError::Malformed("table count"));
    }
    let mut total: usize = 12 + 16 * n;
    for t in &tables {
        total = total
            .checked_add(t.data.len().div_ceil(4) * 4)
            .ok_or(FontError::Malformed("size overflow"))?;
    }
    if total > MAX_SFNT_BYTES {
        return Err(FontError::Malformed("font too large"));
    }
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(&flavor.to_be_bytes());
    let n16 = u16::try_from(n).map_err(|_| FontError::Malformed("table count"))?;
    out.extend_from_slice(&n16.to_be_bytes());
    let mut pow = 1u16;
    let mut log = 0u16;
    while pow.saturating_mul(2) <= n16 {
        pow *= 2;
        log += 1;
    }
    out.extend_from_slice(&(pow * 16).to_be_bytes());
    out.extend_from_slice(&log.to_be_bytes());
    out.extend_from_slice(&(n16 * 16 - pow * 16).to_be_bytes());
    let mut offset = 12 + 16 * n;
    for t in &tables {
        out.extend_from_slice(&t.tag);
        out.extend_from_slice(&t.checksum.to_be_bytes());
        out.extend_from_slice(&u32::try_from(offset).unwrap_or(u32::MAX).to_be_bytes());
        out.extend_from_slice(
            &u32::try_from(t.data.len())
                .unwrap_or(u32::MAX)
                .to_be_bytes(),
        );
        offset += t.data.len().div_ceil(4) * 4;
    }
    for t in &tables {
        out.extend_from_slice(&t.data);
        while !out.len().is_multiple_of(4) {
            out.push(0);
        }
    }
    Ok(out)
}

/// The sfnt table checksum of `data` (big-endian u32 sum, zero padded).
pub(crate) fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut w = [0u8; 4];
        for (d, s) in w.iter_mut().zip(chunk) {
            *d = *s;
        }
        sum = sum.wrapping_add(u32::from_be_bytes(w));
    }
    sum
}

/// WOFF 1.0 (W3C Recommendation): a 44-byte header, 20-byte table records, each
/// table either stored or zlib-compressed.
fn woff1(data: &[u8]) -> Result<Vec<u8>, FontError> {
    let bad = FontError::Malformed;
    let flavor = u32_at(data, 4).ok_or(FontError::Truncated)?;
    let length = u32_at(data, 8).ok_or(FontError::Truncated)? as usize;
    let num_tables = usize::from(u16_at(data, 12).ok_or(FontError::Truncated)?);
    let total_sfnt = u32_at(data, 16).ok_or(FontError::Truncated)? as usize;
    if length != data.len() {
        return Err(bad("WOFF length does not match the file"));
    }
    if num_tables == 0 || num_tables > MAX_TABLES {
        return Err(bad("WOFF table count"));
    }
    if total_sfnt > MAX_SFNT_BYTES {
        return Err(bad("WOFF sfnt size too large"));
    }
    if !matches!(flavor, SIG_TRUETYPE | SIG_TRUE) {
        return Err(if flavor == 0x4F54_544F {
            FontError::Unsupported("CFF outlines")
        } else {
            FontError::BadMagic
        });
    }
    let mut tables = Vec::with_capacity(num_tables);
    let mut budget = total_sfnt;
    let mut prev: Option<[u8; 4]> = None;
    for i in 0..num_tables {
        let rec = 44 + i * 20;
        let tag: [u8; 4] = data
            .get(rec..rec + 4)
            .and_then(|s| s.try_into().ok())
            .ok_or(FontError::Truncated)?;
        let offset = u32_at(data, rec + 4).ok_or(FontError::Truncated)? as usize;
        let comp = u32_at(data, rec + 8).ok_or(FontError::Truncated)? as usize;
        let orig = u32_at(data, rec + 12).ok_or(FontError::Truncated)? as usize;
        let sum = u32_at(data, rec + 16).ok_or(FontError::Truncated)?;
        if prev.is_some_and(|p| p >= tag) {
            return Err(bad("WOFF tables not in ascending tag order"));
        }
        prev = Some(tag);
        if comp > orig {
            return Err(bad("WOFF table larger compressed than original"));
        }
        budget = budget
            .checked_sub(orig)
            .ok_or(bad("WOFF tables exceed the declared sfnt size"))?;
        let end = offset.checked_add(comp).ok_or(FontError::Truncated)?;
        let stored = data.get(offset..end).ok_or(FontError::Truncated)?;
        let body = if comp == orig {
            stored.to_vec()
        } else {
            let v = px_image::inflate::zlib_decompress(stored, orig)
                .map_err(|_| bad("WOFF table does not inflate"))?;
            if v.len() != orig {
                return Err(bad("WOFF table inflates to the wrong size"));
            }
            v
        };
        tables.push(OutTable {
            tag,
            checksum: sum,
            data: body,
        });
    }
    build_sfnt(flavor, tables)
}

/// Brotli, exposed so [`woff2`] and tests share the one entry point.
pub(crate) fn brotli_bounded(input: &[u8], limit: usize) -> Result<Vec<u8>, FontError> {
    brotli::decompress(input, limit).map_err(|e| match e {
        brotli::BrotliError::Truncated => FontError::Truncated,
        brotli::BrotliError::Malformed(_) => FontError::Malformed("WOFF2 Brotli stream"),
        brotli::BrotliError::TooLarge => FontError::Malformed("WOFF2 Brotli output too large"),
    })
}

/// Unwrap `data` (raw sfnt, WOFF or WOFF2) and validate it as a [`Font`].
///
/// The one entry point for a web font: nothing else needs to know the container.
pub fn load_font(data: &[u8]) -> Result<Font, FontError> {
    Font::from_bytes(to_sfnt(data)?)
}
