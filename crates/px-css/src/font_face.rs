//! `@font-face` rules as plain data.
//!
//! stylo parses the rule (CSS Fonts 4 section 4) and the cascade keeps the
//! effective ones (inside a matching `@media`/`@supports`, in layer order). This
//! module converts each into [`FontFaceDecl`], so that `px-layout` and `px-shell`
//! name no stylo type: family, the `url()` sources already resolved against the
//! stylesheet's base, weight and style ranges, `unicode-range`, `font-display`.
//!
//! Decisions (ADR 049):
//!
//! * `local()` sources are dropped. Reading a name from the installed set would be
//!   a fingerprinting oracle (a page could probe which fonts exist by whether the
//!   text changes width) and would make rendering depend on the machine.
//! * A `url()` source whose `format()` hint names something this engine cannot
//!   read (`svg`, `embedded-opentype`, `collection`, an unknown string) is
//!   dropped, as the spec directs: the next source is tried instead.
//! * A `url()` that did not resolve (a malformed reference) is dropped.
//! * Rules are returned in document order. Among equal descriptors the later rule
//!   wins; `px-layout` honours that.

use crate::engine::StyleEngine;

/// A font container this engine can read, from a `format()` hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatHint {
    /// No hint: sniff the bytes.
    Any,
    Woff2,
    Woff,
    Truetype,
    Opentype,
}

/// One `url()` source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontSource {
    /// Absolute URL, fragment kept (a fragment selects nothing here).
    pub url: String,
    pub format: FormatHint,
}

/// `font-style` of a face.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaceStyle {
    Normal,
    Italic,
    Oblique,
}

/// `font-display`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontDisplay {
    Auto,
    Block,
    Swap,
    Fallback,
    Optional,
}

/// One `@font-face` rule that can ever load: it has a family and at least one
/// usable `url()` source.
#[derive(Clone, Debug, PartialEq)]
pub struct FontFaceDecl {
    /// `font-family`, as written (case is compared insensitively by consumers).
    pub family: String,
    /// Sources in the order tried.
    pub sources: Vec<FontSource>,
    /// `font-weight` range in 1..=1000 (`normal` is 400..=400).
    pub weight: (f32, f32),
    pub style: FaceStyle,
    /// `unicode-range`; empty means the initial `U+0-10FFFF`.
    pub ranges: Vec<(u32, u32)>,
    pub display: FontDisplay,
}

impl StyleEngine {
    /// The effective `@font-face` rules of every sheet added so far.
    #[must_use]
    pub fn font_faces(&self) -> Vec<FontFaceDecl> {
        use style::font_face::{FontFaceSourceFormat, FontFaceSourceFormatKeyword, Source};
        let guard = self.shared_lock().read();
        let mut out = Vec::new();
        for (data, _origin) in self.stylist().iter_extra_data_origins() {
            for (rule, _layer) in data.font_faces.iter() {
                let rule = rule.read_with(&guard);
                let d = &rule.descriptors;
                let Some(family) = d.font_family.as_ref() else {
                    continue;
                };
                let Some(src) = d.src.as_ref() else {
                    continue;
                };
                let mut sources = Vec::new();
                for s in &src.0 {
                    let Source::Url(u) = s else { continue };
                    let format = match &u.format_hint {
                        None => FormatHint::Any,
                        Some(FontFaceSourceFormat::Keyword(k)) => match k {
                            FontFaceSourceFormatKeyword::Woff2 => FormatHint::Woff2,
                            FontFaceSourceFormatKeyword::Woff => FormatHint::Woff,
                            FontFaceSourceFormatKeyword::Truetype => FormatHint::Truetype,
                            FontFaceSourceFormatKeyword::Opentype => FormatHint::Opentype,
                            _ => continue,
                        },
                        Some(FontFaceSourceFormat::String(s)) => {
                            match s.to_ascii_lowercase().as_str() {
                                "woff2" => FormatHint::Woff2,
                                "woff" => FormatHint::Woff,
                                "truetype" => FormatHint::Truetype,
                                "opentype" => FormatHint::Opentype,
                                _ => continue,
                            }
                        }
                    };
                    let Some(url) = u.url.url() else { continue };
                    sources.push(FontSource {
                        url: url.as_str().to_string(),
                        format,
                    });
                }
                if sources.is_empty() {
                    continue;
                }
                let weight = d
                    .font_weight
                    .as_ref()
                    .and_then(|w| w.compute())
                    .map_or((400.0, 400.0), |w| (w.0.value(), w.1.value()));
                let style = match d.font_style.as_ref() {
                    None => FaceStyle::Normal,
                    Some(style::font_face::FontStyleRange::Italic) => FaceStyle::Italic,
                    Some(style::font_face::FontStyleRange::Oblique(a, b)) => {
                        let zero = style::values::specified::Angle::zero();
                        if *a == zero && *b == zero {
                            FaceStyle::Normal
                        } else {
                            FaceStyle::Oblique
                        }
                    }
                };
                let ranges = d
                    .unicode_range
                    .as_ref()
                    .map(|v| v.iter().map(|r| (r.start, r.end)).collect())
                    .unwrap_or_default();
                let display = match d.font_display {
                    None | Some(style::font_face::FontDisplay::Auto) => FontDisplay::Auto,
                    Some(style::font_face::FontDisplay::Block) => FontDisplay::Block,
                    Some(style::font_face::FontDisplay::Swap) => FontDisplay::Swap,
                    Some(style::font_face::FontDisplay::Fallback) => FontDisplay::Fallback,
                    Some(style::font_face::FontDisplay::Optional) => FontDisplay::Optional,
                };
                out.push(FontFaceDecl {
                    family: family.name.to_string(),
                    sources,
                    weight,
                    style,
                    ranges,
                    display,
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use style::context::QuirksMode;

    fn faces(css: &str) -> Vec<FontFaceDecl> {
        let mut e = StyleEngine::new(800.0, 600.0, QuirksMode::NoQuirks);
        e.add_author_stylesheet(css, "https://example.test/dir/page.css");
        e.font_faces()
    }

    #[test]
    fn a_plain_rule_resolves_its_url_against_the_sheet() {
        let f = faces(
            "@font-face { font-family: 'Foo Icons'; src: url(../f/foo.woff2) format('woff2'), \
             url(foo.woff) format(woff); font-weight: 300 600; font-style: italic; \
             unicode-range: U+E000-E0FF, U+41; font-display: swap }",
        );
        assert_eq!(f.len(), 1);
        let f = &f[0];
        assert_eq!(f.family, "Foo Icons");
        assert_eq!(f.sources.len(), 2);
        assert_eq!(f.sources[0].url, "https://example.test/f/foo.woff2");
        assert_eq!(f.sources[0].format, FormatHint::Woff2);
        assert_eq!(f.sources[1].url, "https://example.test/dir/foo.woff");
        assert_eq!(f.weight, (300.0, 600.0));
        assert_eq!(f.style, FaceStyle::Italic);
        assert_eq!(f.ranges, vec![(0xE000, 0xE0FF), (0x41, 0x41)]);
        assert_eq!(f.display, FontDisplay::Swap);
    }

    #[test]
    fn defaults_and_dropped_sources() {
        let f = faces(
            "@font-face { font-family: A; src: local(Arial), url(a.svg) format('svg'), url(a.ttf) }\
             @font-face { font-family: B; src: local(Arial) }\
             @font-face { src: url(c.ttf) }\
             @media print { @font-face { font-family: C; src: url(c.ttf) } }",
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert_eq!(f[0].family, "A");
        assert_eq!(f[0].sources.len(), 1);
        assert_eq!(f[0].sources[0].format, FormatHint::Any);
        assert_eq!(f[0].weight, (400.0, 400.0));
        assert_eq!(f[0].style, FaceStyle::Normal);
        assert!(f[0].ranges.is_empty());
        assert_eq!(f[0].display, FontDisplay::Auto);
    }
}
