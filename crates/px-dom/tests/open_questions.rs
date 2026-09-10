//! Questions this project has raised and not answered.
//!
//! `/CLAUDE.md`: *"If a spec is ambiguous, encode the ambiguity as an
//! `#[ignore]` test with a comment and raise it. Do not guess."*
//!
//! Nothing here is a known defect and nothing here is known-correct behaviour.
//! Each test carries a reproducer for something that is genuinely undecided,
//! so that the question survives in a form somebody can run, rather than as a
//! sentence in a document.
//!
//! **These are deliberately not in `ci/gate-dom.sh`'s suite list**, and the
//! names avoid its filters. A gate item with an `#[ignore]`d test in it is a
//! gate item nobody did — that check exists and is right. An open question is
//! a different thing and should not be dressed as either a pass or a failure.

#![cfg(feature = "testing")]

/// Can a live range's start come to follow its own end, through mutations that
/// are each individually legal?
///
/// # What is known
///
/// The mutation fuzz harness produces one. Seed 189 below, at 101 bytes, ends
/// with a range whose `compare_boundaries(start, end)` is `After`, with **both
/// boundary points still individually valid** — they resolve, and their
/// offsets are in bounds. So this is not a dangling range; it is a range that
/// describes a span running backwards.
///
/// # What is not known
///
/// Whether that is a defect in `Arena`'s implementation of the DOM's range
/// mutation rules, or an inherent property of `(node, offset)` boundary
/// points.
///
/// The argument for *inherent*: a boundary point names a container and an
/// offset within it. Moving the container carries the boundary point with it,
/// and none of the DOM's mutation rules re-check ordering afterwards. Two
/// ranges' endpoints in two sibling subtrees will swap document order if the
/// subtrees are reordered, and no rule fires at all — neither node was
/// removed.
///
/// The argument for *defect*: every case reachable by hand keeps the ordering.
/// Constructing the obvious inversions — moving a container past the other
/// endpoint, removing a node between them, reparenting — all produce a
/// correctly ordered result, because the removal rule collapses the moved
/// endpoint to the removal site and the insertion rule shifts the other to
/// match. The fuzzer's sequence is 33 operations long and spends part of it
/// with subtrees detached, where `compare_boundaries` has no answer at all and
/// the invariant is unobservable.
///
/// # Why it is not asserted
///
/// Asserting it would fail the build on a property this project has not
/// established. Deleting it would lose the question. The DOM specification
/// states that a range's start is before or equal to its end, but states it as
/// a property of the *constructors*, and the mutation rules are given as
/// imperative steps with no restated invariant — which is exactly the
/// ambiguity `/CLAUDE.md` says to encode rather than guess at.
///
/// # How to settle it
///
/// Reduce the sequence below to something a human can read, and check each
/// step against <https://dom.spec.whatwg.org/#concept-node-remove> and
/// `#concept-node-insert`. If every step follows the spec and the result is
/// still inverted, the answer is "inherent" and this test becomes a test that
/// asserts inversion is *possible*. If a step diverges, it is a defect in
/// `Arena` and this test becomes a regression test.
#[test]
#[ignore = "open question: see the doc comment. Not a known defect."]
fn open_question_inverted_span_after_mutation() {
    use px_dom::{Arena, BoundaryPoint, NodeData, NodeId, Position, RangeId};

    fn pseudorandom(seed: u64, len: usize) -> Vec<u8> {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
        (0..len)
            .map(|_| {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
            })
            .collect()
    }

    fn pick(ids: &[NodeId], n: u8) -> Option<NodeId> {
        if ids.is_empty() {
            return None;
        }
        ids.get(usize::from(n) % ids.len()).copied()
    }

    // The harness's operation mix, inlined so this reproducer keeps working if
    // the harness is reshaped. A reproducer that drifts with the code it
    // reproduces is not one.
    let data = pseudorandom(189, 402);
    let data = &data[..101];

    let mut arena = Arena::new();
    let document = arena.document();
    let mut known = vec![document];
    let mut ranges: Vec<RangeId> = Vec::new();
    arena.record_snapshots(true);

    let mut bytes = data.iter().copied();
    while let Some(op) = bytes.next() {
        let a = bytes.next().unwrap_or(0);
        let b = bytes.next().unwrap_or(0);
        match op % 11 {
            0 | 1 => {
                if let Ok(id) = arena.create(NodeData::Text {
                    contents: "t".into(),
                }) {
                    if let Some(parent) = pick(&known, a) {
                        let _ = arena.append_child(parent, id);
                    }
                    known.push(id);
                }
            }
            2 => {
                if let Ok(id) = arena.create(NodeData::Element {
                    name: html5ever::QualName::new(
                        None,
                        html5ever::ns!(html),
                        html5ever::LocalName::from("div"),
                    ),
                    attrs: Vec::new(),
                    template_contents: None,
                    script_already_started: false,
                }) {
                    if let Some(parent) = pick(&known, a) {
                        let _ = arena.append_child(parent, id);
                    }
                    known.push(id);
                }
            }
            3 => {
                if let (Some(parent), Some(child)) = (pick(&known, a), pick(&known, b)) {
                    let _ = arena.append_child(parent, child);
                }
            }
            4 => {
                if let Ok(id) = arena.create(NodeData::Comment {
                    contents: "c".into(),
                }) {
                    if let Some(sibling) = pick(&known, a) {
                        let _ = arena.insert_before(sibling, id);
                    }
                    known.push(id);
                }
            }
            5 => {
                if let Some(id) = pick(&known, a) {
                    let _ = arena.detach(id);
                }
            }
            6 => {
                if let Some(id) = pick(&known, a) {
                    let _ = arena.remove_subtree(id);
                }
            }
            7 => {
                if let (Some(from), Some(to)) = (pick(&known, a), pick(&known, b)) {
                    let _ = arena.reparent_children(from, to);
                }
            }
            8 => {
                if ranges.len() >= 8 {
                    if let Some(id) = ranges.first().copied() {
                        arena.drop_range(id);
                        ranges.remove(0);
                    }
                } else if let (Some(start), Some(end)) = (pick(&known, a), pick(&known, b)) {
                    let start = BoundaryPoint::new(start, usize::from(a) % 3);
                    let end = BoundaryPoint::new(end, usize::from(b) % 3);
                    if let Ok(id) = arena.new_range(start, end) {
                        ranges.push(id);
                    }
                }
            }
            _ => {}
        }
    }

    let inverted = ranges.iter().any(|id| {
        arena
            .range(*id)
            .and_then(|range| arena.compare_boundaries(range.start, range.end))
            == Some(Position::After)
    });

    assert!(
        !inverted,
        "a live range's start follows its end. Both boundary points are still \
         valid, so this is a span running backwards rather than a dangling \
         range. See this test's doc comment: it is not established whether \
         this is a defect here or inherent to (node, offset) boundary points."
    );
}
