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
//! compile error — a whole class of failure the borrow checker used to catch
//! for us, moved to run time.
//!
//! The rule that keeps it safe is **no borrow is held across a call to another
//! method on `self`**, which is weaker than "nothing calls anything" and is
//! what the code actually does. [`append_based_on_parent_node`] delegates to
//! [`append`] and [`append_before_sibling`]; it is sound because the borrow it
//! takes lives in a `let` binding that ends before the delegation, so the
//! `Ref` is dropped at the semicolon.
//!
//! That distinction is the thing to preserve when editing here. Widening a
//! `let has_parent = self.arena.borrow()...;` into a `let arena =
//! self.arena.borrow();` block around the same delegation compiles, passes
//! every test that does not reach that branch, and panics on a page with
//! foster-parented table content.
//!
//! [`append_based_on_parent_node`]: Sink::append_based_on_parent_node
//! [`append`]: Sink::append
//! [`append_before_sibling`]: Sink::append_before_sibling
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

    /// Through `Arena::add_attributes_if_missing`, not by reaching into the
    /// node.
    ///
    /// This is a mutation site, and every mutation site has to go through the
    /// arena so that stylo's prior-state snapshots are recorded. Writing
    /// straight into `attrs` here would work, would be shorter, and would
    /// silently produce a DOM whose style invalidation is wrong the moment
    /// Phase 5 turns recording on — which is exactly the retrofit
    /// `docs/research/stylo-requirements.md` says to avoid by doing this in
    /// Phase 4.
    ///
    /// `ci/gate-dom.sh` checks the source for the shortcut, because this is a
    /// rule that erodes quietly: the direct version is correct in every
    /// observable way today.
    fn add_attrs_if_missing(&self, target: &NodeId, attrs: Vec<Attribute>) {
        if let Err(error) = self
            .arena
            .borrow_mut()
            .add_attributes_if_missing(*target, attrs)
        {
            self.note_refusal(error);
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

    /// <https://html.spec.whatwg.org/#maybe-clone-an-option-into-selectedcontent>
    ///
    /// When an `<option>` closes inside a `<select>` that contains a
    /// `<selectedcontent>`, the option's children are cloned into it. This is
    /// how `<selectedcontent>` renders the chosen option's markup rather than
    /// its text.
    ///
    /// Implemented rather than left as the trait's no-op default, whose
    /// documentation says only that it "will result in a (slightly) incorrect
    /// DOM tree". It does not currently move the conformance number, because
    /// html5ever calls this only for an *explicit* `</option>` -- its own
    /// FIXME, servo/html5ever#712 -- and the corpus cases that exercise
    /// `<selectedcontent>` close the option implicitly. It is here because the
    /// contract says so, and a sink that is correct only where it is currently
    /// measured is a sink that breaks when the measurement improves.
    fn maybe_clone_an_option_into_selectedcontent(&self, option: &NodeId) {
        let mut arena = self.arena.borrow_mut();

        // The nearest <select> ancestor.
        let mut select = None;
        let mut cursor = arena.get(*option).and_then(Node::parent);
        let mut budget = crate::arena::MAX_DEPTH;
        while let Some(current) = cursor {
            if budget == 0 {
                return;
            }
            budget -= 1;
            let Some(node) = arena.get(current) else {
                return;
            };
            if node
                .element_name()
                .is_some_and(|name| name.local == local_name!("select"))
            {
                select = Some(current);
                break;
            }
            cursor = node.parent();
        }
        let Some(select) = select else {
            return;
        };

        // The first <selectedcontent> under it, in document order.
        let Some(target) = arena.descendants(select).find(|id| {
            arena
                .get(*id)
                .and_then(Node::element_name)
                .is_some_and(|name| name.local == local_name!("selectedcontent"))
        }) else {
            return;
        };

        // Replace its contents with a copy of the option's children.
        let Some(existing) = arena.children(target) else {
            return;
        };
        for child in existing {
            let _ = arena.remove_subtree(child);
        }
        let Some(children) = arena.children(*option) else {
            return;
        };
        for child in children {
            match arena.clone_subtree(child) {
                Ok(copy) => {
                    if arena.append_child(target, copy).is_err() {
                        // Past the depth limit, or out of slots. Stop rather
                        // than leaving a half-copied subtree attached.
                        let _ = arena.remove_subtree(copy);
                        return;
                    }
                }
                Err(_) => return,
            }
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

/// Hand `html` to `parser` in bounded chunks, stopping if the tree starts
/// refusing content.
///
/// Shared by [`parse`] and [`parse_fragment`] so that the bound cannot be
/// present in one and forgotten in the other. See [`parse`] for why it exists.
fn feed<S>(mut parser: html5ever::Parser<S>, html: &str, truncated: &Rc<Cell<usize>>) -> Dom
where
    S: TreeSink<Output = Dom>,
{
    use html5ever::tendril::TendrilSink;

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
        // A split point that walked all the way back to zero would mean the
        // remainder is a single character longer than the chunk size, which
        // cannot happen for a valid `str`. Feed the rest rather than loop.
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
/// times over. With the bound in place a million-deep document costs about
/// 65 ms, flat in input size.
///
/// Abandoning loses content, so it is reported in [`Dom::abandoned`] rather
/// than being silent. A truncated page that says it is truncated is a bug
/// report; one that does not is a mystery.
pub fn parse(html: &str) -> Dom {
    parse_with(html, ParseOptions::default())
}

/// How to parse.
///
/// Separate from html5ever's `ParseOpts` on purpose: that type carries knobs
/// this project has no business exposing (`drop_doctype`, `exact_errors`), and
/// a browser that lets a caller ask for a DOCTYPE-free tree has handed out a
/// way to force quirks mode.
#[derive(Clone, Copy, Debug)]
pub struct ParseOptions {
    /// Whether scripting is enabled for this document.
    ///
    /// This is not a preference, it changes the tree: with scripting on, the
    /// contents of a `<noscript>` element are a single text node; with it off
    /// they are parsed as markup. A document parsed with the wrong value has
    /// the wrong DOM, not merely a differently rendered one.
    pub scripting: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        // Enabled, because that is what a browser is. Phase 11 makes it true
        // in fact as well as in the parser.
        Self { scripting: true }
    }
}

/// Parse a complete HTML document with explicit options. See [`parse`].
pub fn parse_with(html: &str, options: ParseOptions) -> Dom {
    let sink = Sink::new();
    let truncated = Rc::clone(&sink.truncated);
    let mut opts = html5ever::ParseOpts::default();
    opts.tree_builder.scripting_enabled = options.scripting;
    feed(html5ever::parse_document(sink, opts), html, &truncated)
}

/// Parse an HTML fragment in the context of `context`, the way `innerHTML`
/// does.
///
/// Carries the same feed bound as [`parse`], for the same reason: a fragment
/// is attacker-controlled markup just as much as a document, and `innerHTML`
/// is a more convenient place to point a nesting bomb than a navigation is.
///
/// `scripting` is the context element's "allows scripting" flag, which changes
/// how `<noscript>` tokenises.
pub fn parse_fragment(
    html: &str,
    context: QualName,
    context_attrs: Vec<Attribute>,
    scripting: bool,
) -> Dom {
    let sink = Sink::new();
    let truncated = Rc::clone(&sink.truncated);
    let mut opts = html5ever::ParseOpts::default();
    opts.tree_builder.scripting_enabled = scripting;
    feed(
        html5ever::parse_fragment(sink, opts, context, context_attrs, scripting),
        html,
        &truncated,
    )
}
