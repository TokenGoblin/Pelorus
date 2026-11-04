//! Phase 6 gate item 3: iterative tree walks, proven by a deep-nesting test.
//!
//! A hundred thousand nested boxes overflows the stack of any recursive walk, and
//! a layout engine has walks everywhere: box-tree construction, width resolution
//! down, height resolution up, the fragment-order traversal, and the `Drop` the
//! compiler writes. Phase 4 built the same gate item around the same failure in
//! `px-dom`, and its report recorded the lesson that passing it once by fixing one
//! function is not passing it.
//!
//! # Why the thread stack is deliberately small
//!
//! These run on a 256 KB stack rather than the default. A recursive layout would
//! survive 100,000 levels on a main thread with a large stack on some platforms
//! and not others, which would make this test pass on Linux and fail on Windows —
//! or worse, pass everywhere until a page nested slightly deeper. Constraining
//! the stack makes the property being tested independent of the platform's
//! default, which is the same thing `px-dom`'s depth tests do.

use px_layout::block::layout_document;
use px_layout::fragment::{Fragment, FragmentKind, FragmentTree};
use px_layout::geom::{LogicalSize, px};

/// The stack these tests run on.
///
/// Small enough that recursion over the tree depth fails, large enough that the
/// iterative implementation and the parser beneath it do not.
const STACK: usize = 256 * 1024;

/// Run `body` on a thread with a deliberately small stack.
fn on_a_small_stack<F>(name: &str, body: F)
where
    F: FnOnce() + Send + 'static,
{
    let handle = std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(STACK)
        .spawn(body)
        .expect("the test thread spawns");
    handle
        .join()
        .unwrap_or_else(|_| panic!("{name} overflowed its stack or panicked"));
}

/// `<div>` nested `depth` times, innermost first.
fn nested_divs(depth: usize) -> String {
    let mut html = String::with_capacity(depth * 11 + 40);
    html.push_str("<html><body>");
    for _ in 0..depth {
        html.push_str("<div>");
    }
    for _ in 0..depth {
        html.push_str("</div>");
    }
    html.push_str("</body></html>");
    html
}

#[test]
fn layout_depth_lays_out_deeply_nested_boxes_without_overflowing() {
    on_a_small_stack("layout-depth", || {
        // px-dom refuses to nest past `px_dom::MAX_DEPTH` (512), so a document
        // cannot actually reach ten thousand levels no matter what the markup
        // says — the tree builder stops attaching and the surplus divs become
        // siblings. Asking for far more than the limit is the point: layout must
        // handle whatever the DOM will hand it, and must not add a *lower* limit
        // of its own.
        //
        // The first version of this test asserted more than a thousand fragments
        // and failed, because 512 is the most there can be. The assertion was
        // wrong, not the engine.
        let html = nested_divs(10_000);
        let dom = px_dom::parse(&html);
        let quirks = px_css::engine::quirks_mode_of(&dom);
        let arena = dom.arena;

        let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
        engine.add_author_stylesheet(
            "html, body, div { display: block; margin: 0; padding: 0 }",
            "https://example.invalid/a.css",
        );
        let style_root = engine.style_root_for(&arena);
        engine
            .resolve(&arena, &style_root)
            .expect("the document has a root element");

        let tree = layout_document(&arena, &style_root, px(800)).expect("a root element");
        assert!(
            tree.len() >= px_dom::MAX_DEPTH,
            "layout must reach px-dom's own depth limit of {}, got {} fragments",
            px_dom::MAX_DEPTH,
            tree.len()
        );

        // And the tree really is deep, not merely large: a bug that flattened
        // everything into siblings would satisfy the count above.
        let deepest = tree
            .in_layout_order()
            .into_iter()
            .map(|(depth, _)| depth)
            .max()
            .unwrap_or(0);
        assert!(
            deepest >= px_dom::MAX_DEPTH - 2,
            "the fragment tree must be as deep as the DOM, deepest was {deepest}"
        );
    });
}

#[test]
fn layout_depth_walks_a_deep_fragment_tree_without_overflowing() {
    on_a_small_stack("layout-depth-walk", || {
        const DEPTH: usize = 100_000;
        let mut tree = FragmentTree::new();
        let mut chain = Vec::with_capacity(DEPTH);
        for _ in 0..DEPTH {
            chain.push(tree.push(Fragment::new(
                FragmentKind::Block,
                LogicalSize::new(px(10), px(10)),
            )));
        }
        if let Some(first) = chain.first() {
            tree.set_root(*first);
        }
        for window in chain.windows(2) {
            tree.set_children(window[0], &[window[1]]);
        }

        let order = tree.in_layout_order();
        assert_eq!(order.len(), DEPTH);
        assert_eq!(order.last().map(|(depth, _)| *depth), Some(DEPTH - 1));
    });
}

#[test]
fn layout_depth_drops_a_deep_fragment_tree_without_overflowing() {
    on_a_small_stack("layout-depth-drop", || {
        let mut tree = FragmentTree::new();
        let mut previous = None;
        for _ in 0..100_000 {
            let id = tree.push(Fragment::new(
                FragmentKind::Block,
                LogicalSize::new(px(10), px(10)),
            ));
            if let Some(parent) = previous {
                tree.set_children(parent, &[id]);
            }
            previous = Some(id);
        }
        drop(tree);
    });
}

/// The structural guarantee behind the drop test, asserted by the compiler.
///
/// A `Fragment` that owned its children could not be `Copy`, so this is the type
/// system refusing the shape that produces a recursive `Drop`. Two attempts at
/// catching that with a source scan in `ci/gate-layout.sh` both fired on correct
/// code; this cannot, because it is not a heuristic.
///
/// It lives in gate item 3's own test file so that removing the guarantee fails
/// the item it belongs to, rather than a scan somebody could silence.
#[test]
fn layout_depth_fragments_are_copy_so_drop_cannot_recurse() {
    fn assert_copy<T: Copy>() {}
    assert_copy::<Fragment>();
}
