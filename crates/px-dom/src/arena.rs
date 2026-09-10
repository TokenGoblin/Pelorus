//! The generational arena.
//!
//! Nodes live in a flat `Vec` and are named by `NodeId`. Nothing owns a node
//! but the arena, so dropping a document is dropping one allocation however
//! deep the document was, and every traversal here carries an explicit work
//! stack rather than using the call stack.

use core::num::NonZeroU32;

use crate::handle::NodeId;
use crate::node::{Node, NodeData};

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

/// A DOM tree.
pub struct Arena {
    slots: Vec<Slot>,
    /// Indices of vacant slots. Retired slots are never in here.
    free: Vec<u32>,
    live: usize,
    retired: usize,
    document: NodeId,
}

impl Arena {
    /// A new arena holding nothing but a `Document` node.
    pub fn new() -> Self {
        let mut arena = Self {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
            retired: 0,
            // Replaced immediately below. The arena is never observable in
            // this state: `create` on an empty arena cannot fail, because the
            // only failure is exhaustion and nothing has been allocated.
            document: NodeId::new(0, NonZeroU32::MIN),
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
