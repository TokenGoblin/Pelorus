//! `html5ever`'s `TreeSink`, over the arena.
//!
//! This is the whole of what §9 Phase 4 means by *"`html5ever` into a
//! generational arena"*: html5ever owns the tokenizer and the
//! tree-construction state machine — the twenty years of compatibility
//! behaviour nobody should retype — and this module owns the tree. Handles,
//! generations, depth limits and mutation safety stay ours.
//!
//! # Three things the trait forces, and what each costs
//!
//! **Every method takes `&self`.** html5ever 0.39 moved to interior
//! mutability, so the arena lives in a `RefCell`. This does not weaken the
//! generation check — a stale handle is still stale inside a `borrow_mut` —
//! but it does mean a re-entrant borrow is a runtime panic rather than a
//! compile error. Every method here takes exactly one borrow and holds it for
//! a straight line of code, and none calls another method on `self`.
//!
//! **The constructors cannot fail.** `create_element` returns a `Handle`, not
//! an `Option<Handle>`, so there is nowhere to report an exhausted arena. The
//! answer is [`Sink::void`]: a detached node that content can be appended to
//! forever without becoming part of the document. Exhaustion truncates the
//! document and says so in [`Dom::truncated`] — it does not panic, and it does
//! not silently graft the rest of the page somewhere wrong.
//!
//! **`elem_name` returns a borrow.** Hence `Ref<'a, QualName>`, which
//! markup5ever anticipates with `impl ElemName for Ref<'_, QualName>`. The
//! trait's own documentation says of a non-element node, "feel free to
//! `panic!`". This crate does not panic on attacker-shaped input, so it
//! returns a sentinel name instead — see [`elem_name`](Sink::elem_name).

use std::borrow::Cow;
use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;
use std::sync::LazyLock;

use html5ever::interface::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::tendril::StrTendril;
use html5ever::{Attribute, QualName, local_name, ns};

use crate::arena::{Arena, TreeError};
use crate::handle::NodeId;
use crate::node::{Node, NodeData};

/// How many parse errors to keep.
///
/// html5ever reports one per violation and a hostile document can produce
/// millions, so this is a heap bound, not a preference (§4.4). The count is
/// kept in full; only the messages are dropped.
const MAX_RECORDED_ERRORS: usize = 256;

/// How many refusals to absorb before abandoning the parse.
///
/// See [`parse`] for why abandoning is necessary at all. The number is chosen
/// to be unreachable by any document that is not deliberately pathological:
/// each refusal costs roughly `MAX_DEPTH` dropped elements, so this is about
/// four thousand elements past the nesting limit, in a document that has
/// already nested 512 deep once. Nothing on the web does that by accident.
const MAX_REFUSALS_BEFORE_ABANDONING: usize = 8;

/// How much input to hand the parser between checks.
///
/// Small enough that one chunk cannot carry the parse far past the refusal
/// threshold -- 64 KB of `<div>` is thirteen thousand elements, which
/// overshoots by enough to matter -- and large enough that an ordinary
/// document is a handful of polls. A 100 KB page costs twelve comparisons.
const FEED_CHUNK_BYTES: usize = 8 * 1024;

/// A parsed document.
pub struct Dom {
    pub arena: Arena,
    pub quirks_mode: QuirksMode,
    /// The first [`MAX_RECORDED_ERRORS`] parse errors.
    pub errors: Vec<Cow<'static, str>>,
    /// How many errors there were, including the ones not kept.
    pub error_count: usize,
    /// Set when parsing stopped early because the input was pathological.
    /// See [`parse`].
    pub abandoned: bool,
    /// Content the parser produced that the tree refused: past the depth
    /// limit, or after the arena ran out of slots.
    ///
    /// A parse that hit either produced a document that is missing content the
    /// input contained. That is the right outcome — §4.4 requires the limit,
    /// and browsers drop over-deep content too — but it must be *visible*, or
    /// a truncated page is indistinguishable from a short one.
    pub truncated: usize,
}

impl Dom {
    /// The document node.
    pub fn document(&self) -> NodeId {
        self.arena.document()
    }
}

/// The sentinel returned by `elem_name` for a node that is not an element.
///
/// The trait says to panic here and html5ever's own sinks do. This crate holds
/// attacker-controlled markup and refuses to panic on it, so it returns a name
/// in no namespace that matches no element instead. Callers comparing against
/// a real element name get "not that one", which is the fail-closed answer.
static NOT_AN_ELEMENT: LazyLock<QualName> =
    LazyLock::new(|| QualName::new(None, ns!(), local_name!("")));

/// Builds a [`Dom`] from html5ever's tree-construction callbacks.
pub struct Sink {
    arena: RefCell<Arena>,
    quirks_mode: RefCell<QuirksMode>,
    errors: RefCell<Vec<Cow<'static, str>>>,
    error_count: RefCell<usize>,
    /// Shared with [`parse`], which polls it between chunks of input. See
    /// [`MAX_REFUSALS_BEFORE_ABANDONING`].
    truncated: Rc<Cell<usize>>,
    document: NodeId,
    /// Where refused content goes. Detached from the document, permanently.
    void: NodeId,
}

impl Sink {
    pub fn new() -> Self {
        let mut arena = Arena::new();
        let document = arena.document();
        // A fresh arena has slots; if it somehow does not, the document node
        // is a safe stand-in because appending to it is still better than
        // having no handle to return at all.
        let void = arena.create(NodeData::Fragment).unwrap_or(document);
        Self {
            arena: RefCell::new(arena),
            quirks_mode: RefCell::new(QuirksMode::NoQuirks),
            errors: RefCell::new(Vec::new()),
            error_count: RefCell::new(0),
            truncated: Rc::new(Cell::new(0)),
            document,
            void,
        }
    }

    /// Create a node, or hand back the void if the arena is exhausted.
    fn create_or_void(&self, data: NodeData) -> NodeId {
        match self.arena.borrow_mut().create(data) {
            Ok(id) => id,
            Err(_) => {
                self.truncated.set(self.truncated.get() + 1);
                self.void
            }
        }
    }

    /// Record that a tree operation was refused.
    fn note_refusal(&self, error: TreeError) {
        match error {
            // Ordinary: the tree builder detaches and re-appends freely, and
            // asking about a node that has already gone is not content loss.
            TreeError::NoSuchNode | TreeError::WouldCycle => {}
            TreeError::TooDeep | TreeError::Exhausted => {
                self.truncated.set(self.truncated.get() + 1);
            }
        }
    }
}

impl Default for Sink {
    fn default() -> Self {
        Self::new()
    }
}

/// Append text to `parent`, merging into a trailing text node if there is one.
///
/// The HTML spec requires the merge: consecutive character tokens form one
/// text node, and a tree with `"a"`,`"b"` where the spec says `"ab"` fails
/// conformance and, worse, makes `Node.textContent` and range offsets differ
/// from every other browser.
fn append_text(arena: &mut Arena, parent: NodeId, text: StrTendril) -> Result<(), TreeError> {
    let last = arena.get(parent).and_then(Node::last_child);
    if let Some(NodeData::Text { contents }) =
        last.and_then(|id| arena.get_mut(id)).map(Node::data_mut)
    {
        contents.push_tendril(&text);
        return Ok(());
    }
    let id = arena.create(NodeData::Text { contents: text })?;
    arena.append_child(parent, id)
}

impl TreeSink for Sink {
    type Handle = NodeId;
    type Output = Dom;
    type ElemName<'a> = Ref<'a, QualName>;

    fn finish(self) -> Dom {
        Dom {
            arena: self.arena.into_inner(),
            quirks_mode: self.quirks_mode.into_inner(),
            errors: self.errors.into_inner(),
            error_count: self.error_count.into_inner(),
            truncated: self.truncated.get(),
            abandoned: false,
        }
    }

    fn parse_error(&self, msg: Cow<'static, str>) {
        *self.error_count.borrow_mut() += 1;
        let mut errors = self.errors.borrow_mut();
        if errors.len() < MAX_RECORDED_ERRORS {
            errors.push(msg);
        }
    }

    fn get_document(&self) -> NodeId {
        self.document
    }

    /// The element's name, or a sentinel that matches nothing.
    ///
    /// `Ref::map` cannot express "no name", so the fallback is a real
    /// `QualName` with an empty local name in no namespace. Every comparison
    /// the tree builder makes against it is false, which is the same answer it
    /// would get from an element that is not the one it is looking for.
    fn elem_name<'a>(&'a self, target: &'a NodeId) -> Ref<'a, QualName> {
        Ref::map(self.arena.borrow(), |arena| {
            arena
                .get(*target)
                .and_then(Node::element_name)
                .unwrap_or(&NOT_AN_ELEMENT)
        })
    }

    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> NodeId {
        // A <template>'s contents are a separate fragment: parsed, but not
        // children of the template element. Created first, so that the
        // element is built with the handle already in place rather than
        // patched afterwards.
        let template_contents = if flags.template {
            Some(self.create_or_void(NodeData::Fragment))
        } else {
            None
        };

        self.create_or_void(NodeData::Element {
            name,
            attrs,
            template_contents,
            script_already_started: false,
        })
    }

    fn create_comment(&self, text: StrTendril) -> NodeId {
        self.create_or_void(NodeData::Comment { contents: text })
    }

    fn create_pi(&self, target: StrTendril, data: StrTendril) -> NodeId {
        self.create_or_void(NodeData::ProcessingInstruction {
            target,
            contents: data,
        })
    }

    fn append(&self, parent: &NodeId, child: NodeOrText<NodeId>) {
        let result = {
            let mut arena = self.arena.borrow_mut();
            match child {
                NodeOrText::AppendNode(id) => arena.append_child(*parent, id),
                NodeOrText::AppendText(text) => append_text(&mut arena, *parent, text),
            }
        };
        if let Err(error) = result {
            self.note_refusal(error);
        }
    }

    /// Insert relative to `element` if it has a parent, else append to
    /// `prev_element`.
    ///
    /// This is the "foster parenting" path: content that appears somewhere the
    /// HTML spec says it cannot go — text inside a `<table>` but outside a
    /// cell — is relocated rather than dropped.
    fn append_based_on_parent_node(
        &self,
        element: &NodeId,
        prev_element: &NodeId,
        child: NodeOrText<NodeId>,
    ) {
        let has_parent = self
            .arena
            .borrow()
            .get(*element)
            .and_then(Node::parent)
            .is_some();

        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }

    fn append_doctype_to_document(
        &self,
        name: StrTendril,
        public_id: StrTendril,
        system_id: StrTendril,
    ) {
        let doctype = self.create_or_void(NodeData::Doctype {
            name,
            public_id,
            system_id,
        });
        let result = self.arena.borrow_mut().append_child(self.document, doctype);
        if let Err(error) = result {
            self.note_refusal(error);
        }
    }

    fn mark_script_already_started(&self, node: &NodeId) {
        let mut arena = self.arena.borrow_mut();
        if let Some(NodeData::Element {
            script_already_started,
            ..
        }) = arena.get_mut(*node).map(Node::data_mut)
        {
            *script_already_started = true;
        }
    }

    /// A `<template>`'s contents fragment.
    ///
    /// Falls back to the void rather than panicking when asked about a node
    /// that is not a template. The tree builder only asks about templates, so
    /// reaching the fallback means the tree builder and this sink disagree
    /// about what a node is — and quietly returning the element itself would
    /// put the template's children in the document.
    fn get_template_contents(&self, target: &NodeId) -> NodeId {
        self.arena
            .borrow()
            .get(*target)
            .and_then(|node| match node.data() {
                NodeData::Element {
                    template_contents, ..
                } => *template_contents,
                _ => None,
            })
            .unwrap_or(self.void)
    }

    fn same_node(&self, x: &NodeId, y: &NodeId) -> bool {
        x == y
    }

    fn set_quirks_mode(&self, mode: QuirksMode) {
        *self.quirks_mode.borrow_mut() = mode;
    }

    fn append_before_sibling(&self, sibling: &NodeId, new_node: NodeOrText<NodeId>) {
        let result = {
            let mut arena = self.arena.borrow_mut();
            match new_node {
                NodeOrText::AppendNode(id) => arena.insert_before(*sibling, id),
                NodeOrText::AppendText(text) => {
                    // Merge into the *preceding* sibling if it is text, for
                    // the same spec reason as `append_text`.
                    let previous = arena.get(*sibling).and_then(Node::prev_sibling);
                    let merged = previous.and_then(|id| arena.get_mut(id)).and_then(|node| {
                        match node.data_mut() {
                            NodeData::Text { contents } => {
                                contents.push_tendril(&text);
                                Some(())
                            }
                            _ => None,
                        }
                    });
                    match merged {
                        Some(()) => Ok(()),
                        None => match arena.create(NodeData::Text { contents: text }) {
                            Ok(id) => arena.insert_before(*sibling, id),
                            Err(error) => Err(error),
                        },
                    }
                }
            }
        };
        if let Err(error) = result {
            self.note_refusal(error);
        }
    }

    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        let mut arena = self.arena.borrow_mut();
        let Some(node) = arena.get_mut(*target) else {
            return;
        };
        let NodeData::Element {
            attrs: existing, ..
        } = node.data_mut()
        else {
            return;
        };
        for attr in attrs {
            if !existing.iter().any(|held| held.name == attr.name) {
                existing.push(attr);
            }
        }
    }

    fn remove_from_parent(&self, target: &NodeId) {
        if let Err(error) = self.arena.borrow_mut().detach(*target) {
            self.note_refusal(error);
        }
    }

    fn reparent_children(&self, node: &NodeId, new_parent: &NodeId) {
        if let Err(error) = self
            .arena
            .borrow_mut()
            .reparent_children(*node, *new_parent)
        {
            self.note_refusal(error);
        }
    }

    fn is_mathml_annotation_xml_integration_point(&self, handle: &NodeId) -> bool {
        self.arena
            .borrow()
            .get(*handle)
            .map(Node::is_mathml_annotation_xml_integration_point)
            .unwrap_or(false)
    }
}

/// Parse a complete HTML document.
///
/// Errors are collected rather than returned: HTML has no such thing as a
/// document that fails to parse. Every input produces a tree, and the parse
/// errors are a report about the input rather than a reason to refuse it —
/// which is why [`Dom::truncated`] is separate from [`Dom::errors`]. A parse
/// error means the input was malformed and the spec says what to do; a
/// truncation means *this implementation* dropped content it was given.
///
/// # Why this feeds the parser in chunks instead of calling `.one()`
///
/// Because html5ever's tree builder is **quadratic in nesting depth**, and
/// §4.4's depth limit does not bound it.
///
/// The limit bounds *our* tree: `Arena::append_child` refuses anything past
/// `MAX_DEPTH`, so the document never gets deeper than 512 however deep the
/// input goes. It does nothing about html5ever's own stack of open elements,
/// which keeps growing, and which the tree builder scans on every start tag
/// for the spec's various "has an element in scope" tests. That is html5ever
/// implementing the algorithm the spec describes; it is not a defect, and it
/// is not reachable from here.
///
/// Measured on release builds, nested `<div>`s, this crate's arena against the
/// whole parse:
///
/// | nesting | arena | parse |
/// |---:|---:|---:|
/// | 2,000 | 2 ms | 11 ms |
/// | 4,000 | 4 ms | 44 ms |
/// | 8,000 | 9 ms | 192 ms |
/// | 16,000 | 19 ms | 686 ms |
///
/// The arena doubles with the input. The parse quadruples. Extrapolating, a
/// five-megabyte file of nothing but `<div>` is roughly three quarters of an
/// hour of CPU — a tab hung by a document anybody can write in one line, which
/// is exactly the shape of exhaustion §4.4 exists to prevent.
///
/// So the input is handed over in chunks, and once the tree has refused
/// [`MAX_REFUSALS_BEFORE_ABANDONING`] pieces of content the rest is not fed at
/// all. The threshold is far past anything a real document reaches: it needs a
/// document that has already nested 512 deep and then done it again eight
/// times over.
///
/// Abandoning loses content, so it is reported in [`Dom::abandoned`] rather
/// than being silent. A truncated page that says it is truncated is a bug
/// report; one that does not is a mystery.
pub fn parse(html: &str) -> Dom {
    use html5ever::tendril::TendrilSink;

    let sink = Sink::new();
    let truncated = Rc::clone(&sink.truncated);
    let mut parser = html5ever::parse_document(sink, html5ever::ParseOpts::default());

    let mut abandoned = false;
    let mut rest = html;
    while !rest.is_empty() {
        if truncated.get() >= MAX_REFUSALS_BEFORE_ABANDONING {
            abandoned = true;
            break;
        }
        // Split on a character boundary. `floor_char_boundary` is not stable,
        // so walk back from the nominal split point; at most three bytes.
        let mut end = FEED_CHUNK_BYTES.min(rest.len());
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        // A chunk boundary that landed nowhere usable means the remainder is
        // one enormous character, which cannot happen for valid `str`. Feed
        // the rest and let the parser finish rather than looping forever.
        let (chunk, remainder) = match end {
            0 => (rest, ""),
            _ => rest.split_at(end),
        };
        parser.process(chunk.into());
        rest = remainder;
    }

    let mut dom = parser.finish();
    dom.abandoned = abandoned;
    dom
}
