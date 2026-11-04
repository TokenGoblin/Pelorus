//! Phase 9 unit 3: real shaped text in layout.
//!
//! Every expectation here is hand-computed from Ahem's metrics, which is the
//! point of using it: units per em 1000, every glyph (a space included) one em
//! wide, ascent 800, descent 200, line gap 0. At `font-size: 10px` a glyph is
//! 10px wide, the content area is 10px tall and the baseline sits 8px below the
//! top of it. See `crates/px-layout/src/text.rs` for the font policy (every
//! family resolves to Ahem until system fonts land) and the list of what is not
//! implemented.

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::px;
use px_text::FontRegistry;

const BASE: &str = "html, body, div { display: block; margin: 0; padding: 0 } \
                    div { font: 10px/10px Ahem }";

fn lay(body: &str, css: &str) -> FragmentTree {
    lay_html(&format!("<html><body>{body}</body></html>"), css)
}

fn lay_html(html: &str, css: &str) -> FragmentTree {
    let dom = px_dom::parse(html);
    // px-dom refuses to nest past 512 levels and may abandon a pathological
    // parse; that is not a layout concern (see depth.rs).
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(&format!("{BASE} {css}"), "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("a root element");
    layout_document(&arena, &root, px(800)).expect("a laid-out document")
}

/// The line fragments, in layout order.
fn lines(tree: &FragmentTree) -> Vec<FragmentId> {
    tree.in_layout_order()
        .into_iter()
        .filter(|(_, id)| tree.get(*id).is_some_and(|f| f.kind == FragmentKind::Line))
        .map(|(_, id)| id)
        .collect()
}

/// `(inline_offset, block_offset, width, height)` of every line.
fn rects(tree: &FragmentTree) -> Vec<(Au, Au, Au, Au)> {
    lines(tree)
        .into_iter()
        .filter_map(|id| tree.get(id))
        .map(|f| (f.inline_offset, f.block_offset, f.size.inline, f.size.block))
        .collect()
}

fn widths(tree: &FragmentTree) -> Vec<Au> {
    rects(tree).into_iter().map(|r| r.2).collect()
}

/// The inline sizes of the block fragments narrower than the viewport.
fn narrow_blocks(tree: &FragmentTree) -> Vec<Au> {
    tree.in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| tree.get(id))
        .filter(|f| f.kind == FragmentKind::Block && f.size.inline < px(800))
        .map(|f| f.size.inline)
        .collect()
}

/// The glyph ids of Ahem for `s`.
fn ahem_glyphs(s: &str) -> Vec<u32> {
    let reg = FontRegistry::with_ahem().expect("Ahem loads");
    let font = reg
        .get(reg.resolve("Ahem").expect("Ahem is registered"))
        .expect("font");
    s.chars()
        .map(|c| u32::from(font.glyph_index(c).expect("Ahem maps it")))
        .collect()
}

fn run_glyph_ids(tree: &FragmentTree, line: FragmentId) -> Vec<Vec<u32>> {
    tree.text_runs(line)
        .iter()
        .map(|r| tree.glyphs(r).iter().map(|g| g.glyph_id).collect())
        .collect()
}

#[test]
fn layout_text_a_word_is_one_em_per_glyph_wide() {
    let tree = lay("<div>abcd</div>", "");
    assert_eq!(rects(&tree), vec![(px(0), px(0), px(40), px(10))]);
}

#[test]
fn layout_text_glyphs_are_retained_for_paint() {
    let tree = lay("<div style='line-height: 20px'>abc</div>", "");
    let line = lines(&tree)[0];
    let runs = tree.text_runs(line);
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!(run.font_size, px(10));
    assert!(!run.rtl);
    assert!(run.node.is_some(), "paint needs the element for the colour");
    assert_eq!(run.inline_offset, px(0));
    assert_eq!(run.width, px(30));
    // Half-leading (20 - 10) / 2 = 5 above the 8px ascent.
    assert_eq!(run.baseline, px(13));
    let glyphs = tree.glyphs(run);
    assert_eq!(
        glyphs.iter().map(|g| g.glyph_id).collect::<Vec<_>>(),
        ahem_glyphs("abc")
    );
    assert_eq!(
        glyphs.iter().map(|g| g.x).collect::<Vec<_>>(),
        vec![px(0), px(10), px(20)]
    );
    assert!(glyphs.iter().all(|g| g.advance == px(10) && g.y == Au(0)));
}

#[test]
fn layout_text_wraps_at_the_container_width() {
    // "aaaa bbbb" is 90px and fits 100; "aaaa bbbb cccc" would be 140.
    let tree = lay("<div style='width: 100px'>aaaa bbbb cccc</div>", "");
    assert_eq!(widths(&tree), vec![px(90), px(40)]);
    // Narrower: one word per line.
    let tree = lay("<div style='width: 85px'>aaaa bbbb cccc</div>", "");
    assert_eq!(widths(&tree), vec![px(40), px(40), px(40)]);
    assert_eq!(
        rects(&tree).iter().map(|r| r.1).collect::<Vec<_>>(),
        vec![px(0), px(10), px(20)]
    );
}

#[test]
fn layout_text_trailing_space_is_not_part_of_a_broken_line() {
    let tree = lay("<div style='width: 45px'>aaaa bbbb</div>", "");
    assert_eq!(widths(&tree), vec![px(40), px(40)]);
}

#[test]
fn layout_text_line_height_normal_comes_from_the_font() {
    // Ahem: ascent 800 + descent 200 + line gap 0 over 1000 units.
    let tree = lay("<div style='font: 20px Ahem'>ab cd</div>", "");
    assert_eq!(rects(&tree), vec![(px(0), px(0), px(100), px(20))]);
    let tree = lay("<div style='font: 20px Ahem; width: 50px'>ab cd</div>", "");
    assert_eq!(
        rects(&tree).iter().map(|r| r.3).collect::<Vec<_>>(),
        vec![px(20), px(20)]
    );
}

#[test]
fn layout_text_line_height_number_and_length_override_the_font() {
    let tree = lay("<div style='font: 10px/2 Ahem'>x</div>", "");
    assert_eq!(rects(&tree)[0].3, px(20));
    let tree = lay("<div style='font: 10px/15px Ahem'>x</div>", "");
    assert_eq!(rects(&tree)[0].3, px(15));
    let line = lines(&tree)[0];
    // (15 - 10) / 2 = 2.5px above the 8px ascent: 10.5px.
    assert_eq!(tree.text_runs(line)[0].baseline, px(8) + px(5) / 2);
}

#[test]
fn layout_text_the_block_is_as_tall_as_its_lines() {
    let tree = lay(
        "<div style='width: 45px; line-height: 12px'>aaaa bbbb cccc</div>",
        "",
    );
    assert_eq!(narrow_blocks(&tree), vec![px(45)]);
    let block = tree
        .in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| tree.get(id))
        .find(|f| f.kind == FragmentKind::Block && f.size.inline == px(45))
        .expect("the div");
    assert_eq!(block.size.block, px(36));
}

#[test]
fn layout_text_unknown_and_generic_families_resolve_to_ahem() {
    for family in [
        "Nonexistent",
        "serif",
        "monospace",
        "'Some Font', sans-serif",
    ] {
        let tree = lay(
            &format!("<div style=\"font-family: {family}; font-size: 10px\">abcd</div>"),
            "",
        );
        assert_eq!(widths(&tree), vec![px(40)], "family {family}");
        let line = lines(&tree)[0];
        assert_eq!(
            run_glyph_ids(&tree, line),
            vec![ahem_glyphs("abcd")],
            "family {family}"
        );
    }
}

#[test]
fn layout_text_font_size_scales_advances() {
    let tree = lay("<div style='font-size: 25px'>abc</div>", "");
    assert_eq!(widths(&tree), vec![px(75)]);
}

#[test]
fn layout_text_nowrap_never_breaks() {
    let tree = lay(
        "<div style='width: 50px; white-space: nowrap'>aaaa bbbb cccc</div>",
        "",
    );
    assert_eq!(widths(&tree), vec![px(140)]);
}

#[test]
fn layout_text_normal_collapses_runs_of_spaces_and_newlines() {
    let tree = lay("<div>a   b\n\n  c</div>", "");
    assert_eq!(widths(&tree), vec![px(50)], "'a b c' is five glyphs");
}

#[test]
fn layout_text_pre_keeps_spaces_and_breaks_at_newlines_only() {
    let tree = lay(
        "<div style='white-space: pre; width: 20px'>a  b\nc\n\nd</div>",
        "",
    );
    // "a  b" is four glyphs (nothing collapses), the blank line has height, and
    // `pre` does not wrap at 20px.
    assert_eq!(
        rects(&tree),
        vec![
            (px(0), px(0), px(40), px(10)),
            (px(0), px(10), px(10), px(10)),
            (px(0), px(20), px(0), px(10)),
            (px(0), px(30), px(10), px(10)),
        ]
    );
}

#[test]
fn layout_text_pre_wrap_keeps_spaces_and_wraps() {
    let tree = lay("<div style='white-space: pre-wrap'>a  b</div>", "");
    assert_eq!(widths(&tree), vec![px(40)]);
    // Spaces hang: "aa" fits 25px, the space after it does not count.
    let tree = lay(
        "<div style='white-space: pre-wrap; width: 25px'>aa bb</div>",
        "",
    );
    assert_eq!(widths(&tree), vec![px(20), px(20)]);
}

#[test]
fn layout_text_pre_line_collapses_spaces_but_keeps_newlines() {
    let tree = lay("<div style='white-space: pre-line'>a   b\nc</div>", "");
    assert_eq!(widths(&tree), vec![px(30), px(10)]);
}

#[test]
fn layout_text_soft_hyphen_breaks_with_a_visible_hyphen() {
    let hyphen = ahem_glyphs("-")[0];
    // 40px does not fit 35px: break after the soft hyphen, which now takes
    // 10px as a drawn hyphen: "ab-" is 30px.
    let tree = lay("<div style='width: 35px'>ab&shy;cd</div>", "");
    assert_eq!(widths(&tree), vec![px(30), px(20)]);
    let ls = lines(&tree);
    let mut expect = ahem_glyphs("ab");
    expect.push(hyphen);
    assert_eq!(run_glyph_ids(&tree, ls[0]), vec![expect]);
    assert_eq!(run_glyph_ids(&tree, ls[1]), vec![ahem_glyphs("cd")]);
    // Fits: no break, and the soft hyphen is invisible and 0 wide.
    let tree = lay("<div style='width: 100px'>ab&shy;cd</div>", "");
    assert_eq!(widths(&tree), vec![px(40)]);
    assert_eq!(
        run_glyph_ids(&tree, lines(&tree)[0]),
        vec![ahem_glyphs("abcd")]
    );
}

#[test]
fn layout_text_overflow_wrap_anywhere_breaks_inside_a_word() {
    let tree = lay("<div style='width: 45px'>abcdefghij</div>", "");
    assert_eq!(widths(&tree), vec![px(100)], "unbreakable text overflows");
    for value in ["anywhere", "break-word"] {
        let tree = lay(
            &format!("<div style='width: 45px; overflow-wrap: {value}'>abcdefghij</div>"),
            "",
        );
        assert_eq!(widths(&tree), vec![px(40), px(40), px(20)], "{value}");
    }
}

#[test]
fn layout_text_overflow_wrap_anywhere_counts_in_min_content_and_break_word_does_not() {
    // A float shrinks to fit: min(max(min-content, available), max-content).
    let float = |value: &str| {
        let tree = lay(
            &format!(
                "<div style='width: 45px'><div style='float: left; overflow-wrap: {value}'>abcdefghij</div></div>"
            ),
            "",
        );
        narrow_blocks(&tree)
    };
    assert!(float("normal").contains(&px(100)));
    assert!(float("break-word").contains(&px(100)));
    assert!(float("anywhere").contains(&px(45)));
}

#[test]
fn layout_text_word_break_break_all() {
    let tree = lay(
        "<div style='width: 45px; word-break: break-all'>abcdefghij</div>",
        "",
    );
    assert_eq!(widths(&tree), vec![px(40), px(40), px(20)]);
}

#[test]
fn layout_text_a_very_long_unbreakable_word_is_one_overflowing_line() {
    let word = "x".repeat(5000);
    let tree = lay(&format!("<div style='width: 100px'>{word}</div>"), "");
    assert_eq!(widths(&tree), vec![px(50_000)]);
    let line = lines(&tree)[0];
    assert_eq!(tree.glyphs(&tree.text_runs(line)[0]).len(), 5000);
}

#[test]
fn layout_text_a_run_past_the_shaper_limit_does_not_panic() {
    // px-text refuses runs over 256 KiB; layout must survive that as a line with
    // no glyphs, not as a crash.
    let word = "x".repeat(300_000);
    let tree = lay(&format!("<div style='width: 100px'>{word}</div>"), "");
    assert!(lines(&tree).len() <= 1);
}

#[test]
fn layout_text_empty_and_blank_text_make_no_line() {
    for body in ["<div></div>", "<div>   </div>", "<div>\n\t\n</div>"] {
        let tree = lay(body, "");
        assert!(lines(&tree).is_empty(), "{body:?}");
    }
}

#[test]
fn layout_text_rtl_paragraph_is_right_aligned() {
    let tree = lay("<div style='direction: rtl; width: 100px'>abc</div>", "");
    assert_eq!(rects(&tree), vec![(px(70), px(0), px(30), px(10))]);
    // The Latin text keeps its own left-to-right order inside the line.
    let line = lines(&tree)[0];
    assert_eq!(run_glyph_ids(&tree, line), vec![ahem_glyphs("abc")]);
}

#[test]
fn layout_text_hebrew_is_shaped_as_a_right_to_left_run() {
    let tree = lay("<div style='width: 200px'>&#1488;&#1489;&#1490;</div>", "");
    let line = lines(&tree)[0];
    let runs = tree.text_runs(line);
    assert_eq!(runs.len(), 1);
    assert!(runs[0].rtl);
    assert_eq!(tree.glyphs(&runs[0]).len(), 3);
    // Left-to-right paragraph: the line starts at the left edge.
    assert_eq!(rects(&tree)[0].0, px(0));
}

#[test]
fn layout_text_mixed_direction_line_is_reordered() {
    // LTR paragraph: "abc " (L), the Hebrew letters (R), " def" (L).
    let tree = lay(
        "<div style='width: 400px'>abc &#1488;&#1489;&#1490; def</div>",
        "",
    );
    let line = lines(&tree)[0];
    let runs = tree.text_runs(line);
    assert_eq!(
        runs.iter().map(|r| r.rtl).collect::<Vec<_>>(),
        vec![false, true, false]
    );
    assert_eq!(
        runs.iter().map(|r| r.inline_offset).collect::<Vec<_>>(),
        vec![px(0), px(40), px(70)]
    );
    assert_eq!(
        runs.iter().map(|r| r.width).collect::<Vec<_>>(),
        vec![px(40), px(30), px(40)]
    );
}

#[test]
fn layout_text_rtl_paragraph_reorders_runs_visually() {
    // RTL paragraph: logical order is abc, space, Hebrew, space, def. Visually,
    // left to right: def, the Hebrew run, abc.
    let tree = lay(
        "<div style='direction: rtl; width: 400px'>abc &#1488;&#1489;&#1490; def</div>",
        "",
    );
    let line = lines(&tree)[0];
    let ids = run_glyph_ids(&tree, line);
    assert_eq!(ids.first(), Some(&ahem_glyphs("def")));
    assert_eq!(ids.last(), Some(&ahem_glyphs("abc")));
    let runs = tree.text_runs(line);
    assert_eq!(runs.len(), 3);
    assert!(runs[1].rtl);
    // 3 + 1 + 3 + 1 + 3 = 11 glyphs = 110px, right-aligned in 400px.
    assert_eq!(rects(&tree)[0].0, px(290));
    assert_eq!(rects(&tree)[0].2, px(110));
}

#[test]
fn layout_text_align_moves_lines() {
    let at = |align: &str| {
        let tree = lay(
            &format!("<div style='width: 100px; text-align: {align}'>aa bb</div>"),
            "",
        );
        rects(&tree)[0].0
    };
    assert_eq!(at("left"), px(0));
    assert_eq!(at("start"), px(0));
    assert_eq!(at("right"), px(50));
    assert_eq!(at("end"), px(50));
    assert_eq!(at("center"), px(25));
    // A single (last) line is not justified.
    assert_eq!(at("justify"), px(0));
}

#[test]
fn layout_text_align_justify_stretches_all_but_the_last_line() {
    let tree = lay(
        "<div style='width: 75px; text-align: justify'>aaa bbb ccc ddd</div>",
        "",
    );
    // Line 1 "aaa bbb" is 70px with one space: 5px more fills 75px. The space
    // glyph's advance grows and the second word moves right by 5px.
    assert_eq!(widths(&tree), vec![px(75), px(70)]);
    let first = lines(&tree)[0];
    let run = &tree.text_runs(first)[0];
    let xs: Vec<Au> = tree.glyphs(run).iter().map(|g| g.x).collect();
    assert_eq!(xs[4], px(45), "the first glyph after the widened space");
}

#[test]
fn layout_text_start_alignment_follows_direction_for_start_and_end() {
    let at = |align: &str| {
        let tree = lay(
            &format!("<div style='direction: rtl; width: 100px; text-align: {align}'>aa</div>"),
            "",
        );
        rects(&tree)[0].0
    };
    assert_eq!(at("start"), px(80));
    assert_eq!(at("end"), px(0));
    assert_eq!(at("left"), px(0));
    assert_eq!(at("right"), px(80));
}

#[test]
fn layout_text_floats_shorten_lines_by_real_widths() {
    let tree = lay(
        "<div style='width: 100px'><div style='float: left; width: 40px; height: 10px'></div>aaa bbb ccc</div>",
        "",
    );
    // Line 1 has 60px beside the 10px-tall float: "aaa" (30) fits, "aaa bbb"
    // (70) does not. Line 2 is clear of the float and has 100px.
    let r = rects(&tree);
    assert_eq!(r.len(), 2);
    assert_eq!((r[0].0, r[0].2), (px(40), px(30)));
    assert_eq!((r[1].0, r[1].2), (px(0), px(70)));
}

#[test]
fn layout_text_intrinsic_widths_drive_shrink_to_fit() {
    // A float with auto width is as wide as its text: min(max(min, avail), max).
    let tree = lay("<div style='float: left'>abc de</div>", "");
    assert_eq!(narrow_blocks(&tree), vec![px(60)]);
}

#[test]
fn layout_text_output_is_deterministic() {
    let html = "<div style='width: 95px; text-align: justify'>the quick brown fox \
                &#1488;&#1489;&#1490; jumps over&shy;the lazy dog</div>\
                <div style='direction: rtl; white-space: pre-wrap'>a  b\n&#1488; c</div>";
    let a = lay(html, "");
    let b = lay(html, "");
    assert_eq!(a, b);
}

#[test]
fn layout_text_fragments_stay_copy() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<px_layout::fragment::Fragment>();
    assert_copy::<px_layout::fragment::TextRun>();
    assert_copy::<px_layout::fragment::GlyphPos>();
}

/// The Phase 6 depth gate, with text at the bottom: shaping and line breaking
/// are iterative too, on the same deliberately small stack.
#[test]
fn layout_text_deep_document_with_text_lays_out_on_a_small_stack() {
    let handle = std::thread::Builder::new()
        .name("layout-text-depth".to_owned())
        .stack_size(256 * 1024)
        .spawn(|| {
            // px-dom stops nesting at 512 levels, so a 100,000-node document is
            // 250 chains of 400 nested divs, each holding text at the bottom.
            let (chains, depth) = (250usize, 400usize);
            let mut html = String::from("<html><body>");
            for _ in 0..chains {
                for _ in 0..depth {
                    html.push_str("<div>");
                }
                html.push_str("the quick brown fox &#1488;&#1489;&#1490; jumps");
                for _ in 0..depth {
                    html.push_str("</div>");
                }
            }
            html.push_str("</body></html>");
            let tree = lay_html(&html, "");
            assert!(tree.len() >= chains * depth, "a hundred thousand boxes");
            assert!(lines(&tree).len() >= chains, "the text was laid out");
            drop(tree);
        })
        .expect("spawns");
    handle.join().expect("no stack overflow");
}
