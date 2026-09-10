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
mod iter;
mod node;

pub use arena::{Arena, MAX_DEPTH, TreeError};
pub use handle::NodeId;
pub use iter::{Ancestors, Children, Descendants};
pub use node::{Node, NodeData};

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
}
