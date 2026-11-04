//! IPC-safe data: what the content process forwards to the browser process.
//!
//! # Not the wire format
//!
//! These are plain, owned, reference-free types with explicit size limits.
//! **They define no encoding.** IPC in this project is postcard over typed
//! enums, and `px-ipc` owns the wire: this crate has no `serde` or `postcard`
//! dependency and must not gain one. When the broker forwards these, `px-ipc`
//! either derives its serialisation for a mirror of these types or, after the
//! ADR a new dependency needs, for these types directly. `Role`, `States`,
//! `Props`, `Bounds` and the other leaf types are fieldless enums and plain
//! structs on purpose, so either route is mechanical.
//!
//! # Trust
//!
//! The receiver is the more privileged process and the sender is the process
//! that renders hostile pages. [`A11yUpdate::validate`] is the receiver's
//! gate: it must be called on every decoded update before anything is done
//! with it, and it fails closed. It checks counts, string lengths, NULs,
//! non-finite numbers, duplicate keys and part numbering; it does not (cannot)
//! check that the content of a name is truthful. Keys are opaque 64-bit
//! values naming DOM nodes of the sending process; the receiver must treat
//! them as labels only. Authority comes from the channel, not from anything
//! in an update: there is no tab id, origin or identity field here and none
//! may be added.
//!
//! # Size
//!
//! A tree has up to [`crate::MAX_NODES`] nodes; an update carries at most
//! [`MAX_UPDATE_NODES`], so a snapshot is a sequence of parts
//! (`part` / `parts`), as is a large delta. Children are not listed: each
//! node carries its parent and its index among siblings, which keeps every
//! node bounded no matter how wide its parent is.

use std::collections::HashSet;

use px_dom::NodeId;

use crate::attr::MAX_STRING;
use crate::diff::{Change, diff};
use crate::input::Bounds;
use crate::role::Role;
use crate::state::{Props, States};
use crate::tree::{A11yNode, AccessibilityTree};

/// The most nodes in one update.
pub const MAX_UPDATE_NODES: usize = 1024;
/// The most removed keys in one update.
pub const MAX_UPDATE_REMOVED: usize = 4096;
/// The most parts a logical update may be split into: enough for a tree at
/// the node cap plus its removals.
pub const MAX_UPDATE_PARTS: u32 = 1024;

/// A key naming a node across updates: the sending process's `NodeId`,
/// packed. Never zero.
pub type NodeKey = u64;

/// The key for a DOM node.
#[must_use]
pub fn key_of(node: NodeId) -> NodeKey {
    node.to_opaque().get() as u64
}

/// One node as sent.
#[derive(Clone, Debug, PartialEq)]
pub struct WireNode {
    pub key: NodeKey,
    pub parent: Option<NodeKey>,
    /// Position among the parent's children.
    pub index_in_parent: u32,
    pub role: Role,
    pub name: String,
    pub description: String,
    pub states: States,
    pub props: Props,
    pub bounds: Option<Bounds>,
    pub tab_index: Option<i32>,
}

/// Whether an update replaces everything or amends it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateKind {
    /// The receiver discards what it holds and rebuilds from the parts.
    Snapshot,
    /// Upsert `nodes`, delete `removed`.
    Delta,
}

/// A bounded update from the content process.
#[derive(Clone, Debug, PartialEq)]
pub struct A11yUpdate {
    pub kind: UpdateKind,
    /// The key of the root node, in the first part of a snapshot.
    pub root: Option<NodeKey>,
    /// This part, from 0.
    pub part: u32,
    /// How many parts make up the logical update (at least 1).
    pub parts: u32,
    pub nodes: Vec<WireNode>,
    pub removed: Vec<NodeKey>,
}

/// Why an update was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateError {
    TooManyNodes,
    TooManyRemoved,
    BadParts,
    StringTooLong,
    StringHasNul,
    ZeroKey,
    DuplicateKey,
    NonFiniteNumber,
    RootInvalid,
    SnapshotRemovesNodes,
}

fn node_error(n: &WireNode) -> Option<UpdateError> {
    if n.key == 0 {
        return Some(UpdateError::ZeroKey);
    }
    if n.parent == Some(0) {
        return Some(UpdateError::ZeroKey);
    }
    let strings = [
        Some(n.name.as_str()),
        Some(n.description.as_str()),
        n.props.value.as_deref(),
        n.props.href.as_deref(),
    ];
    for s in strings.into_iter().flatten() {
        if s.len() > MAX_STRING {
            return Some(UpdateError::StringTooLong);
        }
        if s.contains('\0') {
            return Some(UpdateError::StringHasNul);
        }
    }
    if let Some(r) = n.props.range {
        for v in [r.now, r.min, r.max].into_iter().flatten() {
            if !v.is_finite() {
                return Some(UpdateError::NonFiniteNumber);
            }
        }
    }
    None
}

impl A11yUpdate {
    /// Check an update received from another process. Fail closed.
    ///
    /// # Errors
    ///
    /// The first limit the update breaks.
    pub fn validate(&self) -> Result<(), UpdateError> {
        if self.nodes.len() > MAX_UPDATE_NODES {
            return Err(UpdateError::TooManyNodes);
        }
        if self.removed.len() > MAX_UPDATE_REMOVED {
            return Err(UpdateError::TooManyRemoved);
        }
        if self.parts == 0 || self.parts > MAX_UPDATE_PARTS || self.part >= self.parts {
            return Err(UpdateError::BadParts);
        }
        if self.kind == UpdateKind::Snapshot && !self.removed.is_empty() {
            return Err(UpdateError::SnapshotRemovesNodes);
        }
        if self.removed.contains(&0) {
            return Err(UpdateError::ZeroKey);
        }
        if let Some(root) = self.root {
            // A root key only makes sense in the first part of a snapshot, and
            // must be a key of this update's nodes.
            let ok = self.kind == UpdateKind::Snapshot
                && self.part == 0
                && root != 0
                && self.nodes.iter().any(|n| n.key == root);
            if !ok {
                return Err(UpdateError::RootInvalid);
            }
        }
        let mut seen = HashSet::with_capacity(self.nodes.len());
        for n in &self.nodes {
            if let Some(e) = node_error(n) {
                return Err(e);
            }
            if !seen.insert(n.key) {
                return Err(UpdateError::DuplicateKey);
            }
        }
        Ok(())
    }

    /// The whole tree as a sequence of snapshot parts.
    #[must_use]
    pub fn snapshot(tree: &AccessibilityTree) -> Vec<A11yUpdate> {
        let nodes: Vec<WireNode> = tree
            .all()
            .into_iter()
            .filter_map(|id| tree.node(id).map(|n| wire_node(tree, n)))
            .collect();
        let root = tree
            .root()
            .and_then(|r| tree.node(r))
            .map(|n| key_of(n.dom_node()));
        split(UpdateKind::Snapshot, root, nodes, Vec::new())
    }

    /// The changes from `old` to `new` as delta parts: every added or changed
    /// node of `new` once, and the keys of removed nodes.
    #[must_use]
    pub fn delta(old: &AccessibilityTree, new: &AccessibilityTree) -> Vec<A11yUpdate> {
        let mut removed = Vec::new();
        let mut touched: HashSet<NodeId> = HashSet::new();
        let mut order: Vec<NodeId> = Vec::new();
        for change in diff(old, new) {
            match change {
                Change::Removed { node } => removed.push(key_of(node)),
                other => {
                    let node = other.node();
                    if touched.insert(node) {
                        order.push(node);
                    }
                }
            }
        }
        let nodes: Vec<WireNode> = order
            .into_iter()
            .filter_map(|d| new.find(d).and_then(|id| new.node(id)))
            .map(|n| wire_node(new, n))
            .collect();
        split(UpdateKind::Delta, None, nodes, removed)
    }
}

fn wire_node(tree: &AccessibilityTree, n: &A11yNode) -> WireNode {
    WireNode {
        key: key_of(n.dom_node()),
        parent: n
            .parent
            .and_then(|p| tree.raw_nodes().get(p as usize))
            .map(|p| key_of(p.dom_node())),
        index_in_parent: n.sibling_index,
        role: n.role,
        name: n.name.clone(),
        description: n.description.clone(),
        states: n.states,
        props: n.props.clone(),
        bounds: n.bounds,
        tab_index: n.tab_index,
    }
}

fn split(
    kind: UpdateKind,
    root: Option<NodeKey>,
    nodes: Vec<WireNode>,
    removed: Vec<NodeKey>,
) -> Vec<A11yUpdate> {
    let node_parts = nodes.len().div_ceil(MAX_UPDATE_NODES);
    let removed_parts = removed.len().div_ceil(MAX_UPDATE_REMOVED);
    let parts = node_parts.max(removed_parts).max(1);
    let parts = u32::try_from(parts)
        .unwrap_or(MAX_UPDATE_PARTS)
        .min(MAX_UPDATE_PARTS);
    let mut node_chunks = nodes.chunks(MAX_UPDATE_NODES);
    let mut removed_chunks = removed.chunks(MAX_UPDATE_REMOVED);
    (0..parts)
        .map(|part| A11yUpdate {
            kind,
            root: if part == 0 { root } else { None },
            part,
            parts,
            nodes: node_chunks
                .next()
                .map(<[WireNode]>::to_vec)
                .unwrap_or_default(),
            removed: removed_chunks
                .next()
                .map(<[NodeKey]>::to_vec)
                .unwrap_or_default(),
        })
        .collect()
}
