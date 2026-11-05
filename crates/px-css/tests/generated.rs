//! Generated content: `::before`, `::after`, counters, quotes and list markers.
//!
//! Each test styles a document with `StyleEngine::style_document` and reads the
//! generated elements back in document order as `(tag, text)`.

use px_css::engine::StyleEngine;
use px_css::generated::{AFTER, BEFORE, MARKER};

const UA: &str = "html, body, div, p, ul, ol, li, blockquote { display: block } \
    li { display: list-item } ol { list-style-type: decimal } head, style, title { display: none }";

fn styled(html: &str, css: &str) -> (px_dom::Arena, px_css::view::StyleRoot) {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    engine.add_author_stylesheet(css, "https://e.invalid/a.css");
    let root = engine.style_document(&mut arena).expect("styled");
    (arena, root)
}

/// Only `li { display: list-item }`: everything else is the engine's own.
fn styled_bare(html: &str) -> (px_dom::Arena, px_css::view::StyleRoot) {
    let dom = px_dom::parse(html);
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(
        "li { display: list-item } ol, ul { display: block } head { display: none }",
        "about:ua",
    );
    let root = engine.style_document(&mut arena).expect("styled");
    (arena, root)
}

fn text_of(arena: &px_dom::Arena, id: px_dom::NodeId) -> String {
    arena
        .child_ids(id)
        .filter_map(|k| arena.get(k).and_then(|n| n.text().map(|t| t.to_string())))
        .collect()
}

/// `(tag, text)` of every generated element, in document order.
fn generated(html: &str, css: &str) -> Vec<(String, String)> {
    let (arena, _root) = styled(html, css);
    arena
        .descendants(arena.document())
        .filter(|id| px_css::generated::is_generated(&arena, *id))
        .map(|id| {
            let tag = arena
                .get(id)
                .and_then(|n| n.element_name())
                .map(|q| q.local.to_string())
                .unwrap_or_default();
            (tag, text_of(&arena, id))
        })
        .collect()
}

fn texts(html: &str, css: &str) -> Vec<String> {
    generated(html, css)
        .into_iter()
        .filter(|(tag, _)| tag != MARKER)
        .map(|(_, t)| t)
        .collect()
}

#[test]
fn before_and_after_strings() {
    let g = generated(
        "<div>x</div>",
        "div::before { content: 'a' } div::after { content: 'b' 'c' }",
    );
    assert_eq!(
        g,
        vec![
            (BEFORE.to_string(), "a".to_string()),
            (AFTER.to_string(), "bc".to_string())
        ]
    );
}

#[test]
fn before_is_first_child_and_after_last() {
    let (arena, _) = styled(
        "<div id=d><b>x</b></div>",
        "div::before { content: 'a' } div::after { content: 'b' }",
    );
    let div = arena
        .descendants(arena.document())
        .find(|id| {
            arena
                .get(*id)
                .and_then(|n| n.element_name())
                .is_some_and(|q| &*q.local == "div")
        })
        .unwrap();
    let kids: Vec<String> = arena
        .child_ids(div)
        .map(|k| {
            arena
                .get(k)
                .and_then(|n| n.element_name())
                .map(|q| q.local.to_string())
                .unwrap_or_default()
        })
        .collect();
    assert_eq!(kids, vec![BEFORE, "b", AFTER]);
}

#[test]
fn no_content_no_box() {
    assert!(generated("<div>x</div>", "div::before { color: red }").is_empty());
    assert!(generated("<div>x</div>", "div::before { content: none }").is_empty());
    assert!(generated("<div>x</div>", "div::before { content: normal }").is_empty());
}

#[test]
fn empty_string_still_generates_a_box() {
    let g = generated("<div>x</div>", "div::after { content: ''; display: block }");
    assert_eq!(g, vec![(AFTER.to_string(), String::new())]);
}

#[test]
fn display_none_pseudo_and_display_none_host_generate_nothing() {
    assert!(
        generated(
            "<div>x</div>",
            "div::before { content: 'a'; display: none }"
        )
        .is_empty()
    );
    assert!(
        generated(
            "<div>x</div>",
            "div { display: none } div::before { content: 'a' }"
        )
        .is_empty()
    );
}

#[test]
fn replaced_and_void_elements_get_none() {
    assert!(
        generated(
            "<p>a<br>b<img></p>",
            "br::before, img::after, hr::before { content: 'x' }"
        )
        .is_empty()
    );
}

#[test]
fn attr_reads_the_originating_element() {
    let t = texts(
        "<div data-x='hi' title='T'>x</div><p>y</p>",
        "div::before { content: attr(data-x) ' ' attr(title) } p::before { content: attr(nope) 'z' }",
    );
    assert_eq!(t, vec!["hi T", "z"]);
}

#[test]
fn counters_count_in_document_order() {
    let t = texts(
        "<div><p>a</p><p>b</p><p>c</p></div>",
        "div { counter-reset: n } p { counter-increment: n } p::before { content: counter(n) '.' }",
    );
    assert_eq!(t, vec!["1.", "2.", "3."]);
}

#[test]
fn counter_styles_in_content() {
    let t = texts(
        "<div><p>a</p><p>b</p><p>c</p><p>d</p></div>",
        "div { counter-reset: n 3 } p { counter-increment: n } \
         p::before { content: counter(n, upper-roman) counter(n, lower-alpha) counter(n, decimal-leading-zero) }",
    );
    assert_eq!(t, vec!["IVd04", "Ve05", "VIf06", "VIIg07"]);
}

#[test]
fn nested_counters_join() {
    let t = texts(
        "<ol><li>a<ol><li>b</li><li>c</li></ol></li><li>d</li></ol>",
        "ol { counter-reset: s } li { counter-increment: s } li::before { content: counters(s, '.') ' ' }",
    );
    assert_eq!(t, vec!["1 ", "1.1 ", "1.2 ", "2 "]);
}

#[test]
fn counter_without_reset_is_zero_then_instantiates() {
    let t = texts(
        "<p>a</p><p>b</p>",
        "p { counter-increment: q } p::before { content: counter(q) }",
    );
    assert_eq!(t, vec!["1", "2"]);
}

#[test]
fn display_none_elements_do_not_count() {
    let t = texts(
        "<div><p>a</p><p style='display:none'>b</p><p>c</p></div>",
        "div { counter-reset: n } p { counter-increment: n } p::before { content: counter(n) }",
    );
    assert_eq!(t, vec!["1", "2"]);
}

#[test]
fn quotes_nest() {
    let t = texts(
        "<blockquote>a<blockquote>b</blockquote></blockquote>",
        "blockquote { quotes: '<' '>' '[' ']' } \
         blockquote::before { content: open-quote } blockquote::after { content: close-quote }",
    );
    assert_eq!(t, vec!["<", "[", "]", ">"]);
}

#[test]
fn quotes_auto_and_none() {
    let t = texts(
        "<div>a</div>",
        "div::before { content: open-quote } div::after { content: close-quote }",
    );
    assert_eq!(t, vec!["\u{201c}", "\u{201d}"]);
    let t = texts(
        "<div>a</div>",
        "div { quotes: none } div::before { content: open-quote 'x' }",
    );
    assert_eq!(t, vec!["x"]);
}

#[test]
fn no_open_quote_still_counts_depth() {
    let t = texts(
        "<div>a</div>",
        "div { quotes: '<' '>' '[' ']' } div::before { content: no-open-quote open-quote }",
    );
    assert_eq!(t, vec!["["]);
}

// ---- lists ---------------------------------------------------------------

fn markers(html: &str, css: &str) -> Vec<String> {
    generated(html, css)
        .into_iter()
        .filter(|(tag, _)| tag == MARKER)
        .map(|(_, t)| t)
        .collect()
}

#[test]
fn ordered_list_markers() {
    assert_eq!(
        markers(
            "<ol><li>a</li><li>b</li><li>c</li></ol>",
            "ol { list-style-type: decimal }"
        ),
        vec!["1.", "2.", "3."]
    );
}

#[test]
fn bullets_have_no_text() {
    let m = markers("<ul><li>a</li><li>b</li></ul>", "");
    assert_eq!(m, vec!["", ""]);
}

#[test]
fn list_style_types() {
    for (ty, want) in [
        ("lower-alpha", "b."),
        ("upper-alpha", "B."),
        ("lower-roman", "ii."),
        ("upper-roman", "II."),
        ("lower-latin", "b."),
        ("lower-greek", "\u{3b2}."),
        ("decimal-leading-zero", "02."),
        ("'- '", "-"),
    ] {
        let m = markers(
            "<ol><li>a</li><li>b</li></ol>",
            &format!("ol {{ list-style-type: {ty} }}"),
        );
        assert_eq!(m.get(1).map(String::as_str), Some(want), "{ty}");
    }
}

#[test]
fn list_defaults_and_type_attribute() {
    // No UA rules here: the defaults come from the presentational hints.
    let (arena, root) = styled_bare(
        "<ol><li>a</li><li type=i>b</li></ol><ol type=A><li>c</li></ol><ul><li>d<ul><li>e<ul><li>f</li></ul></li></ul></li></ul><ul type=square><li>g</li></ul>",
    );
    let _ = (&arena, &root);
    let ms: Vec<String> = arena
        .descendants(arena.document())
        .filter(|id| px_css::generated::is_generated(&arena, *id))
        .map(|id| text_of(&arena, id))
        .collect();
    assert_eq!(ms, vec!["1.", "ii.", "A.", "", "", "", ""]);
}

#[test]
fn marker_pseudo_content_and_colour() {
    assert_eq!(
        markers("<ul><li>a</li></ul>", "li::marker { content: '>> ' }"),
        vec![">> "]
            .into_iter()
            .map(|s: &str| s.trim_end().to_string())
            .collect::<Vec<_>>()
    );
    assert!(markers("<ul><li>a</li></ul>", "li::marker { content: none }").is_empty());
    let (arena, root) = styled(
        "<ul><li>a</li></ul>",
        "li::marker { color: rgb(255, 0, 0) }",
    );
    let m = arena
        .descendants(arena.document())
        .find(|id| px_css::generated::is_generated(&arena, *id))
        .unwrap();
    px_css::view::with_dom(&arena, &root, |dom| {
        let el = px_css::dom::StyleElement::new(dom.node(m).unwrap()).unwrap();
        let data = style::dom::TElement::borrow_data(&el).unwrap();
        let c = data
            .styles
            .primary()
            .get_inherited_text()
            .clone_color()
            .into_srgb_legacy();
        assert!(c.components.0 > 0.99 && c.components.1 < 0.01, "{c:?}");
    })
    .unwrap();
}

#[test]
fn list_style_none_has_no_marker() {
    assert!(markers("<ul><li>a</li></ul>", "ul { list-style: none }").is_empty());
    assert!(markers("<ul><li>a</li></ul>", "li { display: block }").is_empty());
}

#[test]
fn ol_start_reversed_and_li_value() {
    assert_eq!(
        markers("<ol start=5><li>a</li><li>b</li></ol>", ""),
        vec!["5.", "6."]
    );
    assert_eq!(
        markers("<ol reversed><li>a</li><li>b</li><li>c</li></ol>", ""),
        vec!["3.", "2.", "1."]
    );
    assert_eq!(
        markers("<ol reversed start=10><li>a</li><li>b</li></ol>", ""),
        vec!["10.", "9."]
    );
    assert_eq!(
        markers("<ol><li>a</li><li value=7>b</li><li>c</li></ol>", ""),
        vec!["1.", "7.", "8."]
    );
}

#[test]
fn nested_lists_count_separately() {
    assert_eq!(
        markers(
            "<ol><li>a<ol><li>x</li><li>y</li></ol></li><li>b</li></ol>",
            ""
        ),
        vec!["1.", "1.", "2.", "2."]
    );
}

#[test]
fn author_counter_increment_of_list_item() {
    assert_eq!(
        markers(
            "<ol><li>a</li><li>b</li></ol>",
            "li { counter-increment: list-item 10 }"
        ),
        vec!["10.", "20."]
    );
}

#[test]
fn outside_marker_is_flagged_and_inside_is_not() {
    let (arena, _) = styled("<ul><li>a</li></ul>", "");
    let m = arena
        .descendants(arena.document())
        .find(|id| px_css::generated::is_generated(&arena, *id))
        .unwrap();
    assert!(px_css::generated::is_outside_marker(&arena, m));
    let (arena, _) = styled("<ul><li>a</li></ul>", "ul { list-style-position: inside }");
    let m = arena
        .descendants(arena.document())
        .find(|id| px_css::generated::is_generated(&arena, *id))
        .unwrap();
    assert!(!px_css::generated::is_outside_marker(&arena, m));
}

#[test]
fn marker_moves_into_a_leading_block_child() {
    let (arena, _) = styled("<ul><li><p>a</p></li></ul>", "");
    let m = arena
        .descendants(arena.document())
        .find(|id| px_css::generated::is_generated(&arena, *id))
        .unwrap();
    let parent = arena.get(m).and_then(|n| n.parent()).unwrap();
    let name = arena.get(parent).and_then(|n| n.element_name()).unwrap();
    assert_eq!(&*name.local, "p");
}

#[test]
fn restyling_does_not_accumulate_generated_nodes() {
    let dom = px_dom::parse("<div>x</div>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA, "about:ua");
    engine.add_author_stylesheet("div::before { content: 'a' }", "https://e.invalid/a.css");
    let count = |arena: &px_dom::Arena| {
        arena
            .descendants(arena.document())
            .filter(|id| px_css::generated::is_generated(arena, *id))
            .count()
    };
    engine.style_document(&mut arena).unwrap();
    assert_eq!(count(&arena), 1);
    engine.style_document(&mut arena).unwrap();
    assert_eq!(count(&arena), 1);
}

#[test]
fn selectors_do_not_see_generated_children() {
    // A plain `resolve` over an arena that still holds generated nodes (what a
    // careless caller would do) must not let them change structural selectors.
    let dom = px_dom::parse("<div><b>x</b></div>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(
        "div::before { content: 'a' } div::after { content: 'z' } \
         b:first-child:last-child { width: 9px }",
        "https://e.invalid/a.css",
    );
    engine.style_document(&mut arena).unwrap();
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).unwrap();
    let b = arena
        .descendants(arena.document())
        .find(|id| {
            arena
                .get(*id)
                .and_then(|n| n.element_name())
                .is_some_and(|q| &*q.local == "b")
        })
        .unwrap();
    px_css::view::with_dom(&arena, &root, |dom| {
        let el = px_css::dom::StyleElement::new(dom.node(b).unwrap()).unwrap();
        let data = style::dom::TElement::borrow_data(&el).unwrap();
        let w = format!("{:?}", data.styles.primary().clone_width());
        assert!(w.contains('9'), "{w}");
    })
    .unwrap();
}

#[test]
fn generated_elements_have_computed_style() {
    use style::values::specified::box_::{DisplayInside, DisplayOutside};
    let (arena, root) = styled(
        "<div>x</div>",
        "div::after { content: ''; display: block; width: 7px }",
    );
    let after = arena
        .descendants(arena.document())
        .find(|id| px_css::generated::is_generated(&arena, *id))
        .unwrap();
    px_css::view::with_dom(&arena, &root, |dom| {
        let el = px_css::dom::StyleElement::new(dom.node(after).unwrap()).unwrap();
        let data = style::dom::TElement::borrow_data(&el).unwrap();
        let d = data.styles.primary().clone_display();
        assert!(matches!(d.outside(), DisplayOutside::Block));
        assert!(matches!(d.inside(), DisplayInside::Flow));
    })
    .unwrap();
}
