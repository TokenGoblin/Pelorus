//! Fonts: the bundled Noto Sans through `px-layout`'s registry, behind the
//! chrome's `TextShaper` and the rasterizer's `GlyphSource`. The page and the
//! chrome use one registry, so one `GlyphSource` serves both. Advances only (no
//! shaping) for the chrome, like the px-ui render baselines.
//!
//! # Web fonts (ADR 049)
//!
//! A page's `@font-face` rules are read from its stylesheets ([`crate::page::font_plan`]),
//! each `url()` source is fetched ([`load_web`] through `px-net`, [`load_local`]
//! next to a local page's file), unwrapped and validated by `px-text` (TTF,
//! WOFF, WOFF2), registered in `px-layout::fontset` and handed to layout as
//! [`Resources::fonts`]. Limits: at most [`px_layout::fontset::MAX_FACE_RULES`]
//! rules, [`px_net::browse::MAX_FONT_BYTES`] per file, [`PHASE_BUDGET`] for the
//! whole phase.
//!
//! **First paint is never blocked indefinitely and the outcome is deterministic.**
//! The page is laid out once, after the font phase, so `font-display` reduces to
//! "is the font in hand by then": a font that has not arrived within
//! [`PHASE_BUDGET`] (the 3 s block period of CSS Fonts 4) is dropped and text
//! uses the next family in its `font-family` list; `font-display: optional` is
//! stricter, a font that took longer than [`OPTIONAL_BUDGET`] (100 ms, its block
//! period) is dropped too. Nothing is swapped in later, so no page ever relays
//! out on a late font. A failed, refused (CSP `font-src`, mixed content, CORS),
//! oversized or invalid font is the same as an absent one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use px_css::font_face::{FontDisplay, FontFaceDecl, FontSource};
use px_layout::fontset::build_page_fonts;
use px_paint::FontId;
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::GlyphSource;
use px_text::sysfonts::{GenericFamily, SystemFace, SystemFonts};

use crate::page::Resources;
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

/// How long the whole font phase of one page load may take.
pub const PHASE_BUDGET: Duration = Duration::from_secs(3);

/// How long a `font-display: optional` font may take to arrive and still be used.
pub const OPTIONAL_BUDGET: Duration = Duration::from_millis(100);

/// Fetch the fonts of a web page through `loader` and attach them to `res`.
pub fn load_web(loader: &mut px_net::browse::Loader<'_>, source: &str, res: &mut Resources) {
    let decls = crate::page::font_plan(source, res);
    if decls.is_empty() {
        return;
    }
    let start = Instant::now();
    let mut fetch = |decl: &FontFaceDecl, src: &FontSource| -> Option<Vec<u8>> {
        if start.elapsed() > PHASE_BUDGET {
            return None;
        }
        let t = Instant::now();
        let body = loader.font(&src.url).ok()?.body;
        let late = start.elapsed() > PHASE_BUDGET
            || (decl.display == FontDisplay::Optional && t.elapsed() > OPTIONAL_BUDGET);
        (!late).then_some(body)
    };
    res.fonts = Arc::new(build_page_fonts(&decls, &mut fetch));
}

/// Read the fonts of a local page from next to its file (at or below its
/// directory, see `Resources::key`) and attach them to `res`.
pub fn load_local(source: &str, res: &mut Resources) {
    let decls = crate::page::font_plan(source, res);
    if decls.is_empty() {
        return;
    }
    let fonts = {
        let res: &Resources = res;
        let mut fetch = |_: &FontFaceDecl, src: &FontSource| -> Option<Vec<u8>> {
            let key = res.key(&src.url)?;
            let path = std::path::Path::new(&key);
            let meta = std::fs::metadata(path).ok()?;
            if !meta.is_file() || meta.len() > px_net::browse::MAX_FONT_BYTES as u64 {
                return None;
            }
            std::fs::read(path).ok()
        };
        build_page_fonts(&decls, &mut fetch)
    };
    res.fonts = Arc::new(fonts);
}

/// The largest system font file read for a generic family.
pub const MAX_SYSTEM_FONT_BYTES: u64 = 16 * 1024 * 1024;

/// The faces for the six generic families found in `sys`.
///
/// For each family, its preference list ([`GenericFamily::preferences`], or
/// whatever `prefs` returns) is walked in order; the first name the machine has
/// whose file `read` returns and that `px-text` accepts as a TrueType font wins.
/// A name that is installed but unreadable, a collection or CFF-flavoured (not
/// supported yet) is skipped in favour of the next. A pure function of its
/// inputs, so two runs on one machine agree.
pub fn resolve_generics(
    sys: &SystemFonts,
    read: &dyn Fn(&SystemFace) -> Option<Vec<u8>>,
    prefs: &dyn Fn(GenericFamily) -> Vec<String>,
) -> Vec<(GenericFamily, px_text::FontId)> {
    use GenericFamily::{Cursive, Fantasy, Monospace, SansSerif, Serif, SystemUi};
    let mut out = Vec::new();
    for g in [Serif, SansSerif, Monospace, Cursive, Fantasy, SystemUi] {
        for name in prefs(g) {
            let Some(face) = sys.best_match(&name, 400, false) else {
                continue;
            };
            if face.index != 0 {
                continue;
            }
            let Some(bytes) = read(face) else { continue };
            if let Ok(id) = px_layout::fontset::register_font(&bytes) {
                out.push((g, id));
                break;
            }
        }
    }
    out
}

/// Scan this machine's fonts and install the generic families (once, at
/// startup). Returns how many of the six resolved; one that did not keeps its
/// deterministic fallback, the bundled Noto Sans.
pub fn install_system_generics() -> usize {
    use px_text::sysfonts::DirReader;
    let sys = px_broker::system_fonts::scan_system_fonts();
    let reader = px_broker::system_fonts::FsReader;
    let found = resolve_generics(
        &sys,
        &|f| reader.read_bytes(&f.path, MAX_SYSTEM_FONT_BYTES).ok(),
        &|g| g.preferences().iter().map(|s| (*s).to_string()).collect(),
    );
    for (g, id) in &found {
        px_layout::fontset::set_system_generic(*g, Some(*id));
    }
    found.len()
}
