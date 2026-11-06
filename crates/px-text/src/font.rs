//! [`Font`]: a validated handle over the bytes of one TrueType or CFF/OpenType font.
//!
//! [`Font::from_bytes`] checks every table it will later read (the table directory,
//! `head`, `hhea`, `maxp`, that `hmtx`, `cmap` and the outline tables are large
//! enough) so the accessors can only fail by returning `None` or an error, never by
//! reading out of bounds. All metrics are integers in font units; scaling to pixels
//! or `Au` is the caller's one documented step.
//!
//! Outlines come from `glyf` (TrueType) or `CFF ` (Type 2 charstrings, see
//! [`crate::cff`]). A `ttcf` collection reads as its first face; use
//! [`crate::woff::to_sfnt_face`] to pick another.

use std::sync::Arc;

use crate::cff::Cff;
use crate::cmap::Cmap;
use crate::glyf::{GlyfTables, Outline};
use crate::sfnt::{Directory, FontError, Span, i16_at, u16_at, u32_at};

/// Identifies a font within a [`crate::registry::FontRegistry`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontId(pub u32);

/// `head.magicNumber`.
const HEAD_MAGIC: u32 = 0x5F0F_3CF5;

/// Where a font's outlines live.
#[derive(Clone, Debug)]
enum Outlines {
    Glyf {
        glyf: Span,
        loca: Span,
        long_loca: bool,
    },
    Cff(Cff),
}

/// A parsed TrueType or CFF/OpenType font. Cloning shares the bytes.
#[derive(Clone, Debug)]
pub struct Font {
    data: Arc<[u8]>,
    cmap: Cmap,
    outlines: Outlines,
    hmtx: Span,
    units_per_em: u16,
    ascent: i16,
    descent: i16,
    line_gap: i16,
    num_glyphs: u16,
    num_hmetrics: u16,
}

impl Font {
    /// Validate and wrap `data`. Never panics, whatever the bytes.
    pub fn from_bytes(data: impl Into<Arc<[u8]>>) -> Result<Font, FontError> {
        let data: Arc<[u8]> = data.into();
        let dir = Directory::parse(&data)?;
        let head = dir.require("head")?;
        let hhea = dir.require("hhea")?;
        let maxp = dir.require("maxp")?;
        let hmtx = dir.require("hmtx")?;
        let cmap_span = dir.require("cmap")?;
        let has_glyf = dir.find(b"glyf").is_some();
        if !has_glyf && dir.find(b"CFF2").is_some() {
            return Err(FontError::Unsupported("CFF2 outlines"));
        }

        let h = head.slice(&data);
        if head.len < 54 || u32_at(h, 12) != Some(HEAD_MAGIC) {
            return Err(FontError::Malformed("head"));
        }
        let units_per_em = u16_at(h, 18).ok_or(FontError::Truncated)?;
        if !(16..=16384).contains(&units_per_em) {
            return Err(FontError::Malformed("unitsPerEm out of range"));
        }

        let hh = hhea.slice(&data);
        if hhea.len < 36 {
            return Err(FontError::Malformed("hhea"));
        }
        let ascent = i16_at(hh, 4).ok_or(FontError::Truncated)?;
        let descent = i16_at(hh, 6).ok_or(FontError::Truncated)?;
        let line_gap = i16_at(hh, 8).ok_or(FontError::Truncated)?;
        let num_hmetrics = u16_at(hh, 34).ok_or(FontError::Truncated)?;

        let num_glyphs = u16_at(maxp.slice(&data), 4).ok_or(FontError::Truncated)?;
        if num_glyphs == 0 || num_hmetrics == 0 || num_hmetrics > num_glyphs {
            return Err(FontError::Malformed("glyph counts"));
        }
        if hmtx.len < usize::from(num_hmetrics) * 4 {
            return Err(FontError::Malformed("hmtx too short"));
        }
        let outlines = if has_glyf {
            let loca = dir.require("loca")?;
            let glyf = dir.require("glyf")?;
            let long_loca = match i16_at(h, 50) {
                Some(0) => false,
                Some(1) => true,
                _ => return Err(FontError::Malformed("indexToLocFormat")),
            };
            let loca_entry = if long_loca { 4 } else { 2 };
            if loca.len < (usize::from(num_glyphs) + 1) * loca_entry {
                return Err(FontError::Malformed("loca too short"));
            }
            Outlines::Glyf {
                glyf,
                loca,
                long_loca,
            }
        } else {
            let cff = Cff::parse(&data, dir.require("CFF ")?)?;
            if cff.num_glyphs() < usize::from(num_glyphs) {
                return Err(FontError::Malformed("CharStrings shorter than maxp"));
            }
            Outlines::Cff(cff)
        };
        let cmap = Cmap::parse(&data, cmap_span)?;
        Ok(Font {
            data,
            cmap,
            outlines,
            hmtx,
            units_per_em,
            ascent,
            descent,
            line_gap,
            num_glyphs,
            num_hmetrics,
        })
    }

    /// The raw font bytes.
    pub fn bytes(&self) -> &Arc<[u8]> {
        &self.data
    }

    /// `head.unitsPerEm`, in `16..=16384`.
    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// `hhea.ascender` in font units (positive above the baseline).
    pub fn ascent(&self) -> i16 {
        self.ascent
    }

    /// `hhea.descender` in font units (negative below the baseline).
    pub fn descent(&self) -> i16 {
        self.descent
    }

    /// `hhea.lineGap` in font units.
    pub fn line_gap(&self) -> i16 {
        self.line_gap
    }

    /// `maxp.numGlyphs`.
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// The glyph for `c`, or `None` if the font does not map it.
    pub fn glyph_index(&self, c: char) -> Option<u16> {
        self.cmap
            .glyph(&self.data, c)
            .filter(|&g| g < self.num_glyphs)
    }

    /// The horizontal advance of `glyph` in font units, or `None` for a glyph id
    /// outside the font. Glyphs past `numberOfHMetrics` share the last advance.
    pub fn advance(&self, glyph: u16) -> Option<u16> {
        if glyph >= self.num_glyphs {
            return None;
        }
        let hmtx = self.hmtx.slice(&self.data);
        let index = glyph.min(self.num_hmetrics.checked_sub(1)?);
        u16_at(hmtx, usize::from(index) * 4)
    }

    /// The outline of `glyph` in font units. CFF cubics come back as the
    /// quadratics [`crate::cff`] documents.
    pub fn outline(&self, glyph: u16) -> Result<Outline, FontError> {
        match &self.outlines {
            Outlines::Glyf {
                glyf,
                loca,
                long_loca,
            } => GlyfTables {
                glyf: glyf.slice(&self.data),
                loca: loca.slice(&self.data),
                long_loca: *long_loca,
                num_glyphs: self.num_glyphs,
            }
            .outline(glyph),
            Outlines::Cff(cff) => {
                if glyph >= self.num_glyphs {
                    return Err(FontError::NoSuchGlyph);
                }
                cff.glyph(&self.data, usize::from(glyph)).map(|(o, _)| o)
            }
        }
    }

    /// `true` when the outlines are CFF charstrings rather than `glyf`.
    pub fn is_cff(&self) -> bool {
        matches!(self.outlines, Outlines::Cff(_))
    }

    /// The width a CFF charstring declares for `glyph` (`None` for a `glyf` font
    /// or a bad glyph). Layout uses `hmtx` ([`Font::advance`]); this is for checks.
    pub fn charstring_width(&self, glyph: u16) -> Option<i32> {
        match &self.outlines {
            Outlines::Cff(cff) if glyph < self.num_glyphs => cff
                .glyph(&self.data, usize::from(glyph))
                .ok()
                .map(|(_, w)| w),
            _ => None,
        }
    }
}
