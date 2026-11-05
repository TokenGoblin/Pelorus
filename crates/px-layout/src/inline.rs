//! Inline layout: inline content into line boxes (CSS 2.1 sections 9.4.2, 10.8, 16).
//!
//! # Anonymous block boxes, and why they are the point
//!
//! CSS 2.1 §9.2.1.1: when a block container has both inline and block-level
//! children, the inline content is wrapped in **anonymous block boxes**. So
//!
//! ```text
//! <span>text <div>block</div> text</span>
//! ```
//!
//! produces three block-level boxes, two of them anonymous, not one inline box
//! containing a block.
//!
//! That rule is most of what ADR 029's reftest comparison exercises.
//! `normal-flow/block-in-inline-align-001.html` — the pair read while writing
//! that ADR — relies on the anonymous boxes its test generates lining up with the
//! three explicit `<div>`s its reference spells out. An engine that skipped
//! anonymous box generation would produce a *shorter* fragment list, which is why
//! [`crate::fragment::FragmentKind::AnonymousBlock`] is a distinct kind: the bug
//! does not look wrong, it looks absent.
//!
//! # The inline formatting context
//!
//! A run of inline content is an [`InlineRunData`]: the **concatenated, white-space
//! processed text** of the run, and beside it the structure that text came from.
//! Every character belongs to a [`StyleRange`] (the innermost element's text style
//! and the node whose `color` paints it); every inline element is an
//! [`InlineBox`] with a start and an end byte offset and the margin, border and
//! padding it puts on the line; an `inline-block` is one `U+FFFC` in the text; a
//! `<br>` is one `U+2028`, a forced break. White space collapses across element
//! boundaries because it is collapsed over the whole run as it is built.
//!
//! Lines are broken by [`crate::text::Prepared`] over that whole text, so a break
//! may fall at a span boundary, bidi is resolved over the whole paragraph and then
//! cut by style, and the measure of a line is its glyph advances plus the edges of
//! the boxes that open and close on it. Each shaped piece is in its own element's
//! font and size.
//!
//! # Line boxes (CSS 2.1 §10.8.1)
//!
//! A line box is as tall as its tallest part. The block's own strut (its font and
//! `line-height`, as if an empty inline box started the line), every inline box
//! on the line (each with *its* `line-height`, half-leading above and below its
//! content area) and every atomic inline (its margin box, on its baseline) are
//! aligned on one baseline; the line's top is the highest of them and its bottom
//! the lowest. Only `vertical-align: baseline` is implemented.
//!
//! # Fragments
//!
//! An inline box with a border or a background gets one
//! [`crate::fragment::FragmentKind::Inline`] fragment **per line it occupies**,
//! a child of that line, so paint can draw it. Margins, borders and padding are
//! only on the first and last of them (`box-decoration-break: slice`); the kind
//! records which of the left and right edges that fragment owns. An inline box
//! with neither gets no fragment: it changes where glyphs go and what colour they
//! are, not what is drawn behind them. An atomic inline's own fragment is a child
//! of its line.
//!
//! # What is not done
//!
//! No `vertical-align` but `baseline`; `text-decoration`, `letter-spacing`,
//! `word-spacing` and `text-transform` are not applied; `unicode-bidi` embeds and
//! overrides on spans are not (the paragraph direction is the block's); a span's
//! own `white-space` is used to collapse its text but the breaking options are the
//! block's; a percentage margin, border or padding on an inline box resolves
//! against zero; a float's band is queried with the block's strut height, not the
//! line's final one; a mixed-direction line gives a box one fragment spanning the
//! extent of its content rather than one per visual piece.

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::properties::ComputedValues;

use crate::block::Edges;
use crate::fragment::{Fragment, FragmentId, FragmentKind, FragmentTree, TextRun};
use crate::geom::{LogicalEdges, LogicalSize};
use crate::text::{self, Align, Atomic, Edge, OBJECT, Prepared, StyleRange, TextStyle, WhiteSpace};

/// The forced break `<br>` stands for in a run's text.
pub const LINE_BREAK: char = '\u{2028}';

/// One inline element in a run.
#[derive(Clone)]
pub(crate) struct InlineBox {
    /// The element.
    pub node: NodeId,
    /// Its text style (font, size, `line-height`, direction).
    pub text: TextStyle,
    /// Its resolved margin, border and padding.
    pub edges: Edges,
    /// Whether anything is painted for it: a border or a background colour.
    pub decorated: bool,
    /// The start edge is not here: the box began in an earlier run, before a
    /// block-level box interrupted it (§9.2.1.1).
    pub no_start: bool,
    /// The end edge is not here: a block-level box interrupts the box.
    pub no_end: bool,
    /// How far `vertical-align` raises it (and so its text) above the baseline
    /// of the line, including its ancestors' shifts.
    pub raise: Au,
    /// First byte of its content in the run's text.
    pub start: usize,
    /// One past its last byte.
    pub end: usize,
}

/// A box edge in document order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoxEvent {
    /// Where in the text.
    pub offset: usize,
    /// Start (`true`) or end.
    pub open: bool,
    /// The box, an index into [`InlineRunData::boxes`].
    pub owner: usize,
}

/// An `inline-block` in a run.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AtomicSpec {
    /// The element.
    pub node: NodeId,
    /// Where its `U+FFFC` is.
    pub offset: usize,
}

/// A run of inline content: the text and the structure it came from.
#[derive(Clone, Default)]
pub(crate) struct InlineRunData {
    /// The concatenated, white-space processed text.
    pub text: String,
    /// Style ranges tiling `text`; empty means all of it in the container's style.
    pub ranges: Vec<StyleRange>,
    /// Inline elements, in document (open) order.
    pub boxes: Vec<InlineBox>,
    /// Box starts and ends in document order.
    pub events: Vec<BoxEvent>,
    /// Inline-blocks, in text order.
    pub atomics: Vec<AtomicSpec>,
}

impl std::fmt::Debug for InlineRunData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InlineRunData")
            .field("text", &self.text)
            .field("boxes", &self.boxes.len())
            .field("atomics", &self.atomics.len())
            .finish()
    }
}

fn is_forced_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r' | '\u{b}' | '\u{c}' | '\u{85}' | '\u{2028}' | '\u{2029}'
    )
}

impl InlineRunData {
    /// A run of one style and no structure.
    #[must_use]
    pub fn plain(text: String) -> Self {
        Self {
            text,
            ..Self::default()
        }
    }

    fn extend_range(&mut self, start: usize, style: TextStyle, node: Option<NodeId>, raise: Au) {
        let end = self.text.len();
        if end <= start {
            return;
        }
        match self.ranges.last_mut() {
            Some(last) if last.node == node && last.end == start && last.raise == raise => {
                last.end = end;
            }
            _ => self.ranges.push(StyleRange {
                start,
                end,
                style,
                node,
                raise,
            }),
        }
    }

    /// Append the text of one text node, processed for `white_space` and
    /// collapsed against what is already there.
    pub fn push_text(
        &mut self,
        raw: &str,
        white_space: WhiteSpace,
        style: TextStyle,
        node: Option<NodeId>,
        raise: Au,
    ) {
        let start = self.text.len();
        match white_space {
            WhiteSpace::Normal | WhiteSpace::Nowrap => {
                // CSS Text 3 phase I: a collapsible space after another, or at
                // the start of a line, is removed, whichever element it is in.
                let mut last_space = self
                    .text
                    .chars()
                    .next_back()
                    .is_none_or(|c| c == ' ' || is_forced_break(c));
                for ch in raw.chars() {
                    if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                        if !last_space {
                            self.text.push(' ');
                            last_space = true;
                        }
                    } else {
                        self.text.push(ch);
                        last_space = false;
                    }
                }
            }
            other => self.text.push_str(&collapse_for(raw, other)),
        }
        self.extend_range(start, style, node, raise);
    }

    /// Append a single structural character (`U+2028` or `U+FFFC`).
    pub fn push_char(&mut self, ch: char, style: TextStyle, node: Option<NodeId>, raise: Au) {
        let start = self.text.len();
        self.text.push(ch);
        self.extend_range(start, style, node, raise);
    }

    /// Start a box at the current end of the text; returns its index.
    pub fn open(&mut self, mut b: InlineBox) -> usize {
        b.start = self.text.len();
        b.end = b.start;
        let owner = self.boxes.len();
        self.events.push(BoxEvent {
            offset: b.start,
            open: true,
            owner,
        });
        self.boxes.push(b);
        owner
    }

    /// End box `owner` at the current end of the text.
    pub fn close(&mut self, owner: usize) {
        let offset = self.text.len();
        if let Some(b) = self.boxes.get_mut(owner) {
            b.end = offset;
        }
        self.events.push(BoxEvent {
            offset,
            open: false,
            owner,
        });
    }

    /// Append an inline-block's object character.
    pub fn push_atomic(
        &mut self,
        node: NodeId,
        style: TextStyle,
        range_node: Option<NodeId>,
        raise: Au,
    ) {
        let offset = self.text.len();
        self.push_char(OBJECT, style, range_node, raise);
        self.atomics.push(AtomicSpec { node, offset });
    }

    /// Whether the run has nothing to lay out: no object, and no text but
    /// collapsible space.
    #[must_use]
    pub fn is_blank(&self, white_space: WhiteSpace) -> bool {
        if !self.atomics.is_empty() {
            return false;
        }
        match white_space {
            WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => self.text.is_empty(),
            WhiteSpace::PreLine => self.text.trim_matches([' ', '\t']).is_empty(),
            WhiteSpace::Normal | WhiteSpace::Nowrap => self.text.trim_matches(' ').is_empty(),
        }
    }

    /// Remove the trailing collapsible space of a collapsing run and clamp every
    /// offset to the shortened text. A box that starts at or past the end is
    /// dropped with its end.
    pub fn finish(&mut self, white_space: WhiteSpace) {
        if matches!(white_space, WhiteSpace::Normal | WhiteSpace::Nowrap) {
            while self.text.ends_with(' ') {
                self.text.pop();
            }
        }
        let len = self.text.len();
        while self.ranges.last().is_some_and(|r| r.start >= len) {
            self.ranges.pop();
        }
        if let Some(last) = self.ranges.last_mut() {
            last.end = last.end.min(len);
        }
        let dead: Vec<bool> = self.boxes.iter().map(|b| b.start >= len).collect();
        self.events
            .retain(|e| !dead.get(e.owner).copied().unwrap_or(true));
        for e in &mut self.events {
            e.offset = e.offset.min(len);
        }
        for b in &mut self.boxes {
            b.end = b.end.min(len);
        }
    }
}

/// Where `vertical-align` puts an atomic inline (CSS 2.1 §10.8.1).
///
/// `sub`, `super`, a length and a percentage raise or lower the box's own
/// baseline against its parent's. `top` and `bottom` align its margin box with the
/// line box's top and bottom edges. `middle`, `text-top` and `text-bottom` are
/// treated as `baseline`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VAlign {
    /// Raise by this much (negative lowers); zero is `baseline`.
    Raise(Au),
    /// Margin-box top at the line's top.
    Top,
    /// Margin-box bottom at the line's bottom.
    Bottom,
}

/// The `vertical-align` of an element, as far as this engine honours it.
///
/// `vertical-align` is a shorthand in the style engine this build links
/// (css-inline-3): `top`, `bottom`, `sub`, `super` and lengths are the
/// `baseline-shift` longhand. A percentage is of the element's own `line-height`;
/// `sub` lowers by a fifth and `super` raises by a third of the font size.
pub(crate) fn vertical_align_of(style: &ComputedValues, text: TextStyle) -> VAlign {
    use style::values::computed::BaselineShift;
    use style::values::generics::box_::BaselineShiftKeyword as K;
    match style.clone_baseline_shift() {
        BaselineShift::Keyword(K::Top) => VAlign::Top,
        BaselineShift::Keyword(K::Bottom) => VAlign::Bottom,
        BaselineShift::Keyword(K::Sub) => VAlign::Raise(Au(0) - text.size / 5),
        BaselineShift::Keyword(K::Super) => VAlign::Raise(text.size / 3),
        BaselineShift::Keyword(K::Center) => VAlign::Raise(Au(0)),
        BaselineShift::Length(lp) => VAlign::Raise(lp.to_used_value(text.line_height)),
    }
}

/// An inline-block that has been laid out and is waiting to be placed on a line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AtomicInfo {
    /// Its `vertical-align`.
    pub align: VAlign,
    /// Its fragment, already sized.
    pub fragment: FragmentId,
    /// Its margins.
    pub margin: LogicalEdges,
    /// Its border-box size.
    pub size: LogicalSize,
    /// The baseline, from the top of the border box (CSS 2.1 §10.8.1: the last
    /// line box's, or the bottom margin edge when there is none).
    pub baseline: Au,
}

impl AtomicInfo {
    fn margin_box_inline(&self) -> Au {
        self.size.inline + self.margin.inline_sum()
    }

    /// Margin-box top to baseline.
    fn above(&self) -> Au {
        self.margin.block_start + self.baseline
    }

    /// The margin box's block size.
    fn margin_box_block(&self) -> Au {
        self.size.block + self.margin.block_sum()
    }

    /// Baseline to margin-box bottom.
    fn below(&self) -> Au {
        self.size.block + self.margin.block_sum() - self.above()
    }
}

/// The structure of a run, as `layout_lines` takes it.
#[derive(Clone, Copy)]
pub(crate) struct RunContent<'a> {
    /// The run.
    pub run: &'a InlineRunData,
    /// Its inline-blocks, laid out, in the order of `run.atomics`.
    pub atomics: &'a [AtomicInfo],
}

/// The edges of a run's boxes as the shaper's measure wants them.
#[must_use]
pub(crate) fn run_edges(run: &InlineRunData) -> Vec<Edge> {
    run.events
        .iter()
        .filter_map(|ev| {
            let b = run.boxes.get(ev.owner)?;
            let e = b.edges;
            // Physical left for a left-to-right box's start; a right-to-left
            // box starts on its right.
            let left = ev.open != b.text.rtl;
            let pick = |m: LogicalEdges| if left { m.inline_start } else { m.inline_end };
            let width = pick(e.margin) + pick(e.border) + pick(e.padding);
            let absent = if ev.open { b.no_start } else { b.no_end };
            Some(Edge {
                offset: ev.offset,
                open: ev.open,
                width: if absent { Au(0) } else { width },
                outer: if absent { Au(0) } else { pick(e.margin) },
                owner: ev.owner,
            })
        })
        .collect()
}

/// The style ranges of a run, or one range of `style` when it has none.
fn ranges_of(
    run: Option<&InlineRunData>,
    len: usize,
    style: TextStyle,
    node: Option<NodeId>,
) -> Vec<StyleRange> {
    match run {
        Some(r) if !r.ranges.is_empty() => r.ranges.clone(),
        _ => vec![StyleRange {
            start: 0,
            end: len,
            style,
            node,
            raise: Au(0),
        }],
    }
}

/// A run of inline content to be laid out into lines.
pub(crate) struct InlineContext<'a> {
    /// The text, already concatenated in document order and white-space
    /// processed (see [`collapse_for`]).
    pub text: &'a str,
    /// The font, size, line height, breaking and direction the text is laid out
    /// with: the block container's.
    pub style: TextStyle,
    /// The element whose computed style gives the text colour of text with no
    /// element of its own; recorded on every text run so paint can find it.
    pub node: Option<NodeId>,
    /// The inline size available for lines, before floats narrow it.
    pub available: Au,
    /// The floats these lines must flow around (§9.5), if any.
    ///
    /// In the same coordinate space as [`InlineContext::block_origin`]: the
    /// content box of the block container these lines belong to.
    pub floats: Option<&'a crate::float::FloatContext>,
    /// Where the first line box starts along the block axis.
    ///
    /// Not always zero, because a run can follow other content in its container —
    /// and because which floats a line has to avoid depends on where it is.
    pub block_origin: Au,
    /// The inline boxes and inline-blocks of the run, if it has any.
    pub content: Option<RunContent<'a>>,
}

/// Lay `context` out into line fragments, appended to `tree`.
///
/// Returns the line fragments in order, and the total block size they occupy.
/// The caller attaches them — this function does not know whether they belong
/// directly to a block container or to an anonymous block wrapping them.
///
/// # One line at a time
///
/// Lines are built individually rather than measured all at once, because with
/// floats they are not all the same width: CSS 2.1 §9.5 shortens a line box to
/// the space its block position leaves free, so how much text fits on a line
/// depends on where that line ended up, which depends on how much text fit on the
/// lines above it. The loop is the dependency.
///
/// Without floats every band is the full width and this reduces to the greedy
/// single-width break.
///
/// A line fragment is as wide as its content (trailing spaces excluded, §16.6.1)
/// and is offset along the inline axis by the float band's edge and `text-align`.
/// Its shaped glyphs are in the tree's side arena
/// ([`FragmentTree::text_runs`]); its children are the fragments of the inline
/// boxes drawn on it and of the inline-blocks sitting on it.
#[expect(
    clippy::too_many_lines,
    reason = "one loop body: a line is measured, aligned and then every part \
              placed against the same baseline; splitting it passes a dozen \
              locals between functions"
)]
pub(crate) fn layout_lines(
    tree: &mut FragmentTree,
    context: &InlineContext<'_>,
) -> (Vec<FragmentId>, Au) {
    let mut lines = Vec::new();
    let mut block_cursor = context.block_origin;
    let style = context.style;

    let run = context.content.map(|c| c.run);
    let infos: &[AtomicInfo] = context.content.map_or(&[], |c| c.atomics);
    let boxes: &[InlineBox] = run.map_or(&[], |r| &r.boxes);
    let specs: &[AtomicSpec] = run.map_or(&[], |r| &r.atomics);
    let ranges = ranges_of(run, context.text.len(), style, context.node);
    let edge_list = run.map(run_edges).unwrap_or_default();
    let atomic_list: Vec<Atomic> = specs
        .iter()
        .zip(infos)
        .map(|(s, i)| Atomic {
            offset: s.offset,
            width: i.margin_box_inline(),
        })
        .collect();
    let prepared = Prepared::paragraph(context.text, style, &ranges, &edge_list, &atomic_list);

    let strut = style.metrics();
    let strut_above = strut.baseline_in(style.line_height);
    let strut_below = style.line_height - strut_above;
    let mut from = 0usize;
    // Inline boxes that reach the current line, outermost first.
    let mut active: Vec<usize> = Vec::new();
    let mut next_box = 0usize;

    while !prepared.is_done(from) {
        // The band is asked for with the strut's height: the line's own height is
        // not known until its content is (see the module documentation).
        let (start, end) = match context.floats {
            Some(floats) => floats.band(
                block_cursor,
                block_cursor + style.line_height,
                context.available,
            ),
            None => (Au(0), context.available),
        };

        // No room at this block position at all. CSS 2.1 §9.5 moves the line box
        // down past the float rather than overflowing it, and the next band edge
        // is strictly below, so this cannot spin.
        if end <= start
            && let Some(below) = context.floats.and_then(|f| f.next_edge_below(block_cursor))
        {
            block_cursor = below;
            continue;
        }

        let room = end - start;
        let Some(line) = prepared.next_line(from, room) else {
            break;
        };
        // §9.5: "If a shortened line box is too small to contain any content,
        // then the line box is shifted downward (and its width recomputed) until
        // either some content fits or there are no more floats present." The line
        // overflows its room only when its first unbreakable piece does not fit;
        // the next float edge is strictly below, so this cannot spin.
        if line.width > room
            && room < context.available
            && let Some(below) = context.floats.and_then(|f| f.next_edge_below(block_cursor))
        {
            block_cursor = below;
            continue;
        }
        // Progress is guaranteed by `next_line` (it returns a break strictly past
        // `from`), but stopping on a violation is what makes that a property of
        // this loop rather than a comment about another function.
        if line.next <= from {
            break;
        }
        from = line.next;

        // A line with no content exists only where a preserved newline made it
        // (`white-space: pre`); collapsible space on its own generates none
        // (§9.4.2).
        if line.content_end <= line.start && !line.forced {
            continue;
        }

        // `text-align`. Overflowing lines align to their start edge.
        let slack = (room - line.width).max(Au(0));
        let spaces = context
            .text
            .get(line.start..line.content_end)
            .map_or(0, |s| s.bytes().filter(|b| *b == b' ').count());
        let justify = style.align == Align::Justify && !line.last && !line.forced && spaces > 0;
        let (shift, extra_space, _width) = match style.align {
            Align::Justify if justify => {
                let n = i32::try_from(spaces).unwrap_or(i32::MAX);
                let each = slack / n;
                (Au(0), each, line.width + each * n)
            }
            Align::Right => (slack, Au(0), line.width),
            Align::Center => (slack / 2, Au(0), line.width),
            Align::Justify if style.rtl => (slack, Au(0), line.width),
            Align::Left | Align::Justify => (Au(0), Au(0), line.width),
        };
        let width = if justify {
            line.width + extra_space * i32::try_from(spaces).unwrap_or(i32::MAX)
        } else {
            line.width
        };

        let content = prepared.line_runs(&line, extra_space);

        // The inline boxes that reach this line. Boxes are in open order, so
        // those starting before the line's end are exactly a prefix; a box that
        // ended before the line starts is gone for every later line too.
        while let Some(b) = boxes.get(next_box)
            && b.start < line.content_end.max(line.start + 1)
        {
            active.push(next_box);
            next_box += 1;
        }
        active.retain(|&i| {
            boxes
                .get(i)
                .is_some_and(|b| b.end > line.start || (b.start == b.end && b.start >= line.start))
        });

        // §10.8.1: every part aligned on the baseline; the line is as tall as the
        // highest top and the lowest bottom.
        let mut above = strut_above;
        let mut below = strut_below;
        for &i in &active {
            let Some(b) = boxes.get(i) else { continue };
            let a = b.text.metrics().baseline_in(b.text.line_height);
            above = above.max(a + b.raise);
            below = below.max(b.text.line_height - a - b.raise);
        }
        // Atomic inlines that `vertical-align: top | bottom` take out of the
        // baseline alignment: they only make the line taller if they must.
        let mut tallest_aligned = Au(0);
        for slot in content.slots.iter().filter(|s| s.atomic) {
            if let Some(info) = specs
                .binary_search_by_key(&slot.cluster, |s| s.offset)
                .ok()
                .and_then(|k| infos.get(k))
            {
                match info.align {
                    VAlign::Raise(r) => {
                        above = above.max(info.above() + r);
                        below = below.max(info.below() - r);
                    }
                    VAlign::Top | VAlign::Bottom => {
                        tallest_aligned = tallest_aligned.max(info.margin_box_block());
                    }
                }
            }
        }
        // A tall top- or bottom-aligned box extends the line below the baseline.
        if tallest_aligned > above + below {
            below = tallest_aligned - above;
        }
        let line_height = above + below;
        let baseline = above;

        let fragment = tree.push(Fragment::new(
            FragmentKind::Line,
            LogicalSize::new(width, line_height),
        ));
        if let Some(f) = tree.get_mut(fragment) {
            f.inline_offset = start + shift;
            f.block_offset = block_cursor;
            f.baseline = baseline;
        }
        for r in &content.runs {
            if r.glyphs.is_empty() {
                continue;
            }
            let Some(range) = ranges.get(r.style) else {
                continue;
            };
            let record = TextRun::new(
                range.style.font,
                range.style.size,
                r.rtl,
                range.node,
                r.inline_offset,
                baseline - range.raise,
                r.width,
            );
            tree.add_text_run(fragment, record, &r.glyphs);
        }

        let mut kids: Vec<FragmentId> = Vec::new();
        // Painted boxes, outermost first.
        let mut by_cluster: Vec<text::Slot> = Vec::new();
        for &i in &active {
            let Some(b) = boxes.get(i).filter(|b| b.decorated) else {
                continue;
            };
            if by_cluster.is_empty() {
                by_cluster = content.slots.clone();
                by_cluster.sort_by_key(|s| s.cluster);
            }
            let lo = b.start.max(line.start);
            let hi = b.end.min(line.content_end);
            let first = by_cluster.partition_point(|s| s.cluster < lo);
            let last = by_cluster.partition_point(|s| s.cluster < hi);
            let mut x0: Option<Au> = None;
            let mut x1: Option<Au> = None;
            for s in by_cluster.get(first..last).unwrap_or(&[]) {
                x0 = Some(x0.map_or(s.x0, |v| v.min(s.x0)));
                x1 = Some(x1.map_or(s.x1, |v| v.max(s.x1)));
            }
            let (mut left, mut right) = (false, false);
            for e in content.edges.iter().filter(|e| e.owner == i) {
                x0 = Some(x0.map_or(e.x0, |v| v.min(e.x0)));
                x1 = Some(x1.map_or(e.x1, |v| v.max(e.x1)));
                if e.left {
                    left = true;
                } else {
                    right = true;
                }
            }
            let (Some(x0), Some(x1)) = (x0, x1) else {
                continue;
            };
            // An empty box with no edges has no area to paint.
            if x1 <= x0 {
                continue;
            }
            let m = b.text.metrics();
            let top = baseline
                - b.raise
                - m.ascent
                - b.edges.padding.block_start
                - b.edges.border.block_start;
            let bottom =
                baseline + m.descent + b.edges.padding.block_end + b.edges.border.block_end;
            let mut f = Fragment::new(
                FragmentKind::Inline { left, right },
                LogicalSize::new(x1 - x0, bottom - top),
            );
            f.node = Some(b.node);
            f.inline_offset = shift + x0;
            f.block_offset = top;
            kids.push(tree.push(f));
        }
        for slot in content.slots.iter().filter(|s| s.atomic) {
            if let Some(info) = specs
                .binary_search_by_key(&slot.cluster, |s| s.offset)
                .ok()
                .and_then(|k| infos.get(k))
                && let Some(f) = tree.get_mut(info.fragment)
            {
                f.inline_offset = shift + slot.x0 + info.margin.inline_start;
                f.block_offset = match info.align {
                    VAlign::Raise(r) => baseline - info.above() - r + info.margin.block_start,
                    VAlign::Top => info.margin.block_start,
                    VAlign::Bottom => {
                        line_height - info.margin_box_block() + info.margin.block_start
                    }
                };
                kids.push(info.fragment);
            }
        }
        if !kids.is_empty() {
            tree.set_children(fragment, &kids);
        }

        lines.push(fragment);
        block_cursor += line_height;
    }

    (lines, block_cursor - context.block_origin)
}

/// The preferred minimum and maximum inline sizes of a run (CSS 2.1 §10.3.5):
/// the text and box edges only. An inline-block counts as zero here; the caller
/// measures those boxes itself.
#[must_use]
pub(crate) fn run_intrinsic(run: &InlineRunData, style: TextStyle) -> (Au, Au) {
    let ranges = ranges_of(Some(run), run.text.len(), style, None);
    let edges = run_edges(run);
    let atomics: Vec<Atomic> = run
        .atomics
        .iter()
        .map(|a| Atomic {
            offset: a.offset,
            width: Au(0),
        })
        .collect();
    Prepared::paragraph(&run.text, style, &ranges, &edges, &atomics).intrinsic()
}

/// The text content of `node`'s subtree, in document order.
///
/// Concatenated rather than kept as separate runs, because Phase 6 has no inline
/// boxes — a `<span>` inside a paragraph contributes its text and nothing else.
/// That is wrong the moment a span has its own font size or padding, and it is
/// the next thing inline layout needs rather than a permanent simplification.
#[must_use]
pub fn collect_text(arena: &Arena, node: NodeId) -> String {
    let mut out = String::new();
    // Iterative, like every other walk in this crate.
    let mut stack = vec![node];
    let mut ordered = Vec::new();
    while let Some(id) = stack.pop() {
        ordered.push(id);
        let Some(n) = arena.get(id) else { continue };
        let mut child = n.first_child();
        let mut kids = Vec::new();
        while let Some(c) = child {
            kids.push(c);
            child = arena.get(c).and_then(px_dom::Node::next_sibling);
        }
        for kid in kids.into_iter().rev() {
            stack.push(kid);
        }
    }
    for id in ordered {
        if let Some(n) = arena.get(id)
            && let Some(t) = n.text()
        {
            out.push_str(t);
        }
    }
    out
}

/// Process the white space of a run of text for `white-space` (CSS Text 3,
/// section 4.1, phase I).
///
/// - `normal`, `nowrap`: runs of spaces, tabs and newlines become one space and
///   leading and trailing space is removed (see [`collapse_whitespace`]).
/// - `pre-line`: spaces and tabs collapse and are removed around newlines;
///   newlines stay and force breaks.
/// - `pre`, `pre-wrap`, `break-spaces`: everything is kept. Tabs become eight
///   spaces (`tab-size` is not implemented) and CR LF becomes LF.
#[must_use]
pub fn collapse_for(text: &str, white_space: WhiteSpace) -> String {
    match white_space {
        WhiteSpace::Normal | WhiteSpace::Nowrap => collapse_whitespace(text),
        WhiteSpace::PreLine => {
            let mut out = String::with_capacity(text.len());
            let mut pending_space = false;
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    '\r' if chars.peek() == Some(&'\n') => {}
                    '\n' | '\r' => {
                        pending_space = false;
                        while out.ends_with(' ') {
                            out.pop();
                        }
                        out.push('\n');
                    }
                    ' ' | '\t' => pending_space = true,
                    c => {
                        if pending_space && !out.is_empty() && !out.ends_with('\n') {
                            out.push(' ');
                        }
                        pending_space = false;
                        out.push(c);
                    }
                }
            }
            out
        }
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => {
            let mut out = String::with_capacity(text.len());
            let mut chars = text.chars().peekable();
            while let Some(ch) = chars.next() {
                match ch {
                    '\t' => out.push_str("        "),
                    '\r' => {
                        if chars.peek() != Some(&'\n') {
                            out.push('\n');
                        }
                    }
                    c => out.push(c),
                }
            }
            out
        }
    }
}

/// Whether a processed run has nothing to lay out: nothing at all when white
/// space is preserved, nothing but collapsible space otherwise.
#[must_use]
pub fn is_blank(text: &str, white_space: WhiteSpace) -> bool {
    match white_space {
        WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::BreakSpaces => text.is_empty(),
        WhiteSpace::PreLine => text.trim_matches([' ', '\t']).is_empty(),
        WhiteSpace::Normal | WhiteSpace::Nowrap => text.trim().is_empty(),
    }
}

/// Collapse white space per CSS 2.1 §16.6.1 for `white-space: normal`.
///
/// Runs of spaces, tabs and newlines become a single space, and leading and
/// trailing space is removed. Markup is written with indentation, so without this
/// every `<div>\n  text\n</div>` measures as if its indentation were content —
/// which would make every box in the vendored corpus wider than its reference.
#[must_use]
pub fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_space = false;
    for ch in text.chars() {
        if ch.is_whitespace() {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(' ');
            }
            in_space = false;
            out.push(ch);
        }
    }
    out
}

/// The line height for `style`, in app units: `normal` from the font's own
/// metrics, a number times the font size, or a length. See
/// [`TextStyle::of`].
#[must_use]
pub fn line_height_of(style: &ComputedValues) -> Au {
    text::TextStyle::of(style).line_height
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    /// 10px Ahem with a 12px line: an em is 10px wide, so "abc" is 30px.
    fn style() -> TextStyle {
        let mut s = TextStyle::initial();
        s.size = px(10);
        s.line_height = px(12);
        s
    }

    fn context<'a>(text: &'a str, available_px: i32) -> InlineContext<'a> {
        InlineContext {
            text,
            style: style(),
            node: None,
            available: px(available_px),
            floats: None,
            block_origin: Au(0),
            content: None,
        }
    }

    /// The width of each line a context produces, in order.
    fn widths(context: &InlineContext<'_>) -> Vec<Au> {
        let mut tree = FragmentTree::new();
        let (lines, _) = layout_lines(&mut tree, context);
        lines
            .into_iter()
            .filter_map(|id| tree.get(id).map(|f| f.size.inline))
            .collect()
    }

    #[test]
    fn inline_text_that_fits_is_one_line() {
        assert_eq!(widths(&context("abc", 100)), vec![px(30)]);
    }

    #[test]
    fn inline_text_breaks_greedily_at_spaces() {
        // Words are 30px; "foo bar" is 70px. At 80px "foo bar" fits and
        // "baz" would make 110.
        let lines = widths(&context("foo bar baz", 80));
        assert_eq!(lines, vec![px(70), px(30)]);
    }

    #[test]
    fn inline_trailing_space_does_not_widen_a_broken_line() {
        let lines = widths(&context("foo bar", 40));
        assert_eq!(lines, vec![px(30), px(30)]);
    }

    #[test]
    fn inline_an_unbreakable_word_overflows_rather_than_breaking() {
        let lines = widths(&context("abcdefgh", 20));
        assert_eq!(lines, vec![px(80)]);
    }

    #[test]
    fn inline_empty_text_produces_no_lines() {
        assert!(widths(&context("", 100)).is_empty());
        assert!(widths(&context("   ", 100)).is_empty());
    }

    /// §9.5: a line box shortens to the space a float leaves it.
    #[test]
    fn inline_a_float_shortens_the_lines_beside_it() {
        let mut floats = crate::float::FloatContext::new();
        // 40px wide and 12px tall: exactly one line box deep.
        floats.place(
            crate::float::FloatSide::Start,
            crate::geom::LogicalSize::new(px(40), px(12)),
            Au(0),
            px(100),
        );

        let mut context = context("foo bar baz", 100);
        context.floats = Some(&floats);

        let mut tree = FragmentTree::new();
        let (lines, _) = layout_lines(&mut tree, &context);

        // Without the float "foo bar" is 70px and fits 100, "baz" would not.
        // With it the first line has 60px: only "foo" fits.
        assert_eq!(lines.len(), 2, "the float pushed a word onto another line");
        assert_eq!(tree.get(lines[0]).map(|f| f.inline_offset), Some(px(40)));
        assert_eq!(
            tree.get(lines[1]).map(|f| f.inline_offset),
            Some(px(0)),
            "the second line is below the float and starts at the edge"
        );
    }

    /// A float leaving no room at all moves the line down rather than overlapping.
    #[test]
    fn inline_a_full_width_float_pushes_the_line_below_it() {
        let mut floats = crate::float::FloatContext::new();
        floats.place(
            crate::float::FloatSide::Start,
            crate::geom::LogicalSize::new(px(100), px(50)),
            Au(0),
            px(100),
        );

        let mut context = context("foo", 100);
        context.floats = Some(&floats);

        let mut tree = FragmentTree::new();
        let (lines, height) = layout_lines(&mut tree, &context);
        assert_eq!(lines.len(), 1);
        assert_eq!(tree.get(lines[0]).map(|f| f.block_offset), Some(px(50)));
        assert_eq!(height, px(62), "the height includes the space skipped");
    }

    /// The run starts where its container's cursor is, not at zero.
    #[test]
    fn inline_the_block_origin_offsets_the_first_line() {
        let mut context = context("foo", 100);
        context.block_origin = px(33);
        let mut tree = FragmentTree::new();
        let (lines, height) = layout_lines(&mut tree, &context);
        assert_eq!(tree.get(lines[0]).map(|f| f.block_offset), Some(px(33)));
        assert_eq!(height, px(12), "the height is the run's, not the offset's");
    }

    #[test]
    fn inline_whitespace_collapses_to_single_spaces() {
        assert_eq!(collapse_whitespace("  foo \n  bar  "), "foo bar");
        assert_eq!(collapse_whitespace("foo"), "foo");
        assert_eq!(collapse_whitespace("\n\t "), "");
    }

    #[test]
    fn inline_lines_stack_by_line_height() {
        let mut tree = FragmentTree::new();
        let (lines, total) = layout_lines(&mut tree, &context("foo bar baz", 40));
        assert_eq!(lines.len(), 3);
        assert_eq!(total, px(36), "three lines at 12px");

        let first = tree.get(lines[0]).expect("resolves");
        let second = tree.get(lines[1]).expect("resolves");
        assert_eq!(first.block_offset, px(0));
        assert_eq!(second.block_offset, px(12));
        assert_eq!(first.kind, FragmentKind::Line);
    }
}
