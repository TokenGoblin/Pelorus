//! The demo's font stack: the bundled Noto Sans through `px-text`, behind the
//! chrome's `TextShaper` and the rasterizer's `GlyphSource`. Advances only (no
//! kerning, no shaping), like the px-ui render baselines.

use px_paint::FontId;
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::GlyphSource;
use px_text::Font;
use px_ui::text::{AU_PER_PX, FontMetrics, Shaped, ShapedGlyph, TextShaper};

pub struct Noto {
    font: Font,
}

impl Noto {
    /// `PX_DEMO_FONT` names a TTF; otherwise the repository's bundled one.
    pub fn load() -> Result<Noto, String> {
        let path = std::env::var("PX_DEMO_FONT").unwrap_or_else(|_| {
            concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/fonts/NotoSans-Regular.ttf").into()
        });
        let bytes = std::fs::read(&path).map_err(|e| format!("cannot read font {path}: {e}"))?;
        let font = Font::from_bytes(bytes).map_err(|e| format!("bad font {path}: {e:?}"))?;
        Ok(Noto { font })
    }

    fn scale(&self, units: i32, size_px: i32) -> i32 {
        let upem = i64::from(self.font.units_per_em()).max(1);
        let v = i64::from(units) * i64::from(size_px) * i64::from(AU_PER_PX);
        ((v * 2 + upem) / (2 * upem)) as i32
    }
}

impl TextShaper for Noto {
    fn shape(&self, text: &str, size_px: i32) -> Shaped {
        let mut glyphs = Vec::new();
        let mut x = 0;
        for (i, c) in text.char_indices() {
            let g = self.font.glyph_index(c).unwrap_or(0);
            glyphs.push(ShapedGlyph {
                glyph_id: u32::from(g),
                x,
                cluster: i,
            });
            x += self.scale(i32::from(self.font.advance(g).unwrap_or(0)), size_px);
        }
        Shaped {
            font: FontId(0),
            size_px,
            glyphs,
            width: x,
        }
    }

    fn metrics(&self, size_px: i32) -> FontMetrics {
        FontMetrics {
            ascent: self.scale(i32::from(self.font.ascent()), size_px),
            descent: self.scale(-i32::from(self.font.descent()), size_px),
        }
    }
}

impl GlyphSource for Noto {
    fn units_per_em(&self, _font: FontId) -> Option<u32> {
        Some(u32::from(self.font.units_per_em()))
    }

    fn outline(&self, _font: FontId, glyph: u32) -> Option<Vec<PathCmd>> {
        let o = self.font.outline(u16::try_from(glyph).ok()?).ok()?;
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
