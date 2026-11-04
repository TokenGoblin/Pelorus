//! Accessible name and description computation (accname 1.2, "Text
//! Equivalent Computation") with the HTML-AAM element rules for step 2D.
//!
//! # Shape
//!
//! The specification is written as a recursion: a node's text alternative is
//! built from its children's, which are built from theirs. Page depth is
//! attacker-controlled, so none of it recurses here. The recursion becomes an
//! explicit stack of [`Frame`]s. A frame is either the children of a node being
//! read for its content ([`Frame::Content`]) or a list of nodes whose text is
//! being collected on behalf of another node ([`Frame::Refs`]: the targets of
//! `aria-labelledby`, a control's `label` elements, a `fieldset`'s `legend`).
//! [`Computer::step`] runs the ordered steps for one node and either finishes
//! it or says which frame to push; when a frame finishes, its result flows to
//! the frame beneath it, or, for a [`Frame::Refs`] that found nothing, the
//! owning node resumes at the step after the one that spawned the frame.
//!
//! # Steps, in the specification's order
//!
//! * Initialisation: a root whose role prohibits naming has no name.
//! * 2A, hidden and not referenced: a hidden node yields "" unless the walk
//!   started inside a hidden node that was directly referenced.
//! * 2B, `aria-labelledby`, not followed inside another `aria-labelledby` or
//!   `aria-describedby` traversal.
//! * 2E, embedded control: the value of a textbox, the chosen option of a
//!   combobox or listbox, the value of a range, when the control is not the
//!   node being named.
//! * 2C, `aria-label`.
//! * 2D, host language label (HTML-AAM): `label` elements, `alt`, `value` of
//!   buttons, `legend`, `caption`, `figcaption` for a lone `img`, the `label`
//!   attribute of `option` and `optgroup`.
//! * 2F, name from content: for roles that allow it, referenced nodes, and
//!   native text-alternative elements.
//! * 2G, text node.
//! * 2H, recursive name from content: every descendant is read for its content.
//! * 2I, tooltip: `title`; for a text entry control then `placeholder`.
//!
//! "Each node in the subtree is consulted only once": a set of visited nodes
//! is kept per computation. Nodes entered as the *target* of a reference are
//! exempt from the check, otherwise a node that labels itself
//! (`aria-labelledby="self other"`) could never read its own `aria-label`.
//! That exemption cannot loop: `aria-labelledby` is not followed inside a
//! reference traversal, and a `label` that contains the control it labels is
//! reached again only through the control, which is already in the set.
//!
//! # Bounds
//!
//! At most [`MAX_NODES_PER_NAME`] nodes are entered for one name, at most the
//! caller's budget across a whole build, the frame stack is capped, and the
//! accumulated text is capped. When a limit is hit the name is what was
//! collected so far; nothing fails.

use std::collections::HashSet;

use px_dom::{Arena, NodeId};

use crate::attr::{El, MAX_STRING, clean, flatten, push_flat, push_sep, text_of, trim_ws};
use crate::doc::{Doc, input_type};
use crate::role::{NameFrom, Role};

/// The most nodes one name or description computation will enter.
pub const MAX_NODES_PER_NAME: usize = 4096;
const MAX_FRAMES: usize = 2048;
const ACC_CAP: usize = 2 * MAX_STRING;

/// Where the name (or the chosen host-language alternative) came from.
/// Needed so the description does not repeat it (HTML-AAM: "if it was not
/// used as the accessible name").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Src {
    None,
    Author,
    Label,
    Alt,
    Value,
    Caption,
    Legend,
    Content,
    Title,
    Placeholder,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Entry {
    Root,
    RefRoot,
    Child,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    LabelledBy,
    AriaLabel,
    Native(usize),
    Content,
    Tooltip,
}

enum Cand {
    Text(String, Src),
    Refs(Vec<NodeId>, Src),
}

enum Frame {
    Content {
        id: NodeId,
        next: Option<NodeId>,
        acc: String,
        hidden_ok: bool,
        pending_block: bool,
        entry: Entry,
    },
    Refs {
        owner: Option<(NodeId, Entry, bool)>,
        ids: Vec<NodeId>,
        i: usize,
        acc: String,
        resume: Stage,
        src: Src,
        clears_in_ref: bool,
    },
}

enum Eval {
    Done(String),
    Push(Frame),
}

pub(crate) struct Computed {
    pub text: String,
    pub src: Src,
}

pub(crate) struct Computer<'d, 'a> {
    doc: &'d Doc<'a>,
    arena: &'a Arena,
    root: NodeId,
    visited: HashSet<NodeId>,
    in_ref: bool,
    entered: usize,
    budget: &'d mut usize,
    src: Src,
}

fn is_block(local: &str) -> bool {
    matches!(
        local,
        "address"
            | "article"
            | "aside"
            | "blockquote"
            | "br"
            | "caption"
            | "dd"
            | "details"
            | "dialog"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "header"
            | "hgroup"
            | "hr"
            | "legend"
            | "li"
            | "main"
            | "nav"
            | "ol"
            | "optgroup"
            | "option"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "td"
            | "th"
            | "tr"
            | "ul"
    )
}

/// All text under `id`, whitespace-collapsed, capped. Used for `option` text,
/// a `textarea`'s value and the document `title`.
pub(crate) fn text_content(arena: &Arena, id: NodeId, cap: usize) -> String {
    let mut out = String::new();
    for d in arena.descendants(id).take(MAX_NODES_PER_NAME) {
        if out.len() >= cap {
            break;
        }
        if let Some(t) = text_of(arena, d) {
            push_flat(&mut out, t, cap);
        }
    }
    clean(&out)
}

impl<'d, 'a> Computer<'d, 'a> {
    fn new(doc: &'d Doc<'a>, budget: &'d mut usize, root: NodeId) -> Self {
        let mut visited = HashSet::new();
        visited.insert(root);
        Computer {
            doc,
            arena: doc.arena,
            root,
            visited,
            in_ref: false,
            entered: 0,
            budget,
            src: Src::None,
        }
    }

    /// The accessible name of `root`.
    pub fn name(doc: &'d Doc<'a>, budget: &'d mut usize, root: NodeId) -> Computed {
        let mut c = Computer::new(doc, budget, root);
        let Some(info) = doc.info(root) else {
            return Computed {
                text: String::new(),
                src: Src::None,
            };
        };
        if info.role.name_from() == NameFrom::Prohibited {
            return Computed {
                text: String::new(),
                src: Src::None,
            };
        }
        let first = c.enter(root, Entry::Root, false);
        let text = c.run(first);
        Computed {
            text: clean(&text),
            src: if text.trim().is_empty() {
                Src::None
            } else {
                c.src
            },
        }
    }

    /// The accessible description of `root`, given where its name came from.
    pub fn description(
        doc: &'d Doc<'a>,
        budget: &'d mut usize,
        root: NodeId,
        name_src: Src,
    ) -> String {
        let mut c = Computer::new(doc, budget, root);
        let Some(el) = El::of(doc.arena, root) else {
            return String::new();
        };
        // 1. aria-describedby
        let ids = doc.idrefs(&el, "aria-describedby");
        if !ids.is_empty() {
            c.in_ref = true;
            let first = Eval::Push(Frame::Refs {
                owner: None,
                ids,
                i: 0,
                acc: String::new(),
                resume: Stage::Content,
                src: Src::Author,
                clears_in_ref: true,
            });
            return clean(&c.run(first));
        }
        // 2. aria-description
        if let Some(d) = el.attr("aria-description")
            && !trim_ws(d).is_empty()
        {
            return flatten(d);
        }
        // 3. host language
        if el.is("table")
            && name_src != Src::Caption
            && let Some(caption) = first_child_element(doc.arena, root, "caption")
        {
            let first = Eval::Push(Frame::Refs {
                owner: None,
                ids: vec![caption],
                i: 0,
                acc: String::new(),
                resume: Stage::Content,
                src: Src::Caption,
                clears_in_ref: false,
            });
            let text = clean(&c.run(first));
            if !text.is_empty() {
                return text;
            }
        }
        if el.is("input")
            && matches!(input_type(&el).as_str(), "button" | "submit" | "reset")
            && name_src != Src::Value
            && let Some(v) = el.attr("value")
            && !trim_ws(v).is_empty()
        {
            return flatten(v);
        }
        // 4. title, if it was not the name.
        if name_src != Src::Title
            && let Some(t) = el.attr("title")
        {
            return flatten(t);
        }
        String::new()
    }

    fn spend(&mut self) -> bool {
        if *self.budget == 0 || self.entered >= MAX_NODES_PER_NAME {
            return false;
        }
        *self.budget -= 1;
        self.entered += 1;
        true
    }

    fn set_src(&mut self, id: NodeId, src: Src) {
        if id == self.root {
            self.src = src;
        }
    }

    /// Begin a node: the steps that need no children.
    fn enter(&mut self, id: NodeId, entry: Entry, hidden_ok: bool) -> Eval {
        if !self.spend() {
            return Eval::Done(String::new());
        }
        let Some(info) = self.doc.info(id) else {
            return Eval::Done(String::new());
        };
        // 2A.
        if !hidden_ok && (info.hidden || info.vis_hidden) {
            return Eval::Done(String::new());
        }
        // 2G, a text node.
        if let Some(t) = text_of(self.arena, id) {
            let mut s = String::new();
            push_flat(&mut s, t, ACC_CAP);
            return Eval::Done(s);
        }
        let Some(el) = El::of(self.arena, id) else {
            return Eval::Done(String::new());
        };
        self.step(&el, entry, hidden_ok, Stage::LabelledBy)
    }

    fn step(&mut self, el: &El<'a>, entry: Entry, hidden_ok: bool, from: Stage) -> Eval {
        let id = el.id;
        let Some(info) = self.doc.info(id).copied() else {
            return Eval::Done(String::new());
        };
        let role = info.role;

        // 2B.
        if from <= Stage::LabelledBy && !self.in_ref {
            let ids = self.doc.idrefs(el, "aria-labelledby");
            if !ids.is_empty() {
                self.in_ref = true;
                return Eval::Push(Frame::Refs {
                    owner: Some((id, entry, hidden_ok)),
                    ids,
                    i: 0,
                    acc: String::new(),
                    resume: Stage::AriaLabel,
                    src: Src::Author,
                    clears_in_ref: true,
                });
            }
        }

        // 2E and 2C.
        if from <= Stage::AriaLabel {
            if id != self.root
                && role.is_embedded_control()
                && let Some(v) = self.embedded_value(el, role)
            {
                return Eval::Done(v);
            }
            if let Some(label) = el.attr("aria-label")
                && !trim_ws(label).is_empty()
            {
                self.set_src(id, Src::Author);
                return Eval::Done(flatten(label));
            }
        }

        // 2D.
        if role != Role::Presentation {
            let start = match from {
                Stage::Native(k) => k,
                Stage::LabelledBy | Stage::AriaLabel => 0,
                Stage::Content | Stage::Tooltip => usize::MAX,
            };
            if start != usize::MAX {
                let cands = self.native_candidates(el);
                for (k, cand) in cands.into_iter().enumerate().skip(start) {
                    match cand {
                        Cand::Text(text, src) => {
                            if !trim_ws(&text).is_empty() {
                                self.set_src(id, src);
                                return Eval::Done(flatten(&text));
                            }
                        }
                        Cand::Refs(ids, src) => {
                            if !ids.is_empty() {
                                return Eval::Push(Frame::Refs {
                                    owner: Some((id, entry, hidden_ok)),
                                    ids,
                                    i: 0,
                                    acc: String::new(),
                                    resume: Stage::Native(k + 1),
                                    src,
                                    clears_in_ref: false,
                                });
                            }
                        }
                    }
                }
            }
        }

        // 2F / 2H.
        if from <= Stage::Content {
            let allowed = id != self.root
                || entry == Entry::RefRoot
                || role.name_from() == NameFrom::Contents;
            if allowed {
                if el.is("br") {
                    return Eval::Done(" ".to_string());
                }
                let first = self.arena.get(id).and_then(|n| n.first_child());
                if first.is_some() {
                    return Eval::Push(Frame::Content {
                        id,
                        next: first,
                        acc: String::new(),
                        hidden_ok,
                        pending_block: false,
                        entry,
                    });
                }
            }
        }

        // 2I.
        self.tooltip(el)
    }

    fn tooltip(&mut self, el: &El<'a>) -> Eval {
        if let Some(t) = el.attr("title")
            && !trim_ws(t).is_empty()
        {
            self.set_src(el.id, Src::Title);
            return Eval::Done(flatten(t));
        }
        // An image button with nothing else to go on is a submit button.
        if el.is("input") && input_type(el) == "image" {
            self.set_src(el.id, Src::Value);
            return Eval::Done("Submit".to_string());
        }
        let text_entry = el.is("textarea")
            || (el.is("input")
                && matches!(
                    input_type(el).as_str(),
                    "" | "text" | "search" | "email" | "tel" | "url" | "password" | "number"
                ));
        if text_entry {
            for attr in ["placeholder", "aria-placeholder"] {
                if let Some(p) = el.attr(attr)
                    && !trim_ws(p).is_empty()
                {
                    self.set_src(el.id, Src::Placeholder);
                    return Eval::Done(flatten(p));
                }
            }
        }
        Eval::Done(String::new())
    }

    /// HTML-AAM step 2D candidates, in order, for this element.
    fn native_candidates(&self, el: &El<'a>) -> Vec<Cand> {
        let mut out = Vec::new();
        if !el.is_html() {
            return out;
        }
        let labels = self.doc.labels.get(&el.id).cloned().unwrap_or_default();
        let attr_text = |name: &str| el.attr(name).unwrap_or("").to_string();
        match el.local {
            "input" => {
                let ty = input_type(el);
                out.push(Cand::Refs(labels, Src::Label));
                match ty.as_str() {
                    "image" => {
                        out.push(Cand::Text(attr_text("alt"), Src::Alt));
                    }
                    "button" | "submit" | "reset" => {
                        out.push(Cand::Text(attr_text("value"), Src::Value));
                        if !el.has("value") {
                            match ty.as_str() {
                                "submit" => out.push(Cand::Text("Submit".into(), Src::Value)),
                                "reset" => out.push(Cand::Text("Reset".into(), Src::Value)),
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            "textarea" | "select" | "meter" | "progress" | "output" | "button" => {
                out.push(Cand::Refs(labels, Src::Label));
            }
            "img" => {
                out.push(Cand::Text(attr_text("alt"), Src::Alt));
                if !el.has("alt")
                    && !el.has("title")
                    && let Some(caption) = self.lone_figure_caption(el)
                {
                    out.push(Cand::Refs(vec![caption], Src::Caption));
                }
            }
            "area" => out.push(Cand::Text(attr_text("alt"), Src::Alt)),
            "fieldset" => {
                if let Some(legend) = first_child_element(self.arena, el.id, "legend") {
                    out.push(Cand::Refs(vec![legend], Src::Legend));
                }
            }
            "table" => {
                if let Some(caption) = first_child_element(self.arena, el.id, "caption") {
                    out.push(Cand::Refs(vec![caption], Src::Caption));
                }
            }
            "optgroup" | "option" => out.push(Cand::Text(attr_text("label"), Src::Alt)),
            _ => {}
        }
        out
    }

    /// An `img` that is the only content of its `figure`, beside a
    /// `figcaption`: the caption names the image (HTML-AAM `img`).
    fn lone_figure_caption(&self, img: &El<'a>) -> Option<NodeId> {
        let parent = self.arena.get(img.id)?.parent()?;
        if !El::of(self.arena, parent)?.is("figure") {
            return None;
        }
        let mut caption = None;
        for child in self.arena.child_ids(parent).take(64) {
            if child == img.id {
                continue;
            }
            if let Some(t) = text_of(self.arena, child) {
                if trim_ws(t).is_empty() {
                    continue;
                }
                return None;
            }
            match El::of(self.arena, child) {
                Some(e) if e.is("figcaption") && caption.is_none() => caption = Some(child),
                Some(_) => return None,
                None => {}
            }
        }
        caption
    }

    /// 2E. `None` when the control has no value to contribute.
    fn embedded_value(&self, el: &El<'a>, role: Role) -> Option<String> {
        match role {
            Role::TextBox | Role::SearchBox => {
                if el.is("input") {
                    if input_type(el) == "password" {
                        return None;
                    }
                    el.attr("value").map(flatten)
                } else if el.is("textarea") || !el.is_html() || role == Role::TextBox {
                    // A native textarea, or an ARIA textbox such as a
                    // contenteditable element: the value is its text.
                    Some(text_content(self.arena, el.id, MAX_STRING))
                } else {
                    None
                }
            }
            Role::Combobox | Role::Listbox => {
                if !el.is("select") {
                    return None;
                }
                let mut first = None;
                for d in self
                    .arena
                    .descendants(el.id)
                    .skip(1)
                    .take(MAX_NODES_PER_NAME)
                {
                    let Some(opt) = El::of(self.arena, d) else {
                        continue;
                    };
                    if !opt.is("option") {
                        continue;
                    }
                    if opt.has("selected") {
                        return Some(text_content(self.arena, d, MAX_STRING));
                    }
                    first.get_or_insert(d);
                }
                first.map(|d| text_content(self.arena, d, MAX_STRING))
            }
            Role::Slider | Role::SpinButton | Role::ScrollBar => {
                for attr in ["aria-valuetext", "aria-valuenow"] {
                    if let Some(v) = el.attr(attr)
                        && !trim_ws(v).is_empty()
                    {
                        return Some(flatten(v));
                    }
                }
                if el.is("input") {
                    return el.attr("value").map(flatten);
                }
                None
            }
            _ => None,
        }
    }

    fn run(&mut self, first: Eval) -> String {
        let mut stack: Vec<Frame> = Vec::new();
        let mut ret: Option<String> = None;
        self.dispatch(first, &mut stack, &mut ret);
        loop {
            if let Some(r) = ret.take() {
                match stack.last_mut() {
                    None => return r,
                    Some(Frame::Content {
                        acc, pending_block, ..
                    }) => {
                        push_flat(acc, &r, ACC_CAP);
                        if *pending_block {
                            push_sep(acc, ACC_CAP);
                            *pending_block = false;
                        }
                    }
                    Some(Frame::Refs { acc, .. }) => {
                        push_sep(acc, ACC_CAP);
                        push_flat(acc, &r, ACC_CAP);
                    }
                }
            }
            // What the top frame does next. Collected first so the borrow of
            // the stack ends before the stack is pushed to.
            enum Next {
                Enter(NodeId, Entry, bool),
                Skip,
                FinishContent,
                FinishRefs,
            }
            let next = match stack.last_mut() {
                None => return String::new(),
                Some(Frame::Content {
                    next,
                    hidden_ok,
                    pending_block,
                    acc,
                    ..
                }) => match next.take() {
                    None => Next::FinishContent,
                    Some(child) => {
                        *next = self.arena.get(child).and_then(|n| n.next_sibling());
                        if self.visited.insert(child) {
                            let block = El::of(self.arena, child)
                                .is_some_and(|e| e.is_html() && is_block(e.local));
                            if block {
                                push_sep(acc, ACC_CAP);
                                *pending_block = true;
                            }
                            Next::Enter(child, Entry::Child, *hidden_ok)
                        } else {
                            Next::Skip
                        }
                    }
                },
                Some(Frame::Refs { ids, i, .. }) => match ids.get(*i).copied() {
                    None => Next::FinishRefs,
                    Some(target) => {
                        *i += 1;
                        self.visited.insert(target);
                        // A directly referenced hidden node is read in full.
                        let hidden = self
                            .doc
                            .info(target)
                            .is_none_or(|n| n.hidden || n.vis_hidden);
                        Next::Enter(target, Entry::RefRoot, hidden)
                    }
                },
            };
            match next {
                Next::Skip => {}
                Next::Enter(id, entry, hidden_ok) => {
                    let eval = self.enter(id, entry, hidden_ok);
                    self.dispatch(eval, &mut stack, &mut ret);
                }
                Next::FinishContent => {
                    let Some(Frame::Content {
                        id,
                        acc,
                        entry,
                        hidden_ok,
                        ..
                    }) = stack.pop()
                    else {
                        return String::new();
                    };
                    if !acc.trim().is_empty() {
                        self.set_src(id, Src::Content);
                        ret = Some(acc);
                    } else if let Some(el) = El::of(self.arena, id) {
                        let eval = self.step(&el, entry, hidden_ok, Stage::Tooltip);
                        self.dispatch(eval, &mut stack, &mut ret);
                    } else {
                        ret = Some(String::new());
                    }
                }
                Next::FinishRefs => {
                    let Some(Frame::Refs {
                        owner,
                        acc,
                        resume,
                        src,
                        clears_in_ref,
                        ..
                    }) = stack.pop()
                    else {
                        return String::new();
                    };
                    if clears_in_ref {
                        self.in_ref = false;
                    }
                    let text = acc.trim().to_string();
                    if !text.is_empty() {
                        if let Some((owner_id, _, _)) = owner {
                            self.set_src(owner_id, src);
                        } else {
                            self.src = src;
                        }
                        ret = Some(text);
                    } else if let Some((owner_id, entry, hidden_ok)) = owner {
                        if let Some(el) = El::of(self.arena, owner_id) {
                            let eval = self.step(&el, entry, hidden_ok, resume);
                            self.dispatch(eval, &mut stack, &mut ret);
                        } else {
                            ret = Some(String::new());
                        }
                    } else {
                        ret = Some(String::new());
                    }
                }
            }
        }
    }

    fn dispatch(&mut self, eval: Eval, stack: &mut Vec<Frame>, ret: &mut Option<String>) {
        match eval {
            Eval::Done(s) => *ret = Some(s),
            Eval::Push(frame) => {
                if stack.len() >= MAX_FRAMES {
                    if let Frame::Refs {
                        clears_in_ref: true,
                        ..
                    } = frame
                    {
                        self.in_ref = false;
                    }
                    *ret = Some(String::new());
                } else {
                    stack.push(frame);
                }
            }
        }
    }
}

/// The first child element of `parent` that is an HTML element named `local`.
pub(crate) fn first_child_element(arena: &Arena, parent: NodeId, local: &str) -> Option<NodeId> {
    arena
        .child_ids(parent)
        .take(MAX_NODES_PER_NAME)
        .find(|c| El::of(arena, *c).is_some_and(|e| e.is(local)))
}
