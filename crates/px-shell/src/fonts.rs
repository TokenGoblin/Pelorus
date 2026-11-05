//! Fonts: the bundled Noto Sans through `px-layout`'s registry, behind the
//! chrome's `TextShaper` and the rasterizer's `GlyphSource`. The page and the
//! chrome use one registry, so one `GlyphSource` serves both. Advances only (no
//! shaping) for the chrome, like the px-ui render baselines.

use px_paint::FontId;
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::GlyphSource;
use px_ui::text::{AU_PER_PX, FontMetrics, Shaped, ShapedGlyph, TextShaper};

/// Glyph outlines from the registry layout shaped with. Layout's `FontId`
/// numbers are the display list's.
pub struct LayoutFonts;

impl GlyphSource for LayoutFonts {
    fn units_per_em(&self, font: FontId) -> Option<u32> {
        px_layout::text::font_of(px_text::FontId(font.0)).map(|f| u32::from(f.units_per_em()))
    }

    fn outline(&self, font: FontId, glyph: u32) -> Option<Vec<PathCmd>> {
        let f = px_layout::text::font_of(px_text::FontId(font.0))?;
        let o = f.outline(u16::try_from(glyph).ok()?).ok()?;
        let p = |p: px_text::Pt| Pt { x: p.x, y: p.y };
        Some(
            o.commands
                .iter()
                .map(|c| match *c {
                    px_text::PathCmd::MoveTo(a) => PathCmd::MoveTo(p(a)),
                    px_text::PathCmd::LineTo(a) => PathCmd::LineTo(p(a)),
                    px_text::PathCmd::QuadTo(a, b) => PathCmd::QuadTo(p(a), p(b)),
                    px_text::PathCmd::Close => PathCmd::Close,
                })
                .collect(),
        )
    }
}

/// The chrome's text: Noto Sans from the shared registry.
pub struct ChromeFont {
    id: px_text::FontId,
}

impl Default for ChromeFont {
    fn default() -> Self {
        ChromeFont::new()
    }
}

impl ChromeFont {
    pub fn new() -> ChromeFont {
        ChromeFont {
            id: px_layout::text::resolve_font(["Noto Sans"]),
        }
    }

    fn scale(&self, units: i32, size_px: i32) -> i32 {
        let upem = px_layout::text::font_of(self.id).map_or(1000, |f| i64::from(f.units_per_em()));
        let upem = upem.max(1);
        let v = i64::from(units) * i64::from(size_px) * i64::from(AU_PER_PX);
        ((v * 2 + upem) / (2 * upem)) as i32
    }
}

impl TextShaper for ChromeFont {
    fn shape(&self, text: &str, size_px: i32) -> Shaped {
        let font = px_layout::text::font_of(self.id);
        let mut glyphs = Vec::new();
        let mut x = 0;
        for (i, c) in text.char_indices() {
            let g = font.and_then(|f| f.glyph_index(c)).unwrap_or(0);
            glyphs.push(ShapedGlyph {
                glyph_id: u32::from(g),
                x,
                cluster: i,
            });
            let adv = font.and_then(|f| f.advance(g)).unwrap_or(0);
            x += self.scale(i32::from(adv), size_px);
        }
        Shaped {
            font: FontId(self.id.0),
            size_px,
            glyphs,
            width: x,
        }
    }

    fn metrics(&self, size_px: i32) -> FontMetrics {
        let (a, d) = px_layout::text::font_of(self.id).map_or((800, 200), |f| {
            (i32::from(f.ascent()), -i32::from(f.descent()))
        });
        FontMetrics {
            ascent: self.scale(a, size_px),
            descent: self.scale(d, size_px),
        }
    }
}
