//! The accessibility tree: building it from a DOM, and navigating it.
//!
//! # Identity
//!
//! An [`A11yId`] names a node *and the build that produced it*, exactly as a
//! `px_dom::NodeId` names a slot and a moment. Every build takes a fresh
//! generation from a process-wide counter, so an id from a previous tree
//! never resolves in a rebuilt one: it returns `None`, not whichever node now
//! sits at that index. There is no accessor that takes an index and cannot
//! fail. Identity *across* rebuilds, which a platform bridge needs in order to
//! keep a screen reader's place, is the DOM node: [`AccessibilityTree::find`]
//! maps a `px_dom::NodeId` to the current id, and the [`crate::diff`] API
//! reports changes keyed by it.
//!
//! # Determinism
//!
//! The tree is a pure function of the DOM, the style and bounds inputs and the
//! options. Nothing iterates a hash map to produce output; hash maps are used
//! for lookup only. Two builds of the same input produce trees that dump
//! identically (only the generation differs).
//!
//! # Bounds
//!
//! At most [`BuildOptions::max_nodes`] nodes; every string at most
//! [`crate::MAX_STRING`] bytes; name computation work bounded as described in
//! the `accname` module; `aria-owns` processed for at most [`MAX_OWNS`]
//! relationships and never to a depth over [`MAX_TREE_DEPTH`]. Exceeding any
//! of them truncates and sets [`AccessibilityTree::is_truncated`].
//! Construction and traversal are iterative: a hostile document cannot
//! overflow the stack.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};

use px_dom::{Arena, NodeData, NodeId};

use crate::accname::{Computer, Src, text_content};
use crate::attr::{
    El, MAX_STRING, aria_bool, aria_tristate, clean, flatten, parse_number, parse_positive,
    text_of, trim_ws,
};
use crate::doc::{Cond, Ctx, Doc, focus_tab_index, heading_level, input_type, is_disabled};
use crate::input::{Bounds, BuildInputs, BuildOptions};
use crate::role::{Role, is_ascii_ws};
use crate::state::{
    Current, HasPopup, Invalid, LiveRegion, Politeness, Props, RangeValue, Relevant, States, Tri,
};

/// The deepest the tree may become through `aria-owns`. The DOM's own nesting
/// limit (512) bounds the structural tree below this.
pub const MAX_TREE_DEPTH: usize = 1024;

/// The most `aria-owns` relationships honoured in one build.
pub const MAX_OWNS: usize = 10_000;

/// The most targets honoured from one `aria-owns` attribute.
const MAX_OWNS_PER_NODE: usize = 64;

static NEXT_GENERATION: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> NonZeroU64 {
    // `fetch_add` on a u64 starting at 1 cannot reach zero in the lifetime of
    // a process; the fallback exists so that there is no panic path.
    NonZeroU64::new(NEXT_GENERATION.fetch_add(1, Ordering::Relaxed)).unwrap_or(NonZeroU64::MIN)
}

/// A handle to a node of one built tree. See the module documentation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct A11yId {
    index: u32,
    generation: NonZeroU64,
}

impl A11yId {
    /// The slot this handle names. Readable for diagnostics; not constructible.
    #[must_use]
    pub fn index(self) -> u32 {
        self.index
    }

    /// The build this handle belongs to.
    #[must_use]
    pub fn generation(self) -> NonZeroU64 {
        self.generation
    }
}

/// One node of the accessibility tree.
#[derive(Clone, Debug)]
pub struct A11yNode {
    pub(crate) dom: NodeId,
    pub(crate) role: Role,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) states: States,
    pub(crate) props: Props,
    pub(crate) bounds: Option<Bounds>,
    pub(crate) tab_index: Option<i32>,
    pub(crate) parent: Option<u32>,
    pub(crate) children: Vec<u32>,
    pub(crate) sibling_index: u32,
}

impl A11yNode {
    /// The DOM node this node reports. Generational like every `NodeId`: it
    /// may not resolve if the DOM has changed since the build.
    #[must_use]
    pub fn dom_node(&self) -> NodeId {
        self.dom
    }
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }
    /// The accessible name, "" if none.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
    /// The accessible description, "" if none.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }
    #[must_use]
    pub fn states(&self) -> &States {
        &self.states
    }
    #[must_use]
    pub fn props(&self) -> &Props {
        &self.props
    }
    /// Page-space border box, when layout was supplied and the node has a box.
    #[must_use]
    pub fn bounds(&self) -> Option<Bounds> {
        self.bounds
    }
    /// The sequential-focus-navigation value: `Some(n)` if focusable.
    /// Negative values are focusable by script and pointer but skipped by Tab.
    #[must_use]
    pub fn tab_index(&self) -> Option<i32> {
        self.tab_index
    }
}

/// A built accessibility tree.
#[derive(Debug)]
pub struct AccessibilityTree {
    generation: NonZeroU64,
    nodes: Vec<A11yNode>,
    by_dom: HashMap<NodeId, u32>,
    truncated: bool,
}

struct Builder<'a> {
    doc: Doc<'a>,
    options: BuildOptions,
    nodes: Vec<A11yNode>,
    owners: Vec<u32>,
    truncated: bool,
    name_budget: usize,
}

impl AccessibilityTree {
    /// Build the tree with default limits.
    ///
    /// `inputs` carries computed style and layout bounds as plain data; see
    /// [`crate::input`]. With neither, `hidden`, `aria-hidden` and the
    /// user-agent never-rendered elements still apply.
    #[must_use]
    pub fn build(arena: &Arena, inputs: BuildInputs<'_>) -> AccessibilityTree {
        AccessibilityTree::build_with(arena, inputs, BuildOptions::default())
    }

    /// Build the tree with explicit limits.
    #[must_use]
    pub fn build_with(
        arena: &Arena,
        inputs: BuildInputs<'_>,
        options: BuildOptions,
    ) -> AccessibilityTree {
        let doc = Doc::analyse(arena, inputs);
        let mut b = Builder {
            doc,
            options,
            nodes: Vec::new(),
            owners: Vec::new(),
            truncated: false,
            name_budget: options.max_name_visits,
        };
        b.walk();
        b.finish()
    }

    /// The build this tree is. Ids carry it.
    #[must_use]
    pub fn generation(&self) -> NonZeroU64 {
        self.generation
    }

    /// Whether a limit cut the tree short.
    #[must_use]
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The number of nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    fn id_of(&self, index: u32) -> A11yId {
        A11yId {
            index,
            generation: self.generation,
        }
    }

    fn slot(&self, id: A11yId) -> Option<&A11yNode> {
        if id.generation != self.generation {
            return None;
        }
        self.nodes.get(id.index as usize)
    }

    /// The root (the document), if the tree is not empty.
    #[must_use]
    pub fn root(&self) -> Option<A11yId> {
        (!self.nodes.is_empty()).then(|| self.id_of(0))
    }

    /// Resolve a handle. `None` if it belongs to another build.
    #[must_use]
    pub fn node(&self, id: A11yId) -> Option<&A11yNode> {
        self.slot(id)
    }

    /// The current id of the node reporting this DOM node.
    #[must_use]
    pub fn find(&self, dom: NodeId) -> Option<A11yId> {
        self.by_dom.get(&dom).map(|i| self.id_of(*i))
    }

    #[must_use]
    pub fn parent(&self, id: A11yId) -> Option<A11yId> {
        self.slot(id)?.parent.map(|p| self.id_of(p))
    }

    /// The children, in order.
    #[must_use]
    pub fn children(&self, id: A11yId) -> Option<Vec<A11yId>> {
        Some(
            self.slot(id)?
                .children
                .iter()
                .map(|c| self.id_of(*c))
                .collect(),
        )
    }

    #[must_use]
    pub fn child_count(&self, id: A11yId) -> Option<usize> {
        Some(self.slot(id)?.children.len())
    }

    #[must_use]
    pub fn first_child(&self, id: A11yId) -> Option<A11yId> {
        self.slot(id)?.children.first().map(|c| self.id_of(*c))
    }

    #[must_use]
    pub fn last_child(&self, id: A11yId) -> Option<A11yId> {
        self.slot(id)?.children.last().map(|c| self.id_of(*c))
    }

    #[must_use]
    pub fn next_sibling(&self, id: A11yId) -> Option<A11yId> {
        let node = self.slot(id)?;
        let parent = self.nodes.get(node.parent? as usize)?;
        let next = parent.children.get(node.sibling_index as usize + 1)?;
        Some(self.id_of(*next))
    }

    #[must_use]
    pub fn prev_sibling(&self, id: A11yId) -> Option<A11yId> {
        let node = self.slot(id)?;
        let parent = self.nodes.get(node.parent? as usize)?;
        let at = (node.sibling_index as usize).checked_sub(1)?;
        Some(self.id_of(*parent.children.get(at)?))
    }

    /// `id` and its descendants in document order, iteratively.
    #[must_use]
    pub fn descendants(&self, id: A11yId) -> Vec<A11yId> {
        let mut out = Vec::new();
        if self.slot(id).is_none() {
            return out;
        }
        let mut stack = vec![id.index];
        while let Some(i) = stack.pop() {
            out.push(self.id_of(i));
            if let Some(n) = self.nodes.get(i as usize) {
                stack.extend(n.children.iter().rev().copied());
            }
        }
        out
    }

    /// All nodes in document order.
    #[must_use]
    pub fn all(&self) -> Vec<A11yId> {
        self.root().map(|r| self.descendants(r)).unwrap_or_default()
    }

    /// Focusable nodes in sequential focus navigation order (HTML "tabindex
    /// ordering"): positive `tabindex` values ascending, ties in document
    /// order, then `tabindex=0` in document order. Negative values are left
    /// out.
    #[must_use]
    pub fn tab_order(&self) -> Vec<A11yId> {
        let mut positive: Vec<(i32, u32)> = Vec::new();
        let mut zero: Vec<u32> = Vec::new();
        for id in self.all() {
            let Some(n) = self.slot(id) else { continue };
            match n.tab_index {
                Some(t) if t > 0 => positive.push((t, id.index)),
                Some(0) => zero.push(id.index),
                _ => {}
            }
        }
        positive.sort_by_key(|(t, i)| (*t, *i));
        positive
            .into_iter()
            .map(|(_, i)| i)
            .chain(zero)
            .map(|i| self.id_of(i))
            .collect()
    }

    pub(crate) fn raw_nodes(&self) -> &[A11yNode] {
        &self.nodes
    }
}

impl<'a> Builder<'a> {
    fn walk(&mut self) {
        let arena = self.doc.arena;
        let root_dom = arena.document();
        let title = self
            .doc
            .title
            .map(|t| text_content(arena, t, MAX_STRING))
            .unwrap_or_default();
        self.nodes.push(A11yNode {
            dom: root_dom,
            role: Role::Document,
            name: title,
            description: String::new(),
            states: States::default(),
            props: Props::default(),
            bounds: self.doc.inputs.bounds.and_then(|b| b.bounds(root_dom)),
            tab_index: None,
            parent: None,
            children: Vec::new(),
            sibling_index: 0,
        });
        self.owners.push(0);
        if let Some(info) = self.doc.info_mut(root_dom) {
            info.container = Some(0);
        }

        let order = std::mem::take(&mut self.doc.order);
        for id in order.iter().skip(1).copied() {
            if self.nodes.len() >= self.options.max_nodes {
                self.truncated = true;
                break;
            }
            self.visit(id);
        }
        self.doc.order = order;
    }

    fn visit(&mut self, id: NodeId) {
        let arena = self.doc.arena;
        let Some(info) = self.doc.info(id).copied() else {
            return;
        };
        let Some(node) = arena.get(id) else { return };
        let parent_info = node.parent().and_then(|p| self.doc.info(p).copied());
        let Some(pinfo) = parent_info else { return };
        let container = pinfo.container;

        if info.hidden || info.prune {
            return;
        }

        match node.data() {
            NodeData::Text { .. } => {
                if info.vis_hidden {
                    return;
                }
                let Some(parent) = container else { return };
                let Some(text) = text_of(arena, id) else {
                    return;
                };
                if trim_ws(text).is_empty() {
                    return;
                }
                let name = flatten(text);
                let bounds = self.doc.inputs.bounds.and_then(|b| b.bounds(id));
                self.push(
                    A11yNode {
                        dom: id,
                        role: Role::StaticText,
                        name,
                        description: String::new(),
                        states: States::default(),
                        props: Props::default(),
                        bounds,
                        tab_index: None,
                        parent: Some(parent),
                        children: Vec::new(),
                        sibling_index: 0,
                    },
                    id,
                    false,
                );
            }
            NodeData::Element { .. } => {
                // Whatever happens, children attach to the parent's container
                // unless this element becomes a node of its own.
                if let Some(i) = self.doc.info_mut(id) {
                    i.container = container;
                }
                if info.vis_hidden {
                    return;
                }
                let Some(parent) = container else { return };
                let Some(el) = El::of(arena, id) else { return };
                let pctx = pinfo.child_ctx;
                let mut role = info.role;
                if role == Role::Presentation {
                    return;
                }

                let mut name = String::new();
                let mut src = Src::None;
                if role.name_from() != crate::NameFrom::Prohibited
                    || info.cond == Cond::NamedElseGeneric
                {
                    let c = Computer::name(&self.doc, &mut self.name_budget, id);
                    name = c.text;
                    src = c.src;
                }
                if info.cond == Cond::NamedElseGeneric && name.is_empty() {
                    role = Role::Generic;
                }
                let (states, props, tab_index) = node_data(&self.doc, &el, role, &pctx);
                if role == Role::Generic
                    && !(tab_index.is_some()
                        || props.live.politeness != Politeness::Off
                        || el.has("aria-owns"))
                {
                    return;
                }
                let description = Computer::description(&self.doc, &mut self.name_budget, id, src);
                let bounds = self.doc.inputs.bounds.and_then(|b| b.bounds(id));
                let has_owns = el.has("aria-owns");
                self.push(
                    A11yNode {
                        dom: id,
                        role,
                        name,
                        description,
                        states,
                        props,
                        bounds,
                        tab_index,
                        parent: Some(parent),
                        children: Vec::new(),
                        sibling_index: 0,
                    },
                    id,
                    has_owns,
                );
            }
            _ => {}
        }
    }

    fn push(&mut self, node: A11yNode, dom: NodeId, has_owns: bool) {
        let index = u32::try_from(self.nodes.len()).unwrap_or(u32::MAX);
        self.nodes.push(node);
        if has_owns {
            self.owners.push(index);
        }
        if let Some(info) = self.doc.info_mut(dom) {
            info.container = Some(index);
        }
    }

    fn finish(mut self) -> AccessibilityTree {
        let n = self.nodes.len();
        let mut parent: Vec<Option<u32>> = self.nodes.iter().map(|x| x.parent).collect();
        let mut kids: Vec<Vec<u32>> = vec![Vec::new(); n];
        for (i, p) in parent.iter().enumerate() {
            if let Some(p) = p
                && let Some(list) = kids.get_mut(*p as usize)
            {
                list.push(u32::try_from(i).unwrap_or(u32::MAX));
            }
        }
        // Heights: parents precede children in creation order, so a reverse
        // sweep sees every child before its parent.
        let mut height = vec![0usize; n];
        for i in (1..n).rev() {
            if let Some(Some(p)) = parent.get(i)
                && let Some(hi) = height.get(i).copied()
                && let Some(hp) = height.get_mut(*p as usize)
            {
                *hp = (*hp).max(hi + 1);
            }
        }

        // aria-owns. The first owner in document order wins; a node already
        // owned, the document, the owner itself and any ancestor of the owner
        // (which would close a cycle) are refused.
        let by_dom: HashMap<NodeId, u32> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, x)| (x.dom, u32::try_from(i).unwrap_or(u32::MAX)))
            .collect();
        let mut owned_by: Vec<Option<u32>> = vec![None; n];
        let mut owned_list: Vec<Vec<u32>> = vec![Vec::new(); n];
        let mut processed = 0usize;
        let owners = std::mem::take(&mut self.owners);
        'owners: for owner in owners.into_iter().skip(1) {
            let Some(dom) = self.nodes.get(owner as usize).map(|x| x.dom) else {
                continue;
            };
            let Some(el) = El::of(self.doc.arena, dom) else {
                continue;
            };
            for target in self
                .doc
                .idrefs(&el, "aria-owns")
                .into_iter()
                .take(MAX_OWNS_PER_NODE)
            {
                if processed >= MAX_OWNS {
                    self.truncated = true;
                    break 'owners;
                }
                processed += 1;
                let Some(t) = by_dom.get(&target).copied() else {
                    continue;
                };
                if t == 0 || t == owner || owned_by.get(t as usize).is_some_and(|o| o.is_some()) {
                    continue;
                }
                // Walk up from the owner: depth, and whether `t` is above it.
                let mut depth = 0usize;
                let mut cursor = Some(owner);
                let mut cycle = false;
                while let Some(c) = cursor {
                    if c == t {
                        cycle = true;
                        break;
                    }
                    depth += 1;
                    if depth > MAX_TREE_DEPTH {
                        break;
                    }
                    cursor = parent.get(c as usize).copied().flatten();
                }
                let t_height = height.get(t as usize).copied().unwrap_or(0);
                if cycle || depth + 1 + t_height > MAX_TREE_DEPTH {
                    continue;
                }
                if let Some(p) = parent.get_mut(t as usize) {
                    *p = Some(owner);
                }
                if let Some(o) = owned_by.get_mut(t as usize) {
                    *o = Some(owner);
                }
                if let Some(l) = owned_list.get_mut(owner as usize) {
                    l.push(t);
                }
                let mut h = t_height + 1;
                let mut a = Some(owner);
                while let Some(x) = a {
                    match height.get_mut(x as usize) {
                        Some(hx) if *hx < h => *hx = h,
                        _ => break,
                    }
                    h += 1;
                    a = parent.get(x as usize).copied().flatten();
                }
            }
        }

        // Final child lists: surviving structural children, then owned ones.
        for (i, list) in kids.iter_mut().enumerate() {
            // An owned node is listed only in its owner's owned list, even
            // when the owner was already its structural parent: ownership
            // moves it to the end.
            list.retain(|c| {
                parent.get(*c as usize).copied().flatten() == u32::try_from(i).ok()
                    && owned_by.get(*c as usize).is_some_and(|o| o.is_none())
            });
            if let Some(owned) = owned_list.get(i) {
                list.extend(owned.iter().copied());
            }
        }
        for (i, node) in self.nodes.iter_mut().enumerate() {
            node.parent = parent.get(i).copied().flatten();
        }
        for list in &kids {
            for (pos, c) in list.iter().enumerate() {
                if let Some(child) = self.nodes.get_mut(*c as usize) {
                    child.sibling_index = u32::try_from(pos).unwrap_or(u32::MAX);
                }
            }
        }
        // Position in set, computed from siblings of the same role.
        for list in &kids {
            set_positions(&mut self.nodes, list);
        }
        for (node, list) in self.nodes.iter_mut().zip(kids) {
            node.children = list;
        }

        AccessibilityTree {
            generation: next_generation(),
            by_dom: self
                .nodes
                .iter()
                .enumerate()
                .map(|(i, x)| (x.dom, u32::try_from(i).unwrap_or(u32::MAX)))
                .collect(),
            nodes: self.nodes,
            truncated: self.truncated,
        }
    }
}

/// Fill `pos_in_set` / `set_size` for siblings that report a position, where
/// the author did not.
fn set_positions(nodes: &mut [A11yNode], children: &[u32]) {
    let mut totals: Vec<(Role, u32)> = Vec::new();
    for c in children {
        let Some(role) = nodes.get(*c as usize).map(|n| n.role) else {
            continue;
        };
        if !role.has_set_position() {
            continue;
        }
        match totals.iter_mut().find(|(r, _)| *r == role) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => totals.push((role, 1)),
        }
    }
    if totals.is_empty() {
        return;
    }
    let mut seen: Vec<(Role, u32)> = totals.iter().map(|(r, _)| (*r, 0)).collect();
    for c in children {
        let Some(node) = nodes.get_mut(*c as usize) else {
            continue;
        };
        if !node.role.has_set_position() {
            continue;
        }
        let total = totals
            .iter()
            .find(|(r, _)| *r == node.role)
            .map_or(0, |(_, n)| *n);
        let pos = match seen.iter_mut().find(|(r, _)| *r == node.role) {
            Some((_, n)) => {
                *n = n.saturating_add(1);
                *n
            }
            None => 0,
        };
        node.props.pos_in_set.get_or_insert(pos);
        node.props.set_size.get_or_insert(total);
    }
}

fn is_text_control_role(role: Role) -> bool {
    matches!(role, Role::TextBox | Role::SearchBox | Role::Combobox)
}

/// States, properties and focus data for one element.
fn node_data(doc: &Doc<'_>, el: &El<'_>, role: Role, pctx: &Ctx) -> (States, Props, Option<i32>) {
    let arena = doc.arena;
    let tab_index = focus_tab_index(el, pctx);
    let html = |name: &str| el.is_html() && el.is(name);

    let mut s = States {
        focusable: tab_index.is_some(),
        disabled: is_disabled(el, pctx),
        ..States::default()
    };
    s.required = aria_bool(el.attr("aria-required")) == Some(true)
        || ((html("input") || html("select") || html("textarea")) && el.has("required"));
    s.read_only = aria_bool(el.attr("aria-readonly")) == Some(true)
        || ((html("input") || html("textarea")) && el.has("readonly"));
    s.invalid = match el.attr("aria-invalid").map(trim_ws) {
        Some(v) if v.eq_ignore_ascii_case("true") => Invalid::True,
        Some(v) if v.eq_ignore_ascii_case("grammar") => Invalid::Grammar,
        Some(v) if v.eq_ignore_ascii_case("spelling") => Invalid::Spelling,
        _ => Invalid::False,
    };

    let native_checkable = html("input") && matches!(input_type(el).as_str(), "checkbox" | "radio");
    s.checked = if native_checkable {
        Some(if el.has("checked") {
            Tri::True
        } else {
            Tri::False
        })
    } else if matches!(
        role,
        Role::Checkbox | Role::Radio | Role::Switch | Role::MenuItemCheckbox | Role::MenuItemRadio
    ) {
        Some(aria_tristate(el.attr("aria-checked")).unwrap_or(Tri::False))
    } else {
        aria_tristate(el.attr("aria-checked"))
    };
    s.pressed = if role == Role::Button {
        aria_tristate(el.attr("aria-pressed"))
    } else {
        None
    };
    s.selected = if html("option") {
        Some(el.has("selected") || aria_bool(el.attr("aria-selected")) == Some(true))
    } else {
        aria_bool(el.attr("aria-selected"))
    };
    s.expanded = if html("details") {
        Some(el.has("open"))
    } else if html("summary") && pctx.parent_is_details && pctx.first_summary == Some(el.id) {
        Some(pctx.details_open)
    } else {
        aria_bool(el.attr("aria-expanded"))
    };
    s.multiline = html("textarea") || aria_bool(el.attr("aria-multiline")) == Some(true);
    s.modal = aria_bool(el.attr("aria-modal")) == Some(true);
    s.busy = aria_bool(el.attr("aria-busy")) == Some(true);
    s.current = match el.attr("aria-current").map(trim_ws) {
        None => Current::False,
        Some(v) if v.eq_ignore_ascii_case("false") || v.is_empty() => Current::False,
        Some(v) if v.eq_ignore_ascii_case("page") => Current::Page,
        Some(v) if v.eq_ignore_ascii_case("step") => Current::Step,
        Some(v) if v.eq_ignore_ascii_case("location") => Current::Location,
        Some(v) if v.eq_ignore_ascii_case("date") => Current::Date,
        Some(v) if v.eq_ignore_ascii_case("time") => Current::Time,
        // ARIA: any other non-empty value is treated as "true".
        Some(_) => Current::True,
    };
    s.has_popup = match el.attr("aria-haspopup").map(trim_ws) {
        Some(v) if v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("menu") => {
            HasPopup::Menu
        }
        Some(v) if v.eq_ignore_ascii_case("listbox") => HasPopup::Listbox,
        Some(v) if v.eq_ignore_ascii_case("tree") => HasPopup::Tree,
        Some(v) if v.eq_ignore_ascii_case("grid") => HasPopup::Grid,
        Some(v) if v.eq_ignore_ascii_case("dialog") => HasPopup::Dialog,
        _ => HasPopup::False,
    };

    let mut p = Props {
        level: if role == Role::Heading {
            Some(
                parse_positive(el.attr("aria-level"))
                    .or_else(|| heading_level(el.local).filter(|_| el.is_html()))
                    .unwrap_or(2),
            )
        } else if role.supports_level() {
            parse_positive(el.attr("aria-level"))
        } else {
            None
        },
        pos_in_set: parse_positive(el.attr("aria-posinset")),
        set_size: parse_positive(el.attr("aria-setsize")),
        live: live_region(el, role),
        ..Props::default()
    };

    if is_text_control_role(role) && el.is_html() {
        p.value = match el.local {
            "input" if input_type(el) != "password" => {
                el.attr("value").map(|v| clean(&flatten_keep(v)))
            }
            "textarea" => Some(text_content(arena, el.id, MAX_STRING)),
            "select" => select_value(doc, el),
            _ => None,
        };
    } else if role == Role::Listbox && html("select") {
        p.value = select_value(doc, el);
    }
    p.range = range_value(el, role);
    if (html("a") || html("area"))
        && let Some(h) = el.attr("href")
    {
        p.href = Some(clean(h));
    }
    (s, p, tab_index)
}

fn flatten_keep(v: &str) -> String {
    v.to_string()
}

fn select_value(doc: &Doc<'_>, select: &El<'_>) -> Option<String> {
    let mut first = None;
    for d in doc.arena.descendants(select.id).skip(1).take(4096) {
        let Some(opt) = El::of(doc.arena, d) else {
            continue;
        };
        if !opt.is("option") {
            continue;
        }
        if opt.has("selected") {
            return Some(text_content(doc.arena, d, MAX_STRING));
        }
        first.get_or_insert(d);
    }
    first.map(|d| text_content(doc.arena, d, MAX_STRING))
}

fn live_region(el: &El<'_>, role: Role) -> LiveRegion {
    let implied = match role {
        Role::Alert => Politeness::Assertive,
        Role::Status | Role::Log => Politeness::Polite,
        _ => Politeness::Off,
    };
    let politeness = match el.attr("aria-live").map(trim_ws) {
        Some(v) if v.eq_ignore_ascii_case("polite") => Politeness::Polite,
        Some(v) if v.eq_ignore_ascii_case("assertive") => Politeness::Assertive,
        Some(v) if v.eq_ignore_ascii_case("off") => Politeness::Off,
        _ => implied,
    };
    let atomic =
        aria_bool(el.attr("aria-atomic")).unwrap_or(matches!(role, Role::Alert | Role::Status));
    let mut relevant = Relevant::default();
    if let Some(v) = el.attr("aria-relevant") {
        let (mut add, mut rem, mut text, mut any) = (false, false, false, false);
        for t in v.split(is_ascii_ws).filter(|t| !t.is_empty()).take(8) {
            if t.eq_ignore_ascii_case("all") {
                (add, rem, text, any) = (true, true, true, true);
            } else if t.eq_ignore_ascii_case("additions") {
                (add, any) = (true, true);
            } else if t.eq_ignore_ascii_case("removals") {
                (rem, any) = (true, true);
            } else if t.eq_ignore_ascii_case("text") {
                (text, any) = (true, true);
            }
        }
        if any {
            relevant = Relevant {
                additions: add,
                removals: rem,
                text,
            };
        }
    }
    LiveRegion {
        politeness,
        atomic,
        relevant,
    }
}

fn range_value(el: &El<'_>, role: Role) -> Option<RangeValue> {
    if !matches!(
        role,
        Role::Slider | Role::SpinButton | Role::ScrollBar | Role::ProgressBar | Role::Meter
    ) {
        return None;
    }
    let mut r = RangeValue {
        now: parse_number(el.attr("aria-valuenow")),
        min: parse_number(el.attr("aria-valuemin")),
        max: parse_number(el.attr("aria-valuemax")),
    };
    if el.is_html() {
        let native = |n: &str| parse_number(el.attr(n));
        match el.local {
            "input" => {
                r.now = r.now.or_else(|| native("value"));
                r.min = r.min.or_else(|| native("min"));
                r.max = r.max.or_else(|| native("max"));
            }
            "progress" => {
                r.now = r.now.or_else(|| native("value"));
                r.min = r.min.or(Some(0.0));
                r.max = r.max.or_else(|| native("max")).or(Some(1.0));
            }
            "meter" => {
                r.now = r.now.or_else(|| native("value"));
                r.min = r.min.or_else(|| native("min")).or(Some(0.0));
                r.max = r.max.or_else(|| native("max")).or(Some(1.0));
            }
            _ => {}
        }
    }
    (r.now.is_some() || r.min.is_some() || r.max.is_some()).then_some(r)
}
