//! The first pass: one walk over the DOM in document order that records, for
//! every node, what later passes need to know without walking again.
//!
//! The name computation follows `aria-labelledby` to nodes anywhere in the
//! document, including ones the tree walk has not reached yet, so "is this
//! node hidden" and "what is its role" have to be answerable for any node
//! before the tree is built. Doing it as one pre-order pass makes both O(1)
//! lookups by slot: a parent is always analysed before its children, so a
//! child derives its answers from its parent's.
//!
//! Nothing here recurses. The walk is `Arena::descendants`, which carries its
//! own stack.

use std::collections::HashMap;

use px_dom::{Arena, NodeData, NodeId};

use crate::attr::{El, aria_bool, parse_int, trim_ws};
use crate::input::BuildInputs;
use crate::role::{Role, is_ascii_ws};

/// A role that depends on the element having an accessible name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cond {
    /// The role stands as computed.
    No,
    /// The role applies only if the element has an accessible name; otherwise
    /// the element is `generic` (`section`, `form`, `aside` in sectioning
    /// content).
    NamedElseGeneric,
}

/// What an element inherits from its ancestors.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Ctx {
    /// Inside `article`, `aside`, `nav` or `section`: `header` and `footer`
    /// lose their landmark roles and `aside` needs a name to be a landmark.
    pub sectioning: bool,
    /// Inside `main`: `header` and `footer` lose their landmark roles.
    pub in_main: bool,
    /// Inside a `grid` or `treegrid`: `td` is a `gridcell`.
    pub grid: bool,
    /// Inside a table whose role is presentational.
    pub pres_table: bool,
    /// Inside a list whose role is presentational.
    pub pres_list: bool,
    /// Inside a `thead`.
    pub thead: bool,
    /// Inside a disabled `fieldset`.
    pub fieldset_disabled: bool,
    /// Inside an `inert` subtree.
    pub inert: bool,
    /// Set for the children of a `details`: whether it is open, and which
    /// child is its first `summary`. Not inherited past the children.
    pub details_open: bool,
    pub first_summary: Option<NodeId>,
    pub parent_is_details: bool,
}

/// What the first pass records about one DOM node.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Info {
    pub id: NodeId,
    /// Not rendered: this node or an ancestor is `display: none`,
    /// `aria-hidden="true"`, `hidden`, never rendered by default, or the
    /// content of a closed `details`. The whole subtree is excluded.
    pub hidden: bool,
    /// This node alone has `visibility: hidden`.
    pub vis_hidden: bool,
    /// An ancestor's role makes its descendants presentational.
    pub prune: bool,
    /// Whether this node's own role makes its descendants presentational,
    /// folded together with `prune`.
    pub child_prune: bool,
    /// The role before name-dependent conditions are applied.
    pub role: Role,
    pub cond: Cond,
    pub child_ctx: Ctx,
    /// The index of the tree node that children of this DOM node attach to,
    /// filled in by the second pass.
    pub container: Option<u32>,
}

pub(crate) struct Doc<'a> {
    pub arena: &'a Arena,
    pub inputs: BuildInputs<'a>,
    /// All reachable nodes, document order.
    pub order: Vec<NodeId>,
    infos: Vec<Option<Info>>,
    /// `id` attribute to the first element in tree order that has it.
    pub ids: HashMap<&'a str, NodeId>,
    /// Labelable control to its `label` elements, in document order.
    pub labels: HashMap<NodeId, Vec<NodeId>>,
    /// The first `title` element, for the document's name.
    pub title: Option<NodeId>,
}

/// Elements the user agent never renders (`display: none` in the HTML
/// rendering section's default style sheet).
fn never_rendered(local: &str) -> bool {
    matches!(
        local,
        "head"
            | "link"
            | "meta"
            | "script"
            | "style"
            | "title"
            | "template"
            | "noscript"
            | "datalist"
            | "base"
            | "param"
            | "source"
            | "track"
            | "area"
            | "slot"
    )
}

pub(crate) fn input_type(el: &El<'_>) -> String {
    let t = trim_ws(el.attr("type").unwrap_or(""));
    let mut s = String::with_capacity(t.len().min(16));
    for c in t.chars().take(16) {
        s.push(c.to_ascii_lowercase());
    }
    s
}

pub(crate) fn heading_level(local: &str) -> Option<u32> {
    match local {
        "h1" => Some(1),
        "h2" => Some(2),
        "h3" => Some(3),
        "h4" => Some(4),
        "h5" => Some(5),
        "h6" => Some(6),
        _ => None,
    }
}

/// Whether an element is a form control that the `disabled` attribute
/// applies to.
fn disableable(local: &str) -> bool {
    matches!(
        local,
        "button" | "input" | "select" | "textarea" | "fieldset" | "optgroup" | "option"
    )
}

pub(crate) fn is_disabled(el: &El<'_>, ctx: &Ctx) -> bool {
    if aria_bool(el.attr("aria-disabled")) == Some(true) {
        return true;
    }
    if !el.is_html() || !disableable(el.local) {
        return false;
    }
    if el.has("disabled") {
        return true;
    }
    ctx.fieldset_disabled
        && matches!(
            el.local,
            "button" | "input" | "select" | "textarea" | "fieldset"
        )
}

/// `tabindex` parsed per HTML, if it is a valid integer.
pub(crate) fn tab_index_attr(el: &El<'_>) -> Option<i64> {
    parse_int(el.attr("tabindex")?)
}

pub(crate) fn is_content_editable(el: &El<'_>) -> bool {
    el.attr("contenteditable").is_some_and(|v| {
        let v = trim_ws(v);
        v.is_empty() || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("plaintext-only")
    })
}

/// Whether the element is a focusable area: natively, by `tabindex`, or by
/// `contenteditable`; and neither disabled nor inert. Returns the sequential
/// focus navigation value: the `tabindex` if present, else 0 for natively
/// focusable elements.
pub(crate) fn focus_tab_index(el: &El<'_>, ctx: &Ctx) -> Option<i32> {
    if ctx.inert || el.has("inert") || is_disabled(el, ctx) {
        return None;
    }
    let explicit = tab_index_attr(el)
        .map(|n| i32::try_from(n).unwrap_or(if n < 0 { i32::MIN } else { i32::MAX }));
    let native = el.is_html()
        && match el.local {
            "a" | "area" => el.has("href"),
            "button" | "select" | "textarea" | "iframe" => true,
            "input" => input_type(el) != "hidden",
            "audio" | "video" => el.has("controls"),
            "summary" => ctx.parent_is_details && ctx.first_summary == Some(el.id),
            _ => false,
        };
    match explicit {
        Some(n) => Some(n),
        None if native || is_content_editable(el) => Some(0),
        None => None,
    }
}

/// The implicit role of an HTML element (HTML-AAM "Element Role Mappings").
pub(crate) fn implicit_role(el: &El<'_>, ctx: &Ctx) -> (Role, Cond) {
    if !el.is_html() {
        return if el.is_mathml() && el.local == "math" {
            (Role::Math, Cond::No)
        } else {
            (Role::Generic, Cond::No)
        };
    }
    let plain = |r: Role| (r, Cond::No);
    // Table parts of a presentational table, and items of a presentational
    // list, are presentational too (ARIA: required owned elements inherit).
    if ctx.pres_table
        && matches!(
            el.local,
            "tbody" | "thead" | "tfoot" | "tr" | "td" | "th" | "caption"
        )
    {
        return plain(Role::Presentation);
    }
    if ctx.pres_list && el.local == "li" {
        return plain(Role::Presentation);
    }
    if heading_level(el.local).is_some() {
        return plain(Role::Heading);
    }
    match el.local {
        "a" | "area" => {
            if el.has("href") {
                plain(Role::Link)
            } else {
                plain(Role::Generic)
            }
        }
        "article" => plain(Role::Article),
        "aside" => {
            if ctx.sectioning {
                (Role::Complementary, Cond::NamedElseGeneric)
            } else {
                plain(Role::Complementary)
            }
        }
        "blockquote" => plain(Role::Blockquote),
        "button" => plain(Role::Button),
        "caption" | "figcaption" => plain(Role::Caption),
        "code" => plain(Role::Code),
        "dd" => plain(Role::Definition),
        "del" | "s" => plain(Role::Deletion),
        "details" | "fieldset" | "optgroup" | "address" => plain(Role::Group),
        "dfn" | "dt" => plain(Role::Term),
        "dialog" => plain(Role::Dialog),
        "em" => plain(Role::Emphasis),
        "figure" => plain(Role::Figure),
        "footer" => {
            if ctx.sectioning || ctx.in_main {
                plain(Role::Generic)
            } else {
                plain(Role::ContentInfo)
            }
        }
        "form" => (Role::Form, Cond::NamedElseGeneric),
        "header" => {
            if ctx.sectioning || ctx.in_main {
                plain(Role::Generic)
            } else {
                plain(Role::Banner)
            }
        }
        "hr" => plain(Role::Separator),
        "img" => {
            // An empty `alt` marks the image decorative, unless something
            // else makes it meaningful.
            let decorative = el.attr("alt").is_some_and(|a| trim_ws(a).is_empty())
                && !el.has_global_aria()
                && !el.has("title")
                && focus_tab_index(el, ctx).is_none();
            if decorative {
                plain(Role::Presentation)
            } else {
                plain(Role::Img)
            }
        }
        "input" => plain(input_role(el)),
        "ins" => plain(Role::Insertion),
        "li" => plain(Role::ListItem),
        "main" => plain(Role::Main),
        "menu" | "ol" | "ul" | "dir" => plain(Role::List),
        "meter" => plain(Role::Meter),
        "nav" => plain(Role::Navigation),
        "option" => plain(Role::Option),
        "output" => plain(Role::Status),
        "p" => plain(Role::Paragraph),
        "progress" => plain(Role::ProgressBar),
        "search" => plain(Role::Search),
        "section" => (Role::Region, Cond::NamedElseGeneric),
        "select" => {
            let size = el.attr("size").and_then(parse_int).unwrap_or(0);
            if el.has("multiple") || size > 1 {
                plain(Role::Listbox)
            } else {
                plain(Role::Combobox)
            }
        }
        "strong" => plain(Role::Strong),
        "sub" => plain(Role::Subscript),
        "sup" => plain(Role::Superscript),
        // HTML-AAM has no ARIA role for `summary`; it is a disclosure
        // control, and `button` with an expanded state is what it behaves as.
        "summary" => plain(Role::Button),
        "table" => plain(Role::Table),
        "tbody" | "thead" | "tfoot" => plain(Role::RowGroup),
        "td" => plain(if ctx.grid { Role::GridCell } else { Role::Cell }),
        "textarea" => plain(Role::TextBox),
        "th" => plain(table_header_role(el, ctx)),
        "time" => plain(Role::Time),
        "tr" => plain(Role::Row),
        _ => plain(Role::Generic),
    }
}

/// `th`: HTML-AAM maps it to `columnheader`, `rowheader` or `cell` according
/// to the table header algorithm. This implements the `scope` attribute and,
/// without it, the position heuristic: a `th` in `thead` heads a column, any
/// other is taken to head its row only if `scope="row"`, else a column.
fn table_header_role(el: &El<'_>, ctx: &Ctx) -> Role {
    let scope = el.attr("scope").map(trim_ws).unwrap_or("");
    if scope.eq_ignore_ascii_case("row") || scope.eq_ignore_ascii_case("rowgroup") {
        Role::RowHeader
    } else if scope.eq_ignore_ascii_case("col")
        || scope.eq_ignore_ascii_case("colgroup")
        || ctx.thead
    {
        Role::ColumnHeader
    } else if ctx.grid {
        Role::GridCell
    } else {
        Role::ColumnHeader
    }
}

fn input_role(el: &El<'_>) -> Role {
    let list = el.has("list");
    match input_type(el).as_str() {
        "button" | "image" | "reset" | "submit" => Role::Button,
        "checkbox" => Role::Checkbox,
        "radio" => Role::Radio,
        "range" => Role::Slider,
        "number" => Role::SpinButton,
        "search" => {
            if list {
                Role::Combobox
            } else {
                Role::SearchBox
            }
        }
        "email" | "tel" | "text" | "url" | "" => {
            if list {
                Role::Combobox
            } else {
                Role::TextBox
            }
        }
        // Not specified by HTML-AAM ("no corresponding role"), but a text
        // field to the user; the value is never exposed.
        "password" => Role::TextBox,
        // color, date, datetime-local, file, month, time, week, hidden and
        // anything unknown (which HTML treats as text).
        "color" | "date" | "datetime-local" | "file" | "month" | "time" | "week" | "hidden" => {
            Role::Generic
        }
        _ => {
            if list {
                Role::Combobox
            } else {
                Role::TextBox
            }
        }
    }
}

/// The role of an element after the `role` attribute is applied.
pub(crate) fn resolve_role(el: &El<'_>, ctx: &Ctx) -> (Role, Cond) {
    let explicit = el.attr("role").and_then(Role::parse_list);
    match explicit {
        Some(Role::Presentation) if focus_tab_index(el, ctx).is_some() || el.has_global_aria() => {
            // ARIA "Presentational Roles Conflict Resolution": a presentational
            // role is ignored on a focusable element or one with global ARIA
            // attributes.
            implicit_role(el, ctx)
        }
        Some(role) => (role, Cond::No),
        None => implicit_role(el, ctx),
    }
}

fn element_hidden(el: &El<'_>, pctx: &Ctx, inputs: &BuildInputs<'_>) -> bool {
    if aria_bool(el.attr("aria-hidden")) == Some(true) {
        return true;
    }
    if el.is_html() {
        if el.has("hidden") || never_rendered(el.local) {
            return true;
        }
        if el.local == "dialog" && !el.has("open") {
            return true;
        }
        if el.local == "input" && input_type(el) == "hidden" {
            return true;
        }
    }
    if let Some(style) = inputs.style
        && style.hints(el.id).is_some_and(|h| h.display_none)
    {
        return true;
    }
    // Content of a closed `details` other than its first `summary`.
    pctx.parent_is_details && !pctx.details_open && pctx.first_summary != Some(el.id)
}

fn child_context(arena: &Arena, el: &El<'_>, pctx: &Ctx, role: Role) -> Ctx {
    let mut c = *pctx;
    c.parent_is_details = false;
    c.details_open = false;
    c.first_summary = None;
    if el.is_html() {
        match el.local {
            "article" | "aside" | "nav" | "section" => c.sectioning = true,
            "main" => c.in_main = true,
            "details" => {
                c.parent_is_details = true;
                c.details_open = el.has("open");
                c.first_summary = arena
                    .child_ids(el.id)
                    .find(|child| El::of(arena, *child).is_some_and(|e| e.is("summary")));
            }
            "thead" => c.thead = true,
            "tbody" | "tfoot" | "table" => c.thead = false,
            "fieldset" if el.has("disabled") => c.fieldset_disabled = true,
            // The first `legend` child of a disabled fieldset is exempt: its
            // descendants are not disabled by it.
            "legend" if pctx.fieldset_disabled => {
                let parent = arena.get(el.id).and_then(|n| n.parent());
                let first_legend = parent.and_then(|p| {
                    arena
                        .child_ids(p)
                        .find(|k| El::of(arena, *k).is_some_and(|e| e.is("legend")))
                });
                if first_legend == Some(el.id) {
                    c.fieldset_disabled = false;
                }
            }
            _ => {}
        }
        if el.has("inert") {
            c.inert = true;
        }
    }
    if role == Role::Presentation && el.is_html() {
        match el.local {
            "table" => c.pres_table = true,
            "ul" | "ol" | "menu" => c.pres_list = true,
            _ => {}
        }
    }
    match role {
        Role::Grid | Role::TreeGrid => c.grid = true,
        Role::Table => c.grid = false,
        _ => {}
    }
    c
}

impl<'a> Doc<'a> {
    pub fn analyse(arena: &'a Arena, inputs: BuildInputs<'a>) -> Doc<'a> {
        let mut doc = Doc {
            arena,
            inputs,
            order: Vec::new(),
            infos: Vec::new(),
            ids: HashMap::new(),
            labels: HashMap::new(),
            title: None,
        };
        doc.infos.resize(arena.slot_count(), None);

        for id in arena.descendants(arena.document()) {
            doc.order.push(id);
            let Some(node) = arena.get(id) else { continue };
            let parent = node.parent().and_then(|p| doc.info(p).copied());
            let info = match node.data() {
                NodeData::Document | NodeData::Fragment => Info {
                    id,
                    hidden: false,
                    vis_hidden: false,
                    prune: false,
                    child_prune: false,
                    role: Role::Document,
                    cond: Cond::No,
                    child_ctx: Ctx::default(),
                    container: None,
                },
                NodeData::Element { .. } => {
                    let (pctx, phidden, pprune) = parent
                        .map_or((Ctx::default(), false, false), |p| {
                            (p.child_ctx, p.hidden, p.child_prune)
                        });
                    match El::of(arena, id) {
                        Some(el) => {
                            let hidden = phidden || element_hidden(&el, &pctx, &inputs);
                            let vis_hidden = inputs
                                .style
                                .and_then(|s| s.hints(id))
                                .is_some_and(|h| h.visibility_hidden);
                            let (role, cond) = resolve_role(&el, &pctx);
                            if let Some(v) = el.attr("id")
                                && !v.is_empty()
                            {
                                doc.ids.entry(v).or_insert(id);
                            }
                            if doc.title.is_none() && el.is("title") {
                                doc.title = Some(id);
                            }
                            Info {
                                id,
                                hidden,
                                vis_hidden,
                                prune: pprune,
                                child_prune: pprune || role.children_presentational(),
                                role,
                                cond,
                                child_ctx: child_context(arena, &el, &pctx, role),
                                container: None,
                            }
                        }
                        None => Info {
                            id,
                            hidden: true,
                            vis_hidden: false,
                            prune: false,
                            child_prune: false,
                            role: Role::Generic,
                            cond: Cond::No,
                            child_ctx: Ctx::default(),
                            container: None,
                        },
                    }
                }
                NodeData::Text { .. } => {
                    let (hidden, prune) = parent.map_or((false, false), |p| {
                        let closed = p.child_ctx.parent_is_details && !p.child_ctx.details_open;
                        (p.hidden || closed, p.child_prune)
                    });
                    Info {
                        id,
                        hidden,
                        vis_hidden: parent.is_some_and(|p| p.vis_hidden),
                        prune,
                        child_prune: prune,
                        role: Role::StaticText,
                        cond: Cond::No,
                        child_ctx: Ctx::default(),
                        container: None,
                    }
                }
                _ => Info {
                    id,
                    hidden: true,
                    vis_hidden: false,
                    prune: false,
                    child_prune: false,
                    role: Role::Generic,
                    cond: Cond::No,
                    child_ctx: Ctx::default(),
                    container: None,
                },
            };
            if let Some(slot) = doc.infos.get_mut(id.index() as usize) {
                *slot = Some(info);
            }
        }
        doc.collect_labels();
        doc
    }

    /// What the first pass recorded for `id`. `None` for a stale handle or a
    /// node not reachable from the document.
    pub fn info(&self, id: NodeId) -> Option<&Info> {
        self.infos
            .get(id.index() as usize)?
            .as_ref()
            .filter(|i| i.id == id)
    }

    pub fn info_mut(&mut self, id: NodeId) -> Option<&mut Info> {
        self.infos
            .get_mut(id.index() as usize)?
            .as_mut()
            .filter(|i| i.id == id)
    }

    /// `label` elements to the controls they label.
    fn collect_labels(&mut self) {
        let arena = self.arena;
        let mut found: Vec<(NodeId, NodeId)> = Vec::new();
        for id in &self.order {
            let Some(el) = El::of(arena, *id) else {
                continue;
            };
            if !el.is("label") {
                continue;
            }
            let target = match el.attr("for") {
                Some(f) => self
                    .ids
                    .get(f)
                    .copied()
                    .filter(|t| El::of(arena, *t).is_some_and(|e| self.labelable(&e))),
                None => arena
                    .descendants(*id)
                    .skip(1)
                    .take(4096)
                    .find(|d| El::of(arena, *d).is_some_and(|e| self.labelable(&e))),
            };
            if let Some(t) = target {
                found.push((t, *id));
            }
        }
        for (target, label) in found {
            self.labels.entry(target).or_default().push(label);
        }
    }

    pub fn labelable(&self, el: &El<'_>) -> bool {
        el.is_html()
            && match el.local {
                "button" | "meter" | "output" | "progress" | "select" | "textarea" => true,
                "input" => input_type(el) != "hidden",
                _ => false,
            }
    }

    /// The ids named by an IDREF-list attribute that resolve to an element, in
    /// order. At most 64 tokens are considered.
    pub fn idrefs(&self, el: &El<'_>, attr: &str) -> Vec<NodeId> {
        let Some(value) = el.attr(attr) else {
            return Vec::new();
        };
        value
            .split(is_ascii_ws)
            .filter(|t| !t.is_empty())
            .take(64)
            .filter_map(|t| self.ids.get(t).copied())
            .collect()
    }
}
