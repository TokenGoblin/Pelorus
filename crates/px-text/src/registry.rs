//! A small font registry: `font-family` name to [`Font`].
//!
//! Family matching is ASCII case-insensitive on the whole name (CSS Fonts 4 section
//! 5.1 compares family names case-insensitively; this registry holds exact names,
//! so no further normalisation is attempted). Later registrations of the same
//! family do not replace earlier ones: the first wins, so a bundled font cannot be
//! shadowed by a later registration of the same name.

use crate::font::{Font, FontId};
use crate::sfnt::FontError;

/// The Ahem test font, committed verbatim (`tests/fonts/PROVENANCE`).
pub const AHEM_BYTES: &[u8] = include_bytes!("../../../tests/fonts/Ahem.ttf");

/// Most fonts one registry holds.
pub const MAX_FONTS: usize = 4096;

#[derive(Debug, Default)]
pub struct FontRegistry {
    fonts: Vec<(String, Font)>,
}

impl FontRegistry {
    pub fn new() -> FontRegistry {
        FontRegistry::default()
    }

    /// A registry holding Ahem under the family `Ahem`.
    pub fn with_ahem() -> Result<FontRegistry, FontError> {
        let mut r = FontRegistry::new();
        r.register("Ahem", Font::from_bytes(AHEM_BYTES)?)
            .ok_or(FontError::TooComplex)?;
        Ok(r)
    }

    /// Add `font` under `family`; `None` if the registry is full.
    pub fn register(&mut self, family: &str, font: Font) -> Option<FontId> {
        if self.fonts.len() >= MAX_FONTS {
            return None;
        }
        let id = FontId(u32::try_from(self.fonts.len()).ok()?);
        self.fonts.push((family.to_owned(), font));
        Some(id)
    }

    /// The first font registered under `family` (ASCII case-insensitive).
    pub fn resolve(&self, family: &str) -> Option<FontId> {
        let i = self
            .fonts
            .iter()
            .position(|(name, _)| name.eq_ignore_ascii_case(family))?;
        u32::try_from(i).ok().map(FontId)
    }

    pub fn get(&self, id: FontId) -> Option<&Font> {
        self.fonts.get(usize::try_from(id.0).ok()?).map(|(_, f)| f)
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}
