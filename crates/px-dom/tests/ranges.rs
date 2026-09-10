//! Ranges, the fourth thing §9 Phase 4 names.
//!
//! The gate's four items do not cover ranges, so these tests are the only
//! thing that does. That is worth saying out loud: a deliverable named in the
//! phase and absent from its gate is one that can be skipped without anything
//! going red.
//!
//! Most of what follows is about **liveness** — what a range does when the
//! tree moves underneath it. That is where ranges are actually wrong in
//! practice, and the failure is invisible: a selection that quietly comes to
//! cover different text than the user highlighted looks like nothing at all
//! until somebody copies it.

use px_dom::{Arena, BoundaryPoint, NodeData, NodeId, Position, TreeError};

fn text(arena: &mut Arena, s: &str) -> NodeId {
    match arena.create(NodeData::Text { contents: s.into() }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

fn link(arena: &mut Arena, parent: NodeId, child: NodeId) {
    if let Err(error) = arena.append_child(parent, child) {
        unreachable!("appending a fresh node cannot fail here: {error:?}");
    }
}

/// `document` with children `a b c`, each a text node.
fn three_children() -> (Arena, NodeId, [NodeId; 3]) {
    let mut arena = Arena::new();
    let root = arena.document();
    let a = text(&mut arena, "aaa");
    let b = text(&mut arena, "bbb");
    let c = text(&mut arena, "ccc");
    link(&mut arena, root, a);
    link(&mut arena, root, b);
    link(&mut arena, root, c);
    (arena, root, [a, b, c])
}

/// A node's length is children for an element, bytes for character data.
///
/// Two different meanings for the same field is the thing about boundary
/// points that catches people, so it is asserted rather than assumed.
#[test]
fn range_boundary_validity_depends_on_what_the_node_is() {
    let (arena, root, [a, _, _]) = three_children();

    // The root has three children, so offsets 0..=3 are valid.
    for offset in 0..=3 {
        assert!(arena.is_valid_boundary(BoundaryPoint::new(root, offset)));
    }
    assert!(!arena.is_valid_boundary(BoundaryPoint::new(root, 4)));

    // "aaa" is three bytes, so offsets 0..=3 are valid there too — for an
    // entirely different reason.
    for offset in 0..=3 {
        assert!(arena.is_valid_boundary(BoundaryPoint::new(a, offset)));
    }
    assert!(!arena.is_valid_boundary(BoundaryPoint::new(a, 4)));
}

/// A range refuses invalid boundary points and inverted ends.
#[test]
fn range_creation_refuses_nonsense() {
    let (mut arena, root, [a, b, _]) = three_children();

    assert_eq!(
        arena.new_range(BoundaryPoint::new(root, 9), BoundaryPoint::new(root, 9)),
        Err(TreeError::NoSuchNode),
        "an offset past the end of the node"
    );

    assert_eq!(
        arena.new_range(BoundaryPoint::new(b, 0), BoundaryPoint::new(a, 0)),
        Err(TreeError::WouldCycle),
        "the DOM normalises an inverted range by collapsing it; doing that \
         silently here would hide a caller bug behind a range covering nothing"
    );

    // A stale handle is refused too.
    let doomed = text(&mut arena, "x");
    link(&mut arena, root, doomed);
    let point = BoundaryPoint::new(doomed, 0);
    arena.remove_subtree(doomed).expect("remove");
    assert_eq!(arena.new_range(point, point), Err(TreeError::NoSuchNode));
}

/// Boundary point comparison is the DOM's, including the ancestor case.
#[test]
fn range_boundary_points_compare_in_document_order() {
    let (arena, root, [a, b, c]) = three_children();

    let at = |node, offset| BoundaryPoint::new(node, offset);
    let cmp = |x, y| arena.compare_boundaries(x, y);

    assert_eq!(cmp(at(a, 0), at(a, 0)), Some(Position::Equal));
    assert_eq!(cmp(at(a, 0), at(a, 1)), Some(Position::Before));
    assert_eq!(cmp(at(a, 1), at(a, 0)), Some(Position::After));

    assert_eq!(cmp(at(a, 0), at(b, 0)), Some(Position::Before));
    assert_eq!(cmp(at(c, 0), at(b, 0)), Some(Position::After));

    // The ancestor case, which is the one with an algorithm rather than an
    // obvious answer: (root, 1) sits between a and b, so it is after any
    // point inside a and before any point inside b.
    assert_eq!(cmp(at(root, 1), at(a, 3)), Some(Position::After));
    assert_eq!(cmp(at(root, 1), at(b, 0)), Some(Position::Before));
    assert_eq!(cmp(at(a, 3), at(root, 1)), Some(Position::Before));

    // And it is symmetric, which the mirrored branch is there to guarantee.
    for x in [
        at(a, 0),
        at(a, 3),
        at(b, 1),
        at(c, 2),
        at(root, 0),
        at(root, 3),
    ] {
        for y in [
            at(a, 0),
            at(a, 3),
            at(b, 1),
            at(c, 2),
            at(root, 0),
            at(root, 3),
        ] {
            let forward = cmp(x, y);
            let backward = cmp(y, x);
            let expected = match forward {
                Some(Position::Before) => Some(Position::After),
                Some(Position::After) => Some(Position::Before),
                other => other,
            };
            assert_eq!(backward, expected, "asymmetry between {x:?} and {y:?}");
        }
    }
}

/// Comparing against a stale handle has no answer, and specifically is not
/// `Equal`.
#[test]
fn range_comparison_with_a_stale_handle_is_none_not_equal() {
    let (mut arena, _root, [a, b, _]) = three_children();
    arena.remove_subtree(a).expect("remove");

    assert_eq!(
        arena.compare_boundaries(BoundaryPoint::new(a, 0), BoundaryPoint::new(b, 0)),
        None,
        "\"cannot be compared\" and \"is the same place\" are different answers"
    );
}

// ---------------------------------------------------------------------------
// Liveness: what happens when the tree moves
// ---------------------------------------------------------------------------

/// Inserting before a boundary point shifts it along.
///
/// <https://dom.spec.whatwg.org/#concept-node-insert>
#[test]
fn range_boundary_shifts_when_a_node_is_inserted_before_it() {
    let (mut arena, root, [_, _, _]) = three_children();

    // A range covering the last two children: (root, 1) to (root, 3).
    let id = arena
        .new_range(BoundaryPoint::new(root, 1), BoundaryPoint::new(root, 3))
        .expect("valid range");

    // Insert at the front. Both offsets must move.
    let fresh = text(&mut arena, "zzz");
    let first = arena.child_ids(root).next().expect("a first child");
    arena.insert_before(first, fresh).expect("insert");

    let range = arena.range(id).expect("the range still exists");
    assert_eq!(range.start.offset(), 2);
    assert_eq!(range.end.offset(), 4);
}

/// Inserting *after* a boundary point leaves it alone.
#[test]
fn range_boundary_does_not_move_when_insertion_is_after_it() {
    let (mut arena, root, [_, _, _]) = three_children();
    let id = arena
        .new_range(BoundaryPoint::new(root, 0), BoundaryPoint::new(root, 1))
        .expect("valid range");

    let fresh = text(&mut arena, "zzz");
    link(&mut arena, root, fresh); // appended last

    let range = arena.range(id).expect("still there");
    assert_eq!(range.start.offset(), 0);
    assert_eq!(range.end.offset(), 1);
}

/// Removing a node before a boundary point shifts it down.
#[test]
fn range_boundary_shifts_down_when_an_earlier_node_is_removed() {
    let (mut arena, root, [a, _, _]) = three_children();
    let id = arena
        .new_range(BoundaryPoint::new(root, 1), BoundaryPoint::new(root, 3))
        .expect("valid range");

    arena.remove_subtree(a).expect("remove the first child");

    let range = arena.range(id).expect("still there");
    assert_eq!(range.start.offset(), 0);
    assert_eq!(range.end.offset(), 2);
}

/// A boundary point *inside* a removed subtree collapses to where that subtree
/// used to be — it does not merely go stale.
///
/// This is the rule that separates "the handle stopped resolving" from "the
/// range is meaningless", and the one a naive implementation gets wrong by
/// treating a removed node as simply gone.
#[test]
fn range_boundary_inside_a_removed_subtree_moves_to_the_removal_site() {
    let mut arena = Arena::new();
    let root = arena.document();
    let before = text(&mut arena, "before");
    let container = match arena.create(NodeData::Fragment) {
        Ok(id) => id,
        Err(error) => unreachable!("{error:?}"),
    };
    let inner = text(&mut arena, "inner");
    link(&mut arena, root, before);
    link(&mut arena, root, container);
    link(&mut arena, container, inner);

    // A range wholly inside the container.
    let id = arena
        .new_range(BoundaryPoint::new(inner, 1), BoundaryPoint::new(inner, 4))
        .expect("valid range");

    arena.remove_subtree(container).expect("remove");

    let range = arena.range(id).expect("the range outlives its nodes");
    assert_eq!(
        range.start,
        BoundaryPoint::new(root, 1),
        "the start must collapse to where the container was, not vanish"
    );
    assert_eq!(range.end, BoundaryPoint::new(root, 1));
    assert!(range.is_collapsed(), "and the range is now empty");

    // The handle it used to name is genuinely stale, which is a separate fact.
    assert!(arena.get(inner).is_none());
}

/// A range straddling the removal — one point inside, one outside — collapses
/// on one side only.
#[test]
fn range_straddling_a_removal_collapses_on_one_side_only() {
    let mut arena = Arena::new();
    let root = arena.document();
    let container = match arena.create(NodeData::Fragment) {
        Ok(id) => id,
        Err(error) => unreachable!("{error:?}"),
    };
    let inner = text(&mut arena, "inner");
    let after = text(&mut arena, "after");
    link(&mut arena, root, container);
    link(&mut arena, container, inner);
    link(&mut arena, root, after);

    // Start inside the container, end in the following sibling.
    let id = arena
        .new_range(BoundaryPoint::new(inner, 2), BoundaryPoint::new(after, 3))
        .expect("valid range");

    arena.remove_subtree(container).expect("remove");

    let range = arena.range(id).expect("still there");
    assert_eq!(
        range.start,
        BoundaryPoint::new(root, 0),
        "the inside point collapses to the removal site"
    );
    assert_eq!(
        range.end,
        BoundaryPoint::new(after, 3),
        "the outside point is untouched -- it was never in the removed subtree"
    );
    assert!(!range.is_collapsed());
}

/// A move — detach and re-append — applies both rules in order.
#[test]
fn range_survives_a_node_being_moved() {
    let (mut arena, root, [a, b, c]) = three_children();
    let id = arena
        .new_range(BoundaryPoint::new(root, 2), BoundaryPoint::new(root, 3))
        .expect("valid range");

    // Move the first child to the end: a b c -> b c a.
    arena.append_child(root, a).expect("re-append");

    let order: Vec<NodeId> = arena.child_ids(root).collect();
    assert_eq!(order, vec![b, c, a]);

    // The removal shifted both points down by one; the re-insertion at the end
    // did not move them, because it landed after them.
    let range = arena.range(id).expect("still there");
    assert_eq!(range.start.offset(), 1);
    assert_eq!(range.end.offset(), 2);
}

/// Range handles are generational, like node handles.
#[test]
fn range_handles_go_stale_and_do_not_resolve_to_their_replacement() {
    let (mut arena, root, [_, _, _]) = three_children();

    let first = arena
        .new_range(BoundaryPoint::new(root, 0), BoundaryPoint::new(root, 1))
        .expect("valid");
    assert_eq!(arena.range_count(), 1);

    assert!(arena.drop_range(first));
    assert!(arena.range(first).is_none());
    assert!(
        !arena.drop_range(first),
        "dropping twice must not double-free"
    );
    assert_eq!(arena.range_count(), 0);

    // The slot is reused; the old handle must not name the new occupant.
    let second = arena
        .new_range(BoundaryPoint::new(root, 2), BoundaryPoint::new(root, 3))
        .expect("valid");
    assert_eq!(
        first.index(),
        second.index(),
        "the test needs the slot to be reused to mean anything"
    );
    assert!(
        arena.range(first).is_none(),
        "a stale RangeId resolved to somebody else's selection"
    );
    assert!(arena.range(second).is_some());
}

/// Containment, which is what a selection actually asks.
#[test]
fn range_contains_reports_what_lies_between_the_boundary_points() {
    let (mut arena, root, [a, b, c]) = three_children();
    let id = arena
        .new_range(BoundaryPoint::new(root, 1), BoundaryPoint::new(root, 2))
        .expect("valid range");

    assert_eq!(arena.range_contains(id, a), Some(false));
    assert_eq!(arena.range_contains(id, b), Some(true));
    assert_eq!(arena.range_contains(id, c), Some(false));
}

/// Many live ranges all update together.
///
/// The tree updates ranges by walking its table, so "it works with one range"
/// and "it works with the range you happen to hold" are different claims.
#[test]
fn range_updates_apply_to_every_live_range() {
    let (mut arena, root, [_, _, _]) = three_children();

    let ids: Vec<_> = (0..16)
        .map(|_| {
            arena
                .new_range(BoundaryPoint::new(root, 1), BoundaryPoint::new(root, 3))
                .expect("valid range")
        })
        .collect();
    assert_eq!(arena.range_count(), 16);

    let fresh = text(&mut arena, "zzz");
    let first = arena.child_ids(root).next().expect("a first child");
    arena.insert_before(first, fresh).expect("insert");

    for id in &ids {
        let range = arena.range(*id).expect("still there");
        assert_eq!(range.start.offset(), 2);
        assert_eq!(range.end.offset(), 4);
    }
}

/// Two boundary points in a **detached** subtree compare correctly.
///
/// This is the minimal form of a defect the mutation fuzz harness found and a
/// delta-reduction pinned down. `precedes` walked only from `document()`, so
/// any pair inside a detached subtree was incomparable — and
/// `compare_boundary_points` treated "cannot compare" as "a comes first",
/// because its last step was a bare `Some(Before)` justified by an earlier
/// step having mirrored the call.
///
/// The visible consequence: a range built inside a fragment compared as
/// correctly ordered while being inverted, and only began reporting the truth
/// once a mutation collapsed one endpoint into an ancestor relationship — at
/// which point the inversion looked like something that mutation had caused.
/// It took reducing a 34-operation sequence to 16 to see that the range had
/// been inverted all along.
#[test]
fn range_boundary_points_in_a_detached_subtree_compare_correctly() {
    let mut arena = Arena::new();

    // A fragment with two children, attached to nothing.
    let fragment = match arena.create(NodeData::Fragment) {
        Ok(id) => id,
        Err(error) => unreachable!("{error:?}"),
    };
    let first = text(&mut arena, "first");
    let second = text(&mut arena, "second");
    link(&mut arena, fragment, first);
    link(&mut arena, fragment, second);

    assert!(
        arena.get(fragment).and_then(px_dom::Node::parent).is_none(),
        "the subtree must be detached for this test to mean anything"
    );

    let at = |node, offset| BoundaryPoint::new(node, offset);
    assert_eq!(
        arena.compare_boundaries(at(first, 0), at(second, 0)),
        Some(Position::Before),
        "document order holds inside a detached subtree too"
    );
    assert_eq!(
        arena.compare_boundaries(at(second, 0), at(first, 0)),
        Some(Position::After),
        "and it is symmetric -- this is the direction that used to answer          `Before`, because the pair was incomparable and the fall-through said          so anyway"
    );

    // A pair in genuinely different trees still has no answer.
    let other = match arena.create(NodeData::Fragment) {
        Ok(id) => id,
        Err(error) => unreachable!("{error:?}"),
    };
    assert_eq!(
        arena.compare_boundaries(at(first, 0), at(other, 0)),
        None,
        "different trees have no relative order, and that is not `Before`"
    );
}

/// A range that once became inverted, as a regression test.
///
/// Replays the reduced sequence that found the defect above. It lived as an
/// `#[ignore]`d open question in `tests/open_questions.rs` while it was
/// undecided whether the inversion was a defect here or inherent to
/// `(node, offset)` boundary points. Reducing it answered that: a defect,
/// fixed, so the question graduates to a test.
#[test]
fn range_stays_ordered_through_the_sequence_that_once_inverted_it() {
    let mut arena = Arena::new();
    let document = arena.document();

    // <div> with a text child, then the div is detached, then a range is made
    // across the detached subtree, then more nodes arrive. The shape that
    // mattered: both endpoints inside a subtree with no path to the document.
    let outer = match arena.create(NodeData::Element {
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
        Err(error) => unreachable!("{error:?}"),
    };
    link(&mut arena, document, outer);
    let a = text(&mut arena, "aaa");
    let b = text(&mut arena, "bbb");
    link(&mut arena, outer, a);
    link(&mut arena, outer, b);

    arena.detach(outer).expect("detach");

    let id = arena
        .new_range(BoundaryPoint::new(a, 1), BoundaryPoint::new(b, 1))
        .expect("a range inside a detached subtree is still a range");

    for _ in 0..8 {
        let extra = text(&mut arena, "x");
        let _ = arena.append_child(outer, extra);
        let range = arena.range(id).expect("still live");
        assert_ne!(
            arena.compare_boundaries(range.start, range.end),
            Some(Position::After),
            "the range inverted; its start now follows its own end"
        );
    }
}

/// A range cannot be built across two different trees.
///
/// The mutation fuzz campaign found this with an eight-operation input, and it
/// is the third and last of the defects behind "a range's start can come to
/// follow its end". The first two were in the comparison; this one is in the
/// constructor.
///
/// `new_range` refused a pair that compared as `After` — but two boundary
/// points in different trees do not compare at all, and `None` is not
/// `Some(After)`, so the pair was accepted. The range is well-formed right up
/// until the two trees are joined, at which point it is inverted and **nothing
/// moved either endpoint**. That is why it kept looking like a mutation-rules
/// problem.
///
/// The DOM never holds such a range either: `setStart` and `setEnd` collapse
/// the range when handed a node in a different tree.
#[test]
fn range_creation_refuses_endpoints_in_different_trees() {
    let mut arena = Arena::new();
    let document = arena.document();

    // A node that belongs to no tree.
    let orphan = text(&mut arena, "orphan");
    assert!(arena.get(orphan).and_then(px_dom::Node::parent).is_none());

    let attached = text(&mut arena, "attached");
    link(&mut arena, document, attached);

    assert_eq!(
        arena.compare_boundaries(
            BoundaryPoint::new(orphan, 0),
            BoundaryPoint::new(attached, 0)
        ),
        None,
        "different trees have no relative order"
    );
    assert_eq!(
        arena.new_range(
            BoundaryPoint::new(orphan, 0),
            BoundaryPoint::new(document, 0)
        ),
        Err(TreeError::WouldCycle),
        "a range whose ordering cannot be established must be refused, not          accepted on the grounds that it is not backwards *yet*"
    );

    // The exact shape the campaign found: build the range, then join the
    // trees. With the range refused above, there is nothing left to invert.
    // Once joined, `orphan` is the document's second child (index 1), so a
    // range from inside it must end at offset 2 -- after both children.
    // Ending at offset 1 is the position *before* `orphan`, and is refused:
    // the same check working in the ordinary direction.
    arena
        .append_child(document, orphan)
        .expect("join the trees");
    assert_eq!(
        arena.new_range(
            BoundaryPoint::new(orphan, 0),
            BoundaryPoint::new(document, 1)
        ),
        Err(TreeError::WouldCycle),
        "offset 1 is the position before `orphan`, so this really is backwards"
    );
    let id = arena
        .new_range(
            BoundaryPoint::new(orphan, 0),
            BoundaryPoint::new(document, 2),
        )
        .expect("now they share a tree, so the pair has an order");
    let range = arena.range(id).expect("live");
    assert_ne!(
        arena.compare_boundaries(range.start, range.end),
        Some(Position::After)
    );
}
