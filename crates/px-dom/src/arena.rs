//! The generational arena.
//!
//! Nodes live in a flat `Vec` and are named by `NodeId`. Nothing owns a node
//! but the arena, so dropping a document is dropping one allocation however
//! deep the document was, and every traversal here carries an explicit work
//! stack rather than using the call stack.

use core::num::NonZeroU32;

use html5ever::tendril::StrTendril;
use html5ever::{Attribute, QualName};

use crate::handle::NodeId;
use crate::node::{Node, NodeData};
use crate::range::{
    BoundaryPoint, Position, Range, RangeId, compare_boundary_points, index_of,
    is_inclusive_ancestor, node_length,
};
use crate::snapshot::ElementSnapshot;

/// The maximum depth of the tree, per build-spec §4.4 ("start at 512").
///
/// Chosen to be far above what documents actually use — real pages rarely
/// exceed a hundred — and far below what would let a nesting bomb matter. The
/// arena refuses to make the tree deeper than this and says so; deciding what
/// to *do* about a refusal is the caller's, because the right answer differs
/// between a parser (drop the content, keep going, the way browsers do) and a
/// scripted DOM API (report the error to script).
pub const MAX_DEPTH: usize = 512;

/// Why a tree operation was refused.
///
/// Every one of these is a refusal rather than a panic. A DOM holding
/// attacker-controlled markup that panics on malformed structure is a DOM that
/// turns a parse bug into a dead tab.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TreeError {
    /// A handle did not resolve: wrong generation, retired slot, or never
    /// allocated. The ordinary case, and the one the design is built around.
    NoSuchNode,
    /// The operation would have put a node inside its own subtree.
    WouldCycle,
    /// The operation would have exceeded `MAX_DEPTH`.
    TooDeep,
    /// The arena has no slot to give: 2³² slots allocated, or every remaining
    /// slot retired.
    Exhausted,
    /// The parent cannot have children: it is text, a comment, a doctype or a
    /// processing instruction.
    ///
    /// The DOM calls this a `HierarchyRequestError`. It matters here beyond
    /// tidiness because a boundary point's offset means *children* for a node
    /// that has them and *bytes* for character data — so a text node with a
    /// child list has two incompatible notions of its own length, and every
    /// range that points into it is nonsense. The mutation fuzz harness found
    /// exactly that, by moving a node under a text node.
    CannotHaveChildren,
    /// The operation would have moved or removed the document node.
    ///
    /// The document is the one node the arena assumes exists: `document()`
    /// hands out its handle, every traversal starts there, and `validate`
    /// checks the tree from it. Letting it be removed or re-parented turns
    /// every one of those into a silent lie — which is what happened, and what
    /// the mutation fuzz harness found once it was allowed to try.
    Immovable,
}

/// One slot. Occupied, vacant, or retired.
///
/// The three states are encoded rather than enumerated, because a slot is in
/// every node and an explicit discriminant would cost more than the encoding
/// saves:
///
/// - **occupied** — `node` is `Some`, and `generation` is the live generation.
/// - **vacant** — `node` is `None`, and `generation` is what the *next*
///   occupant will be given. Freeing bumps it, which is what makes every
///   outstanding handle to the previous occupant stop resolving.
/// - **retired** — `node` is `None`, `generation` is `u32::MAX`, and the index
///   is absent from the free list, so nothing is ever allocated here again.
struct Slot {
    generation: NonZeroU32,
    node: Option<Node>,
}

/// Slots per chunk. A power of two so the index split is a shift and a mask.
///
/// 1024 slots is 48 KB of `Slot` at present, which is large enough that the
/// per-chunk overhead is noise and small enough that a document of a few
/// hundred nodes does not pay for a page it never fills.
const CHUNK_SLOTS: usize = 1024;

/// Slot storage with **stable addresses**.
///
/// A flat `Vec<Slot>` reallocates when it grows and moves every slot with it.
/// That is sound in safe Rust — nothing can grow the arena while a `&Node` is
/// outstanding — but it makes the borrow of the *whole tree* the unit of
/// safety, and `docs/research/stylo-requirements.md` §3.3 is explicit that this
/// is the Phase 4 decision:
///
/// > Retrofitting stable addresses onto a flat `Vec` design touches every
/// > accessor, every iterator, and the Miri tests. This single choice is most
/// > of the "px-dom redesign" the risk register is worried about, and it is
/// > fully decidable today.
///
/// Chunks of boxed slices give it: growing pushes a `Box` onto a `Vec`, which
/// moves pointers, never the slots behind them. A slot's address is fixed for
/// the life of the arena.
///
/// See ADR 020 for the measurement and the alternative that was rejected.
struct Slots {
    chunks: Vec<Box<[Slot]>>,
    len: usize,
}

impl Slots {
    fn new() -> Self {
        Self {
            chunks: Vec::new(),
            len: 0,
        }
    }

    fn len(&self) -> usize {
        self.len
    }

    fn get(&self, index: usize) -> Option<&Slot> {
        if index >= self.len {
            return None;
        }
        self.chunks
            .get(index / CHUNK_SLOTS)?
            .get(index % CHUNK_SLOTS)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Slot> {
        if index >= self.len {
            return None;
        }
        self.chunks
            .get_mut(index / CHUNK_SLOTS)?
            .get_mut(index % CHUNK_SLOTS)
    }

    /// Append a slot, allocating a chunk when the last one is full.
    fn push(&mut self, slot: Slot) {
        if self.len.is_multiple_of(CHUNK_SLOTS) {
            let chunk: Vec<Slot> = (0..CHUNK_SLOTS)
                .map(|_| Slot {
                    generation: NonZeroU32::MIN,
                    node: None,
                })
                .collect();
            self.chunks.push(chunk.into_boxed_slice());
        }
        let index = self.len;
        self.len += 1;
        if let Some(existing) = self.get_mut(index) {
            *existing = slot;
        }
    }

    fn iter(&self) -> impl Iterator<Item = &Slot> {
        self.chunks
            .iter()
            .flat_map(|chunk| chunk.iter())
            .take(self.len)
    }
}

/// A DOM tree.
pub struct Arena {
    slots: Slots,
    /// Indices of vacant slots. Retired slots are never in here.
    free: Vec<u32>,
    live: usize,
    retired: usize,
    document: NodeId,
    /// Live ranges, and the generation of each slot.
    ///
    /// Ranges are held by the tree rather than by their creator because the
    /// DOM's ranges are *live*: the tree has to be able to reach every one of
    /// them on every mutation. Same slot-and-generation shape as nodes, for
    /// the same reason — a dropped range's slot gets reused, and a stale
    /// `RangeId` resolving to somebody else's selection is the same class of
    /// bug as a stale `NodeId` resolving to somebody else's node.
    ranges: Vec<RangeSlot>,
    free_ranges: Vec<u32>,
    /// Prior-state records for elements mutated since the last flush.
    ///
    /// A `Vec` of pairs rather than a map: the population is "elements touched
    /// since the last restyle", which is small and is walked in full by the
    /// consumer. A hash map would cost more to maintain than the linear scan
    /// it saves, and stylo takes the whole set at once anyway.
    snapshots: Vec<(NodeId, ElementSnapshot)>,
    /// Whether to record at all. Off until something has computed style.
    ///
    /// During the initial parse every element is new, so there is no prior
    /// state to describe and a snapshot of "it did not exist" invalidates
    /// nothing. Recording through a parse would be pure cost -- one clone of
    /// the attribute list per element -- which is why this is opt-in rather
    /// than always-on.
    recording_snapshots: bool,
}

struct RangeSlot {
    generation: NonZeroU32,
    range: Option<Range>,
}

impl Arena {
    /// A new arena holding nothing but a `Document` node.
    pub fn new() -> Self {
        let mut arena = Self {
            slots: Slots::new(),
            free: Vec::new(),
            live: 0,
            retired: 0,
            // Replaced immediately below. The arena is never observable in
            // this state: `create` on an empty arena cannot fail, because the
            // only failure is exhaustion and nothing has been allocated.
            document: NodeId::new(0, NonZeroU32::MIN),
            ranges: Vec::new(),
            free_ranges: Vec::new(),
            snapshots: Vec::new(),
            recording_snapshots: false,
        };
        if let Ok(id) = arena.create(NodeData::Document) {
            arena.document = id;
        }
        arena
    }

    /// The document node.
    pub fn document(&self) -> NodeId {
        self.document
    }

    /// Live nodes.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Slots retired to generation overflow, and therefore lost for the life
    /// of the arena.
    ///
    /// Public because it is the number that says whether ADR 018's reasoning
    /// is holding up in practice. Under the 32/32 layout it should be zero on
    /// any real workload; a non-zero value here means something is churning
    /// four billion times through one slot, which is worth knowing.
    pub fn retired(&self) -> usize {
        self.retired
    }

    /// Slots allocated, live or not. The bound every traversal uses to stay
    /// finite on a tree it has no reason to trust.
    pub(crate) fn slot_count(&self) -> usize {
        self.slots.len()
    }

    /// Move a slot's generation to the last one it will ever have, and return
    /// a handle at that generation.
    ///
    /// §14.3 asks Phase 4 for "a fuzz target that forces generation
    /// exhaustion", and ADR 018 explains why it has to be forced rather than
    /// reached: the 32/32 layout gives four billion generations per slot, so
    /// retirement is unreachable by churning. That is the whole argument for
    /// the layout — and it means the retirement branch, which is the most
    /// consequential branch in this file, would never execute in testing.
    /// A safety property that only runs in situations nobody can produce is a
    /// safety property nobody has checked.
    ///
    /// Test-only, and gated the way §14.4 requires rather than left `pub` with
    /// a warning in the docs: without the feature this function does not exist
    /// in the build at all.
    ///
    /// There is deliberately **no release-artifact scan for this symbol yet.**
    /// §14.4 asks for one, and `ci/gate-sandbox.sh` has a real one for
    /// `PX_TEST_FORCE_SANDBOX_UNAVAILABLE`, so the omission needs a reason.
    /// The reason is the one `ci/gate-network.sh` already worked out and wrote
    /// down for `client_config_trusting`: no shipping binary links `px-dom`
    /// yet, so a scan would find nothing whether or not the feature was on. A
    /// check that cannot fail reads as assurance and provides none.
    ///
    /// Add the scan when `px-content` links this crate, which is when it
    /// starts being able to fail. Tracked in `docs/backlog.md`.
    #[cfg(feature = "testing")]
    pub fn force_generation_to_last(&mut self, id: NodeId) -> Option<NodeId> {
        let slot = self.slots.get_mut(id.index() as usize)?;
        if slot.generation != id.generation() || slot.node.is_none() {
            return None;
        }
        let last = NonZeroU32::new(u32::MAX)?;
        slot.generation = last;
        Some(NodeId::new(id.index(), last))
    }

    // -----------------------------------------------------------------------
    // Allocation
    // -----------------------------------------------------------------------

    /// Allocate a node.
    pub fn create(&mut self, data: NodeData) -> Result<NodeId, TreeError> {
        let node = Node::new(data);

        if let Some(index) = self.free.pop() {
            let slot = self
                .slots
                .get_mut(index as usize)
                .ok_or(TreeError::Exhausted)?;
            let generation = slot.generation;
            slot.node = Some(node);
            self.live += 1;
            return Ok(NodeId::new(index, generation));
        }

        let index = u32::try_from(self.slots.len()).map_err(|_| TreeError::Exhausted)?;
        // u32::MAX is not a usable index: it is the value a future packed
        // layout would need as its "no slot" sentinel, and reserving it costs
        // one slot out of four billion.
        if index == u32::MAX {
            return Err(TreeError::Exhausted);
        }
        self.slots.push(Slot {
            generation: NonZeroU32::MIN,
            node: Some(node),
        });
        self.live += 1;
        Ok(NodeId::new(index, NonZeroU32::MIN))
    }

    // -----------------------------------------------------------------------
    // Resolution — the only way to reach a node, and it can fail
    // -----------------------------------------------------------------------

    /// Resolve a handle.
    ///
    /// `None` means the handle is stale: the slot was reused, or retired, or
    /// the handle names a slot that does not exist. There is no variant of
    /// this that returns a `Node` directly, by §4.1 and by `ci/gate-dom.sh`.
    pub fn get(&self, id: NodeId) -> Option<&Node> {
        let slot = self.slots.get(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.node.as_ref()
    }

    /// Resolve a handle for mutation. Same contract.
    pub fn get_mut(&mut self, id: NodeId) -> Option<&mut Node> {
        let slot = self.slots.get_mut(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.node.as_mut()
    }

    /// Whether a handle resolves. Equivalent to `get(id).is_some()`, and named
    /// so that call sites reading as a question do not have to pretend to want
    /// the node.
    pub fn contains(&self, id: NodeId) -> bool {
        self.get(id).is_some()
    }

    // -----------------------------------------------------------------------
    // Structure
    // -----------------------------------------------------------------------

    /// Depth of a node below the root, or `TreeError::TooDeep` if the walk
    /// upward exceeds `MAX_DEPTH`.
    ///
    /// The bound does double duty. It caps the work, and it is also what makes
    /// this terminate on a corrupted tree: a parent cycle would otherwise spin
    /// forever, and "the arena hung" is a worse failure than "the arena said
    /// no".
    pub fn depth(&self, id: NodeId) -> Result<usize, TreeError> {
        let mut depth = 0usize;
        let mut cursor = self.get(id).ok_or(TreeError::NoSuchNode)?.parent();
        while let Some(current) = cursor {
            depth += 1;
            if depth > MAX_DEPTH {
                return Err(TreeError::TooDeep);
            }
            cursor = self.get(current).ok_or(TreeError::NoSuchNode)?.parent();
        }
        Ok(depth)
    }

    /// Append `child` as the last child of `parent`.
    ///
    /// Detaches `child` from wherever it was first, which is what every caller
    /// wants and what the HTML tree construction algorithm assumes.
    pub fn append_child(&mut self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        self.check_insertable(parent, child)?;
        self.detach(child)?;

        let last = self.get(parent).ok_or(TreeError::NoSuchNode)?.last_child();

        {
            let node = self.get_mut(child).ok_or(TreeError::NoSuchNode)?;
            node.parent = Some(parent);
            node.prev_sibling = last;
            node.next_sibling = None;
        }

        match last {
            Some(previous) => {
                self.get_mut(previous)
                    .ok_or(TreeError::NoSuchNode)?
                    .next_sibling = Some(child);
            }
            None => {
                self.get_mut(parent)
                    .ok_or(TreeError::NoSuchNode)?
                    .first_child = Some(child);
            }
        }
        self.get_mut(parent)
            .ok_or(TreeError::NoSuchNode)?
            .last_child = Some(child);

        // No range notification here, and it is worth writing down why rather
        // than leaving the asymmetry with `insert_before` to look like an
        // oversight.
        //
        // An append lands at the end, so its insertion index is the parent's
        // previous child count. The DOM's rule moves only boundary points
        // whose offset is *greater* than that index — and the largest valid
        // offset in a parent is exactly its child count, which is the index
        // itself. Nothing can be greater. An append therefore never moves a
        // boundary point.
        //
        // The first version of this called `child_ids(parent).count()` to pass
        // the index anyway. That is a walk of the whole child list on every
        // append, which is quadratic over a parse: 100,000 paragraphs took
        // 145 seconds, against 2 for a million-deep nesting bomb. Proving the
        // notification unnecessary is cheaper than making it fast.
        Ok(())
    }

    /// Insert `new_node` immediately before `sibling`.
    pub fn insert_before(&mut self, sibling: NodeId, new_node: NodeId) -> Result<(), TreeError> {
        let parent = self
            .get(sibling)
            .ok_or(TreeError::NoSuchNode)?
            .parent()
            .ok_or(TreeError::NoSuchNode)?;
        self.check_insertable(parent, new_node)?;
        self.detach(new_node)?;

        let previous = self
            .get(sibling)
            .ok_or(TreeError::NoSuchNode)?
            .prev_sibling();

        {
            let node = self.get_mut(new_node).ok_or(TreeError::NoSuchNode)?;
            node.parent = Some(parent);
            node.prev_sibling = previous;
            node.next_sibling = Some(sibling);
        }
        self.get_mut(sibling)
            .ok_or(TreeError::NoSuchNode)?
            .prev_sibling = Some(new_node);

        match previous {
            Some(previous) => {
                self.get_mut(previous)
                    .ok_or(TreeError::NoSuchNode)?
                    .next_sibling = Some(new_node);
            }
            None => {
                self.get_mut(parent)
                    .ok_or(TreeError::NoSuchNode)?
                    .first_child = Some(new_node);
            }
        }

        if let Some(index) = index_of(self, new_node) {
            self.ranges_after_insert(parent, index);
        }
        Ok(())
    }

    /// Unlink a node from its parent and siblings. The node itself, and its
    /// own subtree, stay allocated.
    pub fn detach(&mut self, id: NodeId) -> Result<(), TreeError> {
        let (parent, previous, next) = {
            let node = self.get(id).ok_or(TreeError::NoSuchNode)?;
            (node.parent(), node.prev_sibling(), node.next_sibling())
        };
        let Some(parent) = parent else {
            // Already detached. Not an error: the tree builder detaches
            // freely and checking first would be the same walk twice.
            return Ok(());
        };

        // Before anything is unlinked. The rule needs this node still in
        // place: it asks for the node index and for which boundary points sit
        // inside its subtree, and neither question survives the unlink.
        self.ranges_before_remove(id);

        match previous {
            Some(previous) => {
                self.get_mut(previous)
                    .ok_or(TreeError::NoSuchNode)?
                    .next_sibling = next;
            }
            None => {
                self.get_mut(parent)
                    .ok_or(TreeError::NoSuchNode)?
                    .first_child = next;
            }
        }
        match next {
            Some(next) => {
                self.get_mut(next)
                    .ok_or(TreeError::NoSuchNode)?
                    .prev_sibling = previous;
            }
            None => {
                self.get_mut(parent)
                    .ok_or(TreeError::NoSuchNode)?
                    .last_child = previous;
            }
        }

        self.get_mut(id)
            .ok_or(TreeError::NoSuchNode)?
            .detach_links();
        Ok(())
    }

    /// Move every child of `from` to the end of `to`, in order.
    ///
    /// The tree builder needs this for the adoption agency algorithm. Written
    /// as an explicit loop over a snapshot of the child list rather than a
    /// walk that mutates as it goes, because the links being rewritten are the
    /// same links the walk would be following.
    pub fn reparent_children(&mut self, from: NodeId, to: NodeId) -> Result<(), TreeError> {
        let children = self.children(from).ok_or(TreeError::NoSuchNode)?;
        for child in children {
            self.append_child(to, child)?;
        }
        Ok(())
    }

    /// Free a node and everything under it.
    ///
    /// Iterative, with an explicit stack. The recursive version of this is the
    /// deep-nesting crash the Phase 4 gate exists to prevent, and it is the
    /// one that gets written by accident because it reads so naturally.
    pub fn remove_subtree(&mut self, id: NodeId) -> Result<usize, TreeError> {
        if !self.contains(id) {
            return Err(TreeError::NoSuchNode);
        }
        // The document is not removable. Without this, `remove_subtree(doc)`
        // succeeded, freed every node, left `document()` handing out a stale
        // handle -- and `validate()` still returned `Ok(())`.
        if id == self.document {
            return Err(TreeError::Immovable);
        }
        self.detach(id)?;

        let mut freed = 0usize;
        let mut stack = vec![id];
        while let Some(current) = stack.pop() {
            let children = match self.children(current) {
                Some(children) => children,
                // A handle that stopped resolving mid-walk means the tree was
                // inconsistent. Skip it rather than failing the whole removal:
                // the alternative is a half-freed subtree with no owner.
                None => continue,
            };
            stack.extend(children);

            if let Some(NodeData::Element {
                template_contents: Some(contents),
                ..
            }) = self.get(current).map(Node::data)
            {
                stack.push(*contents);
            }

            if self.free_slot(current) {
                freed += 1;
            }
        }

        // Drop any range left pointing into what was just freed.
        //
        // The DOM's removal rule collapses a boundary point inside a removed
        // subtree to where that subtree used to be, and `detach` applies it —
        // but `detach` returns early for a node with no parent, so freeing a
        // *detached* subtree ran no rule at all and left live ranges pointing
        // at freed slots. The mutation fuzz harness found it within a second
        // of being taught to create ranges.
        //
        // There is no removal site to collapse to when the subtree had no
        // parent, and a live range whose endpoints do not resolve is worse
        // than no range: it reads as usable and is not. So the range is
        // dropped, and its handle goes stale like any other.
        //
        // This is a px-dom concept with no DOM equivalent. In the DOM a range
        // keeps its nodes alive and the question never arises; here removal is
        // explicit, so the behaviour has to be chosen rather than inherited.
        if !self.ranges.is_empty() {
            let doomed: Vec<RangeId> = self
                .ranges
                .iter()
                .enumerate()
                .filter_map(|(index, slot)| {
                    let range = slot.range?;
                    let dangling =
                        !self.contains(range.start.node()) || !self.contains(range.end.node());
                    if !dangling {
                        return None;
                    }
                    let index = u32::try_from(index).ok()?;
                    Some(RangeId::new(index, slot.generation))
                })
                .collect();
            for id in doomed {
                self.drop_range(id);
            }
        }

        Ok(freed)
    }

    /// Deep-copy a subtree, returning the root of the copy. The copy is
    /// detached.
    ///
    /// Iterative, with parentage carried on the work stack — the recursive
    /// version of this is one of the walks the Phase 4 gate exists to keep
    /// out, and a clone is exactly the kind of operation somebody writes
    /// recursively because it reads so well.
    ///
    /// A clone is a new node with a new handle: nothing that held a handle to
    /// the original now holds one to the copy. That is worth stating because
    /// the DOM's `cloneNode` is the one place where "the same content" and
    /// "the same node" are easiest to confuse.
    pub fn clone_subtree(&mut self, id: NodeId) -> Result<NodeId, TreeError> {
        let data = self.get(id).ok_or(TreeError::NoSuchNode)?.data().clone();
        let root = self.create(data)?;

        // (source, destination parent) pairs still to copy.
        let mut stack: Vec<(NodeId, NodeId)> = self
            .children(id)
            .ok_or(TreeError::NoSuchNode)?
            .into_iter()
            .map(|child| (child, root))
            .collect();
        // Reversed so the first child is processed first and sibling order is
        // preserved on the way out.
        stack.reverse();

        let mut budget = self.slots.len().saturating_add(1);
        while let Some((source, parent)) = stack.pop() {
            if budget == 0 {
                return Err(TreeError::TooDeep);
            }
            budget -= 1;

            let data = self
                .get(source)
                .ok_or(TreeError::NoSuchNode)?
                .data()
                .clone();
            let copy = self.create(data)?;
            self.append_child(parent, copy)?;

            let children = self.children(source).ok_or(TreeError::NoSuchNode)?;
            for child in children.into_iter().rev() {
                stack.push((child, copy));
            }
        }
        Ok(root)
    }

    /// This node's children, as a snapshot.
    ///
    /// A snapshot rather than an iterator borrowing the arena, deliberately.
    /// Every caller here mutates the tree while walking it, and an iterator
    /// that reads `next_sibling` as it goes would be following links the
    /// caller is in the middle of rewriting. `crate::iter` has the borrowing
    /// version for callers that are only reading.
    pub fn children(&self, id: NodeId) -> Option<Vec<NodeId>> {
        let mut children = Vec::new();
        let mut cursor = self.get(id)?.first_child();
        while let Some(current) = cursor {
            children.push(current);
            cursor = self.get(current)?.next_sibling();
            // A sibling ring would loop forever. It cannot happen through this
            // API, and this is what makes that a fact rather than a belief.
            if children.len() > self.slots.len() {
                return None;
            }
        }
        Some(children)
    }

    // -----------------------------------------------------------------------
    // Ranges (DOM 4.4), live across mutation
    // -----------------------------------------------------------------------

    /// Whether a boundary point is currently valid: the node resolves and the
    /// offset is within it.
    ///
    /// A question about *now*, deliberately. A boundary point is a value and
    /// the tree keeps changing, so a type that guaranteed validity at
    /// construction would be lying by the next mutation.
    pub fn is_valid_boundary(&self, point: BoundaryPoint) -> bool {
        node_length(self, point.node()).is_some_and(|length| point.offset() <= length)
    }

    /// Compare two boundary points in document order.
    ///
    /// <https://dom.spec.whatwg.org/#concept-range-bp-position>
    ///
    /// `None` when they cannot be compared -- a stale handle, or nodes in
    /// different trees. Deliberately not `Position::Equal`: "these cannot be
    /// compared" and "these are the same place" are different answers, and
    /// collapsing them is how a range silently starts covering the wrong text.
    pub fn compare_boundaries(&self, a: BoundaryPoint, b: BoundaryPoint) -> Option<Position> {
        compare_boundary_points(self, a, b)
    }

    /// Create a live range. It is updated by every subsequent mutation until
    /// [`Arena::drop_range`].
    ///
    /// Refuses boundary points that are invalid *now*, and refuses a range
    /// whose end precedes its start. The DOM normalises that second case by
    /// moving one point onto the other; doing so silently here would hide a
    /// caller bug behind a range that covers nothing.
    pub fn new_range(
        &mut self,
        start: BoundaryPoint,
        end: BoundaryPoint,
    ) -> Result<RangeId, TreeError> {
        if !self.is_valid_boundary(start) || !self.is_valid_boundary(end) {
            return Err(TreeError::NoSuchNode);
        }
        if compare_boundary_points(self, start, end) == Some(Position::After) {
            return Err(TreeError::WouldCycle);
        }

        let range = Range { start, end };
        if let Some(index) = self.free_ranges.pop() {
            let slot = self
                .ranges
                .get_mut(index as usize)
                .ok_or(TreeError::Exhausted)?;
            let generation = slot.generation;
            slot.range = Some(range);
            return Ok(RangeId::new(index, generation));
        }
        let index = u32::try_from(self.ranges.len()).map_err(|_| TreeError::Exhausted)?;
        self.ranges.push(RangeSlot {
            generation: NonZeroU32::MIN,
            range: Some(range),
        });
        Ok(RangeId::new(index, NonZeroU32::MIN))
    }

    /// Resolve a range handle. `None` if it is stale.
    pub fn range(&self, id: RangeId) -> Option<Range> {
        let slot = self.ranges.get(id.index() as usize)?;
        if slot.generation != id.generation() {
            return None;
        }
        slot.range
    }

    /// Stop tracking a range. Its handle stops resolving, permanently.
    pub fn drop_range(&mut self, id: RangeId) -> bool {
        let Some(slot) = self.ranges.get_mut(id.index() as usize) else {
            return false;
        };
        if slot.generation != id.generation() || slot.range.is_none() {
            return false;
        }
        slot.range = None;
        // Generations spent means the slot is retired, exactly as for nodes:
        // it never returns to the free list, so nothing is ever allocated
        // there again and no future handle can collide with the ones already
        // handed out.
        if let Some(next) = slot.generation.checked_add(1) {
            slot.generation = next;
            self.free_ranges.push(id.index());
        }
        true
    }

    /// Live ranges currently held.
    pub fn range_count(&self) -> usize {
        self.ranges
            .iter()
            .filter(|slot| slot.range.is_some())
            .count()
    }

    /// Whether a node lies within a range, by document position.
    pub fn range_contains(&self, id: RangeId, node: NodeId) -> Option<bool> {
        let range = self.range(id)?;
        let point = BoundaryPoint::new(node, 0);
        let after_start = compare_boundary_points(self, point, range.start)?;
        let before_end = compare_boundary_points(self, point, range.end)?;
        Some(after_start != Position::Before && before_end != Position::After)
    }

    /// The DOM insertion rule for live ranges.
    ///
    /// <https://dom.spec.whatwg.org/#concept-node-insert>
    ///
    /// > For each live range whose start node is parent and start offset is
    /// > greater than child index, increase its start offset by one.
    ///
    /// A boundary point counts *positions between children*, so inserting
    /// ahead of one moves it along. Without this a selection silently comes to
    /// cover different content whenever anything is inserted before it, which
    /// is a bug the user sees and nothing else reports.
    fn ranges_after_insert(&mut self, parent: NodeId, index: usize) {
        // The overwhelmingly common case, and the one a parse is entirely made
        // of: no live ranges at all. Ranges appear when something selects, not
        // when a document is built.
        if self.ranges.is_empty() {
            return;
        }
        for slot in &mut self.ranges {
            let Some(range) = slot.range.as_mut() else {
                continue;
            };
            if range.start.node() == parent && range.start.offset() > index {
                range.start = BoundaryPoint::new(parent, range.start.offset() + 1);
            }
            if range.end.node() == parent && range.end.offset() > index {
                range.end = BoundaryPoint::new(parent, range.end.offset() + 1);
            }
        }
    }

    /// The DOM removal rule for live ranges. Must run **before** the node is
    /// unlinked: it needs the index and the subtree still intact.
    ///
    /// <https://dom.spec.whatwg.org/#concept-node-remove>
    ///
    /// Two separate rules, and conflating them is the classic error:
    ///
    /// - a boundary point *inside* the removed subtree collapses to where that
    ///   subtree used to be, `(parent, index)`;
    /// - a boundary point in the parent *after* the removed node shifts down
    ///   by one, because the parent now has one fewer child.
    ///
    /// A range with one point inside and one outside is ordinary, and must end
    /// up collapsed at the removal site on one side only.
    fn ranges_before_remove(&mut self, node: NodeId) {
        // Same early-out as the insertion rule, and it matters more here: the
        // work below includes an `index_of`, which walks the child list.
        if self.ranges.is_empty() {
            return;
        }
        let Some(parent) = self.get(node).and_then(Node::parent) else {
            return;
        };
        let Some(index) = index_of(self, node) else {
            return;
        };

        // Which boundary points sit inside the doomed subtree, worked out
        // before anything is touched: `is_inclusive_ancestor` needs the tree
        // intact, and the loop below is about to change it.
        let inside: Vec<(usize, bool, bool)> = self
            .ranges
            .iter()
            .enumerate()
            .filter_map(|(i, slot)| {
                let range = slot.range?;
                Some((
                    i,
                    is_inclusive_ancestor(self, node, range.start.node()),
                    is_inclusive_ancestor(self, node, range.end.node()),
                ))
            })
            .collect();

        for (i, start_inside, end_inside) in inside {
            let Some(slot) = self.ranges.get_mut(i) else {
                continue;
            };
            let Some(range) = slot.range.as_mut() else {
                continue;
            };
            if start_inside {
                range.start = BoundaryPoint::new(parent, index);
            } else if range.start.node() == parent && range.start.offset() > index {
                range.start = BoundaryPoint::new(parent, range.start.offset() - 1);
            }
            if end_inside {
                range.end = BoundaryPoint::new(parent, index);
            } else if range.end.node() == parent && range.end.offset() > index {
                range.end = BoundaryPoint::new(parent, range.end.offset() - 1);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Attributes, and the snapshots that record what they were
    // -----------------------------------------------------------------------

    /// Start recording prior-state snapshots on attribute mutation.
    ///
    /// Phase 5 turns this on after the first restyle. It is off during parsing
    /// because there is no prior state to describe: every element is new, and
    /// a snapshot saying "it did not exist" invalidates nothing while costing
    /// a clone of the attribute list per element.
    pub fn record_snapshots(&mut self, on: bool) {
        self.recording_snapshots = on;
    }

    pub fn is_recording_snapshots(&self) -> bool {
        self.recording_snapshots
    }

    /// The snapshot for an element, if it has been mutated since the last
    /// flush.
    pub fn snapshot(&self, id: NodeId) -> Option<&ElementSnapshot> {
        self.snapshots
            .iter()
            .find(|(node, _)| *node == id)
            .map(|(_, snapshot)| snapshot)
    }

    /// Stylo's `has_snapshot` bit, without the bit.
    ///
    /// Derived from the table rather than stored on the node. Two sources of
    /// truth for "does this element have a snapshot" is one more than the
    /// question can support, and the bit exists in Servo because its snapshots
    /// live in a separate map that the element cannot reach.
    pub fn has_snapshot(&self, id: NodeId) -> bool {
        self.snapshot(id).is_some()
    }

    /// How many elements have been mutated since the last flush.
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Take every snapshot, clearing the table.
    ///
    /// **Removing a node does not drop its snapshot**, and that is deliberate.
    /// The record describes what an element looked like as of the last
    /// restyle, and a restyle still needs to know it changed even though it
    /// has since gone — Servo's `SnapshotMap` persists the same way. So the
    /// table can hold records for nodes that no longer resolve, and does.
    ///
    /// That is safe here for a reason that is not true of Servo's pointer
    /// keys: the key is a generational `NodeId`, so a reused slot gets a
    /// different key and a stale record can never be matched to the new
    /// occupant of its slot (see `NodeId::to_opaque`). Without generational
    /// keys this would be the bug rather than the design.
    ///
    /// The cost is that a page which creates, mutates and discards elements
    /// accumulates records until the next flush. Bounded by the restyle
    /// interval — one frame, for anything animating — and worth knowing before
    /// somebody reads a growing table as a leak.
    ///
    /// This is the flush: after it, the next mutation of an element captures
    /// fresh prior state. Stylo calls the equivalent at the start of a restyle
    /// and the `handled_snapshot` bit is what stops it processing one twice --
    /// taking them by value makes that structural rather than a bit somebody
    /// has to remember to set.
    pub fn take_snapshots(&mut self) -> Vec<(NodeId, ElementSnapshot)> {
        core::mem::take(&mut self.snapshots)
    }

    /// Set an attribute, replacing any existing value for that name.
    pub fn set_attribute(
        &mut self,
        id: NodeId,
        name: QualName,
        value: StrTendril,
    ) -> Result<(), TreeError> {
        self.snapshot_attribute_change(id, &name)?;
        let node = self.get_mut(id).ok_or(TreeError::NoSuchNode)?;
        let NodeData::Element { attrs, .. } = node.data_mut() else {
            return Err(TreeError::NoSuchNode);
        };
        match attrs.iter_mut().find(|attr| attr.name == name) {
            Some(existing) => existing.value = value,
            None => attrs.push(Attribute { name, value }),
        }
        Ok(())
    }

    /// Remove an attribute. Returns whether it was there.
    pub fn remove_attribute(&mut self, id: NodeId, name: &QualName) -> Result<bool, TreeError> {
        self.snapshot_attribute_change(id, name)?;
        let node = self.get_mut(id).ok_or(TreeError::NoSuchNode)?;
        let NodeData::Element { attrs, .. } = node.data_mut() else {
            return Err(TreeError::NoSuchNode);
        };
        let before = attrs.len();
        attrs.retain(|attr| attr.name != *name);
        Ok(attrs.len() != before)
    }

    /// Add attributes that are not already present.
    ///
    /// The tree builder's operation, and it is a mutation site like any other:
    /// it goes through the snapshot path rather than reaching into the node,
    /// which is the whole point of doing this in Phase 4.
    pub fn add_attributes_if_missing(
        &mut self,
        id: NodeId,
        incoming: Vec<Attribute>,
    ) -> Result<(), TreeError> {
        let present: Vec<QualName> = self
            .get(id)
            .and_then(Node::attrs)
            .map(|attrs| attrs.iter().map(|attr| attr.name.clone()).collect())
            .unwrap_or_default();

        for attr in incoming {
            if present.contains(&attr.name) {
                continue;
            }
            self.set_attribute(id, attr.name, attr.value)?;
        }
        Ok(())
    }

    /// Capture prior state before an attribute write, and note what changed.
    ///
    /// **The capture happens once per flush, the flag update happens every
    /// time.** A snapshot describes the element as of the last restyle, so a
    /// second write must not overwrite what the first recorded -- otherwise it
    /// describes a change from the second-most-recent value to the most recent,
    /// which is a change that never happened. The flags still accumulate,
    /// because every write since the flush is part of what invalidation has to
    /// account for.
    fn snapshot_attribute_change(&mut self, id: NodeId, name: &QualName) -> Result<(), TreeError> {
        if !self.recording_snapshots {
            return Ok(());
        }
        // Look for an existing record first, and only read the element's
        // attributes when there is none. That is not only to keep the borrow
        // checker happy: capturing is the expensive half -- it clones the
        // attribute list -- and it must happen at most once per flush anyway.
        // Every write after the first is a flag update and nothing more.
        match self.snapshots.iter().position(|(node, _)| *node == id) {
            Some(index) => {
                if let Some((_, snapshot)) = self.snapshots.get_mut(index) {
                    snapshot.note_change(name);
                }
            }
            None => {
                let Some(attrs) = self.get(id).and_then(Node::attrs) else {
                    // Not an element, or a stale handle. The caller is about
                    // to fail for the same reason; nothing to record either
                    // way.
                    return Ok(());
                };
                let mut snapshot = ElementSnapshot::capture(attrs);
                snapshot.note_change(name);
                self.snapshots.push((id, snapshot));
            }
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Structural validation
    // -----------------------------------------------------------------------

    /// Check that the tree is a tree.
    ///
    /// `docs/research/stylo-requirements.md` item 7: *"Each node has at most
    /// one parent; a node appears exactly once in its parent child list; the
    /// child links form a tree. Stylo parallel correctness rests entirely on
    /// this and checks none of it."*
    ///
    /// That last clause is the reason this exists as a crate method rather
    /// than only inside the fuzz harness. Stylo will walk this tree from
    /// several threads on the strength of assumptions it never verifies, so
    /// the verification has to live somewhere it can be called from — a
    /// `debug_assert!(arena.validate().is_ok())` at the start of a traversal
    /// costs nothing in a release build and turns a class of silent
    /// parallel-correctness failure into a loud one.
    ///
    /// Returns the first violation rather than panicking, so the caller
    /// decides. Everything here is O(nodes); it is not for hot paths.
    ///
    /// # How to check this method still works
    ///
    /// Not by a test, because the public API cannot build a corrupt tree —
    /// which is the property the rest of this file exists to provide, so it is
    /// not a gap to be closed. A `validate` that returned `Ok(())`
    /// unconditionally would pass every test in this crate.
    ///
    /// The way to check it is to break the arena on purpose and watch it fail.
    /// Deleting the `first_child` fixup from `detach` produces:
    ///
    /// ```text
    /// the tree stopped being a tree at NodeId { index: 1, generation: 1 }:
    /// no last_child, but the walk found children
    /// ```
    ///
    /// via the mutation fuzz harness, which calls this after every operation.
    /// That is the standing procedure for this method, and the reason it is
    /// written down here is that "the validator is fine, all the tests pass"
    /// is exactly what a broken validator says.
    pub fn validate(&self) -> Result<(), (NodeId, &'static str)> {
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.node.is_none() {
                continue;
            }
            let Ok(index) = u32::try_from(index) else {
                continue;
            };
            let id = NodeId::new(index, slot.generation);
            let Some(node) = self.get(id) else {
                continue;
            };

            // The child list is a well-formed doubly-linked list, every member
            // names this node as its parent, and no member appears twice.
            let children: Vec<NodeId> = self.child_ids(id).collect();

            match node.first_child() {
                Some(first) => {
                    if children.first().copied() != Some(first) {
                        return Err((id, "first_child disagrees with the forward walk"));
                    }
                    if self.get(first).and_then(Node::prev_sibling).is_some() {
                        return Err((id, "the first child has a previous sibling"));
                    }
                }
                None => {
                    if !children.is_empty() {
                        return Err((id, "no first_child, but the walk found children"));
                    }
                }
            }

            match node.last_child() {
                Some(last) => {
                    if children.last().copied() != Some(last) {
                        return Err((id, "last_child is not where the forward walk ends"));
                    }
                    if self.get(last).and_then(Node::next_sibling).is_some() {
                        return Err((id, "the last child has a next sibling"));
                    }
                }
                None => {
                    if !children.is_empty() {
                        return Err((id, "no last_child, but the walk found children"));
                    }
                }
            }

            let mut previous: Option<NodeId> = None;
            for child in &children {
                let Some(child_node) = self.get(*child) else {
                    return Err((id, "a listed child does not resolve"));
                };
                if child_node.parent() != Some(id) {
                    return Err((*child, "a child does not name the parent that lists it"));
                }
                if child_node.prev_sibling() != previous {
                    return Err((*child, "the backward link disagrees with the forward walk"));
                }
                previous = Some(*child);
            }
            // No duplicate-detection scan here, deliberately.
            //
            // The obvious one -- for each child, search the rest of the list --
            // is O(children^2), and a child list can be enormous: 131,072
            // paragraphs under one `<body>` made this two seconds, in a method
            // `dom_mutation` calls after *every* operation.
            //
            // It is also redundant. The downward pass below marks each node as
            // it is visited, so a node appearing twice in one child list is
            // pushed twice and caught the second time, in O(1) rather than
            // O(children). Same property, from the walk that was happening
            // anyway.
        }

        // Acyclic, and inside the depth limit — in one downward pass rather
        // than a `depth()` walk per node.
        //
        // The obvious version calls `depth(id)` for every node, which walks
        // *up* to the root each time and makes this O(nodes x depth). That
        // matters: `dom_mutation` calls this after every single operation, and
        // it made a 1 MB shallow document cost two seconds in the parse
        // harness. Walking down from each root instead carries the depth along
        // and visits every node once.
        //
        // It is also a strictly better cycle check. The upward version relied
        // on `depth`'s internal bound to stop, so a cycle showed up as "this
        // walk went too far" — indistinguishable from a legitimately over-deep
        // node. Here a cycle is a node no root can reach, which is exactly what
        // a cycle is.
        // The document exists and is a root.
        //
        // Checked here because it was not, and both ways of breaking it
        // returned `Ok(())` from this function: removing the document, and
        // giving it a parent. A validator that reports a healthy tree with no
        // document is worse than no validator, because the thing it is trusted
        // for is exactly this.
        match self.get(self.document) {
            None => return Err((self.document, "the document node does not resolve")),
            Some(node) if node.parent().is_some() => {
                return Err((self.document, "the document node has a parent"));
            }
            Some(_) => {}
        }

        let mut visited = vec![false; self.slots.len()];
        let mut stack: Vec<(NodeId, usize)> = Vec::new();

        for (index, slot) in self.slots.iter().enumerate() {
            if slot.node.is_none() {
                continue;
            }
            let Ok(index32) = u32::try_from(index) else {
                continue;
            };
            let id = NodeId::new(index32, slot.generation);
            if self.get(id).and_then(Node::parent).is_none() {
                stack.push((id, 0));
            }
        }

        while let Some((id, depth)) = stack.pop() {
            if depth > MAX_DEPTH {
                return Err((id, "a node sits past the depth limit"));
            }
            let index = id.index() as usize;
            match visited.get_mut(index) {
                Some(seen) if *seen => {
                    return Err((id, "a node is reachable from more than one parent"));
                }
                Some(seen) => *seen = true,
                None => continue,
            }
            for child in self.child_ids(id) {
                stack.push((child, depth + 1));
            }
        }

        for (index, slot) in self.slots.iter().enumerate() {
            if slot.node.is_none() {
                continue;
            }
            if visited.get(index).copied() == Some(false) {
                let Ok(index32) = u32::try_from(index) else {
                    continue;
                };
                return Err((
                    NodeId::new(index32, slot.generation),
                    "a live node is reachable from no root, so it is in a cycle",
                ));
            }
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Internals
    // -----------------------------------------------------------------------

    /// Free one slot, bumping its generation so every outstanding handle to it
    /// stops resolving. Returns whether anything was freed.
    ///
    /// This is where retirement happens, and it is the branch ADR 018 says
    /// cannot be reached by ordinary testing: `u32::MAX` generations is not a
    /// number a document churns through. The fuzz target forces it directly.
    fn free_slot(&mut self, id: NodeId) -> bool {
        let Some(slot) = self.slots.get_mut(id.index() as usize) else {
            return false;
        };
        if slot.generation != id.generation() || slot.node.is_none() {
            return false;
        }
        slot.node = None;
        self.live = self.live.saturating_sub(1);

        match slot.generation.checked_add(1) {
            Some(next) => {
                slot.generation = next;
                self.free.push(id.index());
            }
            None => {
                // Generations exhausted. Retire the slot permanently rather
                // than wrapping: wrapping would make a stale handle valid
                // again, which is the exact bug this design exists to
                // prevent (§14.3).
                self.retired += 1;
            }
        }
        true
    }

    /// Whether `child` may be inserted under `parent`.
    ///
    /// One upward walk answers both questions that matter, which is why they
    /// are checked together: if `child` is seen on the way from `parent` to
    /// the root, then `parent` is inside `child`'s subtree and the insertion
    /// would build a cycle; and if the walk runs past `MAX_DEPTH`, the
    /// insertion would exceed the depth limit.
    fn check_insertable(&self, parent: NodeId, child: NodeId) -> Result<(), TreeError> {
        if !self.contains(parent) || !self.contains(child) {
            return Err(TreeError::NoSuchNode);
        }
        if parent == child {
            return Err(TreeError::WouldCycle);
        }
        // The document cannot become anybody's child. Without this,
        // `append_child(some_detached_node, document)` succeeded and the
        // document acquired a parent, which makes it a node in somebody
        // else's subtree -- removable along with it, and no longer the root
        // every traversal assumes.
        if child == self.document {
            return Err(TreeError::Immovable);
        }
        // Only a document, a fragment or an element can hold children.
        let can_have_children = matches!(
            self.get(parent).map(Node::data),
            Some(NodeData::Document | NodeData::Fragment | NodeData::Element { .. })
        );
        if !can_have_children {
            return Err(TreeError::CannotHaveChildren);
        }

        // The loop visits `parent` and then each of its ancestors, so the
        // iteration count is `depth(parent) + 1` — which is exactly the depth
        // `child` would end up at. Counting from zero rather than one is the
        // difference between a limit of 512 and a limit of 511, and the only
        // way to be sure which is to say what is being counted.
        let mut resulting_depth = 0usize;
        let mut cursor = Some(parent);
        while let Some(current) = cursor {
            if current == child {
                return Err(TreeError::WouldCycle);
            }
            resulting_depth += 1;
            if resulting_depth > MAX_DEPTH {
                return Err(TreeError::TooDeep);
            }
            cursor = self.get(current).ok_or(TreeError::NoSuchNode)?.parent();
        }

        // `child` may be carrying a subtree, and it is the *deepest node in
        // it* that has to fit under the limit.
        //
        // Checking only the child's own new depth is the easy mistake here,
        // and it leaves the limit trivially bypassable: build a 512-deep tree
        // detached, where every insertion was shallow and legal, then attach
        // its root somewhere deep in one legal-looking move. A depth limit
        // with that hole in it is decoration.
        //
        // The cost is paid only by moves that carry children. The parser's
        // appends are leaves, where `subtree_height` returns 0 after looking
        // at one node.
        let height = self.subtree_height(child)?;
        if resulting_depth.saturating_add(height) > MAX_DEPTH {
            return Err(TreeError::TooDeep);
        }
        Ok(())
    }

    /// How far the deepest descendant of `id` sits below it. A leaf is 0.
    ///
    /// Iterative, with the node's own relative depth carried on the work
    /// stack, and bounded by the number of slots so a corrupted tree ends the
    /// walk rather than the process.
    fn subtree_height(&self, id: NodeId) -> Result<usize, TreeError> {
        let mut height = 0usize;
        let mut stack = vec![(id, 0usize)];
        let mut budget = self.slots.len().saturating_add(1);

        while let Some((current, relative)) = stack.pop() {
            if budget == 0 {
                return Err(TreeError::TooDeep);
            }
            budget -= 1;
            height = height.max(relative);
            // Nothing legal can be deeper than the limit, so a walk that gets
            // there has found either a cycle or a tree that should never have
            // been built. Either way the answer is no.
            if relative > MAX_DEPTH {
                return Err(TreeError::TooDeep);
            }
            let node = self.get(current).ok_or(TreeError::NoSuchNode)?;
            let mut child = node.first_child();
            while let Some(current_child) = child {
                stack.push((current_child, relative.saturating_add(1)));
                child = self
                    .get(current_child)
                    .ok_or(TreeError::NoSuchNode)?
                    .next_sibling();
                if stack.len() > self.slots.len().saturating_add(1) {
                    return Err(TreeError::TooDeep);
                }
            }
        }
        Ok(height)
    }
}

impl Default for Arena {
    fn default() -> Self {
        Self::new()
    }
}
