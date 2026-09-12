//! Phase 5 gate item 2: the CSS cascade, checked against WPT `css/css-cascade`.
//!
//! # These are ports, not runs, and the difference matters
//!
//! §9 Phase 5's second gate item is *"WPT `css/css-cascade` subset passes"*.
//! **That cannot be met literally in Phase 5**, and the reason is structural
//! rather than a matter of effort. Every test in that directory is one of two
//! kinds, and both need phases that do not exist yet:
//!
//! - **reftests**, carrying `<link rel="match" href="reference/...">`. Passing one
//!   means rendering two documents and comparing pixels, which needs layout
//!   (Phase 6), paint (Phase 8) and text shaping (Phase 9).
//! - **testharness.js tests**, which load `/resources/testharness.js` and assert
//!   with `assert_equals(getComputedStyle(el).opacity, "0.5")`. Passing one means
//!   running JavaScript against DOM bindings, which is Phases 10 and 11.
//!
//! Verified by reading the directory and three files rather than assumed:
//! `all-prop-unset-color.html` is a reftest, `important-vs-inline-001.html` is
//! testharness, and `parsing/all-valid.html` — the best candidate for a runnable
//! subset, since parsing needs no rendering — is testharness too, calling
//! `test_valid_value("all", "initial")` through CSSOM.
//!
//! So each test below **ports** a named WPT test: it asserts the same behaviour
//! that test asserts, against computed style read through `TElement`, and records
//! which file it came from. What is checked is the cascade. What is not checked is
//! WPT's own harness, and the gate report says so in those words rather than
//! implying a WPT run happened. ADR 028 records the decision.
//!
//! When JavaScript lands, these should be **replaced** by the real tests rather
//! than kept alongside them. A ported assertion that has drifted from its source
//! is worse than no port, because it still looks like coverage.

use px_css::engine::StyleEngine;
use px_dom::NodeId;

/// Resolve `html` with `css` as the single author stylesheet.
fn resolved(html: &str, css: &str) -> (px_dom::Arena, px_css::view::StyleRoot) {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(css, "https://example.invalid/a.css");
    let root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &root)
        .expect("the document has a root element");
    (arena, root)
}

/// The first element whose `id` attribute equals `wanted`.
fn element_with_id(arena: &px_dom::Arena, wanted: &str) -> NodeId {
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
        .unwrap_or_else(|| panic!("no element with id={wanted}"))
}

/// Read one computed value off the element with `id`.
fn computed<T>(
    arena: &px_dom::Arena,
    root: &px_css::view::StyleRoot,
    id: &str,
    read: impl FnOnce(&style::properties::ComputedValues) -> T,
) -> T {
    let target = element_with_id(arena, id);
    px_css::view::with_dom(arena, root, |dom| {
        let node = dom.node(target).expect("the target resolves");
        let element = px_css::dom::StyleElement::new(node).expect("the target is an element");
        let data = style::dom::TElement::borrow_data(&element).expect("the target was styled");
        read(data.styles.primary())
    })
    .expect("the document resolves")
}

/// The three colour channels, normalised to 0..=1 the way stylo stores them.
fn channels(color: &style::color::AbsoluteColor) -> [f32; 3] {
    [color.components.0, color.components.1, color.components.2]
}

/// Ported from `css/css-cascade/important-vs-inline-001.html`.
///
/// An `!important` author declaration beats an inline `style` attribute. WPT
/// asserts `getComputedStyle(el).opacity == "0.5"` after setting
/// `el.style.opacity = "1"`; here the attribute is in the markup and the computed
/// value is read directly.
///
/// This is the test that made inline style worth implementing. Without
/// `TElement::style_attribute` the first half would still have computed 0.5 — so
/// it would have passed while proving nothing, which is what the control below is
/// for.
#[test]
fn cascade_important_author_beats_inline_style() {
    let (arena, root) = resolved(
        "<html><body><p id=a style=\"opacity: 1\">x</p>\
         <p id=b style=\"opacity: 1\">y</p></body></html>",
        "#a { opacity: 0.5 !important }",
    );

    let a = computed(
        &arena,
        &root,
        "a",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (a - 0.5).abs() < 1e-6,
        "!important must beat an inline declaration, got {a}"
    );

    // The control, and the reason this test is trustworthy: a `style_attribute`
    // that returned `None` for everything would satisfy the assertion above and
    // fail this one.
    let b = computed(
        &arena,
        &root,
        "b",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (b - 1.0).abs() < 1e-6,
        "an inline declaration with no !important rule against it must win, got {b}"
    );
}

/// Ported from `css/css-cascade/inherit-initial.html`.
///
/// `inherit` takes the parent's computed value even for a non-inherited property;
/// `initial` takes the property's initial value even for an inherited one.
#[test]
fn cascade_inherit_and_initial_keywords() {
    let (arena, root) = resolved(
        "<html><body><div id=parent><p id=kid>x</p></div></body></html>",
        "#parent { opacity: 0.25; color: rgb(10, 20, 30) } \
         #kid { opacity: inherit; color: initial }",
    );

    let kid_opacity = computed(
        &arena,
        &root,
        "kid",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (kid_opacity - 0.25).abs() < 1e-6,
        "`inherit` on a non-inherited property must take the parent's computed \
         value, got {kid_opacity}"
    );

    let kid = computed(
        &arena,
        &root,
        "kid",
        style::properties::ComputedValues::clone_color,
    );
    let parent = computed(
        &arena,
        &root,
        "parent",
        style::properties::ComputedValues::clone_color,
    );
    assert_ne!(
        channels(&kid),
        channels(&parent),
        "`initial` on an inherited property must not take the parent's value"
    );
}

/// Ported from `css/css-cascade/all-prop-unset-color.html`.
///
/// `all: unset` resets every property, and a later `color` declaration in the same
/// block still wins — order within an origin decides.
#[test]
fn cascade_all_unset_then_a_later_declaration_wins() {
    let (arena, root) = resolved(
        "<html><body><div id=parent><p id=kid>x</p></div></body></html>",
        "#parent { color: rgb(255, 0, 0) } \
         #kid { all: unset; color: rgb(0, 128, 0) }",
    );

    let kid = computed(
        &arena,
        &root,
        "kid",
        style::properties::ComputedValues::clone_color,
    );
    let got = channels(&kid);
    for (channel, want) in got.iter().zip([0.0f32, 128.0 / 255.0, 0.0]) {
        assert!(
            (channel - want).abs() < 1e-6,
            "a declaration after `all: unset` must still apply: got {got:?}"
        );
    }
}

/// Ported from `css/css-cascade/important-prop.html`, plus `css-cascade-3` §6.4.3.
///
/// Specificity decides among declarations of equal importance — id beats class
/// beats type — and `!important` reverses that ordering entirely.
#[test]
fn cascade_specificity_orders_id_class_and_type() {
    let (arena, root) = resolved(
        "<html><body><p id=t class=c>x</p></body></html>",
        "p { opacity: 0.1 } .c { opacity: 0.2 } #t { opacity: 0.3 }",
    );
    let by_id = computed(
        &arena,
        &root,
        "t",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (by_id - 0.3).abs() < 1e-6,
        "an id selector must beat a class and a type selector, got {by_id}"
    );

    let (arena, root) = resolved(
        "<html><body><p id=t class=c>x</p></body></html>",
        "p { opacity: 0.1 !important } .c { opacity: 0.2 } #t { opacity: 0.3 }",
    );
    let important = computed(
        &arena,
        &root,
        "t",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (important - 0.1).abs() < 1e-6,
        "!important on a type selector must beat an id selector without it, got {important}"
    );
}

/// Ported from `css/css-cascade/layer-basic.html`.
///
/// An unlayered declaration beats a layered one at the same origin and
/// specificity, and a later layer beats an earlier one regardless of the order the
/// blocks appear in.
#[test]
fn cascade_layer_order_puts_unlayered_last() {
    let (arena, root) = resolved(
        "<html><body><p id=t>x</p></body></html>",
        "@layer first, second; \
         @layer second { #t { opacity: 0.2 } } \
         @layer first { #t { opacity: 0.1 } } \
         #t { opacity: 0.9 }",
    );
    let unlayered = computed(
        &arena,
        &root,
        "t",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (unlayered - 0.9).abs() < 1e-6,
        "an unlayered declaration must beat any layer at the same origin, got {unlayered}"
    );

    let (arena, root) = resolved(
        "<html><body><p id=t>x</p></body></html>",
        "@layer first, second; \
         @layer second { #t { opacity: 0.2 } } \
         @layer first { #t { opacity: 0.1 } }",
    );
    let later = computed(
        &arena,
        &root,
        "t",
        style::properties::ComputedValues::clone_opacity,
    );
    assert!(
        (later - 0.2).abs() < 1e-6,
        "a later layer must beat an earlier one regardless of source order, got {later}"
    );
}

/// Parse one declaration block the way an inline `style` attribute is parsed.
///
/// Returns how many longhand declarations survived. CSS Syntax requires an invalid
/// declaration to be discarded and its neighbours kept, so "did it parse" is a
/// count rather than a boolean.
fn declarations_in(css: &str) -> usize {
    let url = style::stylesheets::UrlExtraData(style::servo_arc::Arc::new(
        url::Url::parse("https://example.invalid/").expect("a valid literal"),
    ));
    style::properties::declaration_block::parse_style_attribute(
        css,
        &url,
        None,
        style::context::QuirksMode::NoQuirks,
        style::stylesheets::CssRuleType::Style,
    )
    .len()
}

/// Ported from `css/css-cascade/parsing/all-valid.html` and `all-invalid.html`.
///
/// Those are testharness tests too — they load `parsing-testcommon.js` and call
/// `test_valid_value("all", "initial")`, which round-trips through CSSOM — so they
/// cannot run here either. But their assertions are about the *parser* rather than
/// the cascade, which is a different axis worth covering and cheap to port.
///
/// `all` accepts exactly the three CSS-wide keywords. Notably it does **not**
/// accept `revert-layer` in Cascade 3, and it never accepts a property value.
#[test]
fn cascade_all_shorthand_accepts_only_css_wide_keywords() {
    for keyword in ["initial", "inherit", "unset", "revert"] {
        assert!(
            declarations_in(&format!("all: {keyword}")) > 0,
            "`all: {keyword}` is valid and must parse"
        );
    }

    for invalid in ["all: red", "all: 1px", "all: initial initial", "all: "] {
        assert_eq!(
            declarations_in(invalid),
            0,
            "`{invalid}` is invalid and must be discarded"
        );
    }
}

/// An invalid declaration is discarded and its neighbours are kept.
///
/// CSS Syntax §5 requires it, and it is the behaviour that makes a browser usable
/// on pages using features it has not implemented. Worth its own test because the
/// tempting implementation — fail the block — passes every test that only ever
/// feeds it valid CSS.
#[test]
fn cascade_an_invalid_declaration_does_not_take_its_neighbours_with_it() {
    let all_valid = declarations_in("color: red; opacity: 0.5");
    assert_eq!(all_valid, 2, "two valid declarations must both survive");

    let with_garbage = declarations_in("color: red; -px-nonsense: 1; opacity: 0.5");
    assert_eq!(
        with_garbage, 2,
        "an unknown property must be dropped and its neighbours kept"
    );

    let with_bad_value = declarations_in("color: red; opacity: notanumber");
    assert_eq!(
        with_bad_value, 1,
        "a valid property with an invalid value must be dropped, not the block"
    );
}
