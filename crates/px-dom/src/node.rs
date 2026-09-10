//! What a node is.
//!
//! A node holds five links and a payload. It does **not** hold its children:
//! they live in the arena beside it and are named by handle. That is not a
//! stylistic choice, it is what makes a 100,000-level document droppable —
//! `ci/gate-dom.sh` checks the source for `Box<Node>`, `Vec<Node>`, `Rc<Node>`
//! and `Arc<Node>` for exactly this reason.
//!
//! The recursive `Drop` nobody writes on purpose is the classic
//! deep-nesting crash: dropping a node drops its children, which drops
//! theirs, and the compiler wrote all of it. With the tree flat in a `Vec`,
//! dropping a document is dropping one allocation.

use html5ever::tendril::StrTendril;
use html5ever::{Attribute, QualName};

use crate::handle::NodeId;

/// A node's payload.
///
/// The string and name types come from html5ever rather than being converted
/// at the boundary. Converting would mean every text run and every attribute
/// name is copied once on the way in and once on the way out for no gain —
/// `StrTendril` is already a refcounted slice, which is the representation a
/// tokenizer wants to hand over and a serialiser wants to receive.
#[derive(Clone, Debug)]
pub enum NodeData {
    /// The document root. Exactly one per arena, at the first slot.
    Document,
    /// A document fragment: the root of a detached subtree, and what a
    /// `<template>`'s contents hang from.
    Fragment,
    /// `<!DOCTYPE …>`.
    Doctype {
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    },
    /// A run of character data.
    Text { contents: StrTendril },
    /// `<!-- … -->`.
    Comment { contents: StrTendril },
    /// An element.
    Element {
        name: QualName,
        attrs: Vec<Attribute>,
        /// A `<template>`'s contents live in a separate fragment, per the
        /// HTML spec: they are parsed but are not children of the template
        /// element itself.
        template_contents: Option<NodeId>,
        /// Set for a `<script>` the parser has already run, and for an
        /// `<input>`/`<option>` whose state the parser has fixed.
        mathml_annotation_xml_integration_point: bool,
    },
    /// `<?target data?>`.
    ProcessingInstruction {
        target: StrTendril,
        contents: StrTendril,
    },
}

/// A node: five links and a payload.
///
/// The links are 40 bytes together, measured and argued in ADR 018. Five is
/// the minimum for O(1) insertion and removal at either end of a child list
/// plus upward traversal, which is what a DOM's mutation API needs.
#[derive(Clone, Debug)]
pub struct Node {
    pub(crate) parent: Option<NodeId>,
    pub(crate) first_child: Option<NodeId>,
    pub(crate) last_child: Option<NodeId>,
    pub(crate) prev_sibling: Option<NodeId>,
    pub(crate) next_sibling: Option<NodeId>,
    pub(crate) data: NodeData,
}

impl Node {
    pub(crate) fn new(data: NodeData) -> Self {
        Self {
            parent: None,
            first_child: None,
            last_child: None,
            prev_sibling: None,
            next_sibling: None,
            data,
        }
    }

    /// This node's payload.
    pub fn data(&self) -> &NodeData {
        &self.data
    }

    /// This node's parent, if it has one.
    ///
    /// Returns a handle, not a node: resolving it is a separate step that can
    /// fail, and collapsing the two would be the infallible accessor §4.1
    /// forbids.
    pub fn parent(&self) -> Option<NodeId> {
        self.parent
    }

    pub fn first_child(&self) -> Option<NodeId> {
        self.first_child
    }

    pub fn last_child(&self) -> Option<NodeId> {
        self.last_child
    }

    pub fn prev_sibling(&self) -> Option<NodeId> {
        self.prev_sibling
    }

    pub fn next_sibling(&self) -> Option<NodeId> {
        self.next_sibling
    }

    /// The element name, if this is an element.
    pub fn element_name(&self) -> Option<&QualName> {
        match &self.data {
            NodeData::Element { name, .. } => Some(name),
            _ => None,
        }
    }

    /// This element's attributes, if this is an element.
    pub fn attrs(&self) -> Option<&[Attribute]> {
        match &self.data {
            NodeData::Element { attrs, .. } => Some(attrs.as_slice()),
            _ => None,
        }
    }

    /// The character data, if this is a text node.
    pub fn text(&self) -> Option<&StrTendril> {
        match &self.data {
            NodeData::Text { contents } => Some(contents),
            _ => None,
        }
    }

    pub(crate) fn detach_links(&mut self) {
        self.parent = None;
        self.prev_sibling = None;
        self.next_sibling = None;
    }
}
