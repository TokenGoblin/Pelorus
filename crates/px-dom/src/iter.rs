//! Traversal.
//!
//! Every walk here is iterative and carries its own work stack. None of them
//! recurses over tree depth, which is the structural half of the Phase 4 gate:
//! a hundred thousand levels of nesting overflows the stack of any recursive
//! tree walk, and a DOM has walks everywhere.
//!
//! # Mutation safety
//!
//! These iterators borrow the arena, so the compiler already refuses to let a
//! caller mutate the tree mid-walk. That is most of the problem, but not the
//! interesting part of it — the interesting part is a caller who *collects*
//! handles from a walk, mutates, and then uses what it collected.
//!
//! That case is safe here for a reason that has nothing to do with borrowing:
//! a collected handle whose node was removed stops resolving, so the second
//! loop gets `None` rather than whatever now occupies the slot. Mutation
//! safety in this DOM is the generation check, and iteration merely does not
//! undermine it.

use crate::arena::Arena;
use crate::handle::NodeId;

/// A node's children, in order.
pub struct Children<'a> {
    arena: &'a Arena,
    next: Option<NodeId>,
    /// Bounds the walk at the number of slots that exist. A sibling ring
    /// cannot be built through the public API; this is what makes that a
    /// checked fact rather than an assumption held by the code that would
    /// hang if it were wrong.
    budget: usize,
}

impl<'a> Children<'a> {
    pub(crate) fn new(arena: &'a Arena, parent: NodeId) -> Self {
        let next = arena.get(parent).and_then(|node| node.first_child());
        Self {
            arena,
            next,
            budget: arena.slot_count(),
        }
    }
}

impl Iterator for Children<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.budget == 0 {
            return None;
        }
        self.budget -= 1;
        let current = self.next?;
        self.next = self.arena.get(current).and_then(|node| node.next_sibling());
        Some(current)
    }
}

/// A subtree in document order: a node, then its descendants, depth first.
///
/// Document order is what ranges, selection, and `compareDocumentPosition` are
/// defined against, so it is the one traversal the rest of the engine will
/// keep asking for.
pub struct Descendants<'a> {
    arena: &'a Arena,
    /// Explicit work stack. Children are pushed in reverse so that popping
    /// yields them left to right.
    stack: Vec<NodeId>,
    budget: usize,
}

impl<'a> Descendants<'a> {
    pub(crate) fn new(arena: &'a Arena, root: NodeId) -> Self {
        let stack = if arena.contains(root) {
            vec![root]
        } else {
            Vec::new()
        };
        Self {
            arena,
            stack,
            budget: arena.slot_count(),
        }
    }
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        loop {
            if self.budget == 0 {
                return None;
            }
            let current = self.stack.pop()?;
            self.budget -= 1;

            let Some(node) = self.arena.get(current) else {
                // Stale handle mid-walk. Skip it: the alternative is ending
                // the traversal early and silently reporting a partial tree
                // as a whole one.
                continue;
            };

            let mut child = node.first_child();
            let mut children = Vec::new();
            while let Some(current_child) = child {
                children.push(current_child);
                child = self
                    .arena
                    .get(current_child)
                    .and_then(|node| node.next_sibling());
                if children.len() > self.budget {
                    break;
                }
            }
            for child in children.into_iter().rev() {
                self.stack.push(child);
            }

            return Some(current);
        }
    }
}

/// A node's ancestors, nearest first.
pub struct Ancestors<'a> {
    arena: &'a Arena,
    next: Option<NodeId>,
    budget: usize,
}

impl<'a> Ancestors<'a> {
    pub(crate) fn new(arena: &'a Arena, id: NodeId) -> Self {
        let next = arena.get(id).and_then(|node| node.parent());
        Self {
            arena,
            next,
            // One more than the depth limit: a walk that would exceed it is
            // truncated rather than run, and the caller sees a short ancestor
            // chain rather than a hang.
            budget: crate::arena::MAX_DEPTH + 1,
        }
    }
}

impl Iterator for Ancestors<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        if self.budget == 0 {
            return None;
        }
        self.budget -= 1;
        let current = self.next?;
        self.next = self.arena.get(current).and_then(|node| node.parent());
        Some(current)
    }
}
