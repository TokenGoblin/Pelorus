//! Phase 5 gate item 1: computed-style fixtures across the defined property set.
//!
//! The property set is `crates/px-css/tests/properties.toml`, and
//! `ci/gate-style.sh` holds it to a pinned floor so it cannot shrink to fit an
//! implementation that is struggling.
//!
//! Every test here goes through the *product* path — parse HTML with `px-dom`,
//! add a stylesheet, run `StyleEngine::resolve`, read the computed value back.
//! Phase 1's most embarrassing finding was a binary that exited failure while
//! every unit test passed, because nothing ran the thing itself. A fixture that
//! called stylo's cascade directly would repeat that in miniature.

use px_css::engine::StyleEngine;
use px_css::view::StyleRoot;

/// Parse `html`, apply `css`, resolve, and hand back everything the caller needs
/// to inspect computed values.
fn style(html: &str, css: &str) -> (px_dom::Arena, StyleEngine, StyleRoot, usize) {
    let dom = px_dom::parse(html);
    assert!(
        !dom.abandoned,
        "the fixture must parse without being abandoned"
    );
    // The parser's quirks mode, not a hardcoded NoQuirks: see `quirks_mode_of`.
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(css, "https://example.invalid/fixture.css");
    let root = StyleRoot::new(&arena, engine.shared_lock().clone(), quirks);
    let styled = engine
        .resolve(&arena, &root)
        .expect("the document has a root");
    (arena, engine, root, styled)
}

/// Every element gets computed style.
///
/// **This is the test that found ADR 027.** It runs stylo's cascade for real, and
/// the first time it ran stylo panicked before doing any work, in its
/// style-sharing cache, because `TElement` was 32 bytes where a `usize` was
/// required. It passes now.
#[test]
fn computed_style_resolves_for_every_element() {
    let (_arena, _engine, _root, styled) = style(
        "<html><head></head><body><div><p>text</p></div></body></html>",
        "div { color: red }",
    );
    // html, head, body, div, p.
    assert_eq!(
        styled, 5,
        "every element must get computed style, not just the ones a selector matched"
    );
}

/// The cascade actually applies, and the computed value is the one the
/// stylesheet asked for.
///
/// The test above proves every element was *visited*. This proves the visit did
/// something: a declaration in an author stylesheet reaches a computed value that
/// can be read back. Those are different claims, and only the second one says the
/// cascade works — a traversal that walked the tree and computed initial values
/// everywhere would pass the first.
#[test]
fn computed_color_comes_from_the_author_stylesheet() {
    use px_css::view::with_dom;
    use style::dom::TNode as _;

    let dom = px_dom::parse("<html><body><p id=x>hello</p></body></html>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(
        "#x { color: rgb(1, 2, 3) }",
        "https://example.invalid/a.css",
    );
    let root = StyleRoot::new(&arena, engine.shared_lock().clone(), quirks);
    engine
        .resolve(&arena, &root)
        .expect("the document has a root");

    // Find the <p> and read its computed colour back.
    let found = with_dom(&arena, &root, |d| {
        let mut stack = vec![d.node(arena.document()).expect("document resolves")];
        while let Some(node) = stack.pop() {
            if let Some(element) = px_css::dom::StyleElement::new(node)
                && *style::dom::TElement::local_name(&element) == html5ever::local_name!("p")
            {
                let data = style::dom::TElement::borrow_data(&element).expect("the <p> was styled");
                let color = data.styles.primary().clone_color();
                // stylo keeps colour components normalised to 0..=1, not as the
                // 0..=255 the `rgb()` notation is written in. Comparing against
                // (1.0, 2.0, 3.0) is the obvious mistake and it is what the first
                // version of this test did.
                return Some([color.components.0, color.components.1, color.components.2]);
            }
            let mut child = node.first_child();
            while let Some(c) = child {
                stack.push(c);
                child = c.next_sibling();
            }
        }
        None
    })
    .expect("the document resolves");

    let found = found.expect("a <p> must be present and styled");
    for (channel, expected) in found.iter().zip([1.0f32, 2.0, 3.0]) {
        assert!(
            (channel - expected / 255.0).abs() < 1e-6,
            "the <p> must compute the colour its id selector declared:              got {found:?}, wanted rgb(1, 2, 3) normalised to 0..=1"
        );
    }
}
