//! The sfnt container: bounds-checked big-endian reads and the table directory.
//!
//! Every function here takes a byte slice that may be hostile (a truncated or
//! corrupted font) and returns `Option` or `Result`. Nothing indexes a slice and
//! nothing can overflow: offsets are added with `checked_add`, and a read that does
//! not fit returns `None`.

use std::fmt;

/// Why a font could not be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontError {
    /// Bytes ended before a structure did, or an offset points outside the file.
    Truncated,
    /// Not an sfnt at all.
    BadMagic,
    /// A recognised but unsupported format (CFF2, `endchar` accent composition).
    Unsupported(&'static str),
    /// A required table is absent.
    MissingTable(&'static str),
    /// A table's contents contradict themselves or the spec.
    Malformed(&'static str),
    /// A composite glyph nests deeper or is larger than the documented caps.
    TooComplex,
    /// A glyph id at or beyond `maxp.numGlyphs`.
    NoSuchGlyph,
}

impl fmt::Display for FontError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Truncated => f.write_str("font data truncated"),
            Self::BadMagic => f.write_str("not a TrueType sfnt"),
            Self::Unsupported(what) => write!(f, "unsupported font feature: {what}"),
            Self::MissingTable(tag) => write!(f, "missing table {tag}"),
            Self::Malformed(why) => write!(f, "malformed font: {why}"),
            Self::TooComplex => f.write_str("glyph outline exceeds the size caps"),
            Self::NoSuchGlyph => f.write_str("glyph id out of range"),
        }
    }
}

impl std::error::Error for FontError {}

/// Big-endian `u8` at `offset`.
pub(crate) fn u8_at(data: &[u8], offset: usize) -> Option<u8> {
    data.get(offset).copied()
}

/// Big-endian `u16` at `offset`.
pub(crate) fn u16_at(data: &[u8], offset: usize) -> Option<u16> {
    let end = offset.checked_add(2)?;
    data.get(offset..end)?
        .try_into()
        .ok()
        .map(u16::from_be_bytes)
}

/// Big-endian `i16` at `offset`.
pub(crate) fn i16_at(data: &[u8], offset: usize) -> Option<i16> {
    u16_at(data, offset).map(|v| v as i16)
}

/// Big-endian `u32` at `offset`.
pub(crate) fn u32_at(data: &[u8], offset: usize) -> Option<u32> {
    let end = offset.checked_add(4)?;
    data.get(offset..end)?
        .try_into()
        .ok()
        .map(u32::from_be_bytes)
}

/// A byte range inside the font file, already checked to lie within it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Span {
    pub offset: usize,
    pub len: usize,
}

impl Span {
    /// The bytes of this span in `data`, or an empty slice if (impossibly, after
    /// validation) it does not fit.
    pub(crate) fn slice(self, data: &[u8]) -> &[u8] {
        let end = self.offset.saturating_add(self.len);
        data.get(self.offset..end).unwrap_or(&[])
    }
}

/// `sfnt` version for TrueType outlines.
const VERSION_TRUETYPE: u32 = 0x0001_0000;
/// The tag `true`, which Apple fonts use instead.
const VERSION_TRUE: u32 = 0x7472_7565;
const VERSION_OTTO: u32 = 0x4F54_544F;
const VERSION_TTCF: u32 = 0x7474_6366;
/// Most faces a `ttcf` header may declare.
pub const MAX_COLLECTION_FACES: u32 = 256;

/// The table directory: tag and span of each table.
#[derive(Clone, Debug, Default)]
pub(crate) struct Directory {
    entries: Vec<([u8; 4], Span)>,
}

impl Directory {
    /// The table directory of the first face (a collection's first font, or the
    /// only font of a plain file).
    pub(crate) fn parse(data: &[u8]) -> Result<Directory, FontError> {
        Self::parse_face(data, 0)
    }

    /// Every table, in directory order.
    pub(crate) fn tables(&self) -> &[([u8; 4], Span)] {
        &self.entries
    }

    /// The table directory of face `index`. A plain sfnt has one face (index 0);
    /// a `ttcf` collection has up to [`MAX_COLLECTION_FACES`].
    pub(crate) fn parse_face(data: &[u8], index: u32) -> Result<Directory, FontError> {
        let mut base = 0usize;
        let mut version = u32_at(data, 0).ok_or(FontError::Truncated)?;
        if version == VERSION_TTCF {
            let count = u32_at(data, 8).ok_or(FontError::Truncated)?;
            if count == 0 || count > MAX_COLLECTION_FACES {
                return Err(FontError::Malformed("collection face count"));
            }
            if index >= count {
                return Err(FontError::Malformed("collection face index"));
            }
            let at = (index as usize)
                .checked_mul(4)
                .and_then(|o| o.checked_add(12))
                .ok_or(FontError::Truncated)?;
            base = u32_at(data, at).ok_or(FontError::Truncated)? as usize;
            version = u32_at(data, base).ok_or(FontError::Truncated)?;
        } else if index != 0 {
            return Err(FontError::Malformed("collection face index"));
        }
        match version {
            VERSION_TRUETYPE | VERSION_TRUE | VERSION_OTTO => {}
            _ => return Err(FontError::BadMagic),
        }
        let count_at = base.checked_add(4).ok_or(FontError::Truncated)?;
        let count = usize::from(u16_at(data, count_at).ok_or(FontError::Truncated)?);
        let mut entries = Vec::with_capacity(count.min(64));
        for i in 0..count {
            let record = i
                .checked_mul(16)
                .and_then(|o| o.checked_add(base))
                .and_then(|o| o.checked_add(12))
                .ok_or(FontError::Truncated)?;
            let tag_end = record.checked_add(4).ok_or(FontError::Truncated)?;
            let tag: [u8; 4] = data
                .get(record..tag_end)
                .ok_or(FontError::Truncated)?
                .try_into()
                .map_err(|_| FontError::Truncated)?;
            let offset =
                u32_at(data, record.saturating_add(8)).ok_or(FontError::Truncated)? as usize;
            let len = u32_at(data, record.saturating_add(12)).ok_or(FontError::Truncated)? as usize;
            let end = offset.checked_add(len).ok_or(FontError::Truncated)?;
            if end > data.len() {
                return Err(FontError::Truncated);
            }
            entries.push((tag, Span { offset, len }));
        }
        Ok(Directory { entries })
    }

    /// The first table with this tag.
    pub(crate) fn find(&self, tag: &[u8; 4]) -> Option<Span> {
        self.entries.iter().find(|(t, _)| t == tag).map(|(_, s)| *s)
    }

    /// Like [`Directory::find`] but an error naming the table when absent.
    pub(crate) fn require(&self, tag: &'static str) -> Result<Span, FontError> {
        let bytes: [u8; 4] = tag
            .as_bytes()
            .try_into()
            .map_err(|_| FontError::MissingTable(tag))?;
        self.find(&bytes).ok_or(FontError::MissingTable(tag))
    }
}
