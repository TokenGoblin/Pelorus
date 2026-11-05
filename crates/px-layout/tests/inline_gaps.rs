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
