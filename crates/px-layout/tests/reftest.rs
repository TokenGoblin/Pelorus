//! Phase 6 gate item 1: the WPT CSS2 reftest subset.
//!
//! ADR 029. A reftest asserts two documents **render identically**, which normally
//! needs paint (Phase 8) and text shaping (Phase 9). Neither exists, and neither
//! is needed: a reftest asserts an *equality*, and an equality survives any
//! consistent transformation of both sides. So both documents are laid out with
//! the same stub text metric and their **box geometry** is compared — flattened
//! in layout order, not compared as tree shape, because a test and its reference
//! deliberately differ in DOM structure where anonymous box generation is the
//! thing under test.
//!
//! The absolute numbers here are meaningless until Phase 9. The equality is not.
//!
//! # Two kinds of failure, two different instruments
//!
//! ADR 029 anticipated pairs the *comparison method* gets wrong — a paint-only
//! difference that passes without the engine doing anything, or a reference that
//! reaches the same pixels through different geometry and so fails while being
//! correct. Those are exclusions, named individually in [`EXPECTED_FAILURES`]
//! with a reason, and enforced in both directions: one that starts passing is
//! stale and fails too.
//!
//! The 80 pairs that do not match today are **neither**. They are features this
//! phase has not built — floats, positioned boxes, margin collapsing, and the
//! anonymous block boxes a `block-in-inline` needs. Writing 80 individual
//! "reasons" for those would be a guess dressed as documentation, because it
//! would be classification by filename rather than by reading each test.
//!
//! So the engine's own progress is held by a pinned **count** instead:
//! [`MATCH_FLOOR`] may not fall, and the corpus size is pinned separately in
//! `ci/gate-layout.sh`. A threshold with a fixed denominator and a fixed
//! numerator cannot be met by deleting tests, which is the failure ADR 019's
//! pattern exists to prevent — this is that pattern applied to a number that is
//! expected to *rise* rather than to an exclusion set expected to shrink.

use std::path::{Path, PathBuf};

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::FragmentTree;
use px_layout::geom::px;

/// The viewport every pair is laid out against.
///
/// WPT's own default. Both sides of a pair get the same one, so the value only
/// matters for tests whose match depends on the viewport — and for those, using
/// WPT's is the least surprising choice.
const VIEWPORT_PX: i32 = 800;

/// Pairs the *comparison method* gets wrong, each with the reason.
///
/// Not a list of unimplemented features — those are counted by [`MATCH_FLOOR`].
/// An entry here says the geometry comparison is the wrong oracle for this pair:
/// ADR 029's two named cases are a paint-only difference, which matches without
/// the engine doing anything, and a reference that reaches the same rendering
/// through deliberately different geometry, which fails while being correct.
///
/// Empty so far. If it grows past a small fraction of the corpus, ADR 029's
/// central bet is wrong and the ADR says so in its Verification section.
const EXPECTED_FAILURES: &[(&str, &str)] = &[];

/// How many pairs must match.
///
/// Measured, not chosen: 25 of 105 with block layout, basic inline layout and no
/// floats, positioned boxes, margin collapsing or anonymous block generation.
///
/// **It may not fall.** Raising it is a deliberate commit whose diff says the
/// engine improved; a fall means a regression, and the fixed denominator means
/// neither can be arranged by editing the corpus. The remaining 80 are the
/// phase's named gaps, and the gate report lists which features they are waiting
/// on.
const MATCH_FLOOR: usize = 25;

/// One test and its reference.
struct Pair {
    name: String,
    test: PathBuf,
    reference: PathBuf,
}

/// Every test/reference pair in the vendored subset.
fn pairs() -> Vec<Pair> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/wpt/css2");
    let mut found = Vec::new();
    let mut stack = vec![root.clone()];

    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.to_string_lossy().replace('\\', "/");
            if !name.ends_with(".html") || name.ends_with("-ref.html") {
                continue;
            }
            let reference = PathBuf::from(name.replace(".html", "-ref.html"));
            if !reference.exists() {
                continue;
            }
            let short = name.rsplit("css2/").next().unwrap_or(&name).to_owned();
            found.push(Pair {
                name: short,
                test: path,
                reference,
            });
        }
    }
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// One fragment's geometry, as compared.
///
/// The fragment *kind* is deliberately excluded. A test's anonymous block and its
/// reference's explicit `<div>` occupy the same space and are the same thing for
/// a reftest's purposes; comparing kinds would fail every test that exercises
/// anonymous box generation, which is much of what CSS2 normal-flow is about.
type Geometry = (usize, Au, Au, Au, Au);

fn geometry(tree: &FragmentTree) -> Vec<Geometry> {
    tree.in_layout_order()
        .into_iter()
        .filter_map(|(depth, id)| {
            let f = tree.get(id)?;
            Some((
                depth,
                f.inline_offset,
                f.block_offset,
                f.size.inline,
                f.size.block,
            ))
        })
        .collect()
}

/// Parse, style and lay out one file.
fn layout_file(path: &Path) -> Option<Vec<Geometry>> {
    let source = std::fs::read_to_string(path).ok()?;
    let dom = px_dom::parse(&source);
    if dom.abandoned {
        return None;
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = px_css::engine::StyleEngine::new(VIEWPORT_PX as f32, 600.0, quirks);
    // A minimal user-agent stylesheet. Without one every element computes
    // `display: inline` and no test lays out at all — WPT files rely on the UA
    // sheet for `div { display: block }` the way every page does.
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");

    // The document's own <style> elements, in order.
    for css in inline_stylesheets(&arena) {
        engine.add_author_stylesheet(&css, "https://wpt.invalid/test.css");
    }

    let style_root = engine.style_root_for(&arena);
    engine.resolve(&arena, &style_root)?;
    let tree = layout_document(&arena, &style_root, px(VIEWPORT_PX))?;
    Some(geometry(&tree))
}

/// Enough of a user-agent stylesheet for block layout.
///
/// Not the real one — that is Phase 8's, and most of it is about paint. These are
/// the `display` values without which nothing in the corpus lays out, plus the
/// margins CSS 2.1's sample sheet gives `body` and `p`, which several tests in the
/// subset depend on.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     table, form, fieldset, pre, hr { display: block } \
     body { margin: 8px } \
     p, blockquote, figure { margin: 1em 0 } \
     h1 { margin: 0.67em 0; font-size: 2em } \
     head, style, script, title, meta, link { display: none }";

/// The text of every `<style>` element in the document, in order.
fn inline_stylesheets(arena: &px_dom::Arena) -> Vec<String> {
    let document = arena.document();
    let mut out = Vec::new();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        let is_style = node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("style"));
        if !is_style {
            continue;
        }
        out.push(px_layout::inline::collect_text(arena, id));
    }
    out
}

/// Lay out both halves of `pair` and say whether they agree.
fn matches(pair: &Pair) -> bool {
    match (layout_file(&pair.test), layout_file(&pair.reference)) {
        (Some(test), Some(reference)) => {
            // Two empty layouts are trivially equal, and that is a false pass
            // rather than a result. ADR 029 names it as the thing the comparison
            // cannot afford.
            !test.is_empty() && test == reference
        }
        _ => false,
    }
}

/// Every pair, with whether it currently matches.
fn results() -> Vec<(String, bool)> {
    pairs()
        .into_iter()
        .map(|pair| {
            let ok = matches(&pair);
            (pair.name, ok)
        })
        .collect()
}

#[test]
fn layout_reftest_the_subset_is_present_and_paired() {
    let found = pairs();
    assert!(
        found.len() >= 40,
        "ci/gate-layout.sh pins the corpus at 40 pairs or more; found {}",
        found.len()
    );
}

/// The corpus matches at or above the pinned floor, and no excluded pair fails
/// for a reason the exclusion does not name.
#[test]
fn layout_reftest_matches_at_or_above_the_pinned_floor() {
    let excluded: std::collections::BTreeSet<&str> =
        EXPECTED_FAILURES.iter().map(|(name, _)| *name).collect();

    let mut passed = 0usize;
    let mut total = 0usize;
    for (name, ok) in results() {
        total += 1;
        if ok && !excluded.contains(name.as_str()) {
            passed += 1;
        }
    }

    // Printed unconditionally, so the graded and ungraded figures are
    // reconcilable from the output rather than only from the gate report — the
    // habit ADR 019 established in Phase 4.
    println!(
        "css2 reftests: {passed}/{total} matched (floor {MATCH_FLOOR}),          {} method exclusions",
        EXPECTED_FAILURES.len()
    );

    assert!(
        total >= 40,
        "the corpus shrank to {total} pairs; ci/gate-layout.sh pins it at 40"
    );
    assert!(
        passed >= MATCH_FLOOR,
        "only {passed} of {total} pairs matched, floor is {MATCH_FLOOR}. A fall is          a regression -- the corpus is pinned, so this cannot be a corpus change."
    );
}

/// An expected failure that starts passing must be taken off the list.
///
/// The other direction, and the one that makes the list mean something. A stale
/// exclusion looks exactly like a test nobody checked.
#[test]
fn layout_reftest_no_expected_failure_has_started_passing() {
    let results: std::collections::BTreeMap<String, bool> = results().into_iter().collect();

    let mut stale = Vec::new();
    for (name, reason) in EXPECTED_FAILURES {
        match results.get(*name) {
            Some(true) => stale.push(format!("{name} (listed as: {reason})")),
            Some(false) => {}
            None => stale.push(format!("{name} is on the list but not in the corpus")),
        }
    }

    assert!(
        stale.is_empty(),
        "these expected failures are stale — they pass now, or are not in the \
         corpus at all:\n  {}",
        stale.join("\n  ")
    );
}
