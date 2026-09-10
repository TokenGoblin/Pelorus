//! The bodies of the Phase 4 fuzz targets, so that they are not only run by
//! the Phase 4 fuzz targets.
//!
//! # Why this is here and not in `fuzz/fuzz_targets/`
//!
//! Phase 1 shipped a 24-hour fuzz campaign that reported clean while never
//! reaching the code it was built for, because `-max_len` defaulted to 4096.
//! The lesson recorded then was that a fuzzing result is only worth what the
//! harness actually executed — and a harness that runs *only* inside the
//! campaign is one nobody watches fail.
//!
//! With the driver here, `cargo test` runs it on both operating systems on
//! every push, against a spread of deterministic inputs, while the campaign
//! explores the same code with coverage feedback. One body, two schedules. A
//! change that breaks the target's own logic fails in the ordinary test job
//! within a minute instead of surviving until somebody reads a campaign
//! report.
//!
//! It also made these particular targets checkable at all: cargo-fuzz cannot
//! link on this project's Windows development machine (no MSVC ASAN runtime,
//! and `--sanitizer=none` leaves sancov symbols undefined), so without this
//! module both targets would have been committed having only ever compiled.
//!
//! Gated behind `testing` and therefore absent from any release build, along
//! with [`Arena::force_generation_to_last`], which is the point of the gate.

use html5ever::{QualName, local_name, ns};

use crate::{Arena, BoundaryPoint, NodeData, NodeId, Position, RangeId};

/// Pick from a list by a byte, or `None` if the list is empty.
fn pick(ids: &[NodeId], n: u8) -> Option<NodeId> {
    if ids.is_empty() {
        return None;
    }
    ids.get(usize::from(n) % ids.len()).copied()
}

// ---------------------------------------------------------------------------
// Target 1 — stale handles
// ---------------------------------------------------------------------------

/// A handle we handed out, and the tag its node carries.
struct Tracked {
    id: NodeId,
    tag: u32,
    /// Set once the node has been removed. Every lookup after this is `None`.
    dead: bool,
}

/// §4.1's property: *hold stale IDs across mutations, assert every lookup is
/// `None`.*
///
/// Two things make this stronger than the obvious version.
///
/// **It checks identity, not liveness.** `get(stale).is_none()` passes
/// trivially whenever the slot happens to be empty. Every node here carries a
/// tag, and a live handle must resolve to *its own* node — so a handle that
/// quietly starts naming a different live node fails, where the weaker check
/// would not notice.
///
/// **It forces generation exhaustion.** ADR 018 chose 32/32 precisely because
/// four billion generations is unreachable by churn, which leaves the
/// retirement branch — the most consequential branch in the arena — unexecuted
/// by ordinary testing.
pub fn stale_handles(data: &[u8]) {
    let mut arena = Arena::new();
    let mut tracked: Vec<Tracked> = Vec::new();
    let mut next_tag: u32 = 0;
    let document = arena.document();

    let mut bytes = data.iter().copied();
    while let Some(op) = bytes.next() {
        let selector = bytes.next().unwrap_or(0);

        match op % 6 {
            0 | 1 => {
                let tag = next_tag;
                next_tag = next_tag.wrapping_add(1);
                let Ok(id) = arena.create(NodeData::Text {
                    contents: tag.to_string().into(),
                }) else {
                    continue;
                };

                let live: Vec<NodeId> = tracked.iter().filter(|t| !t.dead).map(|t| t.id).collect();
                let parent = pick(&live, selector).unwrap_or(document);
                let _ = arena.append_child(parent, id);

                tracked.push(Tracked {
                    id,
                    tag,
                    dead: false,
                });
            }

            // Remove a subtree. Everything in it goes stale, not only the
            // root, so mark descendants before the removal takes them.
            2 => {
                let all: Vec<NodeId> = tracked.iter().map(|t| t.id).collect();
                let Some(victim) = pick(&all, selector) else {
                    continue;
                };
                let doomed: Vec<NodeId> = arena.descendants(victim).collect();
                if arena.remove_subtree(victim).is_ok() {
                    for entry in tracked.iter_mut() {
                        if doomed.contains(&entry.id) {
                            entry.dead = true;
                        }
                    }
                }
            }

            // Detach without removing: handles must stay valid.
            3 => {
                let live: Vec<NodeId> = tracked.iter().filter(|t| !t.dead).map(|t| t.id).collect();
                if let Some(id) = pick(&live, selector) {
                    let _ = arena.detach(id);
                }
            }

            // Move a node somewhere else.
            4 => {
                let live: Vec<NodeId> = tracked.iter().filter(|t| !t.dead).map(|t| t.id).collect();
                if let (Some(parent), Some(child)) =
                    (pick(&live, selector), pick(&live, selector.rotate_left(3)))
                {
                    let _ = arena.append_child(parent, child);
                }
            }

            // Force a slot to its last generation and free it, so retirement
            // actually runs. Unreachable by churn -- see ADR 018.
            _ => {
                let live: Vec<usize> = tracked
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| !t.dead)
                    .map(|(i, _)| i)
                    .collect();
                if live.is_empty() {
                    continue;
                }
                let Some(index) = live.get(usize::from(selector) % live.len()).copied() else {
                    continue;
                };
                let Some(current) = tracked.get(index).map(|t| t.id) else {
                    continue;
                };
                let Some(moved) = arena.force_generation_to_last(current) else {
                    continue;
                };
                assert!(
                    arena.get(current).is_none(),
                    "a handle whose generation was superseded still resolves"
                );
                if let Some(entry) = tracked.get_mut(index) {
                    entry.id = moved;
                }

                let doomed: Vec<NodeId> = arena.descendants(moved).collect();
                if arena.remove_subtree(moved).is_ok() {
                    for entry in tracked.iter_mut() {
                        if doomed.contains(&entry.id) {
                            entry.dead = true;
                        }
                    }
                }
            }
        }

        // The property, after every operation.
        for entry in &tracked {
            match arena.get(entry.id) {
                None => assert!(
                    entry.dead,
                    "a live node's handle stopped resolving; handles go stale \
                     only when the node is actually removed"
                ),
                Some(node) => {
                    assert!(
                        !entry.dead,
                        "a stale handle resolved -- the use-after-free the \
                         generational design exists to prevent, reachable \
                         from safe Rust"
                    );
                    let tag = node.text().and_then(|text| text.parse::<u32>().ok());
                    assert_eq!(
                        tag,
                        Some(entry.tag),
                        "a handle resolved to a *different* live node; a check \
                         of is_some() alone would have missed this"
                    );
                }
            }
        }
    }

    // Retired slots are never handed out again, however many allocations
    // follow. This is the half of retirement a crash would not reveal: nothing
    // goes wrong when a slot is wrongly reused, only later.
    let retired_before = arena.retired();
    for _ in 0..8 {
        let _ = arena.create(NodeData::Comment {
            contents: "probe".into(),
        });
    }
    assert_eq!(
        arena.retired(),
        retired_before,
        "allocating changed the retired count"
    );
    for entry in tracked.iter().filter(|t| t.dead) {
        assert!(
            arena.get(entry.id).is_none(),
            "a dead handle came back to life after further allocation"
        );
    }
}

// ---------------------------------------------------------------------------
// Target 2 — the tree stays a tree
// ---------------------------------------------------------------------------

/// Check every structural invariant over the arena.
///
/// Delegates to [`Arena::validate`] rather than carrying its own copy. It used
/// to have one, and two implementations of "is this a tree" is one more than
/// the question can support: the fuzz target would have kept passing while the
/// method Phase 5 actually calls drifted away from it.
///
/// "No crash" is a much weaker property than this one. The arena is safe Rust
/// and will not crash; it will happily hold a cycle, or a child whose parent
/// does not list it, or a sibling chain that skips a node. Each of those is
/// silent, and each turns a later traversal into a hang, a truncation, or a
/// node that renders twice.
fn assert_is_a_tree(arena: &Arena, _known: &[NodeId]) {
    if let Err((id, why)) = arena.validate() {
        unreachable!("the tree stopped being a tree at {id:?}: {why}");
    }
}

/// Every live range still points somewhere real.
///
/// Cheap enough to run after every operation: `Option` lookups plus a child
/// count, over a deliberately small set of ranges. The expensive property —
/// that a range's start never comes to *follow* its end — needs a document
/// order comparison, which is O(nodes), so it runs once at the end.
fn assert_ranges_are_well_formed(arena: &Arena, ranges: &[RangeId]) {
    for id in ranges {
        let Some(range) = arena.range(*id) else {
            // Dropped. Its handle stopping resolving is the point.
            continue;
        };
        assert!(
            arena.is_valid_boundary(range.start),
            "a live range's start is out of bounds after a mutation: {:?}",
            range.start
        );
        assert!(
            arena.is_valid_boundary(range.end),
            "a live range's end is out of bounds after a mutation: {:?}",
            range.end
        );
    }
}

/// §9 Phase 4's "24h mutation fuzz clean", as a property rather than an
/// absence of crashes.
///
/// # What counts as "mutation"
///
/// `px-dom` has three mutation surfaces, and an earlier version of this
/// harness exercised one. Tree structure was fuzzed; **live ranges and
/// attribute writes were not**, and both are mutation-path features — a range
/// is updated by every insertion and removal, and an attribute write records a
/// prior-state snapshot. Fuzzing the tree and calling that "mutation fuzz"
/// is the same shape of gap as fuzzing the arena and calling it parser
/// coverage.
///
/// So this drives all three, and checks what a wrong answer would not
/// announce:
///
/// - the tree is still a tree ([`Arena::validate`]);
/// - every live range still points somewhere real, after every operation;
/// - no range's start has come to follow its end;
/// - nothing leaked and nothing was double-freed.
pub fn mutations(data: &[u8]) {
    let mut arena = Arena::new();
    let document = arena.document();
    let mut known: Vec<NodeId> = vec![document];
    let mut ranges: Vec<RangeId> = Vec::new();

    // Snapshot recording on, so attribute writes take the path that captures
    // prior state rather than the cheap one. Off is the parse-time default and
    // is already covered by every other target here.
    arena.record_snapshots(true);

    let mut bytes = data.iter().copied();
    while let Some(op) = bytes.next() {
        let a = bytes.next().unwrap_or(0);
        let b = bytes.next().unwrap_or(0);

        match op % 11 {
            0 | 1 => {
                let Ok(id) = arena.create(NodeData::Text {
                    contents: "t".into(),
                }) else {
                    continue;
                };
                if let Some(parent) = pick(&known, a) {
                    let _ = arena.append_child(parent, id);
                }
                known.push(id);
            }

            // An element, so the attribute operations have somewhere to go.
            2 => {
                let Ok(id) = arena.create(NodeData::Element {
                    name: QualName::new(None, ns!(html), local_name!("div")),
                    attrs: Vec::new(),
                    template_contents: None,
                    script_already_started: false,
                }) else {
                    continue;
                };
                if let Some(parent) = pick(&known, a) {
                    let _ = arena.append_child(parent, id);
                }
                known.push(id);
            }

            // Moving an existing node is the operation most likely to corrupt
            // the sibling chain: it unlinks and relinks in one step.
            3 => {
                if let (Some(parent), Some(child)) = (pick(&known, a), pick(&known, b)) {
                    let _ = arena.append_child(parent, child);
                }
            }

            // insert_before carries the first_child edge case.
            4 => {
                let Ok(id) = arena.create(NodeData::Comment {
                    contents: "c".into(),
                }) else {
                    continue;
                };
                if let Some(sibling) = pick(&known, a) {
                    let _ = arena.insert_before(sibling, id);
                }
                known.push(id);
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

            // A live range, which every later insertion and removal has to
            // keep correct. Bounded at eight: the per-operation check below is
            // linear in this, and the point is that ranges exist during the
            // churn, not that there are many.
            8 => {
                if ranges.len() >= 8 {
                    if let Some(id) = ranges.first().copied() {
                        arena.drop_range(id);
                        ranges.remove(0);
                    }
                    continue;
                }
                let (Some(start_node), Some(end_node)) = (pick(&known, a), pick(&known, b)) else {
                    continue;
                };
                let start = BoundaryPoint::new(start_node, usize::from(a) % 3);
                let end = BoundaryPoint::new(end_node, usize::from(b) % 3);
                // Refused for an invalid or inverted pair, which is most of
                // them; the ones that survive are the interesting ones.
                if let Ok(id) = arena.new_range(start, end) {
                    ranges.push(id);
                }
            }

            9 => {
                if let Some(id) = pick(&known, a) {
                    let name = QualName::new(
                        None,
                        ns!(),
                        match b % 3 {
                            0 => local_name!("class"),
                            1 => local_name!("id"),
                            _ => local_name!("href"),
                        },
                    );
                    let _ = arena.set_attribute(id, name, "v".into());
                }
            }

            _ => {
                if let Some(id) = pick(&known, a) {
                    let name = QualName::new(
                        None,
                        ns!(),
                        if b % 2 == 0 {
                            local_name!("class")
                        } else {
                            local_name!("id")
                        },
                    );
                    let _ = arena.remove_attribute(id, &name);
                }
            }
        }

        assert_is_a_tree(&arena, &known);
        assert_ranges_are_well_formed(&arena, &ranges);
    }

    // A range's start must never have come to follow its own end.
    //
    // This is the property that found the defect it now guards. It was briefly
    // *not* asserted, while it was undecided whether the inversions the
    // harness produced were a bug here or inherent to (node, offset) boundary
    // points — moving a container carries its boundary points with it, and no
    // DOM rule re-checks ordering afterwards.
    //
    // Reducing a 34-operation sequence to 16 settled it: a defect.
    // `compare_boundary_points` treated "these are incomparable" as "the first
    // one comes first", so a range inside a detached subtree read as correctly
    // ordered while being inverted, and only told the truth once a mutation
    // collapsed an endpoint into an ancestor relationship. Both halves are
    // fixed and this assertion is why they stay fixed.
    for id in &ranges {
        let Some(range) = arena.range(*id) else {
            continue;
        };
        if let Some(position) = arena.compare_boundaries(range.start, range.end) {
            assert_ne!(
                position,
                Position::After,
                "a live range's start now follows its end; some mutation moved                  one boundary point past the other"
            );
        }
    }

    // A snapshot per node written to, at most once each -- and **not** bounded
    // by the nodes still alive.
    //
    // The first version of this asserted "no more snapshots than live
    // elements", and it failed on the first run: an element mutated and then
    // removed keeps its record. That is correct and deliberate. The snapshot
    // describes what the element looked like as of the last restyle, and a
    // restyle still needs to know it changed even though it has since gone;
    // Servo's `SnapshotMap` persists the same way, until the flush.
    //
    // It is safe here for a reason that is not true of Servo's pointer keys:
    // the key is a generational `NodeId`, so a reused slot gets a different
    // key and a stale snapshot can never be matched to the new occupant
    // (§3.5, `NodeId::to_opaque`). Without that, this behaviour would be the
    // bug rather than the design.
    assert!(
        arena.snapshot_count() <= known.len(),
        "there are {} snapshots for {} nodes ever created",
        arena.snapshot_count(),
        known.len()
    );

    // Accounting. A leak and a double-free both show up here as a mismatch,
    // and neither shows up as a crash.
    let reachable = arena.descendants(document).count();
    let live = arena.len();
    assert!(
        reachable <= live,
        "more nodes reachable ({reachable}) than alive ({live}) -- the walk \
         visits something twice, meaning a cycle or a shared child"
    );

    let counted = known.iter().filter(|id| arena.contains(**id)).count();
    assert_eq!(
        counted, live,
        "the arena reports {live} live nodes but only {counted} held handles \
         resolve; a slot was freed without its handle going stale, or twice"
    );
}

// ---------------------------------------------------------------------------
// Target 3 — the parser
// ---------------------------------------------------------------------------

/// §4.4: *"Fuzz corpus includes 100,000-level nesting for each parser."*
///
/// The other two targets drive the arena's API with operation bytes. Neither
/// sends a byte of HTML through html5ever and the `TreeSink`, which is the
/// most hostile-input-facing surface in this crate and the one an attacker
/// actually reaches — a page is bytes, not a sequence of `append_child` calls.
///
/// Everything the sink does is in scope here and in scope nowhere else: foster
/// parenting, the adoption agency algorithm's reparenting, template contents,
/// text-run merging, attribute merging on a duplicate `<html>`, the feed bound
/// that stops a nesting bomb, and every interaction between them.
///
/// The properties checked are the ones a wrong answer would not announce:
///
/// - the result is a **tree** — `Arena::validate`, the same check the mutation
///   target uses, so a parse that builds a cycle or a broken sibling chain
///   fails here rather than at style time;
/// - nothing exceeds `MAX_DEPTH`, however deep the input went;
/// - a document that lost content **says so**, because a silently truncated
///   page is indistinguishable from a short one.
pub fn parse_html(data: &[u8]) {
    // Lossy rather than refusing non-UTF-8: a browser does not get to decline
    // bytes, and the tokenizer's behaviour on replacement characters is part
    // of what is being tested.
    let html = String::from_utf8_lossy(data);
    let dom = crate::parse(&html);

    if let Err((id, why)) = dom.arena.validate() {
        unreachable!("parsing produced something that is not a tree at {id:?}: {why}");
    }

    // The depth limit is checked by `validate` above, in one downward pass.
    // Re-walking it here would double the cost of the most expensive thing
    // this harness does, for a second opinion on the same walk.
    let document = dom.arena.document();

    // Accounting: everything reachable is alive, and the walk does not visit
    // anything twice. More reachable than alive means a cycle or a shared
    // child, which `validate` should already have caught -- this is the
    // independent second opinion, because both being wrong the same way is the
    // failure mode a single check has.
    let reachable = dom.arena.descendants(document).count();
    assert!(
        reachable <= dom.arena.len(),
        "more nodes reachable ({reachable}) than alive ({})",
        dom.arena.len()
    );

    // A parse that abandoned must have been refused something first. The
    // converse is not asserted: refusals below the threshold do not abandon.
    if dom.abandoned {
        assert!(
            dom.truncated > 0,
            "the parse was abandoned without the tree having refused anything, \
             so the bound fired for a reason that is not the one it exists for"
        );
    }
}
