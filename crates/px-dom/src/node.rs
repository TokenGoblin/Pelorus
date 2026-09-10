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
use html5ever::{Attribute, QualName, local_name, ns};

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
        /// The HTML spec's "already started" flag on a `<script>`.
        ///
        /// Stored rather than computed because it is genuinely state: it
        /// records something that happened to this element, and nothing about
        /// the element's name or attributes can reconstruct it.
        ///
        /// Note what is *not* stored beside it. Whether an element is a MathML
        /// annotation-xml integration point looks like a flag and is not — it
        /// is a function of the element's name and its `encoding` attribute,
        /// so keeping a copy would mean keeping it correct across every
        /// attribute mutation. See `Node::is_mathml_annotation_xml_integration_point`.
        script_already_started: bool,
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

    /// Whether this is a MathML `annotation-xml` element acting as an HTML
    /// integration point.
    ///
    /// Computed, not stored. The HTML spec defines it as a question about the
    /// element's name and its `encoding` attribute, and the attribute can
    /// change after the element is created — a cached copy would be a second
    /// source of truth that has to be invalidated on every attribute write,
    /// which is the kind of bookkeeping that is right for months and then
    /// silently wrong.
    ///
    /// The comparison is ASCII-case-insensitive because the spec says so:
    /// <https://html.spec.whatwg.org/#html-integration-point>
    pub fn is_mathml_annotation_xml_integration_point(&self) -> bool {
        let NodeData::Element { name, attrs, .. } = &self.data else {
            return false;
        };
        if name.ns != ns!(mathml) || name.local != local_name!("annotation-xml") {
            return false;
        }
        attrs.iter().any(|attr| {
            attr.name.local == local_name!("encoding")
                && (attr.value.eq_ignore_ascii_case("text/html")
                    || attr.value.eq_ignore_ascii_case("application/xhtml+xml"))
        })
    }

    /// The HTML spec's "already started" flag, for a `<script>`.
    pub fn script_already_started(&self) -> bool {
        match &self.data {
            NodeData::Element {
                script_already_started,
                ..
            } => *script_already_started,
            _ => false,
        }
    }

    /// This node's payload, for mutation.
    ///
    /// `pub(crate)` rather than `pub`: the sink needs it to append to a text
    /// run and to set a script's "already started" flag, and both of those are
    /// tree-construction concerns. Handing it to the wider engine would make
    /// it possible to change a node's *kind* out from under whatever holds a
    /// handle to it, which the generation check does not and cannot catch --
    /// the handle stays valid, the node just stops being what it was.
    pub(crate) fn data_mut(&mut self) -> &mut NodeData {
        &mut self.data
    }

    pub(crate) fn detach_links(&mut self) {
        self.parent = None;
        self.prev_sibling = None;
        self.next_sibling = None;
    }
}
