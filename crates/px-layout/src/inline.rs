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
use crate::geom::{LogicalSize, au};
use crate::text;

/// A run of inline content to be laid out into lines.
pub struct InlineContext<'a> {
    /// The text, already concatenated in document order.
    pub text: &'a str,
    /// The font size the text is measured at.
    pub font_size: Au,
    /// The height of one line box.
    pub line_height: Au,
    /// The inline size available for lines.
    pub available: Au,
}

/// Lay `context` out into line fragments, appended to `tree`.
///
/// Returns the line fragments in order, and the total block size they occupy.
/// The caller attaches them — this function does not know whether they belong
/// directly to a block container or to an anonymous block wrapping them.
pub fn layout_lines(
    tree: &mut FragmentTree,
    context: &InlineContext<'_>,
) -> (Vec<crate::fragment::FragmentId>, Au) {
    let mut lines = Vec::new();
    let mut block_cursor = Au(0);

    for line_width in break_into_lines(context) {
        let fragment = tree.push(Fragment::new(
            FragmentKind::Line,
            LogicalSize::new(line_width, context.line_height),
        ));
        if let Some(f) = tree.get_mut(fragment) {
            f.block_offset = block_cursor;
        }
        lines.push(fragment);
        block_cursor += context.line_height;
    }

    (lines, block_cursor)
}

/// The width of each line, in order.
///
/// Greedy: words are added until one does not fit, then the line closes. A word
/// wider than the available size gets a line to itself and overflows, which is
/// what CSS 2.1 §9.4.2 requires — an unbreakable word is not broken, it sticks
/// out.
fn break_into_lines(context: &InlineContext<'_>) -> Vec<Au> {
    let mut lines = Vec::new();
    let mut current = Au(0);
    let mut has_content = false;
    // Tracked separately from `current` because a line's *width* excludes the
    // trailing space that caused the break (§16.6.1) while the fitting decision
    // includes it.
    let mut current_trimmed = Au(0);

    for (word, width) in text::words(context.text, context.font_size) {
        let trimmed = text::width_without_trailing_spaces(word, context.font_size);

        if has_content && current + trimmed > context.available {
            lines.push(current_trimmed);
            current = width;
            current_trimmed = trimmed;
            continue;
        }

        current += width;
        current_trimmed = current - (width - trimmed);
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

    if has_content {
        lines.push(current_trimmed);
    }
    lines
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
        LineHeight::Normal => au(font_size_au.0 * 6 / 5),
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
        }
    }

    #[test]
    fn inline_text_that_fits_is_one_line() {
        // "abc" is three characters at 8px = 24px.
        let lines = break_into_lines(&context("abc", 100));
        assert_eq!(lines, vec![px(24)]);
    }

    #[test]
    fn inline_text_breaks_greedily_at_spaces() {
        // Each word is 3 chars = 24px; "foo " is 32px with its space.
        // At 60px available: "foo " (32) fits, + "bar" (24) = 56 fits,
        // + "baz" would be 80, so it breaks.
        let lines = break_into_lines(&context("foo bar baz", 60));
        assert_eq!(lines.len(), 2, "three words at 60px make two lines");
    }

    #[test]
    fn inline_trailing_space_does_not_widen_a_broken_line() {
        // The first line ends with a space that caused the break; §16.6.1 says
        // the line's width excludes it.
        let lines = break_into_lines(&context("foo bar", 40));
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
        let lines = break_into_lines(&context("abcdefgh", 20));
        assert_eq!(
            lines,
            vec![px(64)],
            "a word wider than the line is not broken; it overflows"
        );
    }

    #[test]
    fn inline_empty_text_produces_no_lines() {
        assert!(break_into_lines(&context("", 100)).is_empty());
        assert!(break_into_lines(&context("   ", 100)).is_empty());
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
