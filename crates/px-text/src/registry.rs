//! A small font registry: `font-family` name to [`Font`].
//!
//! Family matching is ASCII case-insensitive on the whole name (CSS Fonts 4 section
//! 5.1 compares family names case-insensitively; this registry holds exact names,
//! so no further normalisation is attempted). Later registrations of the same
//! family do not replace earlier ones: the first wins, so a bundled font cannot be
//! shadowed by a later registration of the same name.
//!
//! # The bundled set and the fallback order
//!
//! [`FontRegistry::with_bundled`] registers, in this order and under these stable
//! family names, the only fonts the deterministic render corpus ever uses (all
//! committed under `tests/fonts`, provenance and licences in `PROVENANCE`):
//!
//! | id | family | covers |
//! |---|---|---|
//! | 0 | `Ahem` | the WPT test font; **never** a fallback, only when named |
//! | 1 | `Noto Sans` | Latin, Greek, Cyrillic, and the shared digits and punctuation |
//! | 2 | `Noto Sans Hebrew` | Hebrew and its points |
//! | 3 | `Noto Sans Arabic` | Arabic |
//! | 4 | `Noto Sans Devanagari` | Devanagari |
//!
//! The ids are stable: Ahem stays 0 so existing layouts that default to
//! `FontId(0)` are unchanged. A CJK face, when one is bundled, is appended (so the
//! ids above do not move) and added to [`FALLBACK_ORDER`] last.
//!
//! [`FontRegistry::fallback_for`] picks a face for a character from the families
//! the author listed (in order), then from [`FALLBACK_ORDER`]. **For a codepoint
//! several faces map (digits, ASCII punctuation, space) the first face in that
//! order wins, which is Noto Sans.** The choice is a pure function of the
//! character and the lists, so it is the same on every machine. System fonts are
//! never in this registry (see `sysfonts`); a test pins that.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::font::{Font, FontId};
use crate::sfnt::FontError;

/// The Ahem test font, committed verbatim (`tests/fonts/PROVENANCE`).
pub const AHEM_BYTES: &[u8] = include_bytes!("../../../tests/fonts/Ahem.ttf");
/// Noto Sans Regular 2.015 (Latin, Greek, Cyrillic), OFL 1.1.
pub const NOTO_SANS_BYTES: &[u8] = include_bytes!("../../../tests/fonts/NotoSans-Regular.ttf");
/// Noto Sans Hebrew Regular 3.001, OFL 1.1.
pub const NOTO_SANS_HEBREW_BYTES: &[u8] =
    include_bytes!("../../../tests/fonts/NotoSansHebrew-Regular.ttf");
/// Noto Sans Arabic Regular 2.013, OFL 1.1.
pub const NOTO_SANS_ARABIC_BYTES: &[u8] =
    include_bytes!("../../../tests/fonts/NotoSansArabic-Regular.ttf");
/// Noto Sans Devanagari Regular 2.007, OFL 1.1.
pub const NOTO_SANS_DEVANAGARI_BYTES: &[u8] =
    include_bytes!("../../../tests/fonts/NotoSansDevanagari-Regular.ttf");

/// Family names of the bundled faces, in registration order (their `FontId`s).
pub const BUNDLED_FAMILIES: [&str; 5] = [
    "Ahem",
    "Noto Sans",
    "Noto Sans Hebrew",
    "Noto Sans Arabic",
    "Noto Sans Devanagari",
];

/// The faces tried, in this order, for a character no listed family covers.
/// Ahem is deliberately absent.
pub const FALLBACK_ORDER: [&str; 4] = [
    "Noto Sans",
    "Noto Sans Hebrew",
    "Noto Sans Arabic",
    "Noto Sans Devanagari",
];

/// Most fonts one registry holds.
pub const MAX_FONTS: usize = 4096;

/// Most characters the fallback cache remembers. When full it stops inserting;
/// the answer never depends on the cache's contents.
pub const FALLBACK_CACHE_CAP: usize = 4096;

#[derive(Debug, Default)]
pub struct FontRegistry {
    fonts: Vec<(String, Font)>,
    /// `char` to the face [`FALLBACK_ORDER`] alone would pick.
    cache: Mutex<HashMap<char, Option<FontId>>>,
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

    /// Ahem plus the Noto faces, in the order documented on this module.
    pub fn with_bundled() -> Result<FontRegistry, FontError> {
        let mut r = FontRegistry::new();
        let faces: [(&str, &[u8]); 5] = [
            (BUNDLED_FAMILIES[0], AHEM_BYTES),
            (BUNDLED_FAMILIES[1], NOTO_SANS_BYTES),
            (BUNDLED_FAMILIES[2], NOTO_SANS_HEBREW_BYTES),
            (BUNDLED_FAMILIES[3], NOTO_SANS_ARABIC_BYTES),
            (BUNDLED_FAMILIES[4], NOTO_SANS_DEVANAGARI_BYTES),
        ];
        for (family, bytes) in faces {
            r.register(family, Font::from_bytes(bytes)?)
                .ok_or(FontError::TooComplex)?;
        }
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

    /// The family `id` was registered under.
    pub fn family(&self, id: FontId) -> Option<&str> {
        self.fonts
            .get(usize::try_from(id.0).ok()?)
            .map(|(n, _)| n.as_str())
    }

    /// Every registered family name, in registration (id) order.
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.fonts.iter().map(|(n, _)| n.as_str())
    }

    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }

    /// Whether `id` maps `c` to a glyph other than `.notdef`.
    pub fn covers(&self, id: FontId, c: char) -> bool {
        self.get(id).is_some_and(|f| f.glyph_index(c).is_some())
    }

    /// The face that should draw `c`: the first of `families` (in order, by name)
    /// whose `cmap` maps it, else the first of [`FALLBACK_ORDER`] that does, else
    /// `None`. Unregistered names in `families` are skipped. Pure: the same
    /// registry, character and lists give the same answer every time; the cache
    /// only remembers answers for the [`FALLBACK_ORDER`] stage.
    pub fn fallback_for(&self, c: char, families: &[&str]) -> Option<FontId> {
        for name in families {
            if let Some(id) = self.resolve(name)
                && self.covers(id, c)
            {
                return Some(id);
            }
        }
        if let Ok(mut cache) = self.cache.lock() {
            if let Some(hit) = cache.get(&c) {
                return *hit;
            }
            let found = self.scan_fallback_order(c);
            if cache.len() < FALLBACK_CACHE_CAP {
                cache.insert(c, found);
            }
            return found;
        }
        self.scan_fallback_order(c)
    }

    fn scan_fallback_order(&self, c: char) -> Option<FontId> {
        FALLBACK_ORDER
            .iter()
            .filter_map(|name| self.resolve(name))
            .find(|&id| self.covers(id, c))
    }
}
