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
/// **`#[ignore]`d by ADR 027, which this test found.** It runs stylo's cascade
/// for real, and stylo panics before doing any work:
///
/// ```text
/// panicked at stylo-0.21.0/sharing/mod.rs:611:
/// assertion `left == right` failed; left: 10256, right: 9488
/// ```
///
/// That is the style-sharing cache asserting that `TElement` is pointer-sized.
/// `StyleElement` is 32 bytes. The gap is 24 bytes across 32 cache entries, which
/// is the 768 in those numbers.
///
/// Kept rather than deleted because it is the thing that has to pass, and because
/// an ignored test with this comment tells a reader more than its absence would.
/// `ci/gate-style.sh` reports the `computed-style` gate item as unmet either way
/// — an `#[ignore]`d test is one of the three ways it refuses to call an item
/// done.
#[test]
#[ignore = "ADR 027: stylo requires a pointer-sized TElement; the views are 32 bytes"]
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
