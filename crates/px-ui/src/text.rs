//! Text for the chrome: the shaping seam, sanitising, truncation, placement.
//!
//! The chrome does not own fonts. The embedder implements [`TextShaper`] over
//! whatever font stack the product ships (the bundled faces, `px-text`), so the
//! chrome stays free of a font dependency and tests can use a fixed-width fake.

use px_layout::geom::Au;
use px_paint::{Color, DisplayList, FontId, PositionedGlyph};

/// Layout units per CSS pixel (`px_layout`'s `Au`).
pub const AU_PER_PX: i32 = 60;

/// Vertical metrics of the chrome font at one size, in `Au`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontMetrics {
    pub ascent: i32,
    pub descent: i32,
}

/// One shaped glyph. `x` is its origin relative to the start of the run, in `Au`;
/// `cluster` is the byte offset in the source text of the character it renders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShapedGlyph {
    pub glyph_id: u32,
    pub x: i32,
    pub cluster: usize,
}

/// A shaped line: glyphs left to right and the total advance, in `Au`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Shaped {
    pub font: FontId,
    pub size_px: i32,
    pub glyphs: Vec<ShapedGlyph>,
    pub width: i32,
}

impl Shaped {
    /// Pen position, in `Au`, of the boundary before byte `index` of the source
    /// text. LTR only: clusters are assumed to increase with `x`.
    pub fn x_at(&self, index: usize) -> i32 {
        self.glyphs
            .iter()
            .find(|g| g.cluster >= index)
            .map_or(self.width, |g| g.x)
    }

    /// The char boundary of `text` nearest to pen position `x` (`Au`).
    pub fn boundary_at(&self, text: &str, x: i32) -> usize {
        let mut best = 0usize;
        let mut best_d = i64::MAX;
        for i in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
            let d = (i64::from(self.x_at(i)) - i64::from(x)).abs();
            if d < best_d {
                best_d = d;
                best = i;
            }
        }
        best
    }
}

/// The font stack, as the chrome needs it.
pub trait TextShaper {
    /// Shape one line of `text` at `size_px`. Must not panic on any input.
    fn shape(&self, text: &str, size_px: i32) -> Shaped;
    fn metrics(&self, size_px: i32) -> FontMetrics;
}

/// Most characters of page-supplied text (a tab title) the chrome will shape.
pub const MAX_TITLE_CHARS: usize = 256;

/// Characters that never reach the screen from page-controlled or pasted text:
/// controls, and the bidirectional overrides and isolates that let text lie
/// about its own order (U+202A..202E, U+2066..2069).
pub fn is_unsafe_char(c: char) -> bool {
    c.is_control() || ('\u{202A}'..='\u{202E}').contains(&c) || ('\u{2066}'..='\u{2069}').contains(&c)
}

/// Page-controlled text made safe to draw: unsafe characters removed, capped at
/// [`MAX_TITLE_CHARS`].
pub fn sanitize(text: &str) -> String {
    text.chars()
        .filter(|c| !is_unsafe_char(*c))
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// `text`, shortened with an ellipsis if it is wider than `max_width_px`.
pub fn fit(shaper: &dyn TextShaper, text: &str, size_px: i32, max_width_px: i32) -> String {
    let max = max_width_px.saturating_mul(AU_PER_PX);
    if shaper.shape(text, size_px).width <= max {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let (mut lo, mut hi) = (0usize, chars.len());
    // Largest prefix length whose text + ellipsis fits (width is monotone in len).
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let cand: String = chars.iter().take(mid).collect::<String>() + px_brand::ui::ELLIPSIS;
        if shaper.shape(&cand, size_px).width <= max {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let kept: String = chars.iter().take(lo).collect();
    kept.trim_end().to_string() + px_brand::ui::ELLIPSIS
}

/// Baseline (pixels) that centres the font's ascent+descent box in a band.
pub fn baseline_in(shaper: &dyn TextShaper, size_px: i32, top: i32, height: i32) -> i32 {
    let m = shaper.metrics(size_px);
    let box_au = m.ascent + m.descent;
    let pad_au = (height * AU_PER_PX - box_au) / 2;
    top + (pad_au + m.ascent + AU_PER_PX / 2).div_euclid(AU_PER_PX)
}

/// Append `shaped` to the list with its origin at pixel `(x, baseline)`.
pub fn push(list: &mut DisplayList, shaped: &Shaped, x: i32, baseline: i32, color: Color) {
    if shaped.glyphs.is_empty() {
        return;
    }
    let ox = x.saturating_mul(AU_PER_PX);
    let oy = baseline.saturating_mul(AU_PER_PX);
    let glyphs: Vec<PositionedGlyph> = shaped
        .glyphs
        .iter()
        .map(|g| PositionedGlyph {
            glyph_id: g.glyph_id,
            x: Au(ox.saturating_add(g.x)),
            y: Au(oy),
        })
        .collect();
    list.push_glyph_run(
        shaped.font,
        Au(shaped.size_px.saturating_mul(AU_PER_PX)),
        color,
        &glyphs,
    );
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// Every char is `size * 0.6` wide, one glyph per char, ascent 0.8 em,
    /// descent 0.2 em.
    pub struct Mono;

    impl TextShaper for Mono {
        fn shape(&self, text: &str, size_px: i32) -> Shaped {
            let adv = size_px * AU_PER_PX * 6 / 10;
            let mut glyphs = Vec::new();
            let mut x = 0;
            for (i, c) in text.char_indices() {
                glyphs.push(ShapedGlyph {
                    glyph_id: u32::from(c),
                    x,
                    cluster: i,
                });
                x += adv;
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
                ascent: size_px * AU_PER_PX * 8 / 10,
                descent: size_px * AU_PER_PX * 2 / 10,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Mono;
    use super::*;

    #[test]
    fn sanitize_drops_controls_and_bidi_overrides_and_caps_length() {
        assert_eq!(sanitize("a\u{202E}b\nc\u{0}d\u{2066}e"), "abcde");
        assert_eq!(sanitize(&"x".repeat(1000)).chars().count(), MAX_TITLE_CHARS);
    }

    #[test]
    fn fit_keeps_short_text_and_truncates_long_text_to_the_width() {
        let m = Mono;
        assert_eq!(fit(&m, "abc", 10, 100), "abc");
        let out = fit(&m, "abcdefghijklmnop", 10, 50);
        assert!(out.ends_with(px_brand::ui::ELLIPSIS), "{out}");
        assert!(m.shape(&out, 10).width <= 50 * AU_PER_PX, "{out}");
        // One more character would not have fit.
        let more = out.trim_end_matches(px_brand::ui::ELLIPSIS).to_string()
            + "x"
            + px_brand::ui::ELLIPSIS;
        assert!(m.shape(&more, 10).width > 50 * AU_PER_PX);
        // Nothing fits: just the ellipsis, never a panic.
        assert_eq!(fit(&m, "abcdef", 10, 1), px_brand::ui::ELLIPSIS);
    }

    #[test]
    fn caret_positions_round_trip_through_boundaries() {
        let m = Mono;
        let t = "héllo";
        let s = m.shape(t, 10);
        for (i, _) in t.char_indices() {
            assert_eq!(s.boundary_at(t, s.x_at(i)), i);
        }
        assert_eq!(s.boundary_at(t, i32::MAX), t.len());
        assert_eq!(s.boundary_at(t, i32::MIN), 0);
    }

    #[test]
    fn baseline_centres_the_font_box() {
        let m = Mono;
        // 10 px font: box is 10 px tall; in a 32 px band the box starts at +11.
        assert_eq!(baseline_in(&m, 10, 100, 32), 100 + 11 + 8);
    }
}
