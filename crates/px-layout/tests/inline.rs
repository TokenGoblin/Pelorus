//! Inline formatting context: inline boxes, per-element text, `<br>` and
//! `inline-block` (CSS 2.1 sections 9.4.2, 10.8 and 16).
//!
//! Every expectation is hand-derived from Ahem's metrics, which is the point of
//! using it: units per em 1000, every glyph (a space included) one em wide,
//! ascent 800, descent 200, line gap 0. At `font-size: 10px` a glyph is 10px wide,
//! the content area is 10px tall and the baseline sits 8px below its top. The
//! block under test is `font: 10px/10px Ahem`, so its strut is 8px above the
//! baseline and 2px below, a line is 10px tall, and the derivation of each number
//! is in the test that asserts it.
//!
//! What these cannot show is the picture: the display-list tests in `px-paint`
//! (`px_paint_inline_*`) do that.

use app_units::Au;
use px_dom::NodeId;
use px_layout::block::layout_document;
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::px;

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
                    div { font: 10px/10px Ahem }";

struct Laid {
    tree: FragmentTree,
    arena: px_dom::Arena,
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
    Laid { tree, arena }
}

impl Laid {
    /// The element with `id="..."`.
    fn node(&self, id: &str) -> NodeId {
        let document = self.arena.document();
        core::iter::once(document)
            .chain(self.arena.descendants(document))
            .find(|n| {
                self.arena
                    .get(*n)
                    .and_then(|node| node.attrs())
                    .is_some_and(|attrs| {
                        attrs
                            .iter()
                            .any(|a| &*a.name.local == "id" && &*a.value == id)
                    })
            })
            .expect("an element with that id")
    }

    fn of_kind(&self, pick: impl Fn(FragmentKind) -> bool) -> Vec<FragmentId> {
        self.tree
            .in_layout_order()
            .into_iter()
            .filter(|(_, id)| self.tree.get(*id).is_some_and(|f| pick(f.kind)))
            .map(|(_, id)| id)
            .collect()
    }

    fn lines(&self) -> Vec<FragmentId> {
        self.of_kind(|k| k == FragmentKind::Line)
    }

    fn inline_boxes(&self) -> Vec<FragmentId> {
        self.of_kind(|k| matches!(k, FragmentKind::Inline { .. }))
    }

    /// `(x, y, width, height)` in px of a fragment.
    fn rect(&self, id: FragmentId) -> (i32, i32, i32, i32) {
        let f = self.tree.get(id).expect("a fragment");
        let n = |a: Au| a.0 / 60;
        (
            n(f.inline_offset),
            n(f.block_offset),
            n(f.size.inline),
            n(f.size.block),
        )
    }

    fn line_rects(&self) -> Vec<(i32, i32, i32, i32)> {
        self.lines().into_iter().map(|l| self.rect(l)).collect()
    }

    /// For each run of a line: its element, font size in px, and the absolute
    /// pen x (in px, from the line's left edge) of each glyph.
    fn runs(&self, line: FragmentId) -> Vec<(Option<NodeId>, i32, Vec<i32>)> {
        self.tree
            .text_runs(line)
            .iter()
            .map(|r| {
                (
                    r.node,
                    r.font_size.0 / 60,
                    self.tree
                        .glyphs(r)
                        .iter()
                        .map(|g| (r.inline_offset + g.x).0 / 60)
                        .collect(),
                )
            })
            .collect()
    }
}

#[test]
fn layout_inline_spans_in_one_line_take_their_margin_and_padding() {
    // "aa " + "bb" + " cc" is eight glyphs, 80px. The span's left margin 3px and
    // padding 5px plus its right padding 5px add 13px, so the line is 93px.
    let l = lay(
        "<div>aa <span id=s style='margin-left: 3px; padding: 0 5px'>bb</span> cc</div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(0, 0, 93, 10)]);
    let runs = l.runs(l.lines()[0]);
    assert_eq!(runs.len(), 3, "one run per element's text");
    // "aa " at 0, 10, 20. The span's start edge is 3 + 5 = 8px, so "bb" is at 38
    // and 48, then 5px of right padding before " cc" at 63, 73, 83.
    assert_eq!(runs[0].2, vec![0, 10, 20]);
    assert_eq!(runs[1].2, vec![38, 48]);
    assert_eq!(runs[2].2, vec![63, 73, 83]);
    assert_eq!(runs[1].0, Some(l.node("s")));
    // Padding alone paints nothing, so there is no fragment for the box.
    assert!(l.inline_boxes().is_empty());
}

#[test]
fn layout_inline_a_span_that_wraps_has_a_fragment_per_line() {
    // The block is 60px wide. "aaa " then the span "bbb ccc" with a 2px border
    // and a background, then " ddd". Text: "aaa bbb ccc ddd".
    //   line 1  "aaa"            30px (the span's start edge opens at the next
    //                            line: "aaa bbb" would be 70 + 2 = 72 > 60)
    //   line 2  edge 2 + "bbb"   32px
    //   line 3  "ccc" + edge 2   32px
    //   line 4  "ddd"            30px
    let l = lay(
        "<div style='width: 60px'>aaa <span id=s style='border: 2px solid black; \
         background: #0f0'>bbb ccc</span> ddd</div>",
        "",
    );
    assert_eq!(
        l.line_rects(),
        vec![
            (0, 0, 30, 10),
            (0, 10, 32, 10),
            (0, 20, 32, 10),
            (0, 30, 30, 10)
        ]
    );
    let boxes = l.inline_boxes();
    assert_eq!(boxes.len(), 2, "one fragment per line the span occupies");
    let kinds: Vec<_> = boxes
        .iter()
        .map(|b| l.tree.get(*b).expect("fragment").kind)
        .collect();
    assert_eq!(
        kinds,
        vec![
            FragmentKind::Inline {
                left: true,
                right: false
            },
            FragmentKind::Inline {
                left: false,
                right: true
            },
        ],
        "box-decoration-break: slice, the first piece owns the left edge and the last the right"
    );
    // The content area is the font's 10px, plus the 2px border above and below,
    // so 14px tall, starting 2px above the 8px-down baseline of its line.
    // Line 2 is at y = 10: the piece is x 0..32 (edge 2 + "bbb" 30), y 8..22.
    assert_eq!(l.rect(boxes[0]), (0, 8, 32, 14));
    // Line 3 at y = 20: "ccc" 0..30 and the right edge 30..32.
    assert_eq!(l.rect(boxes[1]), (0, 18, 32, 14));
    for b in &boxes {
        assert_eq!(l.tree.get(*b).expect("fragment").node, Some(l.node("s")));
    }
}

#[test]
fn layout_inline_nested_spans_nest_their_edges() {
    // "abc": the outer span (2px padding each side) holds a, the inner span (3px)
    // holding b, and c. Edges in document order: outer 2, a, inner 3, b, inner 3,
    // c, outer 2.  a at 2, b at 2 + 10 + 3 = 15, c at 15 + 10 + 3 = 28; the line
    // ends at 28 + 10 + 2 = 40.
    let l = lay(
        "<div><span id=o style='padding: 0 2px'>a<span id=i style='padding: 0 3px'>b</span>c</span></div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(0, 0, 40, 10)]);
    let runs = l.runs(l.lines()[0]);
    assert_eq!(
        runs.iter().map(|r| r.2.clone()).collect::<Vec<_>>(),
        vec![vec![2], vec![15], vec![28]]
    );
    assert_eq!(
        runs.iter().map(|r| r.0).collect::<Vec<_>>(),
        vec![Some(l.node("o")), Some(l.node("i")), Some(l.node("o"))],
        "each run names the innermost element its text is in"
    );
}

#[test]
fn layout_inline_nested_decorated_spans_get_nested_rectangles() {
    // Outer: border 1px. Inner: border 1px. "x" between: edges outer 1, inner 1,
    // glyph 10, inner 1, outer 1: the outer box is 14 wide, the inner 12 at x 1.
    let l = lay(
        "<div><span id=o style='border: 1px solid'><span id=i style='border: 1px solid'>x</span></span></div>",
        "",
    );
    let boxes = l.inline_boxes();
    assert_eq!(boxes.len(), 2);
    // The content area is 10px tall from 8px above the baseline; the 1px borders
    // add one above and below: y = 8 - 8 - 1 = -1, height 12.
    assert_eq!(
        l.rect(boxes[0]),
        (0, -1, 14, 12),
        "outer, 1px border around 10px"
    );
    assert_eq!(
        l.rect(boxes[1]),
        (1, -1, 12, 12),
        "inner, inside the outer border"
    );
}

#[test]
fn layout_inline_each_run_names_its_own_element() {
    let l = lay("<div id=d>a <span id=r>b</span> <b id=b>c</b> d</div>", "");
    let runs = l.runs(l.lines()[0]);
    let owners: Vec<_> = runs.iter().map(|r| r.0).collect();
    assert_eq!(
        owners,
        vec![
            Some(l.node("d")),
            Some(l.node("r")),
            Some(l.node("d")),
            Some(l.node("b")),
            Some(l.node("d")),
        ]
    );
}

#[test]
fn layout_inline_br_is_a_forced_line_break() {
    let l = lay("<div>ab<br>cd</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 20, 10), (0, 10, 20, 10)]);
    // Two in a row make an empty line between.
    let l = lay("<div>a<br><br>b</div>", "");
    assert_eq!(
        l.line_rects(),
        vec![(0, 0, 10, 10), (0, 10, 0, 10), (0, 20, 10, 10)]
    );
    // A lone <br> is a line.
    let l = lay("<div><br></div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 0, 10)]);
    // Space before a break is removed and space after it is not kept.
    let l = lay("<div>a <br> b</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 10, 10), (0, 10, 10, 10)]);
}

#[test]
fn layout_inline_a_larger_font_raises_the_line() {
    // `line-height: normal` everywhere (Ahem: 1em). "ab" 10px, "cd" 20px, "ef"
    // 10px. The 20px span has ascent 16 and descent 4; the strut 8 and 2. The
    // line is 16 + 4 = 20 tall with its baseline 16 down.
    let l = lay(
        "<div style='font: 10px Ahem'>ab<span style='font-size: 20px'>cd</span>ef</div>",
        "",
    );
    // 2*10 + 2*20 + 2*10.
    assert_eq!(l.line_rects(), vec![(0, 0, 80, 20)]);
    let line = l.lines()[0];
    let runs = l.runs(line);
    assert_eq!(
        runs.iter().map(|r| (r.1, r.2.clone())).collect::<Vec<_>>(),
        vec![(10, vec![0, 10]), (20, vec![20, 40]), (10, vec![60, 70])]
    );
    for r in l.tree.text_runs(line) {
        assert_eq!(
            r.baseline,
            px(16),
            "every run sits on the line's one baseline"
        );
    }
}

#[test]
fn layout_inline_a_fixed_line_height_is_per_element() {
    // The span inherits `line-height: 10px` and is 20px: content 16 + 4, leading
    // 10 - 20 = -10, half -5: it reaches 11 above the baseline and -1 below. The
    // strut reaches 8 above and 2 below. Line: 11 + 2 = 13.
    let l = lay("<div>ab<span style='font-size: 20px'>cd</span></div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 60, 13)]);
    for r in l.tree.text_runs(l.lines()[0]) {
        assert_eq!(r.baseline, px(11));
    }
}

#[test]
fn layout_inline_a_taller_line_height_on_a_span_makes_the_line_taller() {
    // The span's line-height 30 on a 10px font: leading 20, half 10: 18 above,
    // 12 below. The line is 18 + 12 = 30.
    let l = lay("<div>a<span style='line-height: 30px'>b</span>c</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 30, 30)]);
    assert_eq!(l.tree.text_runs(l.lines()[0])[0].baseline, px(18));
}

#[test]
fn layout_inline_a_wrapping_break_can_fall_at_a_span_boundary() {
    // The block is 50px wide. "ab" and "cd" are in different elements; "ab cd"
    // is 50 and fits, "ab cd ef" would be 80.
    let l = lay(
        "<div style='width: 50px'>ab <span>cd</span> <i>ef</i></div>",
        "",
    );
    assert_eq!(l.line_rects(), vec![(0, 0, 50, 10), (0, 10, 20, 10)]);
    let second = l.runs(l.lines()[1]);
    assert_eq!(
        second.len(),
        1,
        "the break fell between the two elements' text"
    );
}

#[test]
fn layout_inline_inline_block_shrinks_to_fit_and_sits_on_the_baseline() {
    // The inline-block holds "bb", a break and "cc": its width is the widest
    // line, 20px, and it is two 10px lines tall: 20px, with the baseline of its
    // last line, 10 + 8 = 18px down. On the container's line it reaches 18 above
    // the baseline and 2 below; the strut is 8 and 2. The line is 20 tall with
    // the baseline 18 down: "a" at 0, the box at x 10, "d" at x 30.
    let l = lay(
        "<div>a<span id=ib style='display: inline-block'>bb<br>cc</span>d</div>",
        "",
    );
    assert_eq!(l.line_rects()[0], (0, 0, 40, 20));
    let blocks = l.of_kind(|k| k == FragmentKind::Block);
    let ib = blocks
        .into_iter()
        .find(|b| l.tree.get(*b).expect("fragment").node == Some(l.node("ib")))
        .expect("the inline-block has a fragment");
    assert_eq!(l.rect(ib), (10, 0, 20, 20));
    // Its own lines.
    let inner: Vec<_> = l.line_rects().into_iter().filter(|r| r.0 == 10).collect();
    assert_eq!(inner, vec![(10, 0, 20, 10), (10, 10, 20, 10)]);
    // The surrounding text is on the shared baseline.
    let line = l.lines()[0];
    for r in l.tree.text_runs(line) {
        assert_eq!(r.baseline, px(18));
    }
    // One run: the object takes room and draws no glyph, so "d" is at 30.
    let runs = l.runs(line);
    assert_eq!(runs.len(), 1);
    assert_eq!(runs[0].2, vec![0, 30]);
}

#[test]
fn layout_inline_inline_block_margins_are_part_of_the_line() {
    // 30x10 box with 4px margins all round on a 10px strut: margin box 38 wide,
    // 18 tall; its baseline is its bottom margin edge less nothing: no lines, so
    // the bottom margin edge, 18 above the baseline. The line is 18 + 2 = 20.
    let l = lay(
        "<div>a<span id=ib style='display: inline-block; width: 30px; height: 10px; margin: 4px'></span>b</div>",
        "",
    );
    assert_eq!(l.line_rects()[0], (0, 0, 10 + 38 + 10, 20));
    let ib = l
        .of_kind(|k| k == FragmentKind::Block)
        .into_iter()
        .find(|b| l.tree.get(*b).expect("fragment").node == Some(l.node("ib")))
        .expect("fragment");
    // x: "a" 10 + margin 4. y: the margin box's top is the line top; the border
    // box is 4 below it.
    assert_eq!(l.rect(ib), (14, 4, 30, 10));
}

#[test]
fn layout_inline_inline_blocks_wrap_like_words() {
    let l = lay(
        "<div style='width: 50px'><span style='display: inline-block; width: 30px; height: 10px'></span><span style='display: inline-block; width: 30px; height: 10px'></span></div>",
        "",
    );
    let lines = l.line_rects();
    assert_eq!(lines.len(), 2, "two 30px boxes do not fit 50px together");
    assert_eq!(lines[0].1, 0);
    assert_eq!(
        lines[1].1, lines[0].3,
        "the second line starts below the first"
    );
}

#[test]
fn layout_inline_bidi_runs_keep_their_element_across_spans() {
    // LTR paragraph: "abc " (div), the Hebrew letters (span), " def" (div).
    let l = lay(
        "<div id=d style='width: 400px'>abc <span id=s>&#1488;&#1489;&#1490;</span> def</div>",
        "",
    );
    let line = l.lines()[0];
    let runs = l.tree.text_runs(line);
    assert_eq!(runs.len(), 3);
    assert_eq!(
        runs.iter().map(|r| r.rtl).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    assert_eq!(
        runs.iter().map(|r| r.node).collect::<Vec<_>>(),
        vec![Some(l.node("d")), Some(l.node("s")), Some(l.node("d"))]
    );
    assert_eq!(
        runs.iter().map(|r| r.inline_offset).collect::<Vec<_>>(),
        vec![px(0), px(40), px(70)]
    );
}

#[test]
fn layout_inline_a_right_to_left_paragraph_reorders_across_spans() {
    // RTL paragraph: logical abc, space, Hebrew (span), space, def. Visually left
    // to right: def, Hebrew, abc, whatever elements they are in.
    let l = lay(
        "<div id=d style='direction: rtl; width: 400px'>abc <span id=s>&#1488;&#1489;&#1490;</span> def</div>",
        "",
    );
    let line = l.lines()[0];
    let runs = l.tree.text_runs(line);
    // The right-to-left run (space, Hebrew, space) crosses three elements' text
    // and is cut at their boundaries; visually, left to right: "def", the space
    // after the Hebrew, the Hebrew, the space before it, "abc".
    assert_eq!(runs.len(), 5);
    assert_eq!(
        runs.iter().map(|r| r.node).collect::<Vec<_>>(),
        vec![
            Some(l.node("d")),
            Some(l.node("d")),
            Some(l.node("s")),
            Some(l.node("d")),
            Some(l.node("d"))
        ]
    );
    assert_eq!(
        runs.iter().map(|r| r.rtl).collect::<Vec<_>>(),
        vec![false, true, true, true, false]
    );
    // 11 glyphs, 110px, right-aligned in 400px.
    assert_eq!(l.line_rects()[0].0, 290);
}

#[test]
fn layout_inline_white_space_collapses_across_span_boundaries() {
    // A space inside a span after a space outside collapses once: "a b c".
    let l = lay("<div>a <span> b</span> c</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 50, 10)]);
    // A space in the span with none outside is kept: "a b".
    let l = lay("<div>a<span> </span>b</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 30, 10)]);
    // Leading space inside a span at the start of the block goes.
    let l = lay("<div><span> a</span></div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 10, 10)]);
    // Trailing space inside a span at the end of the block goes.
    let l = lay("<div><span>a </span></div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 10, 10)]);
    // Newlines and tabs collapse the same way.
    let l = lay("<div>a\n<span>\t\nb</span></div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 30, 10)]);
}

#[test]
fn layout_inline_a_non_breaking_space_does_not_collapse() {
    let l = lay("<div>a&nbsp;&nbsp;b</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 40, 10)]);
}

#[test]
fn layout_inline_an_empty_span_takes_no_room_and_a_bordered_one_does() {
    let l = lay("<div>a<span></span>b</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 20, 10)]);
    assert!(l.inline_boxes().is_empty());
    assert_eq!(l.runs(l.lines()[0]).len(), 1);

    // A 3px border each side of nothing: 6px between a and b, and a fragment for it.
    let l = lay("<div>a<span style='border: 3px solid'></span>b</div>", "");
    assert_eq!(l.line_rects(), vec![(0, 0, 26, 10)]);
    let boxes = l.inline_boxes();
    assert_eq!(boxes.len(), 1);
    assert_eq!(l.rect(boxes[0]), (10, -3, 6, 16));
}

#[test]
fn layout_inline_a_block_inside_a_span_reopens_the_span_after_it() {
    // The span's text before and after the block keeps the span's element.
    let l = lay(
        "<div id=d><span id=s>one<div>two</div>three</span></div>",
        "",
    );
    let owners: Vec<_> = l
        .lines()
        .into_iter()
        .flat_map(|line| l.runs(line).into_iter().map(|r| r.0))
        .collect();
    assert!(owners.contains(&Some(l.node("s"))));
    assert_eq!(l.lines().len(), 3);
}

#[test]
fn layout_inline_a_hundred_thousand_inline_boxes_do_not_overflow_a_small_stack() {
    // 250 sibling chains of 400 nested spans: 100,000 inline elements, none
    // deeper than px-dom's 512.
    let handle = std::thread::Builder::new()
        .name("layout-inline-deep".to_owned())
        .stack_size(256 * 1024)
        .spawn(|| {
            let mut body = String::from("<div>");
            for _ in 0..250 {
                for _ in 0..400 {
                    body.push_str("<span>");
                }
                body.push('x');
                for _ in 0..400 {
                    body.push_str("</span>");
                }
            }
            body.push_str("</div>");
            let l = lay(&body, "");
            // 250 glyphs of 10px on one line.
            assert_eq!(l.line_rects(), vec![(0, 0, 2500, 10)]);
        })
        .expect("the thread spawns");
    handle
        .join()
        .unwrap_or_else(|_| panic!("deep inline nesting overflowed or panicked"));
}

#[test]
fn layout_inline_output_is_deterministic() {
    let body = "<div style='width: 120px'>aa <span style='border: 2px solid; padding: 0 3px; \
                background: red'>bb cc dd</span> <b>ee</b><span style='display: inline-block'>ff<br>gg</span> hh</div>";
    let a = lay(body, "").tree;
    let b = lay(body, "").tree;
    assert_eq!(a, b);
}

#[test]
fn layout_inline_vertical_align_top_and_bottom_align_an_inline_block_to_the_line() {
    // A 10x30 box on the baseline (empty: its bottom margin edge is the
    // baseline, so 30 above and the strut's 2 below) makes the line 32 tall with
    // the baseline 30 down. A 10x10 box aligned to the bottom of the line sits at
    // y 22, aligned to the top at y 0, and neither makes the line taller.
    let html = |align: &str| {
        format!(
            "<div>a<span style='display: inline-block; width: 10px; height: 30px'></span>b\
             <span id=v style='display: inline-block; width: 10px; height: 10px; \
             vertical-align: {align}'></span></div>"
        )
    };
    for (align, y) in [("bottom", 22), ("top", 0)] {
        let l = lay(&html(align), "");
        assert_eq!(l.line_rects()[0].3, 32, "{align}");
        let v = l
            .of_kind(|k| k == FragmentKind::Block)
            .into_iter()
            .find(|b| l.tree.get(*b).expect("fragment").node == Some(l.node("v")))
            .expect("fragment");
        assert_eq!(l.rect(v).1, y, "{align}");
    }
}

#[test]
fn layout_inline_a_tall_bottom_aligned_box_extends_the_line_below_the_baseline() {
    // Only a 30px box, aligned to the bottom: the strut is 8 above and 2 below,
    // the line must be 30 tall, and the extra goes under the baseline.
    let l = lay(
        "<div>a<span style='display: inline-block; width: 10px; height: 30px; \
         vertical-align: bottom'></span>b</div>",
        "",
    );
    assert_eq!(l.line_rects()[0], (0, 0, 30, 30));
    assert_eq!(l.tree.text_runs(l.lines()[0])[0].baseline, px(8));
}

#[test]
fn layout_inline_super_raises_text_by_a_third_of_its_font_size() {
    // 10px font: raised 10/3 = 200 app units (3.33px). The span reaches 8px +
    // 200 above the baseline; the strut is below at 2px; the line is 800 Au
    // tall with the baseline 680 down, and the span's text sits 200 above it.
    let l = lay(
        "<div>a<span style='vertical-align: super'>b</span></div>",
        "",
    );
    let line = l.lines()[0];
    assert_eq!(l.tree.get(line).expect("line").size.block, Au(800));
    let runs = l.tree.text_runs(line);
    assert_eq!(runs.len(), 2);
    assert_eq!(runs[0].baseline, Au(680));
    assert_eq!(runs[1].baseline, Au(480));
}

#[test]
fn layout_inline_inline_flex_and_inline_grid_sit_on_the_line() {
    for display in ["inline-flex", "inline-grid"] {
        let l = lay(
            &format!(
                "<div>a<span style='display: {display}; width: 30px; height: 10px'></span>b</div>"
            ),
            "",
        );
        assert_eq!(l.line_rects()[0].2, 50, "{display}: 10 + 30 + 10");
        let box_at = l
            .of_kind(|k| k == FragmentKind::Block)
            .into_iter()
            .map(|b| l.rect(b))
            .find(|r| r.2 == 30)
            .expect("the box");
        assert_eq!(box_at.0, 10, "{display}");
    }
}
