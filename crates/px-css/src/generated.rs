//! Generated content: `::before`, `::after` and list markers.
//!
//! # The shape of the solution
//!
//! Layout and paint are keyed on `px-dom` nodes: a box is a node, its style is
//! the node's `ElementData`. A pseudo-element is neither, so rather than teach
//! three crates a second kind of box, generated content is **materialised as
//! real elements** in the arena after the style pass, and given the style stylo
//! computed for it. Layout and paint then see ordinary children:
//!
//! - `::before` and `::after` become `<px-before>` / `<px-after>` elements
//!   (first and last child of the originating element) whose style is the
//!   eagerly-cascaded pseudo style stylo already produced, and whose only child
//!   is a text node holding the resolved `content`.
//! - A `display: list-item` element gets a `<px-marker>` first child: text for
//!   the numbered styles, a small box for `disc`, `circle` and `square` (the
//!   bundled faces do not carry the glyphs, and a drawn shape is what browsers
//!   do anyway). Its style is computed from a declaration block against the
//!   list item's style.
//!
//! The nodes are removed again by [`strip_generated`] before the next style
//! pass, so a document that is restyled never accumulates them and selector
//! matching never sees them (they are also skipped by sibling navigation).
//!
//! # Counters and quotes
//!
//! `counter-reset`, `counter-increment`, `counter-set`, `counter()`,
//! `counters()`, `open-quote` and `close-quote` need document-order state, so
//! one iterative pre-order walk carries it. Scopes follow CSS Lists 3 §4: a
//! counter created by an element is visible to its following siblings and their
//! descendants, and dies when the parent closes. `ol`, `ul` and `menu` reset
//! `list-item` and `<ol start reversed>`, `<li value>` are honoured, as the HTML
//! specification's rendering section does with its user-agent rules.

use std::collections::BTreeMap;

use px_dom::{Arena, NodeData, NodeId};
use style::properties::ComputedValues;
use style::servo_arc::Arc;

use crate::dom::StyleElement;
use crate::engine::StyleEngine;
use crate::view::{Dom, StyleRoot};

/// Local name of the element standing for `::before`.
pub const BEFORE: &str = "px-before";
/// Local name of the element standing for `::after`.
pub const AFTER: &str = "px-after";
/// Local name of the element standing for a list marker.
pub const MARKER: &str = "px-marker";

/// Whether this local name belongs to generated content.
#[must_use]
pub fn is_generated_name(local: &str) -> bool {
    matches!(local, BEFORE | AFTER | MARKER)
}

/// Whether `id` is an element standing for generated content.
#[must_use]
pub fn is_generated(arena: &Arena, id: NodeId) -> bool {
    arena
        .get(id)
        .and_then(px_dom::Node::element_name)
        .is_some_and(|q| is_generated_name(&q.local))
}

/// Whether `id` is a list marker positioned outside its list item's box.
///
/// Layout reads this: an outside marker takes no room on its line, so it is
/// placed to the left of the line start by its own width.
#[must_use]
pub fn is_outside_marker(arena: &Arena, id: NodeId) -> bool {
    let Some(node) = arena.get(id) else {
        return false;
    };
    node.element_name().is_some_and(|q| &*q.local == MARKER)
        && node
            .attrs()
            .is_some_and(|a| a.iter().any(|a| &*a.name.local == "outside"))
}

/// Remove every generated element (and its text) from `arena`.
///
/// Returns how many were removed. Idempotent.
pub fn strip_generated(arena: &mut Arena) -> usize {
    let document = arena.document();
    let ids: Vec<NodeId> = arena
        .descendants(document)
        .filter(|id| is_generated(arena, *id))
        .collect();
    let mut removed = 0;
    for id in ids {
        if arena.remove_subtree(id).is_ok() {
            removed += 1;
        }
    }
    removed
}

impl StyleEngine {
    /// Style `arena` and materialise its generated content.
    ///
    /// The single call a page pipeline makes instead of
    /// `style_root_for` + `resolve`: it removes generated content left by an
    /// earlier call, resolves style, then inserts `<px-before>`, `<px-after>`
    /// and `<px-marker>` elements and styles them. `None` if the document has no
    /// root element.
    pub fn style_document(&self, arena: &mut Arena) -> Option<StyleRoot> {
        strip_generated(arena);
        let mut root = self.style_root_for(arena);
        self.resolve(arena, &root)?;

        let plan = crate::view::with_dom(arena, &root, |dom| Planner::new(self, dom).run())?;
        if plan.is_empty() {
            return Some(root);
        }

        // Non-marker content first, markers after, so a marker lands before a
        // `::before` on the same host.
        let mut made: Vec<(NodeId, Arc<ComputedValues>)> = Vec::with_capacity(plan.len());
        for pass_markers in [false, true] {
            for item in plan.iter().filter(|g| g.is_marker() == pass_markers) {
                if let Some(id) = materialise(arena, item) {
                    made.push((id, item.style.clone()));
                }
            }
        }
        root.grow_for(arena);
        for (id, style) in made {
            if let Some(mut data) = root.data().ensure(id) {
                data.styles.primary = Some(style);
            }
        }
        Some(root)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Before,
    After,
    Marker,
}

/// One element to insert.
struct Gen {
    host: NodeId,
    kind: Kind,
    text: String,
    outside: bool,
    style: Arc<ComputedValues>,
}

impl Gen {
    fn is_marker(&self) -> bool {
        self.kind == Kind::Marker
    }
}

fn attr(name: &str, value: &str) -> html5ever::Attribute {
    html5ever::Attribute {
        name: html5ever::QualName::new(None, html5ever::ns!(), html5ever::LocalName::from(name)),
        value: value.into(),
    }
}

/// Create the element and its text, and attach them to the host.
fn materialise(arena: &mut Arena, item: &Gen) -> Option<NodeId> {
    let local = match item.kind {
        Kind::Before => BEFORE,
        Kind::After => AFTER,
        Kind::Marker => MARKER,
    };
    let mut attrs = Vec::new();
    if item.outside {
        attrs.push(attr("outside", ""));
    }
    let element = arena
        .create(NodeData::Element {
            name: html5ever::QualName::new(
                None,
                html5ever::ns!(html),
                html5ever::LocalName::from(local),
            ),
            attrs,
            template_contents: None,
            script_already_started: false,
        })
        .ok()?;
    if !item.text.is_empty() {
        let text = arena
            .create(NodeData::Text {
                contents: item.text.as_str().into(),
            })
            .ok()?;
        arena.append_child(element, text).ok()?;
    }
    let first = arena.get(item.host).and_then(px_dom::Node::first_child);
    let placed = match (item.kind, first) {
        (Kind::After, _) | (_, None) => arena.append_child(item.host, element),
        (_, Some(first)) => arena.insert_before(first, element),
    };
    placed.ok()?;
    Some(element)
}

/// Elements that cannot hold generated content: void, replaced, or without a
/// rendered box of their own.
fn takes_pseudos(local: &str) -> bool {
    !matches!(
        local,
        "img"
            | "input"
            | "br"
            | "hr"
            | "area"
            | "base"
            | "col"
            | "embed"
            | "link"
            | "meta"
            | "param"
            | "source"
            | "track"
            | "wbr"
            | "textarea"
            | "select"
            | "iframe"
            | "video"
            | "audio"
            | "canvas"
            | "object"
            | "script"
            | "style"
            | "template"
            | "head"
            | "title"
            | "noscript"
            | "svg"
            | "math"
    )
}

enum Visit {
    Enter(NodeId),
    Exit(NodeId),
}

struct Counter {
    value: i32,
    /// Index of the frame (open parent) that owns this counter.
    frame: usize,
}

struct Planner<'e, 'd> {
    engine: &'e StyleEngine,
    dom: Dom<'d>,
    plan: Vec<Gen>,
    counters: BTreeMap<String, Vec<Counter>>,
    /// Counter names created inside each open frame.
    frames: Vec<Vec<String>>,
    quote_depth: i32,
}

impl<'e, 'd> Planner<'e, 'd> {
    fn new(engine: &'e StyleEngine, dom: Dom<'d>) -> Self {
        Self {
            engine,
            dom,
            plan: Vec::new(),
            counters: BTreeMap::new(),
            frames: vec![Vec::new()],
            quote_depth: 0,
        }
    }

    fn arena(&self) -> &'d Arena {
        self.dom.arena()
    }

    fn styles_of(
        &self,
        id: NodeId,
    ) -> Option<(
        Arc<ComputedValues>,
        Option<Arc<ComputedValues>>,
        Option<Arc<ComputedValues>>,
    )> {
        use style::selector_parser::PseudoElement as P;
        let element = StyleElement::new(self.dom.node(id)?)?;
        let data = style::dom::TElement::borrow_data(&element)?;
        Some((
            data.styles.primary().clone(),
            data.styles.pseudos.get(&P::Before).cloned(),
            data.styles.pseudos.get(&P::After).cloned(),
        ))
    }

    fn run(mut self) -> Vec<Gen> {
        let arena = self.arena();
        let mut stack = vec![Visit::Enter(arena.document())];
        // The document node has no style; start from its children.
        stack.clear();
        for child in arena.child_ids(arena.document()).collect::<Vec<_>>().into_iter().rev() {
            stack.push(Visit::Enter(child));
        }
        while let Some(visit) = stack.pop() {
            match visit {
                Visit::Enter(id) => self.enter(id, &mut stack),
                Visit::Exit(id) => self.exit(id),
            }
        }
        self.plan
    }

    fn enter(&mut self, id: NodeId, stack: &mut Vec<Visit>) {
        let arena = self.arena();
        let Some(node) = arena.get(id) else { return };
        let Some(name) = node.element_name() else {
            return;
        };
        let Some((style, before, _)) = self.styles_of(id) else {
            return;
        };
        if style.clone_display() == style::values::specified::box_::Display::None {
            return;
        }
        let local: &str = &name.local;
        let html = name.ns == html5ever::ns!(html);

        let display = style.clone_display();
        let list_item = display.is_list_item();
        self.element_counters(id, local, html, &style, list_item);

        // Frame for this element's children (and its pseudos).
        self.frames.push(Vec::new());

        let host_ok = html && takes_pseudos(local);
        let mut has_before = false;
        if host_ok && let Some(ps) = before {
            has_before = self.pseudo(id, Kind::Before, &style, &ps);
        }
        if list_item && html {
            self.marker(id, &style, has_before);
        }

        stack.push(Visit::Exit(id));
        let kids: Vec<NodeId> = arena.child_ids(id).collect();
        for kid in kids.into_iter().rev() {
            stack.push(Visit::Enter(kid));
        }
    }

    fn exit(&mut self, id: NodeId) {
        if let Some((style, _, after)) = self.styles_of(id)
            && let Some(node) = self.arena().get(id)
            && let Some(name) = node.element_name()
            && name.ns == html5ever::ns!(html)
            && takes_pseudos(&name.local)
            && let Some(ps) = after
        {
            self.pseudo(id, Kind::After, &style, &ps);
        }
        if let Some(frame) = self.frames.pop() {
            for name in frame {
                if let Some(stack) = self.counters.get_mut(&name) {
                    stack.pop();
                    if stack.is_empty() {
                        self.counters.remove(&name);
                    }
                }
            }
        }
    }

    // ---- counters ---------------------------------------------------------

    /// Frame index that owns counters created by the element being entered
    /// (its parent's frame).
    fn owner_frame(&self) -> usize {
        self.frames.len().saturating_sub(1)
    }

    fn create(&mut self, name: &str, value: i32) {
        let frame = self.owner_frame();
        let stack = self.counters.entry(name.to_owned()).or_default();
        if let Some(top) = stack.last_mut()
            && top.frame == frame
        {
            top.value = value;
            return;
        }
        stack.push(Counter { value, frame });
        if let Some(f) = self.frames.get_mut(frame) {
            f.push(name.to_owned());
        }
    }

    fn increment(&mut self, name: &str, by: i32) {
        if !self.counters.contains_key(name) {
            self.create(name, 0);
        }
        if let Some(top) = self.counters.get_mut(name).and_then(|s| s.last_mut()) {
            top.value = top.value.saturating_add(by);
        }
    }

    fn set(&mut self, name: &str, value: i32) {
        if !self.counters.contains_key(name) {
            self.create(name, 0);
        }
        if let Some(top) = self.counters.get_mut(name).and_then(|s| s.last_mut()) {
            top.value = value;
        }
    }

    /// The counter operations of one element or pseudo-element's style.
    fn style_counters(&mut self, style: &ComputedValues, auto_list_item: Option<i32>) {
        let resets: Vec<(String, i32)> = style
            .clone_counter_reset()
            .iter()
            .map(|p| (p.name.0.to_string(), p.value))
            .collect();
        for (name, v) in &resets {
            self.create(name, *v);
        }
        let incs: Vec<(String, i32)> = style
            .clone_counter_increment()
            .iter()
            .map(|p| (p.name.0.to_string(), p.value))
            .collect();
        if let Some(by) = auto_list_item
            && !incs.iter().any(|(n, _)| n == "list-item")
        {
            self.increment("list-item", by);
        }
        for (name, v) in &incs {
            self.increment(name, *v);
        }
    }

    fn element_counters(
        &mut self,
        id: NodeId,
        local: &str,
        html: bool,
        style: &ComputedValues,
        list_item: bool,
    ) {
        let arena = self.arena();
        let attr_of = |n: &str| -> Option<String> {
            arena
                .get(id)?
                .attrs()?
                .iter()
                .find(|a| &*a.name.local == n && a.name.ns == html5ever::ns!())
                .map(|a| a.value.to_string())
        };
        let author_resets_list_item = style
            .clone_counter_reset()
            .iter()
            .any(|p| &*p.name.0 == "list-item");
        let mut reversed = false;
        if html && matches!(local, "ol" | "ul" | "menu") && !author_resets_list_item {
            let start = attr_of("start").and_then(|s| s.trim().parse::<i32>().ok());
            let value = if local == "ol" && attr_of("reversed").is_some() {
                reversed = true;
                let count = arena
                    .child_ids(id)
                    .filter(|k| {
                        arena
                            .get(*k)
                            .and_then(px_dom::Node::element_name)
                            .is_some_and(|q| &*q.local == "li")
                    })
                    .count();
                let first = start.unwrap_or(i32::try_from(count).unwrap_or(i32::MAX));
                first.saturating_add(1)
            } else if local == "ol" {
                start.map_or(0, |s| s.saturating_sub(1))
            } else {
                0
            };
            self.create("list-item", value);
        }
        let _ = reversed;
        let in_list = html
            && local == "li"
            && arena
                .get(id)
                .and_then(px_dom::Node::parent)
                .and_then(|p| arena.get(p))
                .and_then(px_dom::Node::element_name)
                .is_some_and(|q| matches!(&*q.local, "ol" | "ul" | "menu"));
        let rev_parent = in_list
            && arena
                .get(id)
                .and_then(px_dom::Node::parent)
                .and_then(|p| arena.get(p))
                .and_then(px_dom::Node::attrs)
                .is_some_and(|a| a.iter().any(|a| &*a.name.local == "reversed"))
            && arena
                .get(id)
                .and_then(px_dom::Node::parent)
                .and_then(|p| arena.get(p))
                .and_then(px_dom::Node::element_name)
                .is_some_and(|q| &*q.local == "ol");
        let by = list_item.then_some(if rev_parent { -1 } else { 1 });
        self.style_counters(style, by);
        if in_list && list_item && let Some(v) = attr_of("value").and_then(|s| s.trim().parse::<i32>().ok()) {
            self.set("list-item", v);
        }
    }

    fn value_of(&self, name: &str) -> i32 {
        self.counters
            .get(name)
            .and_then(|s| s.last())
            .map_or(0, |c| c.value)
    }

    // ---- content ----------------------------------------------------------

    /// Plan a `::before` / `::after`. Returns whether a box was generated.
    fn pseudo(
        &mut self,
        host: NodeId,
        kind: Kind,
        host_style: &ComputedValues,
        ps: &Arc<ComputedValues>,
    ) -> bool {
        use style::values::generics::counters::GenericContent as C;
        if ps.clone_display() == style::values::specified::box_::Display::None {
            return false;
        }
        let content = ps.clone_content();
        let C::Items(items) = &content else {
            return false;
        };
        // Counter operations of the pseudo-element apply before its content is
        // read, and in document order with everything else.
        self.style_counters(ps, None);
        let text = self.content_text(host, host_style, &items.items[..items.alt_start.min(items.items.len())]);
        self.plan.push(Gen {
            host,
            kind,
            text,
            outside: false,
            style: ps.clone(),
        });
        true
    }

    fn content_text(
        &mut self,
        origin: NodeId,
        origin_style: &ComputedValues,
        items: &[style::values::computed::counters::ContentItem],
    ) -> String {
        use style::values::generics::counters::GenericContentItem as I;
        let mut out = String::new();
        for item in items {
            match item {
                I::String(s) => out.push_str(s),
                I::Attr(a) => {
                    let found = self.arena().get(origin).and_then(|n| {
                        n.attrs()?
                            .iter()
                            .find(|x| *x.name.local == *a.attribute && x.name.ns == html5ever::ns!())
                            .map(|x| x.value.to_string())
                    });
                    match found {
                        Some(v) => out.push_str(&v),
                        None => out.push_str(&a.fallback),
                    }
                }
                I::Counter(name, style) => {
                    let v = self.value_of(&name.0);
                    out.push_str(&format_counter(style, v));
                }
                I::Counters(name, sep, style) => {
                    let parts: Vec<String> = self
                        .counters
                        .get(&*name.0)
                        .map(|s| s.iter().map(|c| format_counter(style, c.value)).collect())
                        .unwrap_or_default();
                    if parts.is_empty() {
                        out.push_str(&format_counter(style, 0));
                    } else {
                        out.push_str(&parts.join(&**sep));
                    }
                }
                I::OpenQuote => {
                    out.push_str(&self.quote(origin_style, true));
                    self.quote_depth = self.quote_depth.saturating_add(1);
                }
                I::CloseQuote => {
                    self.quote_depth = (self.quote_depth - 1).max(0);
                    out.push_str(&self.quote(origin_style, false));
                }
                I::NoOpenQuote => self.quote_depth = self.quote_depth.saturating_add(1),
                I::NoCloseQuote => self.quote_depth = (self.quote_depth - 1).max(0),
                I::Image(_) => {}
            }
        }
        out
    }

    /// The quote mark for the current depth. For a close quote the depth has
    /// already been decremented, so the pair index is the same as its open.
    fn quote(&self, style: &ComputedValues, open: bool) -> String {
        use style::values::computed::Quotes;
        let depth = usize::try_from(self.quote_depth).unwrap_or(0);
        let pair = |o: &str, c: &str| if open { o.to_owned() } else { c.to_owned() };
        match style.clone_quotes() {
            Quotes::QuoteList(list) => {
                let pairs = &list.0;
                if pairs.is_empty() {
                    return String::new();
                }
                let p = &pairs[depth.min(pairs.len() - 1)];
                pair(&p.opening, &p.closing)
            }
            Quotes::Auto => {
                if depth == 0 {
                    pair("\u{201c}", "\u{201d}")
                } else {
                    pair("\u{2018}", "\u{2019}")
                }
            }
        }
    }

    // ---- markers ----------------------------------------------------------

    fn marker(&mut self, li: NodeId, style: &Arc<ComputedValues>, has_before: bool) {
        use style::computed_values::list_style_position::T as Pos;
        let outside = style.clone_list_style_position() == Pos::Outside;
        let value = self.value_of("list-item");
        // `li::marker` rules: its `content` replaces the generated text and its
        // `color` colours the marker; `None` when no rule matched.
        let own = self.marker_pseudo(li, style);
        let content = own.as_ref().and_then(|ms| {
            use style::values::generics::counters::GenericContent as C;
            match ms.clone_content() {
                C::Items(items) => Some(self.content_text(li, style, &items.items[..items.alt_start.min(items.items.len())])),
                C::None => Some(String::new()),
                C::Normal => None,
            }
        });
        let shape = match content {
            Some(t) if t.is_empty() => return,
            Some(t) => MarkerShape::Text(t),
            None => {
                let Some(s) = marker_shape(&style.clone_list_style_type().0, value) else {
                    return;
                };
                s
            }
        };
        let (mut css, text) = shape.css_and_text(outside);
        if let Some(ms) = &own {
            let c = ms.get_inherited_text().clone_color().into_srgb_legacy();
            let ch = |v: f32| (if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) } * 255.0).round() as u8;
            css.push_str(&format!(
                ";color:rgba({},{},{},{})",
                ch(c.components.0),
                ch(c.components.1),
                ch(c.components.2),
                c.alpha.clamp(0.0, 1.0)
            ));
        }
        let Some(computed) = self.compute_marker(style, &css) else {
            return;
        };
        // An outside marker belongs on the first line of the first block of
        // the item, not on a line of its own above a leading block child.
        let host = if outside && !has_before {
            self.first_block_host(li)
        } else {
            li
        };
        self.plan.push(Gen {
            host,
            kind: Kind::Marker,
            text,
            outside,
            style: computed,
        });
    }

    /// The `::marker` style of a list item, if any rule matches it.
    fn marker_pseudo(&self, li: NodeId, style: &ComputedValues) -> Option<Arc<ComputedValues>> {
        let element = StyleElement::new(self.dom.node(li)?)?;
        let guard = self.engine.shared_lock().read();
        let guards = style::shared_lock::StylesheetGuards::same(&guard);
        self.engine.stylist().lazily_compute_pseudo_element_style(
            &guards,
            element,
            &style::selector_parser::PseudoElement::Marker,
            style::stylist::RuleInclusion::All,
            style,
            true,
            None,
        )
    }

    fn compute_marker(&self, parent: &ComputedValues, css: &str) -> Option<Arc<ComputedValues>> {
        let block = self.engine.style_attribute_parser().parse(css);
        let guard = self.engine.shared_lock().read();
        let guards = style::shared_lock::StylesheetGuards::same(&guard);
        Some(
            self.engine
                .stylist()
                .compute_for_declarations::<StyleElement<'_>>(&guards, parent, block),
        )
    }

    /// Descend through leading in-flow block children (ignoring blank text).
    fn first_block_host(&self, li: NodeId) -> NodeId {
        let arena = self.arena();
        let mut host = li;
        'down: loop {
            for kid in arena.child_ids(host) {
                let Some(node) = arena.get(kid) else { continue };
                if node.element_name().is_some() {
                    let Some((st, _, _)) = self.styles_of(kid) else {
                        return host;
                    };
                    let d = st.clone_display();
                    use style::values::specified::box_::{DisplayInside, DisplayOutside};
                    let plain_block = matches!(d.outside(), DisplayOutside::Block)
                        && matches!(d.inside(), DisplayInside::Flow | DisplayInside::FlowRoot)
                        && st.clone_float() == style::computed_values::float::T::None
                        && st.clone_position() != style::computed_values::position::T::Absolute
                        && st.clone_position() != style::computed_values::position::T::Fixed;
                    if plain_block && !is_generated(arena, kid) {
                        host = kid;
                        continue 'down;
                    }
                    return host;
                }
                if node.text().is_some_and(|t| !t.trim().is_empty()) {
                    return host;
                }
            }
            return host;
        }
    }
}

// ---- counter styles -------------------------------------------------------

/// What a marker draws.
#[derive(Debug, PartialEq, Eq)]
pub enum MarkerShape {
    Disc,
    Circle,
    Square,
    Text(String),
}

impl MarkerShape {
    /// The declaration block for the marker box, and its text.
    fn css_and_text(&self, outside: bool) -> (String, String) {
        let display = if outside { "inline-block" } else { "inline" };
        match self {
            Self::Disc => (shape_css("border-radius:50%;background-color:currentcolor"), String::new()),
            Self::Circle => (
                shape_css("border-radius:50%;border:1px solid currentcolor;box-sizing:border-box"),
                String::new(),
            ),
            Self::Square => (shape_css("background-color:currentcolor"), String::new()),
            // Outside, the gap to the item is a margin (a trailing space inside a
            // shrink-to-fit box does not survive); inside it stays text.
            Self::Text(t) if outside => (
                format!("display:{display};white-space:pre;text-indent:0;text-align:start;margin-right:0.35em"),
                t.trim_end().to_owned(),
            ),
            Self::Text(t) => (
                format!("display:{display};white-space:pre;text-indent:0;text-align:start"),
                t.clone(),
            ),
        }
    }
}

fn shape_css(paint: &str) -> String {
    format!(
        "display:inline-block;width:0.35em;height:0.35em;margin-right:0.45em;\
         vertical-align:0.12em;{paint}"
    )
}

/// What `list-style-type` draws for list item number `value`.
#[must_use]
pub fn marker_shape(
    ty: &style::counter_style::CounterStyle,
    value: i32,
) -> Option<MarkerShape> {
    use style::counter_style::CounterStyle as C;
    match ty {
        C::None => None,
        C::String(s) => Some(MarkerShape::Text(s.to_string())),
        C::Name(n) => match &*n.0 {
            "disc" => Some(MarkerShape::Disc),
            "circle" => Some(MarkerShape::Circle),
            "square" => Some(MarkerShape::Square),
            "none" => None,
            _ => Some(MarkerShape::Text(format!("{}. ", format_counter(ty, value)))),
        },
        C::Symbols { .. } => Some(MarkerShape::Text(format!("{}. ", format_counter(ty, value)))),
    }
}

/// `value` written in the given counter style, as `counter()` does.
#[must_use]
pub fn format_counter(ty: &style::counter_style::CounterStyle, value: i32) -> String {
    use style::counter_style::CounterStyle as C;
    match ty {
        C::None => String::new(),
        C::String(s) => s.to_string(),
        C::Symbols { .. } => value.to_string(),
        C::Name(n) => format_named(&n.0, value),
    }
}

fn format_named(name: &str, value: i32) -> String {
    match name {
        "disc" => "\u{2022}".into(),
        "circle" => "\u{25e6}".into(),
        "square" => "\u{25aa}".into(),
        "decimal-leading-zero" => {
            if value.unsigned_abs() < 10 {
                if value < 0 {
                    format!("-0{}", value.unsigned_abs())
                } else {
                    format!("0{value}")
                }
            } else {
                value.to_string()
            }
        }
        "lower-roman" => roman(value, false).unwrap_or_else(|| value.to_string()),
        "upper-roman" => roman(value, true).unwrap_or_else(|| value.to_string()),
        "lower-alpha" | "lower-latin" => alpha(value, &LOWER_LATIN).unwrap_or_else(|| value.to_string()),
        "upper-alpha" | "upper-latin" => alpha(value, &UPPER_LATIN).unwrap_or_else(|| value.to_string()),
        "lower-greek" => alpha(value, &LOWER_GREEK).unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

const LOWER_LATIN: [char; 26] = [
    'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o', 'p', 'q', 'r', 's',
    't', 'u', 'v', 'w', 'x', 'y', 'z',
];
const UPPER_LATIN: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];
const LOWER_GREEK: [char; 24] = [
    '\u{3b1}', '\u{3b2}', '\u{3b3}', '\u{3b4}', '\u{3b5}', '\u{3b6}', '\u{3b7}', '\u{3b8}',
    '\u{3b9}', '\u{3ba}', '\u{3bb}', '\u{3bc}', '\u{3bd}', '\u{3be}', '\u{3bf}', '\u{3c0}',
    '\u{3c1}', '\u{3c3}', '\u{3c4}', '\u{3c5}', '\u{3c6}', '\u{3c7}', '\u{3c8}', '\u{3c9}',
];

/// Bijective base-N numbering (`a`, `b`, ..., `z`, `aa`, ...); `None` below 1.
fn alpha(value: i32, digits: &[char]) -> Option<String> {
    if value < 1 {
        return None;
    }
    let base = u32::try_from(digits.len()).ok()?;
    let mut n = value.unsigned_abs();
    let mut out = Vec::new();
    while n > 0 {
        n -= 1;
        out.push(*digits.get(usize::try_from(n % base).ok()?)?);
        n /= base;
    }
    Some(out.iter().rev().collect())
}

/// Roman numerals for 1..=3999.
fn roman(value: i32, upper: bool) -> Option<String> {
    if !(1..=3999).contains(&value) {
        return None;
    }
    const TABLE: [(i32, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut n = value;
    let mut out = String::new();
    for (v, s) in TABLE {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    Some(if upper { out.to_uppercase() } else { out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbering_systems() {
        assert_eq!(format_named("lower-roman", 4), "iv");
        assert_eq!(format_named("upper-roman", 1994), "MCMXCIV");
        assert_eq!(format_named("lower-roman", 0), "0");
        assert_eq!(format_named("lower-alpha", 1), "a");
        assert_eq!(format_named("lower-alpha", 26), "z");
        assert_eq!(format_named("upper-alpha", 27), "AA");
        assert_eq!(format_named("upper-latin", 52), "AZ");
        assert_eq!(format_named("lower-alpha", -3), "-3");
        assert_eq!(format_named("decimal-leading-zero", 7), "07");
        assert_eq!(format_named("decimal-leading-zero", -7), "-07");
        assert_eq!(format_named("decimal-leading-zero", 12), "12");
        assert_eq!(format_named("lower-greek", 2), "\u{3b2}");
        assert_eq!(format_named("decimal", -4), "-4");
    }
}
