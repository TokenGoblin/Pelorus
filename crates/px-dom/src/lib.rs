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
//! a hundred thousand stack frames. Every traversal in [`iter`] carries an
//! explicit work stack.
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

pub use arena::{Arena, MAX_DEPTH, TreeError};
pub use handle::NodeId;
pub use iter::{Ancestors, Children, Descendants};
pub use node::{Node, NodeData};
pub use range::{BoundaryPoint, Position, Range, RangeId};
pub use sink::{Dom, ParseOptions, Sink, parse, parse_fragment, parse_with};

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
        for id in self.descendants(self.document()) {
            if id == a {
                return Some(true);
            }
            if id == b {
                return Some(false);
            }
        }
        None
    }
}
