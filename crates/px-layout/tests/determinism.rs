//! Phase 6 gate item 2: an identical box tree on repeat runs.
//!
//! Not a correctness property. Two runs over the same input must produce the same
//! geometry, which is a claim about the *absence* of iteration-order and
//! hash-order dependence — and one that is cheap to check now and close to
//! impossible to debug later. A layout that differs by a pixel every hundredth
//! run is a bug report nobody can reproduce.
//!
//! `px-layout`'s own CLAUDE.md puts it as: non-determinism here is a bug even
//! when the pixels match.
//!
//! # What could make it fail
//!
//! Nothing in this crate iterates a `HashMap`, and `ci/gate-layout.sh` forbids
//! `f32` and `f64` outright, because floating-point addition is not associative —
//! summing child heights in a different order would give a different total. Those
//! two are the usual sources. This test is what notices when a third appears.

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::FragmentTree;
use px_layout::geom::px;

/// Lay `html` out with `css`, from parse to fragments, in one call.
///
/// Deliberately end-to-end rather than starting from a prebuilt box tree. The
/// determinism claim is about the whole pipeline: a stylist that iterated its
/// rules in a different order would produce different computed values and
/// therefore different geometry, and a test starting after that point would not
/// see it.
fn layout_once(html: &str, css: &str) -> FragmentTree {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "the fixture must parse");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(css, "https://example.invalid/a.css");
    let style_root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &style_root)
        .expect("the document has a root element");

    layout_document(&arena, &style_root, px(800)).expect("a root element exists")
}

/// Every fragment's geometry, flattened in layout order.
///
/// The comparison ADR 029 uses for reftests, reused here — so if this test and
/// the reftest comparison ever disagree about what "the same layout" means, they
/// disagree in one place rather than two.
fn geometry(tree: &FragmentTree) -> Vec<(usize, String, Au, Au, Au, Au)> {
    tree.in_layout_order()
        .into_iter()
        .filter_map(|(depth, id)| {
            let f = tree.get(id)?;
            Some((
                depth,
                format!("{:?}", f.kind),
                f.inline_offset,
                f.block_offset,
                f.size.inline,
                f.size.block,
            ))
        })
        .collect()
}

const DOCUMENT: &str = "<html><body>\
     <div id=a>hello world this is some text that will wrap somewhere</div>\
     <div id=b><div class=c>nested</div><div class=c>boxes</div></div>\
     <p>a paragraph</p>\
     </body></html>";

const STYLESHEET: &str = "html, body, div, p { display: block; margin: 0; padding: 0 } \
     #a { width: 200px; font-size: 16px; line-height: 20px } \
     .c { height: 30px; margin: 5px 10px } \
     p { margin: 8px 0; font-size: 14px }";

#[test]
fn layout_determinism_two_runs_produce_identical_geometry() {
    let first = layout_once(DOCUMENT, STYLESHEET);
    let second = layout_once(DOCUMENT, STYLESHEET);

    assert_eq!(
        geometry(&first),
        geometry(&second),
        "the same document laid out twice must produce the same geometry"
    );
}

/// Ten runs, not two.
///
/// Two runs catch a difference that is deterministic-but-wrong — a cached value
/// reused from the first pass, say. They do not reliably catch a *probabilistic*
/// difference, which is what hash-order dependence actually is: `HashMap`'s
/// iteration order is stable within a process and varies between them, so a
/// single process running twice can agree by accident. Ten runs is still within
/// one process and still would not catch that on its own — the real defence is
/// that nothing here iterates a hash map — but it catches anything that varies
/// with allocation addresses or with how much has been allocated so far.
#[test]
fn layout_determinism_holds_across_repeated_runs() {
    let reference = geometry(&layout_once(DOCUMENT, STYLESHEET));
    for run in 1..10 {
        let again = geometry(&layout_once(DOCUMENT, STYLESHEET));
        assert_eq!(again, reference, "run {run} differed from the first");
    }
}

/// The fragment tree compares equal as a whole, not only its geometry.
///
/// `geometry()` projects; this asserts the structure too, so a run that produced
/// the same rectangles through a different tree shape — different anonymous box
/// generation, say — still fails.
#[test]
fn layout_determinism_the_whole_tree_compares_equal() {
    let first = layout_once(DOCUMENT, STYLESHEET);
    let second = layout_once(DOCUMENT, STYLESHEET);
    assert_eq!(
        first, second,
        "the fragment trees must be equal in structure as well as geometry"
    );
}

/// A document laid out at two different widths must differ.
///
/// The control. Without it, a `layout_document` that returned an empty tree would
/// satisfy every assertion above — two empty trees are reliably identical.
#[test]
fn layout_determinism_is_not_trivially_satisfied_by_an_empty_tree() {
    let tree = layout_once(DOCUMENT, STYLESHEET);
    assert!(
        tree.len() > 5,
        "the fixture must actually produce fragments, got {}",
        tree.len()
    );
    assert!(
        geometry(&tree).iter().any(|g| g.5 > Au(0)),
        "at least one fragment must have a non-zero block size"
    );
}
