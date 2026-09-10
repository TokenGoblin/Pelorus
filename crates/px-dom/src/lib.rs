#![forbid(unsafe_code)]

//! Generational arena DOM.
//!
//! # The one rule
//!
//! A [`NodeId`] names a slot *and a moment*. Resolving one can fail, always,
//! and every accessor that takes one returns `Option` — §4.1 states it with no
//! exceptions, including private ones, and `ci/gate-dom.sh` checks the source
//! rather than trusting that it stayed true.
//!
//! When a node is removed its slot's generation moves on and every outstanding
//! handle to it stops resolving. Permanently: generations are retired on
//! overflow rather than wrapped, because wrapping would make a stale handle
//! valid again, which is the use-after-free the whole design exists to
//! prevent (§14.3, ADR 018).
//!
//! # The other rule
//!
//! Nothing recurses over tree depth. Nodes do not own each other — they sit
//! flat in the arena and reference each other by handle — so dropping a
//! hundred-thousand-level document drops one allocation rather than unwinding
//! a hundred thousand stack frames. Every traversal — [`Children`],
//! [`Descendants`], [`Ancestors`] — carries an explicit work stack.
//!
//! The `Drop` nobody writes on purpose is the one this guards against. It is
//! written by the compiler, and it appears the moment a node holds a
//! `Vec<Node>` or a `Box<Node>`.

mod arena;
mod handle;
#[cfg(feature = "testing")]
pub mod harness;
mod iter;
mod node;
mod range;
mod sink;
mod snapshot;

pub use arena::{Arena, MAX_DEPTH, TreeError};
pub use handle::{NodeId, OpaqueNodeId};
pub use iter::{Ancestors, Children, Descendants};
pub use node::{Node, NodeData};
pub use range::{BoundaryPoint, Position, Range, RangeId};
pub use sink::{
    Dom, MAX_RECORDED_ERRORS, MAX_REFUSALS_BEFORE_ABANDONING, ParseOptions, Sink, parse,
    parse_fragment, parse_with,
};
pub use snapshot::ElementSnapshot;

impl Arena {
    /// This node's children, in order.
    pub fn child_ids(&self, id: NodeId) -> Children<'_> {
        Children::new(self, id)
    }

    /// This node and its descendants, in document order.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants::new(self, id)
    }

    /// This node's ancestors, nearest first.
    pub fn ancestors(&self, id: NodeId) -> Ancestors<'_> {
        Ancestors::new(self, id)
    }

    /// Whether `a` comes strictly before `b` in document order.
    ///
    /// `None` if either handle is stale, or if they are not in the same tree —
    /// a question with no answer, rather than a `false` that would quietly
    /// claim `b` comes first.
    ///
    /// Document order is what ranges, selection and `compareDocumentPosition`
    /// are defined against, so this is the primitive the rest of the engine
    /// keeps needing.
    ///
    /// # Cost
    ///
    /// A walk from the root: O(nodes), not the O(depth) that comparing
    /// ancestor chains would give. Deliberate for now — the O(depth) version
    /// needs each node's index among its siblings, which is either another
    /// walk or a field kept correct across every mutation, and paying that
    /// before anything has measured this as hot would be guessing. Recorded
    /// here so the guess is visible when something does measure it.
    pub fn precedes(&self, a: NodeId, b: NodeId) -> Option<bool> {
        if !self.contains(a) || !self.contains(b) || a == b {
            return None;
        }
        // Both, or neither. Returning on the first one found gives a definite
        // answer for a pair that has none: with `a` detached and `b` in the
        // document, the walk reaches `b` first and reports "a does not precede
        // b" — and the mirrored call reports "b precedes a", so the relation
        // contradicts itself.
        //
        // `compare_boundary_points` inverts the mirrored answer, so an
        // inconsistent `precedes` produced a range whose start compared as
        // following its own end. The mutation fuzz harness found it once it
        // was allowed to create ranges and detach subtrees in the same run.
        //
        // Nodes outside the document tree are not comparable in document
        // order, and `None` is that answer.
        // Order within whatever tree the two share, not only the document's.
        //
        // Walking from `document()` alone made every pair inside a detached
        // subtree incomparable — which then made `compare_boundary_points`
        // fall through to a definite "before" for pairs it could not order.
        // A range built inside a fragment compared as correctly ordered while
        // being inverted, and only started reporting the truth once a
        // mutation collapsed one endpoint into an ancestor relationship.
        //
        // Detached subtrees have a perfectly good document order of their own.
        // What has no order is a pair in *different* trees, and that is the
        // `None` this returns.
        let root_of = |mut id: NodeId| {
            let mut budget = crate::arena::MAX_DEPTH + 1;
            while let Some(parent) = self.get(id).and_then(Node::parent) {
                if budget == 0 {
                    break;
                }
                budget -= 1;
                id = parent;
            }
            id
        };
        let root = root_of(a);
        if root != root_of(b) {
            return None;
        }

        let mut position_of_a = None;
        let mut position_of_b = None;
        for (index, id) in self.descendants(root).enumerate() {
            if id == a {
                position_of_a = Some(index);
            }
            if id == b {
                position_of_b = Some(index);
            }
            if position_of_a.is_some() && position_of_b.is_some() {
                break;
            }
        }
        match (position_of_a, position_of_b) {
            (Some(first), Some(second)) => Some(first < second),
            _ => None,
        }
    }
}
