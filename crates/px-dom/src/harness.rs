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

use crate::{Arena, NodeData, NodeId};

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

/// §9 Phase 4's "24h mutation fuzz clean", as a property rather than an
/// absence of crashes.
pub fn mutations(data: &[u8]) {
    let mut arena = Arena::new();
    let document = arena.document();
    let mut known: Vec<NodeId> = vec![document];

    let mut bytes = data.iter().copied();
    while let Some(op) = bytes.next() {
        let a = bytes.next().unwrap_or(0);
        let b = bytes.next().unwrap_or(0);

        match op % 7 {
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

            // Moving an existing node is the operation most likely to corrupt
            // the sibling chain: it unlinks and relinks in one step.
            2 => {
                if let (Some(parent), Some(child)) = (pick(&known, a), pick(&known, b)) {
                    let _ = arena.append_child(parent, child);
                }
            }

            // insert_before carries the first_child edge case.
            3 => {
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

            4 => {
                if let Some(id) = pick(&known, a) {
                    let _ = arena.detach(id);
                }
            }

            5 => {
                if let Some(id) = pick(&known, a) {
                    let _ = arena.remove_subtree(id);
                }
            }

            _ => {
                if let (Some(from), Some(to)) = (pick(&known, a), pick(&known, b)) {
                    let _ = arena.reparent_children(from, to);
                }
            }
        }

        assert_is_a_tree(&arena, &known);
    }

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
