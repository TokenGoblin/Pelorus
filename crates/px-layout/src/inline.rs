//! Inline layout: text into line boxes.
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
//! # Line breaking
//!
//! Lines are broken by [`crate::text::Prepared::next_line`] over the UAX #14
//! opportunities `px-text` finds, honouring `white-space`, `overflow-wrap`,
//! `word-break` and `line-break`. Each line is one font's worth tall: its box is
//! `line-height` and its baseline sits where the font's ascent and descent put it
//! inside that (see [`crate::text::Metrics::baseline_in`]). What is still a named
//! gap: no `vertical-align`, no inline boxes of their own (an inline element's
//! text is flattened into its block's run), no inline-blocks on a line.

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::properties::ComputedValues;

use crate::fragment::{Fragment, FragmentKind, FragmentTree, TextRun};
use crate::geom::LogicalSize;
use crate::text::{self, Align, Prepared, TextStyle, WhiteSpace};

/// A run of inline content to be laid out into lines.
pub struct InlineContext<'a> {
    /// The text, already concatenated in document order and white-space
    /// processed (see [`collapse_for`]).
    pub text: &'a str,
    /// The font, size, line height, breaking and direction the text is laid out
    /// with.
    pub style: TextStyle,
    /// The element whose computed style gives the text colour; recorded on every
    /// text run so paint can find it.
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
/// ([`FragmentTree::text_runs`]).
pub fn layout_lines(
    tree: &mut FragmentTree,
    context: &InlineContext<'_>,
) -> (Vec<crate::fragment::FragmentId>, Au) {
    let mut lines = Vec::new();
    let mut block_cursor = context.block_origin;
    let style = context.style;
    let prepared = Prepared::new(context.text, style);
    let line_height = style.line_height;
    let baseline = style.metrics().baseline_in(line_height);
    let mut from = 0usize;

    while !prepared.is_done(from) {
        let (start, end) = match context.floats {
            Some(floats) => {
                floats.band(block_cursor, block_cursor + line_height, context.available)
            }
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
        let (shift, extra_space, width) = match style.align {
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

        let fragment = tree.push(Fragment::new(
            FragmentKind::Line,
            LogicalSize::new(width, line_height),
        ));
        if let Some(f) = tree.get_mut(fragment) {
            f.inline_offset = start + shift;
            f.block_offset = block_cursor;
        }
        for run in prepared.line_runs(&line, extra_space) {
            let record = TextRun::new(
                run.font,
                style.size,
                run.rtl,
                context.node,
                run.inline_offset,
                baseline,
                run.width,
            );
            tree.add_text_run(fragment, record, &run.glyphs);
        }
        lines.push(fragment);
        block_cursor += line_height;
    }

    (lines, block_cursor - context.block_origin)
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
