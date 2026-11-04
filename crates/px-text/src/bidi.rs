//! Bidirectional text (Phase 9 unit 7): UAX #9 over the `unicode-bidi` crate.
//!
//! [`analyze`] resolves embedding levels for a text and its base direction and
//! returns [`Runs`]. [`Runs::runs`] splits the text into logical-order
//! directional runs, the units layout shapes (one shaping call per run, with
//! the run's direction). After line breaking, [`reorder_line`] puts the runs of
//! one line in visual order (UAX #9 rules L1 and L2).
//!
//! # What is and is not done here
//!
//! * Explicit embeddings, overrides and isolates (X1-X8, X10), weak types
//!   (W1-W7), neutrals including paired brackets (N0-N2) and implicit levels
//!   (I1, I2) are the crate's.
//! * L1 (trailing whitespace of a line takes the paragraph level) and L2
//!   (reverse by level) are applied by [`reorder_line`] per line, so a space
//!   that ends a line sits at the line's edge, not inside an RTL run.
//! * L3 (combining marks in reversed text) is not done: the shaper reverses
//!   glyphs per run and keeps clusters intact, which is what L3 is for.
//! * L4 (mirroring) is [`mirror`], from `icu_properties`'
//!   `Bidi_Mirroring_Glyph`. Layout or the painter applies it to characters
//!   of right-to-left runs before shaping; [`Runs::visual_string`] shows the
//!   composed result for tests and debugging.
//! * `direction` and `unicode-bidi: embed | isolate | bidi-override |
//!   isolate-override | plaintext` are expressed by the caller inserting the
//!   Unicode control characters (CSS Writing Modes 3, section 2.4) and, for
//!   `plaintext`, passing [`BaseDirection::Auto`].
//!
//! # Hostile input
//!
//! Text is untrusted. No input panics: the line ranges accepted by
//! [`reorder_line`] are validated (inside one paragraph, on `char`
//! boundaries) and anything else returns `None`; the property tests run random
//! code points, unassigned and noncharacters included, and 200-deep
//! embedding nests through every entry point.

#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::ops::Range;

use icu_properties::CodePointMapData;
use icu_properties::props::BidiMirroringGlyph;
use unicode_bidi::{BidiInfo, Level};

/// The paragraph base direction (CSS `direction`, or `unicode-bidi:
/// plaintext`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BaseDirection {
    /// Left to right (CSS `direction: ltr`).
    Ltr,
    /// Right to left (CSS `direction: rtl`).
    Rtl,
    /// Each paragraph takes the direction of its first strong character
    /// (UAX #9 P2 and P3), left to right when it has none.
    Auto,
}

/// One paragraph of the text (UAX #9 BD4: ends after a paragraph separator).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
    /// Byte range in the analysed text, separator included.
    pub range: Range<usize>,
    /// Paragraph embedding level: 0 for left to right, 1 for right to left.
    pub level: u8,
}

/// A maximal span of one embedding level.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    /// Byte range in the analysed text.
    pub range: Range<usize>,
    /// Resolved embedding level, 0 to 126. Odd levels are right to left.
    pub level: u8,
}

impl Run {
    /// True for a right-to-left run (odd level).
    pub fn is_rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// The result of [`analyze`]: resolved levels for a text.
pub struct Runs<'t> {
    text: &'t str,
    info: BidiInfo<'t>,
}

impl std::fmt::Debug for Runs<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runs")
            .field("len", &self.text.len())
            .field("paragraphs", &self.info.paragraphs.len())
            .finish()
    }
}

/// Resolves the embedding levels of `text` (UAX #9 up to rule I2).
pub fn analyze(text: &str, base: BaseDirection) -> Runs<'_> {
    let level = match base {
        BaseDirection::Ltr => Some(Level::ltr()),
        BaseDirection::Rtl => Some(Level::rtl()),
        BaseDirection::Auto => None,
    };
    Runs {
        text,
        info: BidiInfo::new(text, level),
    }
}

/// Visual order of the runs of one line; see [`Runs::reorder_line`].
pub fn reorder_line(runs: &Runs<'_>, line: Range<usize>) -> Option<Vec<Run>> {
    runs.reorder_line(line)
}

/// The mirrored counterpart of `c` (`(` for `)`, and so on), if it has one:
/// `Bidi_Mirroring_Glyph` (UAX #9 L4).
pub fn mirror(c: char) -> Option<char> {
    CodePointMapData::<BidiMirroringGlyph>::new()
        .get(c)
        .mirroring_glyph
}

impl<'t> Runs<'t> {
    /// The analysed text.
    pub fn text(&self) -> &'t str {
        self.text
    }

    /// True if any character resolved to an odd level, that is, if reordering
    /// or right-to-left shaping can matter at all.
    pub fn has_rtl(&self) -> bool {
        self.info.has_rtl()
    }

    /// The paragraphs, in order.
    pub fn paragraphs(&self) -> Vec<Paragraph> {
        self.info
            .paragraphs
            .iter()
            .map(|p| Paragraph {
                range: p.range.clone(),
                level: p.level.number(),
            })
            .collect()
    }

    /// The resolved level of every character, in logical order (one entry per
    /// `char`). Characters removed by rule X9 (embedding controls, boundary
    /// neutrals) carry the level of their surroundings.
    pub fn char_levels(&self) -> Vec<u8> {
        self.text
            .char_indices()
            .map(|(i, _)| self.info.levels.get(i).map_or(0, Level::number))
            .collect()
    }

    /// Logical-order runs: maximal spans of one level inside one paragraph,
    /// covering the whole text.
    pub fn runs(&self) -> Vec<Run> {
        let mut out: Vec<Run> = Vec::new();
        for para in &self.info.paragraphs {
            let mut current: Option<Run> = None;
            let slice = self.text.get(para.range.clone()).unwrap_or("");
            for (i, c) in slice.char_indices() {
                let at = para.range.start + i;
                let level = self.info.levels.get(at).map_or(0, Level::number);
                match current.as_mut() {
                    Some(run) if run.level == level => run.range.end = at + c.len_utf8(),
                    _ => {
                        if let Some(done) = current.take() {
                            out.push(done);
                        }
                        current = Some(Run {
                            range: at..at + c.len_utf8(),
                            level,
                        });
                    }
                }
            }
            if let Some(done) = current {
                out.push(done);
            }
        }
        out
    }

    /// The runs of one line in visual order, left to right (rules L1 and L2).
    ///
    /// `line` is a byte range of the analysed text. It must lie inside a single
    /// paragraph and start and end on `char` boundaries, otherwise the result
    /// is `None`. An empty line is `Some` of no runs. The levels in the result
    /// have rule L1 applied (trailing whitespace at the paragraph level), so
    /// they can differ from [`Runs::runs`] at the end of a line.
    ///
    /// Within a run the glyphs are drawn right to left when the level is odd;
    /// the run's range is still the logical range, in logical order.
    pub fn reorder_line(&self, line: Range<usize>) -> Option<Vec<Run>> {
        if line.start > line.end
            || !self.text.is_char_boundary(line.start)
            || !self.text.is_char_boundary(line.end)
        {
            return None;
        }
        if line.start == line.end {
            return Some(Vec::new());
        }
        let para = self
            .info
            .paragraphs
            .iter()
            .find(|p| p.range.start <= line.start && line.end <= p.range.end)?;
        let (levels, level_runs) = self.info.visual_runs(para, line);
        let mut out = Vec::with_capacity(level_runs.len());
        for r in level_runs {
            let level = levels.get(r.start).map_or(0, Level::number);
            out.push(Run { range: r, level });
        }
        Some(out)
    }

    /// The line as it is drawn, left to right: runs reordered, right-to-left
    /// runs reversed character by character and mirrored. For tests and
    /// debugging; painting goes through shaped runs instead.
    pub fn visual_string(&self, line: Range<usize>) -> Option<String> {
        let runs = self.reorder_line(line)?;
        let mut out = String::new();
        for run in runs {
            let slice = self.text.get(run.range.clone())?;
            if run.is_rtl() {
                out.extend(slice.chars().rev().map(|c| mirror(c).unwrap_or(c)));
            } else {
                out.push_str(slice);
            }
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests;
