//! Line break opportunities (Phase 9 unit 6).
//!
//! A pure function from text and CSS options to the byte offsets at which a
//! line may end: [`opportunities`]. It maps `line-break`, `word-break`,
//! `overflow-wrap`, `white-space` and `hyphens` onto `icu_segmenter`'s UAX #14
//! `LineSegmenter` and UAX #29 `GraphemeClusterSegmenter`, and adds the three
//! things CSS needs on top of UAX #14: forced breaks that depend on
//! `white-space`, emergency breaks for `overflow-wrap`, and breaks after
//! every preserved space for `break-spaces`.
//!
//! # What the Unicode data provides
//!
//! * UAX #14 for every script, including CJK: a break is allowed between
//!   ideographs (class ID) and kana, never before closing punctuation, `!`,
//!   `?`, small kana and similar (CSS `line-break: strict`; `normal` and
//!   `loose` relax some of those, per CSS Text 3).
//! * Thai, Lao, Khmer and Myanmar have no spaces between words. The
//!   segmenter's `auto` mode runs the LSTM models compiled into
//!   `icu_segmenter_data` to find word boundaries. They are statistical: they
//!   find plausible boundaries, not dictionary-exact ones, and have no
//!   language tag input here (the locale option needs `icu_locale_core`, which
//!   ADR 036 does not add as a direct dependency).
//! * Chinese and Japanese need no dictionary for *line* breaking: UAX #14
//!   already allows a break between any two ideographs. Word-level
//!   segmentation (selecting a Japanese word to keep together) is not provided
//!   and not needed for CSS line breaking.
//! * `hyphens: auto` needs hyphenation patterns, which are not in the lock.
//!   It behaves as `manual` here: only U+00AD soft hyphens produce hyphenation
//!   breaks.
//!
//! # Offsets and kinds
//!
//! Every offset is a byte index into `text` on a `char` boundary, strictly
//! increasing, greater than 0, and the last is `text.len()` (UAX #14 LB3: the
//! end of text is a break). Each carries a [`Kind`]:
//!
//! * [`Kind::Hard`]: the line must end here (a preserved newline, a form
//!   feed, U+2028, U+2029, ...), and the end of the text.
//! * [`Kind::Soft`]: the line may end here (the normal wrap opportunities).
//! * [`Kind::Emergency`]: a grapheme-cluster boundary that is not otherwise a
//!   break. Layout may use it only when no `Soft` opportunity makes the line
//!   fit, and only because `overflow-wrap` is `anywhere` or `break-word`.
//!
//! # Segment breaks and `white-space`
//!
//! Collapsing spaces is the caller's step (CSS Text 3 phase I). This function
//! handles the one consequence that belongs to line breaking: whether a
//! U+000A or U+000D is a forced break. It is when `white-space` preserves
//! segment breaks (`pre`, `pre-wrap`, `pre-line`, `break-spaces`) and a
//! collapsible space otherwise. VT, FF, NEL, LS and PS force a break in every
//! mode. `nowrap` and `pre` never produce `Soft` or `Emergency` offsets.

#![deny(
    clippy::indexing_slicing,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::BTreeMap;

use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness, LineBreakWordOption};
use icu_segmenter::{GraphemeClusterSegmenter, LineSegmenter};

/// CSS `line-break`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineBreak {
    /// `auto`: this engine uses the `normal` rules.
    #[default]
    Auto,
    /// `loose`.
    Loose,
    /// `normal`.
    Normal,
    /// `strict`: the UAX #14 default.
    Strict,
    /// `anywhere`: a soft wrap opportunity at every grapheme cluster.
    Anywhere,
}

/// CSS `word-break`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WordBreak {
    /// `normal`.
    #[default]
    Normal,
    /// `break-all`: break between any two letters.
    BreakAll,
    /// `keep-all`: no break between CJK letters either.
    KeepAll,
    /// The legacy `break-word`: `normal` plus `overflow-wrap: anywhere`.
    BreakWord,
}

/// CSS `overflow-wrap` (and its `word-wrap` alias).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverflowWrap {
    /// `normal`: never break inside an unbreakable run.
    #[default]
    Normal,
    /// `break-word`: emergency breaks, ignored for min-content sizing.
    BreakWord,
    /// `anywhere`: emergency breaks that also count for min-content sizing.
    Anywhere,
}

/// CSS `white-space` (the longhand-collapsed values).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WhiteSpace {
    /// Collapse spaces and newlines, wrap.
    #[default]
    Normal,
    /// Collapse, never wrap.
    Nowrap,
    /// Preserve everything, never wrap.
    Pre,
    /// Preserve, wrap.
    PreWrap,
    /// Collapse spaces, preserve newlines, wrap.
    PreLine,
    /// Like `pre-wrap`, with a break opportunity after every preserved space.
    BreakSpaces,
}

/// CSS `hyphens`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hyphens {
    /// Soft hyphens (U+00AD) are ignored.
    None,
    /// Break at soft hyphens only. `auto` is treated as this (see the module
    /// documentation).
    #[default]
    Manual,
}

/// Everything that tailors line breaking.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// `line-break`.
    pub line_break: LineBreak,
    /// `word-break`.
    pub word_break: WordBreak,
    /// `overflow-wrap`.
    pub overflow_wrap: OverflowWrap,
    /// `white-space`.
    pub white_space: WhiteSpace,
    /// `hyphens`.
    pub hyphens: Hyphens,
}

impl Options {
    /// Whether emergency breaks count when computing min-content width: true
    /// for `overflow-wrap: anywhere` and for the legacy `word-break:
    /// break-word` (which CSS Text 3 defines as `anywhere`), false for
    /// `overflow-wrap: break-word`.
    pub fn emergency_breaks_in_min_content(&self) -> bool {
        self.overflow_wrap == OverflowWrap::Anywhere || self.word_break == WordBreak::BreakWord
    }
}

/// What kind of break an offset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Only if nothing better fits (`overflow-wrap`).
    Emergency,
    /// A normal wrap opportunity.
    Soft,
    /// The line must end here.
    Hard,
}

/// One break opportunity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opportunity {
    /// Byte offset in the text: the line ends before this offset.
    pub offset: usize,
    /// How strong the break is.
    pub kind: Kind,
    /// True when the character just before `offset` is a soft hyphen, so a
    /// line ending here must draw a visible hyphen.
    pub hyphen: bool,
}

const SOFT_HYPHEN: char = '\u{ad}';

fn strictness(lb: LineBreak) -> LineBreakStrictness {
    match lb {
        LineBreak::Auto | LineBreak::Normal => LineBreakStrictness::Normal,
        LineBreak::Loose => LineBreakStrictness::Loose,
        LineBreak::Strict => LineBreakStrictness::Strict,
        LineBreak::Anywhere => LineBreakStrictness::Anywhere,
    }
}

fn word_option(wb: WordBreak) -> LineBreakWordOption {
    match wb {
        WordBreak::Normal | WordBreak::BreakWord => LineBreakWordOption::Normal,
        WordBreak::BreakAll => LineBreakWordOption::BreakAll,
        WordBreak::KeepAll => LineBreakWordOption::KeepAll,
    }
}

const STRICTNESSES: [LineBreakStrictness; 4] = [
    LineBreakStrictness::Loose,
    LineBreakStrictness::Normal,
    LineBreakStrictness::Strict,
    LineBreakStrictness::Anywhere,
];
const WORD_OPTIONS: [LineBreakWordOption; 3] = [
    LineBreakWordOption::Normal,
    LineBreakWordOption::BreakAll,
    LineBreakWordOption::KeepAll,
];

fn build_segmenter(s: LineBreakStrictness, w: LineBreakWordOption) -> LineSegmenter {
    let mut o = LineBreakOptions::default();
    o.strictness = Some(s);
    o.word_option = Some(w);
    LineSegmenter::new_auto(o).static_to_owned()
}

thread_local! {
    // ICU's owned segmenter holds `Rc` data, so it is not `Sync` and cannot sit
    // in a static; one per option pair per thread, built on first use. Building
    // loads the LSTM data, which is the cost this avoids repeating.
    static SEGMENTERS: RefCell<[Option<LineSegmenter>; 12]> =
        const { RefCell::new([const { None }; 12]) };
}

/// UAX #14 break offsets of `text` for the option pair (includes the final
/// `text.len()`; may include 0).
fn segment(s: LineBreakStrictness, w: LineBreakWordOption, text: &str) -> Vec<usize> {
    let si = STRICTNESSES.iter().position(|x| *x == s).unwrap_or(0);
    let wi = WORD_OPTIONS.iter().position(|x| *x == w).unwrap_or(0);
    let cached = SEGMENTERS
        .try_with(|cells| {
            let mut cells = cells.try_borrow_mut().ok()?;
            let cell = cells.get_mut(si * 3 + wi)?;
            let seg = cell.get_or_insert_with(|| build_segmenter(s, w));
            Some(seg.as_borrowed().segment_str(text).collect::<Vec<usize>>())
        })
        .ok()
        .flatten();
    cached.unwrap_or_else(|| {
        build_segmenter(s, w)
            .as_borrowed()
            .segment_str(text)
            .collect()
    })
}

/// Characters that force a break after them in every `white-space` mode
/// (UAX #14 BK and NL; CR and LF are handled separately).
fn is_always_hard(c: char) -> bool {
    matches!(c, '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// The break opportunities of `text` under `options`. See the module
/// documentation for the contract. Empty text has none.
pub fn opportunities(text: &str, options: &Options) -> Vec<Opportunity> {
    if text.is_empty() {
        return Vec::new();
    }
    let ws = options.white_space;
    let wraps = !matches!(ws, WhiteSpace::Nowrap | WhiteSpace::Pre);
    let preserves_breaks = matches!(
        ws,
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine | WhiteSpace::BreakSpaces
    );
    let emergency = wraps
        && (options.overflow_wrap != OverflowWrap::Normal
            || options.word_break == WordBreak::BreakWord);

    // Same byte length per character: CR and LF become one space each.
    let seen: Cow<'_, str> = if !preserves_breaks && text.contains(['\r', '\n']) {
        Cow::Owned(
            text.chars()
                .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
                .collect(),
        )
    } else {
        Cow::Borrowed(text)
    };
    let seen: &str = &seen;

    let mut out: BTreeMap<usize, Opportunity> = BTreeMap::new();
    let mut put = |offset: usize, kind: Kind, hyphen: bool| {
        out.entry(offset)
            .and_modify(|o| {
                if kind > o.kind {
                    o.kind = kind;
                }
            })
            .or_insert(Opportunity {
                offset,
                kind,
                hyphen,
            });
    };

    let len = seen.len();
    let raw = segment(
        strictness(options.line_break),
        word_option(options.word_break),
        seen,
    );
    for offset in raw {
        if offset == 0 || offset > len || !seen.is_char_boundary(offset) {
            continue;
        }
        let before = seen.get(..offset).and_then(|s| s.chars().next_back());
        let after = seen.get(offset..).and_then(|s| s.chars().next());
        let hard = offset == len
            || before.is_some_and(|c| {
                is_always_hard(c) || c == '\n' || (c == '\r' && after != Some('\n'))
            });
        let hyphen = before == Some(SOFT_HYPHEN);
        if hard {
            put(offset, Kind::Hard, hyphen && offset != len);
        } else if wraps && !(hyphen && options.hyphens == Hyphens::None) {
            put(offset, Kind::Soft, hyphen);
        }
    }

    if wraps && ws == WhiteSpace::BreakSpaces {
        for (i, c) in seen.char_indices() {
            if c == ' ' || c == '\t' {
                let end = i + c.len_utf8();
                if end < len {
                    put(end, Kind::Soft, false);
                }
            }
        }
    }

    if emergency {
        for offset in GraphemeClusterSegmenter::new().segment_str(seen) {
            if offset != 0 && offset < len && seen.is_char_boundary(offset) {
                put(offset, Kind::Emergency, false);
            }
        }
    }

    out.into_values().collect()
}

#[cfg(test)]
mod tests;
