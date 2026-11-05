//! Text for layout: fonts, shaping, line breaking and bidi, in app units.
//!
//! Phase 9 unit 3 (ADR 036). This replaces the Phase 6 stub that measured every
//! character as half the font size. Everything here goes through `px-text`:
//! [`px_text::shaper`] for glyphs (integer font units), [`px_text::linebreak`]
//! for the break opportunities CSS Text asks for, and [`px_text::bidi`] for
//! directional runs. The one conversion from font units to [`Au`] is
//! [`px_text::shaper::font_units_to_au`], applied **per glyph** and then summed
//! in `Au`, so the number a line is measured with is exactly the sum of the
//! advances paint will draw.
//!
//! # Font resolution policy (the documented fallback)
//!
//! The only font the engine bundles is Ahem (`tests/fonts`, ADR 036 (g)). So
//! **every `font-family` resolves to Ahem**: a named family is looked up in the
//! registry (so a name that *is* registered wins), and an unknown family, a
//! generic family (`serif`, `sans-serif`, `monospace`, ...) and the initial value
//! all fall back to the first registered font. System fonts, the Noto subsets and
//! per-character fallback are later units (plan units 8 and 1's follow-ups); when
//! they land they change [`resolve_font`] and nothing else here.
//!
//! The consequence is deliberate and visible: with Ahem every glyph is one em
//! wide, a space included, and the font has ascent 0.8 em and descent 0.2 em, so
//! `line-height: normal` is `1em`. Text in a page that never names Ahem is
//! therefore twice as wide as the Phase 6 stub made it. That is the metric, not a
//! layout defect.
//!
//! # What is and is not done
//!
//! Done: font size and `line-height` (`normal` from `hhea`, a number, a length),
//! `white-space` (all six values), `overflow-wrap`, `word-break`, `line-break`,
//! `direction` and `unicode-bidi: plaintext`, `text-align`, soft hyphens
//! (`hyphens` is not a property of the style engine this build links, so the
//! initial `manual` is used), forced breaks, emergency breaks, bidi reordering at
//! each line, per-run shaping, mirroring of right-to-left runs.
//!
//! Not done, and named: `letter-spacing`, `word-spacing`, `text-transform`,
//! `text-indent`, `tab-size` (a tab is eight spaces), font features and
//! `font-variant`, `hyphens: auto`, script itemisation finer than a bidi run
//! (one script per run, taken from its first strong character), per-character
//! font fallback, and shaping across an inline element boundary (a paragraph is
//! shaped one piece per element and directional run, so a ligature or a kern
//! pair across a `<span>` is not formed; with Ahem nothing is lost).
//!
//! # Paragraphs of several elements
//!
//! [`Prepared::paragraph`] takes the whole inline run of a block: the text, a
//! [`StyleRange`] per element's stretch of it, the [`Edge`]s inline boxes add (their
//! margin, border and padding), and the [`Atomic`]s (inline-blocks, one `U+FFFC`
//! each whose advance is the margin box). The measure is a prefix sum over the
//! glyph advances plus those edges, so breaking works across element boundaries
//! and a line's width includes the boxes that open and close on it.
//! [`Prepared::line_runs`] returns, besides the glyph runs, where each edge and
//! each glyph's room fell, which is what `crate::inline` builds the inline-box
//! fragments from. Edges follow the *run's* direction: a left-to-right span
//! around right-to-left text has its start edge at the visual right of that run.
//! `font-weight`, `font-style` and `font-family` select between faces, and there
//! is one face (Ahem), so a bold or italic span is the same shapes as plain text.
//!
//! # Integer arithmetic, deliberately
//!
//! No floating point: `cargo xtask layout` forbids it in this crate, and "the
//! box tree must be identical on repeat runs" is a Phase 6 gate item. Widths are
//! prefix sums of per-glyph `Au` advances, so the measure of any byte range is
//! one subtraction.

use std::sync::OnceLock;

use app_units::Au;
use px_dom::NodeId;
use px_text::bidi::{self, BaseDirection};
use px_text::linebreak::{self, Kind, Opportunity, Options};
use px_text::shaper::{Direction, HarfrustShaper, ShapeInput, Shaper, font_units_to_au};
use px_text::{Font, FontId, FontRegistry};
use style::properties::ComputedValues;

use crate::fragment::GlyphPos;
use crate::geom::{au, px};

pub use px_text::linebreak::WhiteSpace;

/// The fonts layout can use: a registry and a shaper per registered font.
struct Fonts {
    registry: FontRegistry,
    shapers: Vec<Option<HarfrustShaper>>,
}

fn fonts() -> &'static Fonts {
    static FONTS: OnceLock<Fonts> = OnceLock::new();
    FONTS.get_or_init(|| {
        let registry = FontRegistry::with_ahem().unwrap_or_default();
        let shapers = (0..registry.len())
            .map(|i| {
                let id = FontId(u32::try_from(i).unwrap_or(u32::MAX));
                let font = registry.get(id)?;
                HarfrustShaper::new(font.bytes().clone(), 0).ok()
            })
            .collect();
        Fonts { registry, shapers }
    })
}

/// The font a `font-family` list resolves to. See the module documentation for
/// the policy: the first listed family the registry has, else the first
/// registered font (Ahem).
#[must_use]
pub fn resolve_font<'a>(families: impl IntoIterator<Item = &'a str>) -> FontId {
    let f = fonts();
    for name in families {
        if let Some(id) = f.registry.resolve(name) {
            return id;
        }
    }
    FontId(0)
}

/// The value of `text-align`, with `start` and `end` already resolved against
/// `direction` by [`TextStyle::of`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    /// Lines start at the left edge.
    Left,
    /// Lines end at the right edge.
    Right,
    /// Lines are centred.
    Center,
    /// Lines other than the last (and than one ended by a forced break) are
    /// stretched to the available width by widening spaces; the others align to
    /// the start edge of `direction`.
    Justify,
}

/// Everything layout reads from computed style to measure and break text.
///
/// Plain `Copy` data, so it travels in the same places `font_size: Au` used to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextStyle {
    /// The resolved font.
    pub font: FontId,
    /// `font-size`.
    pub size: Au,
    /// The used `line-height`.
    pub line_height: Au,
    /// `white-space`, `overflow-wrap`, `word-break`, `line-break`, `hyphens`.
    pub breaking: Options,
    /// `direction: rtl`.
    pub rtl: bool,
    /// `unicode-bidi: plaintext`: the paragraph direction comes from the text.
    pub plaintext: bool,
    /// `text-align`.
    pub align: Align,
}

impl TextStyle {
    /// The style of text with no element around it (the document node): 16px,
    /// every initial value.
    #[must_use]
    pub fn initial() -> Self {
        let mut s = TextStyle {
            font: FontId(0),
            size: px(16),
            line_height: Au(0),
            breaking: Options::default(),
            rtl: false,
            plaintext: false,
            align: Align::Left,
        };
        s.line_height = s.metrics().normal_line_height();
        s
    }

    /// Read the text properties out of `style`.
    #[must_use]
    pub fn of(style: &ComputedValues) -> Self {
        use style::computed_values::direction::T as Direction;
        use style::computed_values::unicode_bidi::T as UnicodeBidi;
        use style::values::generics::font::LineHeight;

        let size = Au::from(style.clone_font_size().computed_size());
        let family = style.clone_font_family();
        let names: Vec<String> = family
            .families
            .iter()
            .filter_map(|f| match f {
                style::values::computed::font::SingleFontFamily::FamilyName(n) => {
                    Some(n.name.to_string())
                }
                style::values::computed::font::SingleFontFamily::Generic(_) => None,
            })
            .collect();
        let font = resolve_font(names.iter().map(String::as_str));

        let rtl = matches!(style.clone_direction(), Direction::Rtl);
        let plaintext = matches!(style.clone_unicode_bidi(), UnicodeBidi::Plaintext);

        let mut out = TextStyle {
            font,
            size,
            line_height: Au(0),
            breaking: breaking_options(style),
            rtl,
            plaintext,
            align: align_of(style, rtl),
        };
        out.line_height = match style.clone_line_height() {
            LineHeight::Normal => out.metrics().normal_line_height(),
            // A number is a multiplier of the font size. `scale_by` does the one
            // deterministic conversion inside `app_units`; nothing accumulates.
            LineHeight::Number(number) => size.scale_by(number.0),
            LineHeight::Length(length) => Au::from(length.0),
        };
        out
    }

    /// The font's vertical metrics at this size.
    #[must_use]
    pub fn metrics(&self) -> Metrics {
        let Some(font) = fonts().registry.get(self.font) else {
            return Metrics::default();
        };
        let upem = font.units_per_em();
        let conv = |units: i16| au(font_units_to_au(i32::from(units), upem, self.size.0));
        Metrics {
            ascent: conv(font.ascent()),
            descent: conv(font.descent().saturating_neg()),
            line_gap: conv(font.line_gap()),
        }
    }
}

fn breaking_options(style: &ComputedValues) -> Options {
    use linebreak::{LineBreak, OverflowWrap, WordBreak};
    use style::computed_values::line_break::T as Lb;
    use style::computed_values::overflow_wrap::T as Ow;
    use style::computed_values::text_wrap_mode::T as Wrap;
    use style::computed_values::white_space_collapse::T as Collapse;
    use style::computed_values::word_break::T as Wb;

    let wrap = matches!(style.clone_text_wrap_mode(), Wrap::Wrap);
    let white_space = match (style.clone_white_space_collapse(), wrap) {
        (Collapse::Collapse, true) => WhiteSpace::Normal,
        (Collapse::Collapse, false) => WhiteSpace::Nowrap,
        (Collapse::Preserve, true) => WhiteSpace::PreWrap,
        (Collapse::Preserve, false) => WhiteSpace::Pre,
        (Collapse::PreserveBreaks, _) => WhiteSpace::PreLine,
        (Collapse::BreakSpaces, _) => WhiteSpace::BreakSpaces,
    };
    Options {
        line_break: match style.clone_line_break() {
            Lb::Auto => LineBreak::Auto,
            Lb::Loose => LineBreak::Loose,
            Lb::Normal => LineBreak::Normal,
            Lb::Strict => LineBreak::Strict,
            Lb::Anywhere => LineBreak::Anywhere,
        },
        word_break: match style.clone_word_break() {
            Wb::Normal => WordBreak::Normal,
            Wb::BreakAll => WordBreak::BreakAll,
            Wb::KeepAll => WordBreak::KeepAll,
        },
        overflow_wrap: match style.clone_overflow_wrap() {
            Ow::Normal => OverflowWrap::Normal,
            Ow::BreakWord => OverflowWrap::BreakWord,
            Ow::Anywhere => OverflowWrap::Anywhere,
        },
        white_space,
        hyphens: linebreak::Hyphens::Manual,
    }
}

fn align_of(style: &ComputedValues, rtl: bool) -> Align {
    use style::values::specified::text::TextAlignKeyword as K;
    // The computed value is the keyword; `match-parent` is resolved by stylo
    // against the parent already.
    let keyword = style.clone_text_align();
    match keyword {
        K::Left | K::MozLeft => Align::Left,
        K::Right | K::MozRight => Align::Right,
        K::Center | K::MozCenter => Align::Center,
        K::Justify => Align::Justify,
        K::Start => {
            if rtl {
                Align::Right
            } else {
                Align::Left
            }
        }
        K::End => {
            if rtl {
                Align::Left
            } else {
                Align::Right
            }
        }
    }
}

/// A font's vertical metrics at a size, in app units.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Metrics {
    /// `hhea.ascender`, above the baseline.
    pub ascent: Au,
    /// `hhea.descender` negated: below the baseline, positive.
    pub descent: Au,
    /// `hhea.lineGap`.
    pub line_gap: Au,
}

impl Metrics {
    /// `line-height: normal`: ascent plus descent plus line gap (CSS Inline 3,
    /// section 4.1: the font's own recommended line spacing).
    #[must_use]
    pub fn normal_line_height(&self) -> Au {
        self.ascent + self.descent + self.line_gap
    }

    /// The distance from the top of a line box `line_height` tall to the
    /// baseline of text in this font. The content area (ascent plus descent) is
    /// centred in the line box: half-leading goes above and below (CSS Inline 3,
    /// section 4.1). An odd leading puts the extra app unit below.
    #[must_use]
    pub fn baseline_in(&self, line_height: Au) -> Au {
        let leading = line_height - (self.ascent + self.descent);
        self.ascent + leading / 2
    }
}

/// A stretch of a paragraph's text drawn with one element's style.
///
/// The ranges handed to [`Prepared::paragraph`] tile the text in order; each one
/// is shaped on its own, with its own font and size, and its `node` is the
/// element whose `color` paints the glyphs.
#[derive(Clone, Copy, Debug)]
pub struct StyleRange {
    /// First byte.
    pub start: usize,
    /// One past the last byte.
    pub end: usize,
    /// The text style of the element the text belongs to.
    pub style: TextStyle,
    /// That element.
    pub node: Option<NodeId>,
    /// How far the text is raised above the line's baseline by `vertical-align`
    /// (`sub`, `super`, a length or percentage), the sum over its inline
    /// ancestors. Negative lowers.
    pub raise: Au,
}

/// The start or end edge of an inline box: its margin, border and padding on
/// that side, which take room on the line.
///
/// Edges are given in document order (so at one offset, ends of inner boxes come
/// before ends of outer ones, and starts of outer boxes before inner ones). An
/// open edge sits immediately before the character at `offset`, a close edge
/// immediately after the character before it.
#[derive(Clone, Copy, Debug)]
pub struct Edge {
    /// The byte offset in the paragraph text.
    pub offset: usize,
    /// A start edge (`true`) or an end edge.
    pub open: bool,
    /// Margin plus border plus padding on this side.
    pub width: Au,
    /// The margin alone: the part outside the border box.
    pub outer: Au,
    /// Which inline box this belongs to, for the caller.
    pub owner: usize,
}

/// An atomic inline (an `inline-block`): one object character in the text whose
/// advance is the margin box's width.
#[derive(Clone, Copy, Debug)]
pub struct Atomic {
    /// The byte offset of its `U+FFFC` in the paragraph text.
    pub offset: usize,
    /// The margin box's inline size.
    pub width: Au,
}

/// The character that stands for an atomic inline in a paragraph's text.
pub const OBJECT: char = '\u{fffc}';

/// One shaped glyph, with the advance already in app units.
#[derive(Clone, Copy, Debug)]
struct Glyph {
    id: u32,
    /// Byte offset in the whole text of the glyph's cluster.
    cluster: usize,
    /// One past the cluster's last byte.
    end: usize,
    advance: Au,
    x_offset: Au,
    y_offset: Au,
    /// The glyph stands for an atomic inline: advance, draw nothing.
    atomic: bool,
}

/// One bidi run, cut at style boundaries, shaped.
#[derive(Debug)]
struct ShapedRun {
    range: std::ops::Range<usize>,
    rtl: bool,
    /// Index into the paragraph's style ranges.
    style: usize,
    /// Visual order, left to right, as the shaper returns them.
    glyphs: Vec<Glyph>,
}

const SOFT_HYPHEN: char = '\u{ad}';

/// Text prepared for measuring and breaking: shaped, with a prefix sum of
/// advances and the break opportunities.
pub struct Prepared<'a> {
    text: &'a str,
    style: TextStyle,
    ranges: Vec<StyleRange>,
    edges: Vec<Edge>,
    runs: Vec<ShapedRun>,
    bidi: Option<bidi::Runs<'a>>,
    /// `prefix[i]` is the sum of the advances of glyphs whose cluster starts
    /// before byte `i`, plus the open edges before it. Length is `text.len() + 1`.
    prefix: Vec<Au>,
    /// `close_prefix[i]` is the sum of the close edges at offsets up to and
    /// including `i`.
    close_prefix: Vec<Au>,
    opps: Vec<Opportunity>,
    hyphen: Option<(u32, Au)>,
}

/// A script tag for a run, from its first character that says anything.
fn script_of(text: &str) -> px_text::shaper::Script {
    use px_text::shaper::Script;
    for c in text.chars() {
        let u = u32::from(c);
        let s = match u {
            0x0590..=0x05FF | 0xFB1D..=0xFB4F => Script::HEBREW,
            0x0600..=0x06FF
            | 0x0750..=0x077F
            | 0x08A0..=0x08FF
            | 0xFB50..=0xFDFF
            | 0xFE70..=0xFEFF => Script::ARABIC,
            0x0900..=0x097F => Script::DEVANAGARI,
            0x0E00..=0x0E7F => Script::THAI,
            0x0400..=0x052F => Script::CYRILLIC,
            0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF => Script::HAN,
            _ if c.is_alphabetic() => Script::LATIN,
            _ => continue,
        };
        return s;
    }
    Script::COMMON
}

/// Shape the bytes `piece` of `text` as one directional run in `style`'s font.
fn shape_piece(
    text: &str,
    piece: std::ops::Range<usize>,
    rtl: bool,
    style: &TextStyle,
    atomics: &[Atomic],
) -> Option<(std::ops::Range<usize>, Vec<Glyph>)> {
    let f = fonts();
    let font = f.registry.get(style.font)?;
    let shaper = f
        .shapers
        .get(usize::try_from(style.font.0).unwrap_or(usize::MAX))
        .and_then(Option::as_ref)?;
    let upem = font.units_per_em();
    let size = style.size.0;
    let slice = text.get(piece.clone())?;
    let shaped_text: std::borrow::Cow<'_, str> = if rtl {
        // L4: mirrored characters of right-to-left runs. Only a same-length
        // substitute is taken, so cluster offsets keep meaning byte offsets into
        // `text`.
        std::borrow::Cow::Owned(
            slice
                .chars()
                .map(|c| {
                    bidi::mirror(c)
                        .filter(|m| m.len_utf8() == c.len_utf8())
                        .unwrap_or(c)
                })
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(slice)
    };
    let input = ShapeInput {
        text: &shaped_text,
        script: script_of(slice),
        language: None,
        direction: if rtl {
            Direction::RightToLeft
        } else {
            Direction::LeftToRight
        },
        features: &[],
    };
    let out = shaper.shape(&input).ok()?;
    let mut glyphs: Vec<Glyph> = out
        .glyphs
        .iter()
        .map(|g| {
            let cluster = piece.start + usize::try_from(g.cluster).unwrap_or(0);
            let atomic = atomics.binary_search_by_key(&cluster, |a| a.offset).ok();
            Glyph {
                id: g.glyph_id,
                cluster,
                end: piece.end,
                advance: match atomic.and_then(|i| atomics.get(i)) {
                    Some(a) => a.width,
                    None => au(font_units_to_au(g.x_advance, upem, size)),
                },
                x_offset: au(font_units_to_au(g.x_offset, upem, size)),
                y_offset: au(font_units_to_au(g.y_offset, upem, size)),
                atomic: atomic.is_some(),
            }
        })
        .collect();
    // A cluster ends where the next one (in logical order) begins.
    let mut starts: Vec<usize> = glyphs.iter().map(|g| g.cluster).collect();
    starts.sort_unstable();
    starts.dedup();
    for g in &mut glyphs {
        let i = starts.partition_point(|s| *s <= g.cluster);
        g.end = starts.get(i).copied().unwrap_or(piece.end);
    }
    Some((piece, glyphs))
}

impl<'a> Prepared<'a> {
    /// Shape `text` (already white-space processed) in one style and find its
    /// break opportunities.
    #[must_use]
    pub fn new(text: &'a str, style: TextStyle) -> Self {
        Self::paragraph(text, style, &[], &[], &[])
    }

    /// Shape a paragraph of several styles.
    ///
    /// `ranges` tile `text` (empty means all of it in `style`); `style` is the
    /// block container's, and decides the paragraph direction, `white-space` and
    /// the breaking options for the whole paragraph. Bidi is resolved over the
    /// whole text and each directional run is then cut at style boundaries, so a
    /// span in the middle of a Hebrew phrase does not split the paragraph's
    /// reordering. Line breaking sees the concatenated text, so a break may fall
    /// at a span boundary. `edges` and `atomics` (sorted by offset) add
    /// inline-box edge widths and object advances to the measure.
    #[must_use]
    pub fn paragraph(
        text: &'a str,
        style: TextStyle,
        ranges: &[StyleRange],
        edges: &[Edge],
        atomics: &[Atomic],
    ) -> Self {
        let f = fonts();
        let font = f.registry.get(style.font);

        let needs_bidi = style.rtl || style.plaintext || !text.is_ascii();
        let analysis = needs_bidi.then(|| {
            bidi::analyze(
                text,
                if style.plaintext {
                    BaseDirection::Auto
                } else if style.rtl {
                    BaseDirection::Rtl
                } else {
                    BaseDirection::Ltr
                },
            )
        });
        let logical: Vec<bidi::Run> = match &analysis {
            Some(a) => a.runs(),
            None => vec![bidi::Run {
                range: 0..text.len(),
                level: 0,
            }],
        };
        let ranges: Vec<StyleRange> = if ranges.is_empty() {
            vec![StyleRange {
                start: 0,
                end: text.len(),
                style,
                node: None,
                raise: Au(0),
            }]
        } else {
            ranges.to_vec()
        };

        let mut runs = Vec::with_capacity(logical.len());
        for run in logical {
            let rtl = run.is_rtl();
            // The style ranges this directional run crosses, found by binary
            // search so a paragraph of many spans costs its length, not its
            // length times its span count.
            let first = ranges.partition_point(|r| r.end <= run.range.start);
            for (k, sr) in ranges.iter().enumerate().skip(first) {
                if sr.start >= run.range.end {
                    break;
                }
                let piece = sr.start.max(run.range.start)..sr.end.min(run.range.end);
                if piece.start >= piece.end {
                    continue;
                }
                let Some((range, glyphs)) = shape_piece(text, piece, rtl, &sr.style, atomics)
                else {
                    continue;
                };
                runs.push(ShapedRun {
                    range,
                    rtl,
                    style: k,
                    glyphs,
                });
            }
        }

        // Prefix sums. A soft hyphen is invisible unless a line ends on it, so it
        // advances nothing here; a line that does end on it adds the hyphen's
        // width back (`hyphen_advance`).
        let mut at = vec![Au(0); text.len() + 1];
        for run in &runs {
            for g in &run.glyphs {
                let soft = text
                    .get(g.cluster..)
                    .and_then(|s| s.chars().next())
                    .is_some_and(|c| c == SOFT_HYPHEN);
                if !soft && let Some(slot) = at.get_mut(g.cluster) {
                    *slot += g.advance;
                }
            }
        }
        let mut closes = vec![Au(0); text.len() + 1];
        for e in edges {
            let slot = if e.open { &mut at } else { &mut closes };
            if let Some(s) = slot.get_mut(e.offset) {
                *s += e.width;
            }
        }
        let mut prefix = Vec::with_capacity(text.len() + 1);
        let mut sum = Au(0);
        prefix.push(sum);
        for slot in at.iter().take(text.len()) {
            sum += *slot;
            prefix.push(sum);
        }
        let mut close_prefix = Vec::with_capacity(text.len() + 1);
        let mut sum = Au(0);
        for slot in &closes {
            sum += *slot;
            close_prefix.push(sum);
        }

        let hyphen = font.and_then(|font| {
            let id = font.glyph_index('-')?;
            let adv = font.advance(id)?;
            let upem = font.units_per_em();
            Some((
                u32::from(id),
                au(font_units_to_au(i32::from(adv), upem, style.size.0)),
            ))
        });

        let mut opps = linebreak::opportunities(text, &style.breaking);
        if !atomics.is_empty()
            && !matches!(
                style.breaking.white_space,
                WhiteSpace::Nowrap | WhiteSpace::Pre
            )
        {
            // CSS Inline 3: an atomic inline is a wrap opportunity on both
            // sides, whatever UAX #14 says of U+FFFC.
            for a in atomics {
                for offset in [a.offset, a.offset + OBJECT.len_utf8()] {
                    if offset > 0 && offset < text.len() {
                        opps.push(Opportunity {
                            offset,
                            kind: Kind::Soft,
                            hyphen: false,
                        });
                    }
                }
            }
            // Stable, so an existing (possibly stronger) entry wins the dedup.
            opps.sort_by_key(|o| o.offset);
            opps.dedup_by_key(|o| o.offset);
        }

        let mut edges = edges.to_vec();
        edges.sort_by_key(|e| e.offset);
        Prepared {
            text,
            style,
            ranges,
            edges,
            runs,
            bidi: analysis,
            prefix,
            close_prefix,
            opps,
            hyphen,
        }
    }

    /// The measure of the byte range `a..b` (on cluster boundaries): glyph
    /// advances, the start edges of boxes opening in `a..b` and the end edges of
    /// boxes closing in `a+1..=b`.
    fn width(&self, a: usize, b: usize) -> Au {
        let (Some(lo), Some(hi)) = (self.prefix.get(a), self.prefix.get(b)) else {
            return Au(0);
        };
        let (Some(clo), Some(chi)) = (self.close_prefix.get(a), self.close_prefix.get(b)) else {
            return Au(0);
        };
        (*hi - *lo) + (*chi - *clo)
    }

    fn hyphen_advance(&self) -> Au {
        self.hyphen.map_or(Au(0), |(_, w)| w)
    }

    /// Whether spaces at the end of a line are removed (or hang) rather than
    /// measured: every mode but `break-spaces` (CSS Text 3, section 4.1.3).
    fn trims_trailing_spaces(&self) -> bool {
        self.style.breaking.white_space != WhiteSpace::BreakSpaces
    }

    /// The end of the content of a line that runs `from..to`: forced-break
    /// characters and, where they are removed, trailing spaces taken off.
    fn content_end(&self, from: usize, to: usize) -> usize {
        let Some(slice) = self.text.get(from..to) else {
            return to;
        };
        let mut s = slice.trim_end_matches([
            '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}',
        ]);
        if self.trims_trailing_spaces() {
            s = s.trim_end_matches(' ');
        }
        from + s.len()
    }

    /// The width of `from..to` as a line would measure it: content only, plus the
    /// hyphen when the break is at a soft hyphen.
    fn line_width(&self, from: usize, to: usize, hyphen: bool) -> Au {
        let end = self.content_end(from, to);
        let w = self.width(from, end);
        if hyphen { w + self.hyphen_advance() } else { w }
    }

    /// The preferred minimum and maximum widths of the text (CSS 2.1 section
    /// 10.3.5): the widest unbreakable run, and the whole text on one line per
    /// forced break.
    #[must_use]
    pub fn intrinsic(&self) -> (Au, Au) {
        let mut min = Au(0);
        let mut max = Au(0);
        let mut seg_start = 0usize; // since the last break of either kind
        let mut line_start = 0usize; // since the last forced break
        let emergency = self.style.breaking.emergency_breaks_in_min_content();
        for o in &self.opps {
            let counts = match o.kind {
                Kind::Hard | Kind::Soft => true,
                Kind::Emergency => emergency,
            };
            if !counts {
                continue;
            }
            min = min.max(self.line_width(seg_start, o.offset, o.hyphen));
            seg_start = o.offset;
            if o.kind == Kind::Hard {
                max = max.max(self.line_width(line_start, o.offset, false));
                line_start = o.offset;
            }
        }
        if seg_start < self.text.len() {
            min = min.max(self.line_width(seg_start, self.text.len(), false));
        }
        if line_start < self.text.len() {
            max = max.max(self.line_width(line_start, self.text.len(), false));
        }
        (min, max)
    }

    /// Whether `from` is at or past the end of the text.
    #[must_use]
    pub fn is_done(&self, from: usize) -> bool {
        from >= self.text.len()
    }

    /// Break one line, starting at byte `from`, into `available` of inline size.
    ///
    /// Greedy, per CSS Text 3 section 5: the line takes everything up to the last
    /// wrap opportunity that fits; an unbreakable run wider than the line gets a
    /// line to itself and overflows (or is broken at a grapheme boundary when
    /// `overflow-wrap` allows it and nothing better fits).
    #[must_use]
    pub fn next_line(&self, from: usize, available: Au) -> Option<LineBreak> {
        let mut from = from;
        if self.trims_trailing_spaces() {
            // Collapsible and hanging spaces at the start of a line after a soft
            // break are removed.
            while self.text.get(from..).is_some_and(|s| s.starts_with(' ')) {
                from += 1;
            }
        }
        if from >= self.text.len() {
            return None;
        }
        let start = self.opps.partition_point(|o| o.offset <= from);
        let mut soft: Option<&Opportunity> = None;
        let mut emergency: Option<&Opportunity> = None;
        let mut chosen: Option<&Opportunity> = None;
        for o in self.opps.get(start..).unwrap_or(&[]) {
            let fits = self.line_width(from, o.offset, o.hyphen) <= available;
            match o.kind {
                Kind::Hard => {
                    chosen = Some(if fits {
                        o
                    } else {
                        soft.or(emergency).unwrap_or(o)
                    });
                }
                Kind::Soft => {
                    if fits {
                        soft = Some(o);
                    } else {
                        chosen = Some(soft.or(emergency).unwrap_or(o));
                    }
                }
                Kind::Emergency => {
                    if fits {
                        emergency = Some(o);
                    } else {
                        chosen = Some(soft.or(emergency).unwrap_or(o));
                    }
                }
            }
            if chosen.is_some() {
                break;
            }
        }
        let end = chosen.or(soft).or(emergency)?;
        let content_end = self.content_end(from, end.offset);
        let forced = matches!(end.kind, Kind::Hard)
            && self.text.get(from..end.offset).is_some_and(|s| {
                s.ends_with([
                    '\n', '\r', '\u{b}', '\u{c}', '\u{85}', '\u{2028}', '\u{2029}',
                ])
            });
        Some(LineBreak {
            start: from,
            content_end,
            next: end.offset,
            width: self.line_width(from, end.offset, end.hyphen),
            hyphen: end.hyphen,
            forced,
            last: end.offset >= self.text.len(),
        })
    }

    /// The content of one line, in visual order, left to right, positioned from
    /// the line's start edge. `extra_space` is added to every space
    /// (justification).
    #[must_use]
    pub fn line_runs(&self, line: &LineBreak, extra_space: Au) -> LineContent {
        let (a, e) = (line.start, line.content_end);
        let visual: Vec<bidi::Run> = match &self.bidi {
            Some(analysis) => analysis.reorder_line(a..e).unwrap_or_else(|| {
                vec![bidi::Run {
                    range: a..e,
                    level: 0,
                }]
            }),
            None => vec![bidi::Run {
                range: a..e,
                level: 0,
            }],
        };
        let mut content = LineContent::default();
        let mut pen = Au(0);
        for vr in visual {
            let rtl = vr.is_rtl();
            // The shaped runs that this visual run overlaps, in the order they
            // are drawn: logical order for left to right, reversed for right to
            // left.
            let first = self.runs.partition_point(|r| r.range.end <= vr.range.start);
            let mut sources: Vec<&ShapedRun> = self
                .runs
                .iter()
                .skip(first)
                .take_while(|r| r.range.start < vr.range.end)
                .filter(|r| vr.range.start < r.range.end)
                .collect();
            if rtl {
                sources.reverse();
            }
            for src in sources {
                let start_pen = pen;
                let mut glyphs: Vec<GlyphPos> = Vec::new();
                let mut picked: Vec<&Glyph> = src
                    .glyphs
                    .iter()
                    .filter(|g| g.cluster >= vr.range.start && g.cluster < vr.range.end)
                    .collect();
                if src.rtl != rtl {
                    picked.reverse();
                }
                for g in picked {
                    // What this glyph carries with it, in logical order: the
                    // start edges of boxes opening in its cluster, the glyph,
                    // the end edges of boxes closing after it. Mirrored for a
                    // right-to-left run, where logical order runs leftward.
                    let lo = self.edges.partition_point(|x| x.offset < g.cluster);
                    let hi = self.edges.partition_point(|x| x.offset <= g.end);
                    let mut items: Vec<Item> = Vec::new();
                    for (i, x) in self.edges.iter().enumerate().take(hi).skip(lo) {
                        if x.open && x.offset < g.end {
                            items.push(Item::Edge(i));
                        }
                    }
                    items.push(Item::Glyph);
                    for (i, x) in self.edges.iter().enumerate().take(hi).skip(lo) {
                        if !x.open && x.offset > g.cluster {
                            items.push(Item::Edge(i));
                        }
                    }
                    if rtl {
                        items.reverse();
                    }
                    for item in items {
                        match item {
                            Item::Edge(i) => {
                                let Some(x) = self.edges.get(i) else { continue };
                                let outside_left = x.open != rtl;
                                let (x0, x1) = if outside_left {
                                    (pen + x.outer, pen + x.width)
                                } else {
                                    (pen, pen + x.width - x.outer)
                                };
                                content.edges.push(EdgeOut {
                                    edge: i,
                                    owner: x.owner,
                                    open: x.open,
                                    left: outside_left,
                                    x0,
                                    x1,
                                });
                                pen += x.width;
                            }
                            Item::Glyph => {
                                let ch = self.text.get(g.cluster..).and_then(|s| s.chars().next());
                                let mut advance = g.advance;
                                if ch == Some(SOFT_HYPHEN) {
                                    // Only the soft hyphen that ends the line is
                                    // drawn.
                                    if line.hyphen
                                        && g.cluster + SOFT_HYPHEN.len_utf8() == line.content_end
                                        && let Some((hid, hadv)) = self.hyphen
                                    {
                                        glyphs.push(GlyphPos {
                                            glyph_id: hid,
                                            x: pen + g.x_offset - start_pen,
                                            y: Au(0) - g.y_offset,
                                            advance: hadv,
                                        });
                                        pen += hadv;
                                    }
                                    continue;
                                }
                                if ch == Some(' ') {
                                    advance += extra_space;
                                }
                                if !g.atomic {
                                    glyphs.push(GlyphPos {
                                        glyph_id: g.id,
                                        // Relative to the run, not the line.
                                        x: pen + g.x_offset - start_pen,
                                        // Positive y_offset is up; layout y grows
                                        // down.
                                        y: Au(0) - g.y_offset,
                                        advance,
                                    });
                                }
                                content.slots.push(Slot {
                                    cluster: g.cluster,
                                    x0: pen,
                                    x1: pen + advance,
                                    atomic: g.atomic,
                                });
                                pen += advance;
                            }
                        }
                    }
                }
                content.runs.push(LineRun {
                    rtl,
                    inline_offset: start_pen,
                    width: pen - start_pen,
                    glyphs,
                    style: src.style,
                });
            }
        }
        content
    }

    /// The style ranges the paragraph was prepared with.
    #[must_use]
    pub fn ranges(&self) -> &[StyleRange] {
        &self.ranges
    }

    /// The text style this was prepared with.
    #[must_use]
    pub fn style(&self) -> &TextStyle {
        &self.style
    }
}

/// One line, as the breaker found it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineBreak {
    /// First byte of the line (leading spaces removed).
    pub start: usize,
    /// One past the last byte of its content (trailing spaces and the forced
    /// break character removed).
    pub content_end: usize,
    /// Where the next line starts.
    pub next: usize,
    /// The line's measure, hyphen included.
    pub width: Au,
    /// The line ends at a soft hyphen, which is drawn as a hyphen.
    pub hyphen: bool,
    /// The line ends at a preserved newline or other forced break.
    pub forced: bool,
    /// No text follows this line.
    pub last: bool,
}

/// One directional run of a line, positioned relative to the line's start edge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineRun {
    /// The run is drawn right to left.
    pub rtl: bool,
    /// Where the run starts, from the line's start edge.
    pub inline_offset: Au,
    /// The run's advance.
    pub width: Au,
    /// Glyphs left to right; `x` is from the run's own start.
    pub glyphs: Vec<GlyphPos>,
    /// The index of the style range the run's text belongs to.
    pub style: usize,
}

/// An inline box edge as placed on a line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EdgeOut {
    /// The index in the paragraph's (offset-sorted) edge list.
    pub edge: usize,
    /// The `owner` the edge was given.
    pub owner: usize,
    /// A start edge.
    pub open: bool,
    /// The edge is on the left of the box's contents.
    pub left: bool,
    /// The part of the edge inside the border box, from the line's start edge.
    pub x0: Au,
    /// The far side of that part.
    pub x1: Au,
}

/// One glyph's room on a line, edges excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// The glyph's cluster, as a byte offset in the paragraph text.
    pub cluster: usize,
    /// Left side, from the line's start edge.
    pub x0: Au,
    /// Right side.
    pub x1: Au,
    /// The slot is an atomic inline's, not a glyph.
    pub atomic: bool,
}

/// What a line holds, in visual order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineContent {
    /// The glyph runs, one per (directional run, style range).
    pub runs: Vec<LineRun>,
    /// Inline box edges.
    pub edges: Vec<EdgeOut>,
    /// One slot per glyph, atomic inlines included.
    pub slots: Vec<Slot>,
}

/// One thing placed on a line, before it is positioned.
#[derive(Clone, Copy)]
enum Item {
    Edge(usize),
    Glyph,
}

/// The preferred minimum and maximum inline sizes of `text` under `style`.
#[must_use]
pub fn intrinsic_widths(text: &str, style: TextStyle) -> (Au, Au) {
    Prepared::new(text, style).intrinsic()
}

/// The font the style resolved to, for fragments that record it.
#[must_use]
pub fn font_of(font: FontId) -> Option<&'static Font> {
    fonts().registry.get(font)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ahem(size_px: i32) -> TextStyle {
        let mut s = TextStyle::initial();
        s.size = px(size_px);
        s.line_height = s.metrics().normal_line_height();
        s
    }

    #[test]
    fn text_ahem_glyphs_are_one_em_wide() {
        let p = Prepared::new("abc", ahem(10));
        assert_eq!(p.width(0, 3), px(30));
        assert_eq!(p.intrinsic(), (px(30), px(30)));
    }

    #[test]
    fn text_normal_line_height_is_ascent_plus_descent_plus_gap() {
        let m = ahem(20).metrics();
        assert_eq!(m.ascent, px(16));
        assert_eq!(m.descent, px(4));
        assert_eq!(m.normal_line_height(), px(20) + m.line_gap);
    }

    #[test]
    fn text_greedy_break_drops_the_trailing_space() {
        let p = Prepared::new("foo bar", ahem(10));
        let l = p.next_line(0, px(50)).expect("a line");
        assert_eq!((l.start, l.content_end, l.next), (0, 3, 4));
        assert_eq!(l.width, px(30));
    }
}
