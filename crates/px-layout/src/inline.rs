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
//! # Line breaking, and what it is not
//!
//! Lines break at ASCII spaces, greedily, at the first word that does not fit.
//! That is not UAX #14 — see [`crate::text`] for what that costs — and it is not
//! the CSS 2.1 §9.4.2 line box algorithm either: there is no vertical alignment,
//! no baselines, no floats intruding. A line box is the full available inline
//! size and one line-height tall.
//!
//! Every one of those is a named gap. None of them is an approximation that
//! silently produces almost-right geometry.

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::properties::ComputedValues;

use crate::fragment::{Fragment, FragmentKind, FragmentTree};
use crate::geom::LogicalSize;
use crate::text;

/// A run of inline content to be laid out into lines.
pub struct InlineContext<'a> {
    /// The text, already concatenated in document order.
    pub text: &'a str,
    /// The font size the text is measured at.
    pub font_size: Au,
    /// The height of one line box.
    pub line_height: Au,
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
/// single-width break it replaced.
pub fn layout_lines(
    tree: &mut FragmentTree,
    context: &InlineContext<'_>,
) -> (Vec<crate::fragment::FragmentId>, Au) {
    let mut lines = Vec::new();
    let mut block_cursor = context.block_origin;
    let words = text::words(context.text, context.font_size);
    let mut next = 0usize;

    while next < words.len() {
        let (start, end) = match context.floats {
            Some(floats) => floats.band(
                block_cursor,
                block_cursor + context.line_height,
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

        let (consumed, width) = fill_line(&words[next..], end - start, context.font_size);
        if consumed == 0 {
            // `fill_line` takes at least one word whenever it is given one, so
            // reaching here means the slice was empty -- but stopping on it is
            // what makes that a property of this loop rather than a comment about
            // another function.
            break;
        }
        next += consumed;

        if width > Au(0) {
            let fragment = tree.push(Fragment::new(
                FragmentKind::Line,
                LogicalSize::new(width, context.line_height),
            ));
            if let Some(f) = tree.get_mut(fragment) {
                f.inline_offset = start;
                f.block_offset = block_cursor;
            }
            lines.push(fragment);
            block_cursor += context.line_height;
        }
    }

    (lines, block_cursor - context.block_origin)
}

/// Fill one line box `available` wide from the front of `words`.
///
/// Returns how many words the line takes and how wide it ends up.
///
/// Greedy: words are added until one does not fit, then the line closes. A word
/// wider than the available size gets a line to itself and overflows, which is
/// what CSS 2.1 §9.4.2 requires — an unbreakable word is not broken, it sticks
/// out. That is also why this always takes at least one word when it is given
/// one: the alternative is a caller that never advances.
///
/// The returned width excludes trailing spaces (§16.6.1) while the fitting
/// decision includes them, which is why the two are tracked separately.
fn fill_line(words: &[(&str, Au)], available: Au, font_size: Au) -> (usize, Au) {
    let mut consumed = 0usize;
    let mut current = Au(0);
    let mut trimmed_width = Au(0);
    let mut has_content = false;

    for (word, width) in words {
        let trimmed = text::width_without_trailing_spaces(word, font_size);

        if has_content && current + trimmed > available {
            break;
        }

        consumed += 1;
        current += *width;
        trimmed_width = current - (*width - trimmed);
        // Only real content opens a line. A "word" that is nothing but spaces
        // trims to zero width, and CSS 2.1 §9.4.2 generates no line box for a
        // block containing only collapsible white space — so `"   "` must produce
        // no lines rather than one empty one. `collapse_whitespace` usually
        // removes it first; this makes the function right on its own rather than
        // right only when called in the expected order.
        if trimmed > Au(0) {
            has_content = true;
        }
    }

    (consumed, if has_content { trimmed_width } else { Au(0) })
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

/// The line height for `style`, in app units.
///
/// `line-height: normal` is taken as 1.2 times the font size, the conventional
/// value. Computed as `font_size * 6 / 5` to stay in integers — the gate forbids
/// floats in this crate, and `* 1.2` would be the obvious way to write it.
#[must_use]
pub fn line_height_of(style: &ComputedValues) -> Au {
    use style::values::generics::font::LineHeight;

    let font_size = style.clone_font_size().computed_size();
    let font_size_au = Au::from(font_size);

    match style.clone_line_height() {
        // `font_size_au * 6 / 5`, not `au(font_size_au.0 * 6 / 5)`. Reaching into
        // `Au`'s public field multiplies a raw `i32`, which panics in debug and
        // wraps in release above ~5.96M px — reachable from page CSS with
        // `font-size: 100000000px`, and precisely the panic-or-wrap §4.2 exists
        // to prevent. app_units' `Mul<i32>` is checked and saturating.
        //
        // `geom.rs` states the rule as "never write `Au(...)`" and this was the
        // hole in it: the hazard here is *field access*, which the tuple-
        // constructor scan in ci/gate-layout.sh cannot see.
        LineHeight::Normal => font_size_au * 6 / 5,
        // `line-height: 1.5` is a float multiplier and there is no integer
        // spelling of it. `Au::scale_by` does the multiply inside `app_units`,
        // which is the same argument Phase 5 made for `GenericAtomIdent::cast`:
        // the operation belongs to the vendored crate, it happens exactly once,
        // and nothing accumulates. §4.2's concern is float *accumulation* making
        // repeat runs differ — a single deterministic conversion of a specified
        // value is what every engine does, including stylo.
        //
        // Rounding the multiplier to an integer instead would turn `1.5` into
        // `2`, which was the first version of this line.
        LineHeight::Number(number) => font_size_au.scale_by(number.0),
        LineHeight::Length(length) => Au::from(length.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    fn context<'a>(text: &'a str, available_px: i32) -> InlineContext<'a> {
        InlineContext {
            text,
            font_size: px(16),
            line_height: px(20),
            available: px(available_px),
            floats: None,
            block_origin: Au(0),
        }
    }

    /// The width of each line a context produces, in order.
    ///
    /// Line breaking used to have a function of this shape and no longer does:
    /// with floats a line's width depends on where it ended up, so the widths only
    /// exist once the lines have been placed. These tests are about the breaking
    /// decisions rather than the placement, so they read the widths back off the
    /// fragments.
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
        // "abc" is three characters at 8px = 24px.
        let lines = widths(&context("abc", 100));
        assert_eq!(lines, vec![px(24)]);
    }

    #[test]
    fn inline_text_breaks_greedily_at_spaces() {
        // Each word is 3 chars = 24px; "foo " is 32px with its space.
        // At 60px available: "foo " (32) fits, + "bar" (24) = 56 fits,
        // + "baz" would be 80, so it breaks.
        let lines = widths(&context("foo bar baz", 60));
        assert_eq!(lines.len(), 2, "three words at 60px make two lines");
    }

    #[test]
    fn inline_trailing_space_does_not_widen_a_broken_line() {
        // The first line ends with a space that caused the break; §16.6.1 says
        // the line's width excludes it.
        let lines = widths(&context("foo bar", 40));
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0],
            px(24),
            "the trailing space is dropped, so 3 chars"
        );
    }

    #[test]
    fn inline_an_unbreakable_word_overflows_rather_than_breaking() {
        // Eight characters at 8px = 64px, in a 20px box.
        let lines = widths(&context("abcdefgh", 20));
        assert_eq!(
            lines,
            vec![px(64)],
            "a word wider than the line is not broken; it overflows"
        );
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
        // 40px wide and 20px tall: exactly one line box deep.
        floats.place(
            crate::float::FloatSide::Start,
            crate::geom::LogicalSize::new(px(40), px(20)),
            Au(0),
            px(100),
        );

        let mut context = context("foo bar baz", 100);
        context.floats = Some(&floats);

        let mut tree = FragmentTree::new();
        let (lines, _) = layout_lines(&mut tree, &context);

        // Without the float all three words fit on one 100px line: 32 + 32 + 24.
        // With it the first line has only 60px, so "baz" moves down -- and the
        // second line is clear of the float, so it starts back at the edge.
        assert_eq!(lines.len(), 2, "the float pushed a word onto a second line");
        assert_eq!(
            tree.get(lines[0]).map(|f| f.inline_offset),
            Some(px(40)),
            "the first line starts past the float"
        );
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
        assert_eq!(
            tree.get(lines[0]).map(|f| f.block_offset),
            Some(px(50)),
            "the line box moved below the float"
        );
        assert_eq!(height, px(70), "the height includes the space skipped");
    }

    /// The run starts where its container's cursor is, not at zero.
    #[test]
    fn inline_the_block_origin_offsets_the_first_line() {
        let mut context = context("foo", 100);
        context.block_origin = px(33);
        let mut tree = FragmentTree::new();
        let (lines, height) = layout_lines(&mut tree, &context);
        assert_eq!(tree.get(lines[0]).map(|f| f.block_offset), Some(px(33)));
        assert_eq!(height, px(20), "the height is the run's, not the offset's");
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
        let (lines, total) = layout_lines(&mut tree, &context("foo bar baz", 60));
        assert_eq!(lines.len(), 2);
        assert_eq!(total, px(40), "two lines at 20px");

        let first = tree.get(lines[0]).expect("resolves");
        let second = tree.get(lines[1]).expect("resolves");
        assert_eq!(first.block_offset, px(0));
        assert_eq!(second.block_offset, px(20));
        assert_eq!(first.kind, FragmentKind::Line);
    }
}
