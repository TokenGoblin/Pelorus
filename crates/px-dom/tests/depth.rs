//! Depth, and the stack.
//!
//! §9 Phase 4's gate item is *"100,000-level nesting handled without stack
//! overflow"*. "Handled" is the operative word: build-spec §4.4 caps HTML
//! nesting at 512, so a hundred thousand levels of input is not meant to
//! produce a hundred-thousand-level tree. It is meant to produce a clean
//! refusal and a live process.
//!
//! Both halves are tested here, because either alone is misleading. A refusal
//! at 512 proves nothing about the stack if nothing ever gets deep, and a deep
//! walk proves nothing about the limit.
//!
//! The stack tests run on a thread with a deliberately small stack. On the
//! main thread with eight megabytes, a walk that recursed once per node would
//! survive a hundred thousand nodes and the test would pass while measuring
//! nothing.

use px_dom::{Arena, MAX_DEPTH, NodeData, TreeError};

/// Small enough that anything recursing per node dies, comfortable for
/// anything iterative. A recursive walk over 100,000 nodes needs megabytes;
/// an iterative one needs a few hundred bytes of frame plus a heap vector.
const SMALL_STACK: usize = 256 * 1024;

const MANY: usize = 100_000;

// `unreachable!` rather than `expect` in these helpers.
//
// The workspace denies `expect_used`, and clippy's `allow-expect-in-tests`
// exemption only reaches code inside a `#[test]` function -- a helper called
// *by* tests is ordinary code as far as the lint is concerned. That is a
// reasonable place for clippy to draw the line and there is no reason to widen
// clippy.toml for it: these really are unreachable. A freshly created arena
// cannot be exhausted, and a node built two lines above is not stale.
fn element(arena: &mut Arena, name: &str) -> px_dom::NodeId {
    let _ = name;
    match arena.create(NodeData::Element {
        name: html5ever::QualName::new(
            None,
            html5ever::ns!(html),
            html5ever::LocalName::from("div"),
        ),
        attrs: Vec::new(),
        template_contents: None,
        script_already_started: false,
    }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

/// Run `body` on a thread with a small stack, and fail if it overflows.
fn on_a_small_stack<F: FnOnce() + Send + 'static>(body: F) {
    let handle = match std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(body)
    {
        Ok(handle) => handle,
        Err(error) => unreachable!("could not spawn a test thread: {error}"),
    };
    // A stack overflow aborts the process rather than unwinding, so this join
    // failing means an ordinary panic. Either way the test fails; the abort
    // case just fails louder.
    assert!(
        handle.join().is_ok(),
        "the small-stack thread overflowed or panicked"
    );
}

/// A hundred thousand levels of nesting is refused, not crashed on.
#[test]
fn dom_depth_refuses_nesting_past_the_limit() {
    on_a_small_stack(|| {
        let mut arena = Arena::new();
        let mut parent = arena.document();
        let mut accepted = 0usize;
        let mut refusals = 0usize;

        for _ in 0..MANY {
            let child = element(&mut arena, "div");
            match arena.append_child(parent, child) {
                Ok(()) => {
                    accepted += 1;
                    parent = child;
                }
                Err(TreeError::TooDeep) => {
                    refusals += 1;
                    // The node stays allocated and unattached, which is what
                    // the parser will do with it: drop the content, keep the
                    // document, carry on.
                }
                Err(other) => panic!("unexpected refusal: {other:?}"),
            }
        }

        assert_eq!(
            accepted, MAX_DEPTH,
            "nesting should be accepted up to the limit and no further"
        );
        assert_eq!(refusals, MANY - MAX_DEPTH);
        assert!(
            arena.len() >= MANY,
            "refused nodes stay allocated; the caller decides what to do with them"
        );
    });
}

/// The limit is enforced on every path that can deepen the tree, not only on
/// `append_child`.
///
/// `insert_before` is the one that would be missed: it looks like a sibling
/// operation and is therefore easy to write as though it could not change
/// anybody's depth.
#[test]
fn dom_depth_limit_holds_for_insert_before_and_reparenting() {
    let mut arena = Arena::new();
    let mut parent = arena.document();

    let mut deepest = parent;
    for _ in 0..MAX_DEPTH {
        let child = element(&mut arena, "div");
        arena.append_child(parent, child).expect("within the limit");
        parent = child;
        deepest = child;
    }
    assert_eq!(arena.depth(deepest), Ok(MAX_DEPTH));

    // One more child of the deepest node is one too many.
    let one_too_far = element(&mut arena, "div");
    assert_eq!(
        arena.append_child(deepest, one_too_far),
        Err(TreeError::TooDeep)
    );

    // A sibling of the deepest node is fine: same depth, not deeper.
    let sibling = element(&mut arena, "div");
    assert_eq!(arena.insert_before(deepest, sibling), Ok(()));

    // Moving a subtree under the deepest node must be refused too, or the
    // depth limit is a fence with a gate in it.
    let subtree_root = element(&mut arena, "div");
    arena
        .append_child(arena.document(), subtree_root)
        .expect("shallow");
    assert_eq!(
        arena.append_child(deepest, subtree_root),
        Err(TreeError::TooDeep)
    );
}

/// The limit counts the deepest node in the subtree being moved, not just the
/// node named in the call.
///
/// This is the bypass a depth check gets wrong by default. Build the deep part
/// *detached*, where every single insertion is shallow and legal, then attach
/// its root somewhere deep in one move that also looks legal. If the check
/// only asks where the root lands, 512 becomes a suggestion.
#[test]
fn dom_depth_limit_counts_the_whole_subtree_being_moved() {
    let mut arena = Arena::new();
    let doc = arena.document();

    // A detached ladder, 20 deep. Nothing here approaches the limit.
    let ladder_root = element(&mut arena, "div");
    let mut rung = ladder_root;
    for _ in 0..20 {
        let next = element(&mut arena, "div");
        arena.append_child(rung, next).expect("shallow and legal");
        rung = next;
    }

    // A spine ending well inside the limit.
    let mut spine = doc;
    for _ in 0..MAX_DEPTH - 10 {
        let child = element(&mut arena, "div");
        arena.append_child(spine, child).expect("within the limit");
        spine = child;
    }
    assert_eq!(arena.depth(spine), Ok(MAX_DEPTH - 10));

    // The ladder's root would land at MAX_DEPTH - 9, comfortably legal. Its
    // deepest rung would land at MAX_DEPTH + 11, which is not.
    assert_eq!(
        arena.append_child(spine, ladder_root),
        Err(TreeError::TooDeep),
        "the move was judged by where the root lands, not by where the \
         subtree's deepest node lands"
    );

    // And the same subtree still attaches where it genuinely fits.
    assert_eq!(arena.append_child(doc, ladder_root), Ok(()));
    assert_eq!(arena.depth(rung), Ok(21));
}

/// A hundred thousand nodes, dropped on a small stack.
///
/// This is the one that catches the `Drop` nobody writes. If a node owned its
/// children, dropping the arena would recurse once per node and this thread
/// would die.
#[test]
fn dom_depth_drops_a_large_tree_without_recursing() {
    on_a_small_stack(|| {
        let mut arena = Arena::new();
        let doc = arena.document();

        // Wide and deep at once: a spine to the depth limit, with the
        // remaining nodes hung off its end.
        let mut parent = doc;
        for _ in 0..MAX_DEPTH - 1 {
            let child = element(&mut arena, "div");
            arena.append_child(parent, child).expect("within the limit");
            parent = child;
        }
        for _ in 0..MANY {
            let child = element(&mut arena, "span");
            arena.append_child(parent, child).expect("wide, not deep");
        }
        assert!(arena.len() > MANY);

        drop(arena);
    });
}

/// The same tree, removed through the API rather than dropped.
///
/// `remove_subtree` walks the whole subtree and is the obvious place for an
/// accidental recursion, because the recursive version is four lines and reads
/// perfectly.
#[test]
fn dom_depth_removes_a_large_subtree_without_recursing() {
    on_a_small_stack(|| {
        let mut arena = Arena::new();
        let doc = arena.document();

        let root = element(&mut arena, "div");
        arena.append_child(doc, root).expect("append");

        let mut parent = root;
        for _ in 0..MAX_DEPTH - 2 {
            let child = element(&mut arena, "div");
            arena.append_child(parent, child).expect("within the limit");
            parent = child;
        }
        for _ in 0..MANY {
            let child = element(&mut arena, "span");
            arena.append_child(parent, child).expect("wide, not deep");
        }

        let before = arena.len();
        let freed = arena.remove_subtree(root).expect("remove");
        assert_eq!(freed, before - 1, "everything but the document");
        assert_eq!(arena.len(), 1);
    });
}

/// Every traversal survives the same tree on the same small stack.
#[test]
fn dom_depth_traversals_do_not_recurse() {
    on_a_small_stack(|| {
        let mut arena = Arena::new();
        let doc = arena.document();

        let mut parent = doc;
        for _ in 0..MAX_DEPTH - 1 {
            let child = element(&mut arena, "div");
            arena.append_child(parent, child).expect("within the limit");
            parent = child;
        }
        let deepest = parent;
        for _ in 0..MANY {
            let child = element(&mut arena, "span");
            arena.append_child(parent, child).expect("wide, not deep");
        }

        assert_eq!(arena.descendants(doc).count(), arena.len());
        assert_eq!(arena.child_ids(deepest).count(), MANY);
        assert_eq!(arena.ancestors(deepest).count(), MAX_DEPTH - 1);
    });
}
