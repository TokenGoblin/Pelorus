//! The shaper seam (ADR 036 (a), Phase 9 unit 2).
//!
//! [`Shaper`] is the trait layout will call; [`HarfrustShaper`] is the one
//! implementation, over `harfrust` (a HarfBuzz port with no `unsafe`) reading
//! the font with `skrifa`'s `read-fonts`. Swapping the library later is a
//! change to this file and a dependency ADR, not a change to layout.
//!
//! # Integers only
//!
//! Everything public here is an integer. Advances and offsets are in **font
//! units**: the font's own design grid, `units_per_em` to the em. Nothing is
//! scaled to a size inside the shaper, so the output is the same for every
//! font size and cacheable by `(font, text, script, language, direction,
//! features)`.
//!
//! # The one conversion to `Au`
//!
//! Layout converts font units to app units (`Au`, 1/60 CSS px, an `i32`) with
//! [`font_units_to_au`], once per value:
//!
//! ```text
//! au = round_half_away_from_zero(units * font_size_au / units_per_em)
//! ```
//!
//! computed in `i64` and saturated to `i32`. Layout should convert each glyph
//! advance, then sum in `Au`; summing font units first and converting the
//! total would give a different (and, for long runs, a more accurate) number
//! that no later glyph position agrees with. That choice is layout's; the
//! rounding rule is fixed here so there is exactly one.
//!
//! # Direction and order
//!
//! Glyphs come back in **visual order, left to right**, as HarfBuzz returns
//! them: for a right-to-left run the first glyph is the one drawn leftmost,
//! which is the end of the logical text. `cluster` is the UTF-8 byte offset
//! into the run's text of the first character of the glyph's cluster, so for a
//! right-to-left run the clusters decrease along the glyph list.
//!
//! # Hostile input
//!
//! Font bytes and text are untrusted. [`HarfrustShaper::new`] returns `Err`
//! for anything the font reader rejects, and shaping returns `Err` for runs and
//! feature lists over the limits below. This module contains no indexing,
//! `unwrap`, `expect` or `panic`; the dependency chain is `unsafe`-free
//! Rust.

#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::sync::Arc;

use harfrust::{Buffer, Language, ShapeOptions, ShaperFont, Tag};

/// Longest text run, in UTF-8 bytes, that [`Shaper::shape`] accepts.
///
/// 256 KiB is far past any single shaping run layout produces (a run is cut at
/// font, script, direction and style changes) and keeps glyph storage to a few
/// megabytes in the worst case.
pub const MAX_RUN_BYTES: usize = 256 * 1024;

/// Most features one request may carry.
pub const MAX_FEATURES: usize = 256;

/// Largest font file [`HarfrustShaper::new`] accepts, in bytes.
pub const MAX_FONT_BYTES: usize = 64 * 1024 * 1024;

/// Why shaping did not produce a run.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShapeError {
    /// The bytes are not a font this reader can use.
    InvalidFont,
    /// The font file is larger than [`MAX_FONT_BYTES`].
    FontTooLarge,
    /// The run is longer than [`MAX_RUN_BYTES`].
    RunTooLarge,
    /// More than [`MAX_FEATURES`] features were requested.
    TooManyFeatures,
    /// The shaper ran out of its glyph budget, which hostile text or a hostile
    /// font can provoke.
    OutOfBudget,
    /// The library refused the request (a misuse it reports as an error).
    Rejected,
}

impl fmt::Display for ShapeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ShapeError::InvalidFont => "not a usable font",
            ShapeError::FontTooLarge => "font file too large",
            ShapeError::RunTooLarge => "text run too long",
            ShapeError::TooManyFeatures => "too many features",
            ShapeError::OutOfBudget => "shaping exceeded its budget",
            ShapeError::Rejected => "shaping request rejected",
        })
    }
}

impl std::error::Error for ShapeError {}

/// The horizontal direction of one run, already resolved by bidi.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// Left to right.
    LeftToRight,
    /// Right to left.
    RightToLeft,
}

/// An ISO 15924 script tag, four ASCII bytes such as `*b"Latn"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Script(pub [u8; 4]);

impl Script {
    /// Latin.
    pub const LATIN: Script = Script(*b"Latn");
    /// Cyrillic.
    pub const CYRILLIC: Script = Script(*b"Cyrl");
    /// Arabic.
    pub const ARABIC: Script = Script(*b"Arab");
    /// Hebrew.
    pub const HEBREW: Script = Script(*b"Hebr");
    /// Devanagari.
    pub const DEVANAGARI: Script = Script(*b"Deva");
    /// Han.
    pub const HAN: Script = Script(*b"Hani");
    /// Thai.
    pub const THAI: Script = Script(*b"Thai");
    /// Common: the shaper guesses nothing and applies the default features.
    pub const COMMON: Script = Script(*b"Zyyy");
}

/// An OpenType feature setting, for example `liga` off or `smcp` on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Feature {
    /// Four-byte feature tag.
    pub tag: [u8; 4],
    /// 0 turns it off, 1 on; larger values select alternates.
    pub value: u32,
    /// First byte offset of the run text the setting covers.
    pub start: u32,
    /// One past the last byte offset it covers (`u32::MAX` for "to the end").
    pub end: u32,
}

impl Feature {
    /// A setting that covers the whole run.
    pub const fn global(tag: [u8; 4], value: u32) -> Feature {
        Feature {
            tag,
            value,
            start: 0,
            end: u32::MAX,
        }
    }
}

/// One text run to shape.
#[derive(Clone, Copy, Debug)]
pub struct ShapeInput<'a> {
    /// The text. One script, one direction, one font: the caller (bidi and
    /// itemization) has already split it.
    pub text: &'a str,
    /// The run's script.
    pub script: Script,
    /// BCP 47 language tag such as `"tr"`, `"ar"`, `"de-CH"`, or `None`.
    pub language: Option<&'a str>,
    /// The run's resolved direction.
    pub direction: Direction,
    /// Feature settings, applied on top of the font's defaults.
    pub features: &'a [Feature],
}

/// One shaped glyph. All metrics are in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    /// Glyph index in the font.
    pub glyph_id: u32,
    /// UTF-8 byte offset in [`ShapeInput::text`] of the start of this glyph's
    /// cluster.
    pub cluster: u32,
    /// Horizontal advance, after kerning.
    pub x_advance: i32,
    /// Vertical advance (zero for horizontal text).
    pub y_advance: i32,
    /// Horizontal offset from the pen position (mark attachment).
    pub x_offset: i32,
    /// Vertical offset from the pen position, positive up.
    pub y_offset: i32,
    /// True when breaking the run before this glyph's cluster would change the
    /// shaping of either side (HarfBuzz `UNSAFE_TO_BREAK`).
    pub unsafe_to_break: bool,
}

/// The result of shaping one run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShapedRun {
    /// Glyphs in visual order, left to right.
    pub glyphs: Vec<ShapedGlyph>,
    /// The font's em size in font units; the divisor of [`font_units_to_au`].
    pub units_per_em: u16,
    /// The direction the run was shaped in.
    pub direction: Direction,
}

impl ShapedRun {
    /// Sum of horizontal advances in font units.
    pub fn total_advance(&self) -> i64 {
        self.glyphs.iter().map(|g| i64::from(g.x_advance)).sum()
    }
}

/// Converts a length in font units to `Au` at `font_size_au` (the font size in
/// `Au`). See the module documentation for the rounding rule.
///
/// A zero `units_per_em` yields 0 rather than dividing by zero.
pub fn font_units_to_au(units: i32, units_per_em: u16, font_size_au: i32) -> i32 {
    if units_per_em == 0 {
        return 0;
    }
    let den = i64::from(units_per_em);
    let num = i64::from(units) * i64::from(font_size_au);
    // Round half away from zero, in integers.
    let half = den / 2;
    let q = if num >= 0 {
        (num + half) / den
    } else {
        -((-num + half) / den)
    };
    i32::try_from(q).unwrap_or(if q < 0 { i32::MIN } else { i32::MAX })
}

/// The shaping seam layout depends on.
pub trait Shaper {
    /// The font's em size in font units.
    fn units_per_em(&self) -> u16;

    /// Shapes one run.
    ///
    /// # Errors
    ///
    /// Returns `Err` for runs and feature lists over the limits, and when the
    /// library gives up. Never panics, whatever the text or font bytes.
    fn shape(&self, input: &ShapeInput<'_>) -> Result<ShapedRun, ShapeError>;
}

/// [`Shaper`] over `harfrust`.
///
/// Holds one parsed font. Cheap to clone (the font data is shared).
#[derive(Clone)]
pub struct HarfrustShaper {
    font: skrifa::Font,
}

impl fmt::Debug for HarfrustShaper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HarfrustShaper")
            .field("units_per_em", &self.font.units_per_em())
            .finish_non_exhaustive()
    }
}

impl HarfrustShaper {
    /// Reads the font at `index` (0 unless the file is a collection).
    ///
    /// # Errors
    ///
    /// [`ShapeError::FontTooLarge`] over [`MAX_FONT_BYTES`];
    /// [`ShapeError::InvalidFont`] when the bytes are not a supported font, or
    /// declare a zero-sized em square.
    pub fn new(bytes: Arc<[u8]>, index: u32) -> Result<Self, ShapeError> {
        if bytes.len() > MAX_FONT_BYTES {
            return Err(ShapeError::FontTooLarge);
        }
        let shared: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
        let font = skrifa::Font::new(shared, index).ok_or(ShapeError::InvalidFont)?;
        if font.units_per_em() == 0 {
            return Err(ShapeError::InvalidFont);
        }
        Ok(HarfrustShaper { font })
    }
}

impl Shaper for HarfrustShaper {
    fn units_per_em(&self) -> u16 {
        self.font.units_per_em()
    }

    fn shape(&self, input: &ShapeInput<'_>) -> Result<ShapedRun, ShapeError> {
        if input.text.len() > MAX_RUN_BYTES {
            return Err(ShapeError::RunTooLarge);
        }
        if input.features.len() > MAX_FEATURES {
            return Err(ShapeError::TooManyFeatures);
        }
        let direction = input.direction;
        let units_per_em = self.font.units_per_em();
        if input.text.is_empty() {
            return Ok(ShapedRun {
                glyphs: Vec::new(),
                units_per_em,
                direction,
            });
        }

        let mut buffer = Buffer::new();
        buffer.push_str(input.text);
        buffer.set_direction(match direction {
            Direction::LeftToRight => harfrust::Direction::LeftToRight,
            Direction::RightToLeft => harfrust::Direction::RightToLeft,
        });
        buffer.set_script(harfrust::Script::from_iso15924_tag(Tag::new(
            &input.script.0,
        )));
        buffer.set_language(input.language.and_then(Language::new));

        let features: Vec<harfrust::Feature> = input
            .features
            .iter()
            .map(|f| harfrust::Feature {
                tag: Tag::new(&f.tag),
                value: f.value,
                start: f.start,
                end: f.end,
            })
            .collect();

        let shaper_font = ShaperFont::new(&self.font);
        harfrust::shape(
            &shaper_font,
            &mut buffer,
            ShapeOptions::new().features(&features),
        )
        .map_err(|_| ShapeError::Rejected)?;
        if !buffer.allocation_successful() {
            return Err(ShapeError::OutOfBudget);
        }

        let infos = buffer.glyph_infos();
        let positions = buffer.glyph_positions();
        let glyphs = infos
            .iter()
            .zip(positions.iter())
            .map(|(info, pos)| ShapedGlyph {
                glyph_id: info.glyph_id,
                cluster: info.cluster,
                x_advance: pos.x_advance,
                y_advance: pos.y_advance,
                x_offset: pos.x_offset,
                y_offset: pos.y_offset,
                unsafe_to_break: info.unsafe_to_break(),
            })
            .collect();
        Ok(ShapedRun {
            glyphs,
            units_per_em,
            direction,
        })
    }
}

#[cfg(test)]
mod synthetic;
#[cfg(test)]
mod tests;
