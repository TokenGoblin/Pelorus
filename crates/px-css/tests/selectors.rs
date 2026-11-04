//! Selector matching, inheritance, `!important` and `@media`, through the
//! product path: parse HTML with `px-dom`, add an author stylesheet, run
//! `StyleEngine::resolve`, read computed values back.
//!
//! Every fixture marks "matched" by `opacity: 0.5` (non-inherited, initial 1),
//! so a test reads as "these ids, and only these, matched". Expected sets are
//! derived by hand from Selectors Level 4 and CSS Cascade, not read back.

use px_css::engine::StyleEngine;
use px_dom::NodeId;

const DOCTYPE: &str = "<!DOCTYPE html>";

fn resolved_at(
    html: &str,
    css: &[&str],
    width: f32,
    height: f32,
) -> (px_dom::Arena, px_css::view::StyleRoot) {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = StyleEngine::new(width, height, quirks);
    for (index, sheet) in css.iter().enumerate() {
        engine.add_author_stylesheet(sheet, &format!("https://example.invalid/{index}.css"));
    }
    let root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &root)
        .expect("the document has a root element");
    (arena, root)
}

fn element_with_id(arena: &px_dom::Arena, wanted: &str) -> Option<NodeId> {
    let document = arena.document();
    core::iter::once(document)
        .chain(arena.descendants(document))
        .find(|id| {
            arena.get(*id).is_some_and(|node| {
                node.attrs().is_some_and(|attrs| {
                    attrs.iter().any(|a| {
                        a.name.local == html5ever::local_name!("id") && &*a.value == wanted
                    })
                })
            })
        })
}

fn computed<T>(
    arena: &px_dom::Arena,
    root: &px_css::view::StyleRoot,
    id: &str,
    read: impl FnOnce(&style::properties::ComputedValues) -> T,
) -> T {
    let target = element_with_id(arena, id).unwrap_or_else(|| unreachable!("no element id={id}"));
    px_css::view::with_dom(arena, root, |dom| {
        let node = dom.node(target).expect("the target resolves");
        let element = px_css::dom::StyleElement::new(node).expect("the target is an element");
        let data = style::dom::TElement::borrow_data(&element).expect("the target was styled");
        read(data.styles.primary())
    })
    .expect("the document resolves")
}

fn opacity(arena: &px_dom::Arena, root: &px_css::view::StyleRoot, id: &str) -> f32 {
    computed(
        arena,
        root,
        id,
        style::properties::ComputedValues::clone_opacity,
    )
}

/// Red channel of the computed `color`, scaled back to 0..=255.
fn red(arena: &px_dom::Arena, root: &px_css::view::StyleRoot, id: &str) -> u32 {
    let c = computed(
        arena,
        root,
        id,
        style::properties::ComputedValues::clone_color,
    );
    (c.components.0 * 255.0).round() as u32
}

/// Which of `ids` have `opacity: 0.5` once `css` is applied.
fn matched_at(html: &str, css: &[&str], ids: &[&'static str], w: f32) -> Vec<&'static str> {
    let (arena, root) = resolved_at(html, css, w, 600.0);
    ids.iter()
        .copied()
        .filter(|id| {
            let o = opacity(&arena, &root, id);
            assert!(
                (o - 0.5).abs() < 1e-6 || (o - 1.0).abs() < 1e-6,
                "unexpected opacity {o} on #{id}"
            );
            o < 0.75
        })
        .collect()
}

fn matched(html: &str, css: &str, ids: &[&'static str]) -> Vec<&'static str> {
    matched_at(html, &[css], ids, 800.0)
}

fn doc(body: &str) -> String {
    format!("{DOCTYPE}<html><body>{body}</body></html>")
}

// ---- attribute selectors (Selectors 4 section 6) ------------------------

const ATTR_HTML: &str = "<p id=a data-x=foo></p>\
    <p id=b data-x=\"foo bar baz\"></p>\
    <p id=c data-x=\"foo-bar\"></p>\
    <p id=d data-x=Foo></p>\
    <p id=e data-x=\"\"></p>\
    <p id=f></p>\
    <p id=g data-x=\"bar foo\"></p>";
const ATTR_IDS: [&str; 7] = ["a", "b", "c", "d", "e", "f", "g"];

#[test]
fn css_selectors_attribute_presence_and_exact_value() {
    let html = doc(ATTR_HTML);
    assert_eq!(
        matched(&html, "[data-x] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "b", "c", "d", "e", "g"],
        "presence matches even an empty value, not a missing attribute"
    );
    assert_eq!(
        matched(&html, "[data-x=foo] { opacity: .5 }", &ATTR_IDS),
        vec!["a"],
        "exact and case-sensitive for a non-enumerated attribute"
    );
    assert_eq!(
        matched(&html, "[data-x=\"\"] { opacity: .5 }", &ATTR_IDS),
        vec!["e"]
    );
}

#[test]
fn css_selectors_attribute_word_prefix_and_dash_match() {
    let html = doc(ATTR_HTML);
    assert_eq!(
        matched(&html, "[data-x~=foo] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "b", "g"],
        "~= matches a whole whitespace-separated word"
    );
    assert_eq!(
        matched(&html, "[data-x|=foo] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "c"],
        "|= matches the value or the value followed by a hyphen"
    );
}

#[test]
fn css_selectors_attribute_substring_operators() {
    let html = doc(ATTR_HTML);
    assert_eq!(
        matched(&html, "[data-x^=foo] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "b", "c"]
    );
    assert_eq!(
        matched(&html, "[data-x$=foo] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "g"]
    );
    assert_eq!(
        matched(&html, "[data-x*=oo] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "b", "c", "d", "g"],
        "Foo contains oo too"
    );
}

#[test]
fn css_selectors_substring_operators_with_an_empty_operand_match_nothing() {
    // Selectors 4 6.2: ^=, $= and *= with an empty value represent nothing.
    let html = doc(ATTR_HTML);
    for op in ["^=", "$=", "*="] {
        let css = format!("[data-x{op}\"\"] {{ opacity: .5 }}");
        assert_eq!(matched(&html, &css, &ATTR_IDS), Vec::<&str>::new(), "{op}");
    }
}

#[test]
fn css_selectors_attribute_case_flags() {
    let html = doc(ATTR_HTML);
    assert_eq!(
        matched(&html, "[data-x=foo i] { opacity: .5 }", &ATTR_IDS),
        vec!["a", "d"],
        "the i flag makes the value comparison ASCII case-insensitive"
    );
    assert_eq!(
        matched(&html, "[data-x=Foo s] { opacity: .5 }", &ATTR_IDS),
        vec!["d"],
        "the s flag forces case-sensitivity"
    );
}

#[test]
fn css_selectors_html_enumerated_attribute_values_ignore_case() {
    // HTML (not Selectors) makes `type` values ASCII case-insensitive.
    let html = doc("<input id=a type=TEXT><input id=b type=text><input id=c type=radio>");
    assert_eq!(
        matched(&html, "[type=text] { opacity: .5 }", &["a", "b", "c"]),
        vec!["a", "b"]
    );
}

#[test]
fn css_selectors_attribute_names_are_case_insensitive_in_html() {
    let html = doc("<p id=a DATA-X=1></p><p id=b></p>");
    assert_eq!(
        matched(&html, "[Data-X] { opacity: .5 }", &["a", "b"]),
        vec!["a"]
    );
}

// ---- structural pseudo-classes (Selectors 4 section 14) -----------------

const LIST: &str = "<ul id=ul><li id=l1>1</li><li id=l2>2</li><li id=l3>3</li>\
    <li id=l4>4</li><li id=l5>5</li></ul>";
const LIST_IDS: [&str; 5] = ["l1", "l2", "l3", "l4", "l5"];

#[test]
fn css_selectors_first_last_and_only_child() {
    let html = doc(LIST);
    assert_eq!(
        matched(&html, "li:first-child { opacity: .5 }", &LIST_IDS),
        vec!["l1"]
    );
    assert_eq!(
        matched(&html, "li:last-child { opacity: .5 }", &LIST_IDS),
        vec!["l5"]
    );
    assert_eq!(
        matched(&html, "li:only-child { opacity: .5 }", &LIST_IDS),
        Vec::<&str>::new()
    );
    let single = doc("<ul><li id=only>x</li></ul>");
    assert_eq!(
        matched(&single, "li:only-child { opacity: .5 }", &["only"]),
        vec!["only"]
    );
}

#[test]
fn css_selectors_nth_child_an_plus_b() {
    let html = doc(LIST);
    let cases: [(&str, Vec<&str>); 9] = [
        ("2", vec!["l2"]),
        ("odd", vec!["l1", "l3", "l5"]),
        ("even", vec!["l2", "l4"]),
        ("2n+1", vec!["l1", "l3", "l5"]),
        ("3n", vec!["l3"]),
        ("-n+2", vec!["l1", "l2"]),
        ("n+4", vec!["l4", "l5"]),
        ("-2n+5", vec!["l1", "l3", "l5"]),
        ("0", vec![]),
    ];
    for (arg, expected) in cases {
        let css = format!("li:nth-child({arg}) {{ opacity: .5 }}");
        assert_eq!(
            matched(&html, &css, &LIST_IDS),
            expected,
            ":nth-child({arg})"
        );
    }
}

#[test]
fn css_selectors_nth_last_child_counts_from_the_end() {
    let html = doc(LIST);
    assert_eq!(
        matched(&html, "li:nth-last-child(2) { opacity: .5 }", &LIST_IDS),
        vec!["l4"]
    );
    assert_eq!(
        matched(&html, "li:nth-last-child(odd) { opacity: .5 }", &LIST_IDS),
        vec!["l1", "l3", "l5"]
    );
}

#[test]
fn css_selectors_text_and_comments_do_not_count_as_siblings() {
    let html = doc(
        "<div>text <!-- c --><span id=s1></span> more text <!-- d --><span id=s2></span> tail</div>",
    );
    assert_eq!(
        matched(&html, "span:first-child { opacity: .5 }", &["s1", "s2"]),
        vec!["s1"]
    );
    assert_eq!(
        matched(&html, "span:nth-child(2) { opacity: .5 }", &["s1", "s2"]),
        vec!["s2"]
    );
    assert_eq!(
        matched(&html, "span:last-child { opacity: .5 }", &["s1", "s2"]),
        vec!["s2"]
    );
}
#[test]
fn css_selectors_of_type_variants_count_only_the_same_element_name() {
    let html = doc(
        "<div><p id=p1></p><span id=s1></span><p id=p2></p><span id=s2></span><p id=p3></p></div>",
    );
    let ids = ["p1", "s1", "p2", "s2", "p3"];
    assert_eq!(
        matched(&html, "div > :nth-of-type(2) { opacity: .5 }", &ids),
        vec!["p2", "s2"]
    );
    assert_eq!(
        matched(
            &html,
            "p:first-of-type, span:last-of-type { opacity: .5 }",
            &ids
        ),
        vec!["p1", "s2"]
    );
    assert_eq!(
        matched(&html, "p:last-of-type { opacity: .5 }", &ids),
        vec!["p3"]
    );
}

#[test]
fn css_selectors_empty_ignores_comments_but_not_text() {
    let html = doc("<p id=a></p><p id=b><!-- c --></p><p id=c> </p><p id=d><i></i></p>");
    assert_eq!(
        matched(&html, "p:empty { opacity: .5 }", &["a", "b", "c", "d"]),
        vec!["a", "b"],
        "whitespace text and element children make an element non-empty"
    );
}

#[test]
fn css_selectors_root_matches_html_and_nothing_inside_it() {
    let html = doc("<p id=a></p>");
    let (arena, root) = resolved_at(&html, &[":root { color: rgb(7,0,0) }"], 800.0, 600.0);
    // :root sets color on <html>; <p> inherits it, <p> itself is not :root.
    assert_eq!(red(&arena, &root, "a"), 7);
    let (arena, root) = resolved_at(&html, &["p:root { color: rgb(7,0,0) }"], 800.0, 600.0);
    assert_ne!(red(&arena, &root, "a"), 7, "a <p> is never :root");
}

// ---- combinators (Selectors 4 section 16) -------------------------------

const TREE: &str = "<div id=d>\
    <p id=p1><b id=b1></b></p>\
    <p id=p2></p>\
    <span id=s1></span>\
    <p id=p3></p>\
    <section id=sec><p id=p4></p></section>\
    </div>";

#[test]
fn css_selectors_child_vs_descendant_combinator() {
    let html = doc(TREE);
    let ids = ["p1", "p2", "p3", "p4", "b1"];
    assert_eq!(
        matched(&html, "div > p { opacity: .5 }", &ids),
        vec!["p1", "p2", "p3"],
        "p4 is a grandchild"
    );
    assert_eq!(
        matched(&html, "div p { opacity: .5 }", &ids),
        vec!["p1", "p2", "p3", "p4"]
    );
    assert_eq!(
        matched(&html, "div > p > b { opacity: .5 }", &ids),
        vec!["b1"]
    );
}

#[test]
fn css_selectors_adjacent_sibling_combinator() {
    let html = doc(TREE);
    let ids = ["p1", "p2", "s1", "p3", "sec"];
    assert_eq!(
        matched(&html, "p + p { opacity: .5 }", &ids),
        vec!["p2"],
        "p3 follows a span, not a p"
    );
    assert_eq!(matched(&html, "span + p { opacity: .5 }", &ids), vec!["p3"]);
    assert_eq!(
        matched(&html, "p + section { opacity: .5 }", &ids),
        vec!["sec"]
    );
}

#[test]
fn css_selectors_general_sibling_combinator() {
    let html = doc(TREE);
    let ids = ["p1", "p2", "s1", "p3", "sec", "p4", "b1"];
    assert_eq!(
        matched(&html, "#p1 ~ * { opacity: .5 }", &ids),
        vec!["p2", "s1", "p3", "sec"],
        "every later sibling of #p1, and not its own descendants or p4"
    );
    assert_eq!(
        matched(&html, "span ~ p { opacity: .5 }", &ids),
        vec!["p3"],
        "p2 precedes the span"
    );
}

#[test]
fn css_selectors_sibling_combinators_skip_text_and_comments() {
    let html = doc("<i id=a></i> text <!-- c --> <b id=b></b>");
    assert_eq!(
        matched(&html, "i + b { opacity: .5 }", &["a", "b"]),
        vec!["b"]
    );
}

#[test]
fn css_selectors_not_is_and_where_and_their_specificity() {
    let html = doc("<p id=a class=x></p><p id=b></p><p id=c class=x></p>");
    let ids = ["a", "b", "c"];
    assert_eq!(matched(&html, "p:not(.x) { opacity: .5 }", &ids), vec!["b"]);
    assert_eq!(
        matched(&html, "p:is(#a, #b) { opacity: .5 }", &ids),
        vec!["a", "b"]
    );
    assert_eq!(
        matched(&html, "p:not(.x, #b) { opacity: .5 }", &ids),
        Vec::<&str>::new()
    );
    // :where contributes zero specificity, :is the highest of its arguments
    // (Selectors 4 17). `.x` (0,1,0) beats `:where(#a)` (0,0,0) even though it
    // is earlier; but `:is(#a)` (1,0,0) beats `.x`.
    let (arena, root) = resolved_at(
        &html,
        &[".x { opacity: .25 } :where(#a) { opacity: .75 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.25).abs() < 1e-6);
    let (arena, root) = resolved_at(
        &html,
        &[".x { opacity: .25 } :is(#a) { opacity: .75 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.75).abs() < 1e-6);
}

#[test]
fn css_selectors_class_and_id_case_sensitivity_depends_on_quirks_mode() {
    let body = "<p id=Box class=Big></p>";
    let css = "#box { opacity: .5 } .big { opacity: .5 }";
    // No-quirks (doctype present): case-sensitive, no match.
    assert_eq!(matched(&doc(body), css, &["Box"]), Vec::<&str>::new());
    // Quirks (no doctype): ASCII case-insensitive (HTML Standard, quirks mode).
    let quirky = format!("<html><body>{body}</body></html>");
    assert_eq!(matched(&quirky, css, &["Box"]), vec!["Box"]);
}

#[test]
fn css_selectors_type_selectors_are_case_insensitive_in_html() {
    let html = doc("<p id=a></p>");
    assert_eq!(matched(&html, "P { opacity: .5 }", &["a"]), vec!["a"]);
}

#[test]
fn css_selectors_class_matches_any_token() {
    let html =
        doc("<p id=a class=\"x y z\"></p><p id=b class=xy></p><p id=c class=\"  y\t\n\"></p>");
    assert_eq!(
        matched(&html, ".y { opacity: .5 }", &["a", "b", "c"]),
        vec!["a", "c"]
    );
    assert_eq!(
        matched(&html, ".x.y.z { opacity: .5 }", &["a", "b", "c"]),
        vec!["a"]
    );
}

// ---- invalid selectors are dropped (CSS Syntax 3 / Selectors 4 3.1) -----

#[test]
fn css_selectors_an_unknown_pseudo_class_drops_its_rule_only() {
    let html = doc("<p id=a></p><p id=b></p>");
    assert_eq!(
        matched(
            &html,
            "#a:bogus { opacity: .5 } #b { opacity: .5 }",
            &["a", "b"]
        ),
        vec!["b"]
    );
}

#[test]
fn css_selectors_one_invalid_selector_invalidates_the_whole_list() {
    let html = doc("<p id=a></p><p id=b></p>");
    assert_eq!(
        matched(&html, "#a, #b:bogus { opacity: .5 }", &["a", "b"]),
        Vec::<&str>::new(),
        "Selectors 4 3.1: an invalid selector in a list invalidates the rule"
    );
}

#[test]
fn css_selectors_is_and_where_are_forgiving() {
    let html = doc("<p id=a></p><p id=b></p>");
    assert_eq!(
        matched(&html, ":is(#a, :bogus) { opacity: .5 }", &["a", "b"]),
        vec!["a"],
        "an invalid argument inside :is() is dropped, not the rule"
    );
    assert_eq!(
        matched(&html, ":where(:bogus, #b) { opacity: .5 }", &["a", "b"]),
        vec!["b"]
    );
}

#[test]
fn css_selectors_malformed_selectors_do_not_stop_later_rules() {
    // Not `p[ {...}`: an unclosed `[` block legitimately swallows the rest of
    // the sheet (CSS Syntax 3 consume-a-simple-block), so nothing follows it.
    for bad in [
        "p[x { opacity: .25 }] p { opacity: .25 }",
        "p:nth-child(foo) { opacity: .25 }",
        ">> p { opacity: .25 }",
        "p..x { opacity: .25 }",
        "# { opacity: .25 }",
    ] {
        let css = format!("{bad}\n#a {{ opacity: .5 }}");
        assert_eq!(
            matched(&doc("<p id=a></p>"), &css, &["a"]),
            vec!["a"],
            "after {bad:?}"
        );
    }
}

#[test]
fn css_selectors_pseudo_elements_do_not_style_the_element_itself() {
    let html = doc("<p id=a></p>");
    assert_eq!(
        matched(
            &html,
            "p::before { opacity: .5 } p::first-line { opacity: .5 }",
            &["a"]
        ),
        Vec::<&str>::new()
    );
}

#[test]
fn css_selectors_unclosed_block_at_eof_is_closed_by_the_parser() {
    // CSS Syntax 3: EOF closes open blocks, so the declaration applies.
    let html = doc("<p id=a></p>");
    assert_eq!(matched(&html, "#a { opacity: .5", &["a"]), vec!["a"]);
}

// ---- inheritance (CSS Cascade 4 section 7.1) ----------------------------

#[test]
fn css_inheritance_inherited_property_flows_through_unstyled_ancestors() {
    let html = doc("<div id=a><section id=b><p id=c><span id=d></span></p></section></div>");
    let (arena, root) = resolved_at(
        &html,
        &["#a { color: rgb(10, 20, 30); opacity: .5 }"],
        800.0,
        600.0,
    );
    for id in ["a", "b", "c", "d"] {
        assert_eq!(red(&arena, &root, id), 10, "color inherits to #{id}");
    }
    assert!((opacity(&arena, &root, "a") - 0.5).abs() < 1e-6);
    for id in ["b", "c", "d"] {
        assert!(
            (opacity(&arena, &root, id) - 1.0).abs() < 1e-6,
            "opacity is not inherited by #{id}"
        );
    }
}

#[test]
fn css_inheritance_nearest_declaration_wins_and_inherit_restores() {
    let html = doc("<div id=a><p id=b><span id=c></span></p><p id=d></p></div>");
    let (arena, root) = resolved_at(
        &html,
        &["#a { color: rgb(1,0,0) } #b { color: rgb(2,0,0) } #d { color: inherit }"],
        800.0,
        600.0,
    );
    assert_eq!(red(&arena, &root, "b"), 2);
    assert_eq!(red(&arena, &root, "c"), 2, "inherits from #b, not #a");
    assert_eq!(red(&arena, &root, "d"), 1);
}

#[test]
fn css_inheritance_a_matched_rule_beats_the_inherited_value() {
    let html = doc("<div id=a><p id=b></p></div>");
    let (arena, root) = resolved_at(
        &html,
        &["#a { color: rgb(1,0,0) } p { color: rgb(9,0,0) }"],
        800.0,
        600.0,
    );
    assert_eq!(red(&arena, &root, "b"), 9);
}

// ---- cascade order ------------------------------------------------------

#[test]
fn css_cascade_later_rule_wins_at_equal_specificity_across_sheets() {
    let html = doc("<p id=a></p>");
    let (arena, root) = resolved_at(
        &html,
        &["p { opacity: .25 }", "p { opacity: .75 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.75).abs() < 1e-6);
    let (arena, root) = resolved_at(
        &html,
        &["p { opacity: .75 }", "p { opacity: .25 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.25).abs() < 1e-6);
}

#[test]
fn css_cascade_important_beats_higher_specificity_and_later_order() {
    let html = doc("<p id=a class=c></p>");
    let (arena, root) = resolved_at(
        &html,
        &["p { opacity: .25 !important } #a.c { opacity: .75 } p { opacity: .9 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.25).abs() < 1e-6);
}

#[test]
fn css_cascade_among_important_declarations_specificity_decides() {
    let html = doc("<p id=a></p>");
    let (arena, root) = resolved_at(
        &html,
        &["#a { opacity: .25 !important } p { opacity: .75 !important }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.25).abs() < 1e-6);
}

#[test]
fn css_cascade_important_only_protects_the_declaration_it_marks() {
    let html = doc("<p id=a></p>");
    let (arena, root) = resolved_at(
        &html,
        &["p { color: rgb(5,0,0) !important; opacity: .25 } p { color: rgb(6,0,0); opacity: .75 }"],
        800.0,
        600.0,
    );
    assert_eq!(red(&arena, &root, "a"), 5);
    assert!((opacity(&arena, &root, "a") - 0.75).abs() < 1e-6);
}

#[test]
fn css_cascade_specificity_beats_source_order() {
    let html = doc("<p id=a class=c></p>");
    let (arena, root) = resolved_at(
        &html,
        &[".c { opacity: .25 } p { opacity: .75 }"],
        800.0,
        600.0,
    );
    assert!((opacity(&arena, &root, "a") - 0.25).abs() < 1e-6);
}

// ---- @media against the engine viewport (Media Queries 4) ---------------

fn media(css: &str, w: f32, h: f32) -> bool {
    let html = doc("<p id=a></p>");
    let (arena, root) = resolved_at(&html, &[css], w, h);
    (opacity(&arena, &root, "a") - 0.5).abs() < 1e-6
}

fn rule(q: &str) -> String {
    format!("@media {q} {{ p {{ opacity: .5 }} }}")
}

#[test]
fn css_media_width_features_use_the_engine_viewport() {
    assert!(
        media(&rule("(min-width: 800px)"), 800.0, 600.0),
        "inclusive"
    );
    assert!(!media(&rule("(min-width: 801px)"), 800.0, 600.0));
    assert!(
        media(&rule("(max-width: 800px)"), 800.0, 600.0),
        "inclusive"
    );
    assert!(!media(&rule("(max-width: 799px)"), 800.0, 600.0));
    assert!(media(&rule("(width: 800px)"), 800.0, 600.0));
    assert!(!media(&rule("(width: 799px)"), 800.0, 600.0));
    assert!(media(&rule("(400px <= width <= 900px)"), 800.0, 600.0));
    assert!(!media(&rule("(400px <= width < 800px)"), 800.0, 600.0));
}

#[test]
fn css_media_the_same_stylesheet_flips_with_the_viewport() {
    let css = "@media (min-width: 700px) { p { opacity: .5 } }";
    assert!(media(css, 800.0, 600.0));
    assert!(!media(css, 640.0, 600.0));
}

#[test]
fn css_media_height_and_orientation() {
    assert!(media(&rule("(min-height: 600px)"), 800.0, 600.0));
    assert!(!media(&rule("(min-height: 601px)"), 800.0, 600.0));
    assert!(media(&rule("(orientation: landscape)"), 800.0, 600.0));
    assert!(!media(&rule("(orientation: portrait)"), 800.0, 600.0));
    assert!(media(&rule("(orientation: portrait)"), 600.0, 800.0));
    // MQ4: portrait is height >= width, so a square viewport is portrait.
    assert!(media(&rule("(orientation: portrait)"), 600.0, 600.0));
}

#[test]
fn css_media_logic_not_and_or_comma_and_types() {
    assert!(media(&rule("screen"), 800.0, 600.0));
    assert!(media(&rule("all"), 800.0, 600.0));
    assert!(!media(&rule("print"), 800.0, 600.0));
    assert!(media(&rule("not print"), 800.0, 600.0));
    assert!(media(&rule("screen and (min-width: 100px)"), 800.0, 600.0));
    assert!(!media(
        &rule("screen and (min-width: 100px) and (max-width: 200px)"),
        800.0,
        600.0
    ));
    assert!(media(
        &rule("(max-width: 100px) or (min-width: 700px)"),
        800.0,
        600.0
    ));
    assert!(media(&rule("print, (min-width: 700px)"), 800.0, 600.0));
    assert!(!media(&rule("print, (max-width: 100px)"), 800.0, 600.0));
    assert!(media(&rule("not (max-width: 100px)"), 800.0, 600.0));
}

#[test]
fn css_media_unknown_feature_is_false_and_does_not_affect_other_rules() {
    let css = "@media (bogus-feature: 1) { p { opacity: .25 } } p { opacity: .5 }";
    assert!(media(css, 800.0, 600.0));
}

#[test]
fn css_media_nested_media_requires_both_conditions() {
    let css = "@media (min-width: 500px) { @media (max-width: 700px) { p { opacity: .5 } } }";
    assert!(media(css, 600.0, 600.0));
    assert!(!media(css, 800.0, 600.0));
    assert!(!media(css, 400.0, 600.0));
}

#[test]
fn css_media_em_lengths_resolve_against_the_initial_font_size() {
    // MQ4 4.1: relative units in a media query use the initial value (16px).
    let css = "@media (min-width: 40em) { p { opacity: .5 } }";
    assert!(media(css, 640.0, 600.0));
    assert!(!media(css, 639.0, 600.0));
}
