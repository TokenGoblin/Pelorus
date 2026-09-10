//! Ranges, and the boundary points they are made of.
//!
//! §9 Phase 4 names ranges alongside mutation-safe iteration, tree ordering and
//! depth limits. They are the last of the four and the one with the most edges,
//! because a range is the only thing in the DOM that points *between* nodes
//! rather than at one.
//!
//! # Why ranges are hard, and why they are hard here in particular
//!
//! A boundary point is a node and an offset, and the offset means different
//! things depending on the node: for a text node it counts characters, for
//! anything else it counts children. So `(p, 1)` and `(text, 1)` are both
//! valid and mean unrelated things, and an offset that was valid a moment ago
//! stops being valid when the node it indexes into changes size.
//!
//! That is the ordinary difficulty, and every engine has it. This engine adds
//! one: **a boundary point holds a `NodeId`, which can go stale.** A range
//! whose node was removed does not become invalid in the DOM sense — the spec
//! has rules that *move* it — so "the handle stopped resolving" and "the range
//! is meaningless" are different states and must not be conflated. Everything
//! here returns `Option` for the first and applies the spec for the second.
//!
//! # Live, not detached
//!
//! Ranges in the DOM are **live**: inserting a node before a range's start
//! shifts that start, and removing the node a boundary point sits in moves the
//! point to where the node used to be. That is not a nicety — it is what makes
//! a selection survive the page mutating underneath it, and getting it wrong
//! produces a selection that silently covers different text than the user
//! highlighted.
//!
//! Liveness means the tree has to know about its ranges, so [`Arena`] keeps
//! them and updates them on every mutation. The cost is O(live ranges) per
//! tree operation; browsers pay the same and for the same reason.

use crate::arena::Arena;
use crate::handle::NodeId;
use crate::node::NodeData;

/// A point between two things: a node, and an offset into it.
///
/// For a text, comment or processing-instruction node the offset counts UTF-8
/// *bytes*, not characters — see [`BoundaryPoint::offset`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct BoundaryPoint {
    node: NodeId,
    offset: usize,
}

impl BoundaryPoint {
    /// A boundary point, unchecked against any tree.
    ///
    /// Unchecked because a `BoundaryPoint` is a value, not a claim: validating
    /// it needs an arena, the arena can change afterwards, and a type whose
    /// construction implies validity would be lying by the next mutation.
    /// [`Arena::is_valid_boundary`] answers the question at the moment it is
    /// asked, which is the only moment an answer is true for.
    pub fn new(node: NodeId, offset: usize) -> Self {
        Self { node, offset }
    }

    pub fn node(self) -> NodeId {
        self.node
    }

    /// The offset. Children for an element, **UTF-8 bytes** for character data.
    ///
    /// Bytes rather than UTF-16 code units, which is what the DOM specifies and
    /// what a browser must eventually expose to script. The conversion belongs
    /// at the scripting boundary in Phase 11, not here: doing it in the tree
    /// would mean every internal offset carries a representation chosen for a
    /// language this crate does not know about, and every string operation
    /// pays for it. Recorded because it is a real difference and the place it
    /// gets fixed is not obvious.
    pub fn offset(self) -> usize {
        self.offset
    }
}

/// Where one boundary point sits relative to another.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Position {
    Before,
    Equal,
    After,
}

/// A live range: two boundary points, kept correct across tree mutation.
///
/// Created through [`Arena::new_range`], which is what registers it for
/// updates. A `Range` value on its own is a snapshot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Range {
    pub start: BoundaryPoint,
    pub end: BoundaryPoint,
}

impl Range {
    /// Whether the range is collapsed — both boundary points identical.
    pub fn is_collapsed(&self) -> bool {
        self.start == self.end
    }
}

/// A handle to a live range held by an [`Arena`].
///
/// Generational, for the same reason `NodeId` is: ranges are dropped, their
/// slots are reused, and a stale `RangeId` resolving to somebody else's
/// selection would be the same class of bug as a stale `NodeId` resolving to
/// somebody else's node.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct RangeId {
    index: u32,
    generation: core::num::NonZeroU32,
}

impl RangeId {
    pub(crate) fn new(index: u32, generation: core::num::NonZeroU32) -> Self {
        Self { index, generation }
    }

    /// The slot this handle names.
    ///
    /// Readable and not constructible, for the reason `NodeId::index` gives:
    /// exposing the parts forges nothing when there is no public way to build
    /// one from them, and it lets a test assert a slot was *actually reused*.
    /// Without that, "the stale handle did not resolve" passes just as well
    /// when the allocator quietly stopped reusing slots.
    pub fn index(self) -> u32 {
        self.index
    }

    /// Which occupant of the slot this handle means.
    pub fn generation(self) -> core::num::NonZeroU32 {
        self.generation
    }
}

/// The number of offsets a node has: its length, in the DOM's sense.
///
/// Character data is measured in bytes (see [`BoundaryPoint::offset`]);
/// everything else in children.
pub(crate) fn node_length(arena: &Arena, id: NodeId) -> Option<usize> {
    let node = arena.get(id)?;
    Some(match node.data() {
        NodeData::Text { contents } | NodeData::Comment { contents } => contents.len(),
        NodeData::ProcessingInstruction { contents, .. } => contents.len(),
        _ => arena.child_ids(id).count(),
    })
}

/// Whether `ancestor` is `descendant` or contains it.
pub(crate) fn is_inclusive_ancestor(arena: &Arena, ancestor: NodeId, descendant: NodeId) -> bool {
    if ancestor == descendant {
        return arena.contains(ancestor);
    }
    arena.ancestors(descendant).any(|id| id == ancestor)
}

/// The index of `child` among its parent's children.
pub(crate) fn index_of(arena: &Arena, child: NodeId) -> Option<usize> {
    let parent = arena.get(child)?.parent()?;
    arena.child_ids(parent).position(|id| id == child)
}

/// Compare two boundary points, per the DOM's "position of boundary point A
/// relative to boundary point B".
///
/// <https://dom.spec.whatwg.org/#concept-range-bp-position>
///
/// `None` when the points are not comparable: a stale handle, or two nodes in
/// different trees. That is deliberately not `Equal` — "these cannot be
/// compared" and "these are the same place" are different answers, and
/// collapsing them is how a range silently starts covering the wrong text.
pub(crate) fn compare_boundary_points(
    arena: &Arena,
    a: BoundaryPoint,
    b: BoundaryPoint,
) -> Option<Position> {
    if !arena.contains(a.node) || !arena.contains(b.node) {
        return None;
    }

    // Step 1: same node, so the offsets decide.
    if a.node == b.node {
        return Some(match a.offset.cmp(&b.offset) {
            core::cmp::Ordering::Less => Position::Before,
            core::cmp::Ordering::Equal => Position::Equal,
            core::cmp::Ordering::Greater => Position::After,
        });
    }

    // Step 2: if A's node follows B's node in document order, answer the
    // mirrored question and invert. Written this way rather than duplicated
    // because the two directions are genuinely the same algorithm, and the
    // duplicated version is where an asymmetry creeps in.
    if arena.precedes(b.node, a.node) == Some(true) {
        return match compare_boundary_points(arena, b, a)? {
            Position::Before => Some(Position::After),
            Position::After => Some(Position::Before),
            Position::Equal => Some(Position::Equal),
        };
    }

    // Step 3: A's node is an ancestor of B's node. The question is then
    // whether B's branch hangs off before or after A's offset.
    if is_inclusive_ancestor(arena, a.node, b.node) {
        let mut child = b.node;
        let mut budget = crate::arena::MAX_DEPTH + 1;
        loop {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            let parent = arena.get(child)?.parent()?;
            if parent == a.node {
                break;
            }
            child = parent;
        }
        let index = index_of(arena, child)?;
        if index < a.offset {
            return Some(Position::After);
        }
    }

    // Step 4.
    Some(Position::Before)
}
