//! Text measurement, before there is a font system.
//!
//! §9 puts text shaping in Phase 9 and `px-text` is an empty skeleton, but inline
//! layout cannot exist without *some* answer to "how wide is this text". This is
//! that answer, and it is a stub with its limits written down rather than a
//! placeholder pretending to be a measurement.
//!
//! # What it claims
//!
//! Every character advances a fixed fraction of the font size, and every font is
//! the same. That is wrong for every real font: `i` and `W` differ by a factor of
//! four, kerning exists, ligatures exist, and a monospace font is not half-width.
//!
//! **What it is right about is the only thing this phase needs it to be.** ADR
//! 029 runs the vendored reftests by comparing a test's box geometry against its
//! reference's, and an equality is preserved by any *consistent* transformation of
//! both sides. So a metric that is uniformly wrong leaves every reftest whose
//! match depends on two text runs agreeing still matching — at the wrong width.
//!
//! The absolute numbers are meaningless until Phase 9. The comparisons are not.
//!
//! # Why the ratio is a half
//!
//! An average Latin proportional face runs about 0.5em per character across
//! ordinary prose. Any constant would satisfy the equality argument above; this
//! one makes the numbers plausible enough that a wildly wrong layout still looks
//! wrong when a person reads the fragment tree, which a ratio of 1 or 0.1 would
//! not.
//!
//! # Integer arithmetic, deliberately
//!
//! No `f32` anywhere — `ci/gate-layout.sh` forbids it in this crate, because
//! floating point makes a repeat run depend on the order operations happened in,
//! and "identical box tree on repeat runs" is §9 Phase 6's second gate item. The
//! advance is computed in app units by integer division, so it is exactly
//! reproducible and its rounding is one documented truncation rather than an
//! accumulation of them.
//!
//! The division goes through `Au`'s own `Div<i32>` rather than through its inner
//! field. Division cannot overflow the way multiplication can, so this one was
//! safe — but a review found a `.0 * 6 / 5` in [`crate::inline`] that was not,
//! and the rule is easier to keep when it has no exceptions.

use app_units::Au;

/// The advance of one character, as a fraction of the font size.
///
/// Expressed as a divisor rather than a multiplier so the arithmetic stays in
/// integers: `font_size / ADVANCE_DIVISOR` rather than `font_size * 0.5`.
const ADVANCE_DIVISOR: i32 = 2;

/// Measure `text` at `font_size`.
///
/// Counts `char`s, not bytes and not grapheme clusters. Bytes would make a
/// multi-byte character wider than a single-byte one, which is wrong in a way
/// that varies with the script. Grapheme clusters would be *more* correct — a
/// combining accent advances nothing — and are deliberately not used, because
/// getting that right needs the segmentation Phase 9 brings, and a half-correct
/// segmentation here would be harder to replace than an obviously uniform one.
#[must_use]
pub fn measure(text: &str, font_size: Au) -> Au {
    let advance = char_advance(font_size);
    let count = i32::try_from(text.chars().count()).unwrap_or(i32::MAX);
    advance * count
}

/// The advance of a single character at `font_size`.
#[must_use]
pub fn char_advance(font_size: Au) -> Au {
    font_size / ADVANCE_DIVISOR
}

/// A place where a line may be broken, and the text before it.
///
/// Phase 6 breaks at ASCII spaces only. Real line breaking is UAX #14, needs the
/// segmentation Phase 9 brings, and gets every CJK document wrong in the meantime
/// — CJK breaks between almost any two characters and this will not break at all.
/// Named as a limitation rather than left to be discovered from a rendering.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BreakOpportunity {
    /// Byte offset of the break in the original string.
    pub offset: usize,
    /// The width of the text from the previous break to this one.
    pub width: Au,
}

/// Split `text` into words at ASCII spaces, with each word's measured width.
///
/// The trailing space belongs to the word before it, which is what makes a line
/// break able to drop it: a line broken after "foo " is "foo" wide, not "foo "
/// wide, and CSS 2.1 §16.6.1 requires that trailing spaces at a soft break are
/// removed.
#[must_use]
pub fn words(text: &str, font_size: Au) -> Vec<(&str, Au)> {
    let advance = char_advance(font_size);
    let mut out = Vec::new();
    let mut start = 0usize;

    for (index, ch) in text.char_indices() {
        if ch == ' ' {
            let end = index + ch.len_utf8();
            let word = &text[start..end];
            let count = i32::try_from(word.chars().count()).unwrap_or(i32::MAX);
            out.push((word, advance * count));
            start = end;
        }
    }
    if start < text.len() {
        let word = &text[start..];
        let count = i32::try_from(word.chars().count()).unwrap_or(i32::MAX);
        out.push((word, advance * count));
    }
    out
}

/// The width of `word` with any trailing spaces removed.
///
/// CSS 2.1 §16.6.1: a sequence of spaces at the end of a line is removed. Used
/// when a line is closed, so the last word on a line does not pay for the space
/// that caused the break.
#[must_use]
pub fn width_without_trailing_spaces(word: &str, font_size: Au) -> Au {
    measure(word.trim_end_matches(' '), font_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    #[test]
    fn text_advance_is_half_the_font_size() {
        assert_eq!(char_advance(px(16)), px(8));
        assert_eq!(char_advance(px(20)), px(10));
    }

    #[test]
    fn text_measures_characters_not_bytes() {
        // Three characters, but more than three bytes. A byte-counting metric
        // would make this three times wider than "abc", which is wrong in a way
        // that varies by script rather than uniformly — and a metric that is
        // *non-uniformly* wrong breaks ADR 029's equality argument.
        assert_eq!(measure("abc", px(16)), measure("日本語", px(16)));
        assert_eq!(measure("abc", px(16)), px(24));
    }

    #[test]
    fn text_empty_string_has_no_width() {
        assert_eq!(measure("", px(16)), px(0));
    }

    #[test]
    fn text_words_keep_their_trailing_space() {
        let split = words("foo bar", px(16));
        assert_eq!(split.len(), 2);
        assert_eq!(split[0].0, "foo ");
        assert_eq!(split[1].0, "bar");
        // "foo " is four characters at 8px.
        assert_eq!(split[0].1, px(32));
        assert_eq!(split[1].1, px(24));
    }

    #[test]
    fn text_trailing_spaces_are_dropped_at_a_break() {
        assert_eq!(width_without_trailing_spaces("foo ", px(16)), px(24));
        assert_eq!(width_without_trailing_spaces("foo   ", px(16)), px(24));
        assert_eq!(width_without_trailing_spaces("foo", px(16)), px(24));
    }

    /// The same input measures the same twice, which gate item 2 is about.
    ///
    /// Trivially true for integer arithmetic and worth asserting anyway: the
    /// obvious implementation of this module multiplies by 0.5 in `f32`, and the
    /// point of the integer version is a property no test would otherwise state.
    #[test]
    fn text_measurement_is_reproducible() {
        let once = measure("the quick brown fox", px(17));
        let twice = measure("the quick brown fox", px(17));
        assert_eq!(once, twice);
        // 17px / 2 truncates to 8px per character, not 8.5.
        assert_eq!(char_advance(px(17)), px(17) / 2);
    }
}
