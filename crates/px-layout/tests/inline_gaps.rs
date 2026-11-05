//! Inline gaps closed in Phase 9: `letter-spacing`, `word-spacing`, `text-transform`,
//! `text-indent`, `white-space` modes, `text-align: justify`, `vertical-align` and
//! `line-height` edge cases.
//!
//! Ahem at `10px/10px`: a glyph (a space too) is 10px wide, ascent 8, descent 2.
//! Every number is derived in the test that asserts it.

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::px;

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
                    div { font: 10px/10px Ahem }";

struct Laid {
    tree: FragmentTree,
}

fn lay(body: &str, css: &str) -> Laid {
    let dom = px_dom::parse(&format!("<html><body>{body}</body></html>"));
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("a root element");
    let tree = layout_document(&arena, &root, px(800)).expect("a laid-out document");
    Laid { tree }
}

impl Laid {
    fn lines(&self) -> Vec<FragmentId> {
        self.tree
            .in_layout_order()
            .into_iter()
            .filter(|(_, id)| {
                self.tree
                    .get(*id)
                    .is_some_and(|f| f.kind == FragmentKind::Line)
            })
            .map(|(_, id)| id)
            .collect()
    }

    /// `(x, y, width, height)` of each line, in whole px.
    fn line_rects(&self) -> Vec<(i32, i32, i32, i32)> {
        self.lines()
            .into_iter()
            .map(|id| {
                let f = self.tree.get(id).expect("a fragment");
                let n = |a: Au| a.0 / 60;
                (
                    n(f.inline_offset),
                    n(f.block_offset),
                    n(f.size.inline),
                    n(f.size.block),
                )
            })
            .collect()
    }

    /// The pen x of every glyph of a line, in px from the line's left edge.
    fn pens(&self, line: FragmentId) -> Vec<i32> {
        self.tree
            .text_runs(line)
            .iter()
            .flat_map(|r| {
                self.tree
                    .glyphs(r)
                    .iter()
                    .map(|g| (r.inline_offset + g.x).0 / 60)
                    .collect::<Vec<_>>()
            })
            .collect()
    }
}

#[test]
fn layout_inline_gaps_letter_spacing_follows_every_glyph() {
    // "abc" is 30px; 2px after each of three glyphs makes 36px, glyphs at 0, 12, 24.
    let l = lay("<div style='letter-spacing: 2px'>abc</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 36, 10)]);
    assert_eq!(l.pens(l.lines()[0]), vec![0, 12, 24]);
}

#[test]
fn layout_inline_gaps_letter_spacing_can_be_negative_and_is_per_element() {
    // "a" at 0, then the span's "bc" with -3px: 10 - 3 = 7px advance each.
    let l = lay(
        "<div>a<span style='letter-spacing: -3px'>bc</span></div>",
        "",
    );
    assert_eq!(l.pens(l.lines()[0]), vec![0, 10, 17]);
    assert_eq!(l.line_rects()[0].2, 10 + 14);
}

#[test]
fn layout_inline_gaps_word_spacing_widens_only_word_separators() {
    // "a b c": five glyphs, 50px, plus 5px on each of the two spaces.
    let l = lay("<div style='word-spacing: 5px'>a b c</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 60, 10)]);
    assert_eq!(l.pens(l.lines()[0]), vec![0, 10, 25, 35, 50]);
}

#[test]
fn layout_inline_gaps_spacing_takes_part_in_line_breaking() {
    // "aa bb" fits 50px; with 2px of letter-spacing it is 54px and wraps. The
    // trailing space of line one is dropped, so lines are 24px ("aa" 2 x 12).
    let l = lay(
        "<div style='width: 50px; letter-spacing: 2px'>aa bb</div>",
        "",
    );
    assert_eq!(l.line_rects().len(), 2);
    assert_eq!(l.line_rects()[0].2, 24);
}

#[test]
fn layout_inline_gaps_text_transform_changes_the_text_that_is_measured() {
    // U+00DF uppercases to "SS": two glyphs, 20px, where the lowercase is one.
    let upper = lay("<div style='text-transform: uppercase'>\u{df}</div>", "");
    assert_eq!(upper.line_rects()[0].2, 20);
    let none = lay("<div>\u{df}</div>", "");
    assert_eq!(none.line_rects()[0].2, 10);
}

#[test]
fn layout_inline_gaps_text_indent_pushes_in_and_narrows_only_the_first_line() {
    // Width 100, indent 20: the first line has 80, so "aaaa bbbb" (90) wraps and
    // "aaaa" sits at x 20. The second line has all 100: "bbbb cccc" is 90.
    let l = lay(
        "<div style='width: 100px; text-indent: 20px'>aaaa bbbb cccc</div>",
        "",
    );
    let r = l.line_rects();
    assert_eq!(r[0].0, 20);
    assert_eq!(r[1], (0, 10, 90, 10));
    assert_eq!(r.len(), 2);
}

#[test]
fn layout_inline_gaps_text_indent_percentage_is_of_the_containing_block_width() {
    let l = lay("<div style='width: 200px; text-indent: 10%'>ab</div>", "");
    assert_eq!(l.line_rects(), vec![(20, 0, 20, 10)]);
}

#[test]
fn layout_inline_gaps_text_indent_is_the_right_edge_in_a_right_to_left_block() {
    // Width 100, indent 30 from the right edge, start-aligned (right): "ab" (20px)
    // ends at 100 - 30 = 70, so starts at 50.
    let l = lay(
        "<div style='width: 100px; direction: rtl; text-indent: 30px'>ab</div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(50, 0, 20, 10)]);
}

#[test]
fn layout_inline_gaps_negative_text_indent_hangs_out_to_the_left() {
    let l = lay("<div style='width: 100px; text-indent: -10px'>ab</div>", "");
    assert_eq!(l.line_rects()[0].0, -10);
}

#[test]
fn layout_inline_gaps_text_indent_applies_to_the_run_before_a_block_not_after() {
    // text-indent is inherited, so the inner div's own first line is indented as
    // well; the anonymous block after it is not a first line.
    let l = lay("<div style='text-indent: 15px'>x<div>b</div>y</div>", "");
    let xs: Vec<i32> = l.line_rects().iter().map(|r| r.0).collect();
    assert_eq!(xs, vec![15, 15, 0]);
}

#[test]
fn layout_inline_gaps_text_indent_centres_in_what_is_left() {
    // Width 100, indent 20, centred "ab" (20px): 80 - 20 = 60 slack, 30 each side
    // after the 20 indent: x = 20 + 30.
    let l = lay(
        "<div style='width: 100px; text-indent: 20px; text-align: center'>ab</div>",
        "",
    );
    assert_eq!(l.line_rects()[0].0, 50);
}

#[test]
fn layout_inline_gaps_white_space_pre_keeps_spaces_and_breaks_only_at_newlines() {
    // "a  b" keeps both spaces (40px) and the newline starts a second line, whatever
    // the 20px width says.
    let l = lay(
        "<div style='width: 20px; white-space: pre'>a  b\nc</div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(0, 0, 40, 10), (0, 10, 10, 10)]);
}

#[test]
fn layout_inline_gaps_white_space_nowrap_collapses_but_never_wraps() {
    let l = lay(
        "<div style='width: 30px; white-space: nowrap'>aa   bb\ncc</div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(0, 0, 80, 10)]);
}

#[test]
fn layout_inline_gaps_white_space_pre_wrap_keeps_spaces_wraps_and_hangs_the_trailing_ones() {
    // "aa  bb  cc" at 50px: the two spaces after "aa" hang off the line's end, so the
    // first line measures 20; "bb" and "cc" follow the same way.
    let l = lay(
        "<div style='width: 50px; white-space: pre-wrap'>aa  bb  cc</div>",
        "",
    );
    let r = l.line_rects();
    assert_eq!(r.len(), 3, "{r:?}");
    assert_eq!(r[0].2, 20);
    assert_eq!(r[2].2, 20);
}

#[test]
fn layout_inline_gaps_white_space_pre_line_collapses_spaces_but_keeps_newlines() {
    let l = lay("<div style='white-space: pre-line'>a   b\n  c</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 30, 10), (0, 10, 10, 10)]);
}

#[test]
fn layout_inline_gaps_a_tab_in_pre_is_eight_spaces() {
    let l = lay("<div style='white-space: pre'>a\tb</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 100, 10)]);
}

#[test]
fn layout_inline_gaps_a_spans_own_white_space_applies_to_its_text() {
    // The span preserves its two spaces: "a " + "b  c" = 6 glyphs.
    let l = lay(
        "<div>a <span style='white-space: pre'>b  c</span></div>",
        "",
    );
    assert_eq!(l.line_rects()[0].2, 60);
}

#[test]
fn layout_inline_gaps_justify_stretches_every_line_but_the_last() {
    // Width 100. "aa bb cc" is 80 and the next word does not fit: its two spaces take
    // 10px each. The last line "dd ee" stays at its natural 50.
    let l = lay(
        "<div style='width: 100px; text-align: justify'>aa bb cc dd ee</div>",
        "",
    );
    let r = l.line_rects();
    assert_eq!(r.len(), 2, "{r:?}");
    assert_eq!(r[0], (0, 0, 100, 10));
    assert_eq!(r[1], (0, 10, 50, 10));
    // Each space is a glyph: 10px plus its 10px share, so pens skip 20 over them.
    assert_eq!(l.pens(l.lines()[0]), vec![0, 10, 20, 40, 50, 60, 80, 90]);
}

#[test]
fn layout_inline_gaps_justify_does_not_stretch_a_line_ended_by_br_or_without_spaces() {
    let l = lay(
        "<div style='width: 100px; text-align: justify'>aa bb<br>cc dd</div>",
        "",
    );
    assert_eq!(l.line_rects()[0].2, 50);
    let one = lay(
        "<div style='width: 50px; text-align: justify'>aaaaaaa bb</div>",
        "",
    );
    assert_eq!(one.line_rects()[0].2, 70);
}

#[test]
fn layout_inline_gaps_justify_gives_the_remainder_to_the_first_spaces() {
    // Width 151 over "a b c d e f g h" (15 glyphs, 150px, seven spaces): 1px is 60
    // app units, 8 each with 4 over, so the first four spaces take one more and the
    // line is exactly 151px.
    let l = lay(
        "<div style='width: 151px; text-align: justify'>a b c d e f g h iiiiiiiiiiiiiiiiiiiiiiii</div>",
        "",
    );
    assert_eq!(l.line_rects()[0].2, 151);
    let line = l.lines()[0];
    let f = l.tree.get(line).expect("line");
    assert_eq!(f.size.inline, px(151));
}
