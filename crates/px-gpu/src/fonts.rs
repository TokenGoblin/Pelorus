//! The fonts a rasterizer draws glyph runs with: [`FontSet`], a
//! [`GlyphSource`] over parsed `px-text` fonts, keyed by the id the display
//! list uses.
//!
//! Both ends of the boundary build one from the same bytes: the GPU process
//! from the chunks the broker sent it, and the session (for its software
//! fallback) from the bytes it registered. Parsing is `px_text::Font::from_bytes`,
//! which never panics whatever the bytes; a font it refuses is
//! [`FontRejected`], and in the GPU process that ends the process.

use std::collections::BTreeMap;
use std::sync::Arc;

use px_paint::FontId;
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::GlyphSource;
use px_text::{Font, PathCmd as TextCmd};

/// A font's bytes did not parse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontRejected;

/// Parsed fonts by display-list id.
#[derive(Debug, Default)]
pub struct FontSet {
    fonts: BTreeMap<u32, Font>,
}

impl FontSet {
    /// No fonts.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse `bytes` and hold it as `id`, replacing nothing: an id already
    /// present is refused, as the wire refuses a duplicate.
    pub fn insert(&mut self, id: u32, bytes: impl Into<Arc<[u8]>>) -> Result<(), FontRejected> {
        if self.fonts.contains_key(&id) {
            return Err(FontRejected);
        }
        let bytes: Arc<[u8]> = bytes.into();
        let font = Font::from_bytes(bytes).map_err(|_| FontRejected)?;
        self.fonts.insert(id, font);
        Ok(())
    }

    /// Whether `id` is held.
    pub fn contains(&self, id: u32) -> bool {
        self.fonts.contains_key(&id)
    }

    /// How many fonts are held.
    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    /// Whether none are.
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

fn pt(p: px_text::Pt) -> Pt {
    Pt { x: p.x, y: p.y }
}

impl GlyphSource for FontSet {
    fn units_per_em(&self, font: FontId) -> Option<u32> {
        self.fonts.get(&font.0).map(|f| u32::from(f.units_per_em()))
    }

    fn outline(&self, font: FontId, glyph: u32) -> Option<Vec<PathCmd>> {
        let outline = self
            .fonts
            .get(&font.0)?
            .outline(u16::try_from(glyph).ok()?)
            .ok()?;
        Some(
            outline
                .commands
                .iter()
                .map(|c| match *c {
                    TextCmd::MoveTo(a) => PathCmd::MoveTo(pt(a)),
                    TextCmd::LineTo(a) => PathCmd::LineTo(pt(a)),
                    TextCmd::QuadTo(a, b) => PathCmd::QuadTo(pt(a), pt(b)),
                    TextCmd::Close => PathCmd::Close,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_text::registry::AHEM_BYTES;

    #[test]
    fn ahem_parses_and_garbage_does_not() {
        let mut set = FontSet::new();
        assert_eq!(set.insert(1, AHEM_BYTES.to_vec()), Ok(()));
        assert_eq!(set.insert(1, AHEM_BYTES.to_vec()), Err(FontRejected));
        assert_eq!(set.insert(2, vec![0xAB; 4096]), Err(FontRejected));
        assert_eq!(set.insert(3, Vec::new()), Err(FontRejected));
        assert_eq!(set.len(), 1);
        assert!(set.contains(1) && !set.contains(2));
        assert_eq!(set.units_per_em(FontId(1)), Some(1000));
        assert_eq!(set.units_per_em(FontId(2)), None);
        assert!(set.outline(FontId(1), 60_000).is_none());
        assert!(set.outline(FontId(1), u32::MAX).is_none());
        assert!(set.outline(FontId(9), 1).is_none());
    }
}
