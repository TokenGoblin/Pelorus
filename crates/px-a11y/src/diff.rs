//! What changed between two builds.
//!
//! Platform accessibility APIs are event-driven: a screen reader is told
//! "this node's name changed", not handed a new tree. [`diff`] turns two
//! builds into those events.
//!
//! # Identity
//!
//! Nodes are matched by the DOM node they report ([`A11yNode::dom_node`]),
//! because an `A11yId` is deliberately invalid across builds. A DOM node that
//! stays in the document and stays exposed is "the same node" even if its
//! role, name or place in the tree changed; a node whose DOM node was removed
//! is `Removed`, and the same DOM node appearing in the new tree and not the
//! old one is `Added`. A DOM node that becomes collapsed out of the tree
//! (`display: none`, `aria-hidden`) is therefore `Removed`, and becoming
//! visible again is `Added`, which is also what platforms expect.
//!
//! # Order
//!
//! Deterministic: all `Removed` first, in the old tree's document order; then,
//! walking the new tree in document order, each node's changes in the fixed
//! order `Added | Moved, RoleChanged, NameChanged, DescriptionChanged,
//! StatesChanged, PropsChanged, BoundsChanged, ChildrenChanged`.
//!
//! # Not detected
//!
//! A change to the new tree's `A11yNode::tab_index` alone (focusability is in
//! `States`, so a focusable/unfocusable flip is reported as `StatesChanged`;
//! a `tabindex` value change that keeps the node focusable is not).

use px_dom::NodeId;

use crate::input::Bounds;
use crate::role::Role;
use crate::state::{Props, States};
use crate::tree::{A11yNode, AccessibilityTree};

/// One change, keyed by DOM node.
#[derive(Clone, Debug, PartialEq)]
pub enum Change {
    /// The node is in the new tree and was not in the old one.
    Added {
        node: NodeId,
        parent: Option<NodeId>,
    },
    /// The node was in the old tree and is not in the new one.
    Removed {
        node: NodeId,
    },
    /// The node's parent in the tree differs (re-parenting by `aria-owns`, or
    /// an intermediate node appearing or disappearing).
    Moved {
        node: NodeId,
        old_parent: Option<NodeId>,
        new_parent: Option<NodeId>,
    },
    RoleChanged {
        node: NodeId,
        old: Role,
        new: Role,
    },
    NameChanged {
        node: NodeId,
        old: String,
        new: String,
    },
    DescriptionChanged {
        node: NodeId,
        old: String,
        new: String,
    },
    StatesChanged {
        node: NodeId,
        old: States,
        new: States,
    },
    /// Level, position in set, live region, value, range or `href` changed.
    PropsChanged {
        node: NodeId,
        old: Box<Props>,
        new: Box<Props>,
    },
    BoundsChanged {
        node: NodeId,
        old: Option<Bounds>,
        new: Option<Bounds>,
    },
    /// The sequence of children (by DOM node) differs.
    ChildrenChanged {
        node: NodeId,
    },
}

impl Change {
    /// The DOM node the change is about.
    #[must_use]
    pub fn node(&self) -> NodeId {
        match self {
            Change::Added { node, .. }
            | Change::Removed { node }
            | Change::Moved { node, .. }
            | Change::RoleChanged { node, .. }
            | Change::NameChanged { node, .. }
            | Change::DescriptionChanged { node, .. }
            | Change::StatesChanged { node, .. }
            | Change::PropsChanged { node, .. }
            | Change::BoundsChanged { node, .. }
            | Change::ChildrenChanged { node } => *node,
        }
    }
}

fn dom_of(tree: &AccessibilityTree, index: Option<u32>) -> Option<NodeId> {
    tree.raw_nodes()
        .get(index? as usize)
        .map(A11yNode::dom_node)
}

fn child_doms(tree: &AccessibilityTree, n: &A11yNode) -> Vec<NodeId> {
    n.children
        .iter()
        .filter_map(|c| dom_of(tree, Some(*c)))
        .collect()
}

/// The changes that turn `old` into `new`.
#[must_use]
pub fn diff(old: &AccessibilityTree, new: &AccessibilityTree) -> Vec<Change> {
    let mut out = Vec::new();

    for id in old.all() {
        let Some(n) = old.node(id) else { continue };
        if new.find(n.dom_node()).is_none() {
            out.push(Change::Removed { node: n.dom_node() });
        }
    }

    for id in new.all() {
        let Some(n) = new.node(id) else { continue };
        let dom = n.dom_node();
        let new_parent = dom_of(new, n.parent);
        let Some(old_id) = old.find(dom) else {
            out.push(Change::Added {
                node: dom,
                parent: new_parent,
            });
            continue;
        };
        let Some(o) = old.node(old_id) else { continue };
        let old_parent = dom_of(old, o.parent);
        if old_parent != new_parent {
            out.push(Change::Moved {
                node: dom,
                old_parent,
                new_parent,
            });
        }
        if o.role != n.role {
            out.push(Change::RoleChanged {
                node: dom,
                old: o.role,
                new: n.role,
            });
        }
        if o.name != n.name {
            out.push(Change::NameChanged {
                node: dom,
                old: o.name.clone(),
                new: n.name.clone(),
            });
        }
        if o.description != n.description {
            out.push(Change::DescriptionChanged {
                node: dom,
                old: o.description.clone(),
                new: n.description.clone(),
            });
        }
        if o.states != n.states {
            out.push(Change::StatesChanged {
                node: dom,
                old: o.states,
                new: n.states,
            });
        }
        if o.props != n.props {
            out.push(Change::PropsChanged {
                node: dom,
                old: Box::new(o.props.clone()),
                new: Box::new(n.props.clone()),
            });
        }
        if o.bounds != n.bounds {
            out.push(Change::BoundsChanged {
                node: dom,
                old: o.bounds,
                new: n.bounds,
            });
        }
        if child_doms(old, o) != child_doms(new, n) {
            out.push(Change::ChildrenChanged { node: dom });
        }
    }
    out
}
