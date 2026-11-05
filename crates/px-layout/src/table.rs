//! Table layout: CSS 2.1 chapter 17, plus the behaviour browsers share.
//!
//! # Shape
//!
//! A table is laid out as a unit, in three moments that line up with the three
//! visits `block.rs` makes of every box:
//!
//! - **Enter** ([`plan`]): the table's structure is built from the DOM (anonymous
//!   boxes generated, 17.2.1), every cell's min-content and max-content widths are
//!   measured, and the column widths are resolved (17.5.2). That fixes the table's
//!   own width, which its parent needs *now*, and every cell's width, which the
//!   cells need *before* their contents are laid out.
//! - **Children** ([`State::schedule`]): the row group, row and anonymous-cell
//!   fragments are created and the real cells and captions are scheduled as
//!   ordinary block formatting contexts of a width this module dictates.
//! - **Exit** ([`State::finish`]): row heights come from the cells' laid-out
//!   heights (rowspans, `vertical-align`, table height), and everything is placed.
//!
//! The cells' contents are laid out by the block algorithm; this module never
//! looks inside a cell except to find its first baseline.
//!
//! # Fragment shape
//!
//! `table` (the grid box, border box) > row groups > rows > cells. A table with
//! captions gets an anonymous wrapper above it (17.4) holding the captions and the
//! grid box. Columns and column groups produce childless fragments, positioned over
//! the columns, so their backgrounds paint under the cells.

mod borders;
mod columns;
mod finish;
mod plan;

pub(crate) use finish::{Sched, State};
pub(crate) use plan::{intrinsic, plan};

use std::collections::BTreeMap;

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::properties::ComputedValues;

use crate::block::{
    Bounds, Edges, Intrinsic, block_bounds, children_in_order, computed_style, inline_bounds,
    is_display_none, resolve_block_size, resolve_edges,
};
use crate::fragment::{Fragment, FragmentId, FragmentKind, FragmentTree};
use crate::geom::{LogicalEdges, LogicalSize};
use columns::{Col, Span};

/// What a table-related `display` value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Table,
    InlineTable,
    Caption,
    Group(GroupKind),
    Row,
    Cell,
    Column,
    ColumnGroup,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GroupKind {
    Header,
    Body,
    Footer,
}

pub(crate) fn kind_of(style: &ComputedValues) -> Option<Kind> {
    use style::values::specified::box_::{DisplayInside as I, DisplayOutside as O};
    let d = style.clone_display();
    if matches!(d.outside(), O::TableCaption) {
        return Some(Kind::Caption);
    }
    match d.inside() {
        I::Table if matches!(d.outside(), O::Block) => Some(Kind::Table),
        I::Table if matches!(d.outside(), O::Inline) => Some(Kind::InlineTable),
        I::TableRowGroup => Some(Kind::Group(GroupKind::Body)),
        I::TableHeaderGroup => Some(Kind::Group(GroupKind::Header)),
        I::TableFooterGroup => Some(Kind::Group(GroupKind::Footer)),
        I::TableRow => Some(Kind::Row),
        I::TableCell => Some(Kind::Cell),
        I::TableColumn => Some(Kind::Column),
        I::TableColumnGroup => Some(Kind::ColumnGroup),
        _ => None,
    }
}

/// Whether `style` is a table-internal box (one that needs a table around it).
pub(crate) fn is_internal(style: &ComputedValues) -> bool {
    matches!(
        kind_of(style),
        Some(
            Kind::Caption
                | Kind::Group(_)
                | Kind::Row
                | Kind::Cell
                | Kind::Column
                | Kind::ColumnGroup
        )
    )
}

pub(crate) fn is_table(style: &ComputedValues) -> bool {
    matches!(kind_of(style), Some(Kind::Table | Kind::InlineTable))
}

/// The siblings from `first` on that belong to the same anonymous table: internal
/// table boxes, with whitespace-only text and `display: none` boxes between them
/// (and after them, which are consumed rather than left to form an anonymous
/// block of their own).
pub(crate) fn internal_run(
    dom: px_css::view::Dom<'_>,
    arena: &Arena,
    first: NodeId,
) -> Vec<NodeId> {
    let mut out = vec![first];
    let mut next = arena.get(first).and_then(px_dom::Node::next_sibling);
    while let Some(id) = next {
        let Some(node) = arena.get(id) else { break };
        let keep = if node.element_name().is_some() {
            computed_style(dom, id).is_some_and(|s| is_internal(&s) || is_display_none(&s))
        } else {
            node.text().is_some_and(|t| t.trim().is_empty())
        };
        if !keep {
            break;
        }
        out.push(id);
        next = node.next_sibling();
    }
    out
}

/// A width as the table algorithm sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Spec {
    Auto,
    Length(Au),
    /// Hundredths of a percent.
    Percent(i64),
}

fn width_spec(style: &ComputedValues) -> Spec {
    use style::values::computed::Size;
    match style.clone_width() {
        Size::LengthPercentage(ref lp) => match lp.0.maybe_to_used_value(None) {
            Some(a) => Spec::Length(a.max(Au(0))),
            None => {
                let basis = crate::geom::au(60_000_000);
                Spec::Percent((i64::from(lp.0.to_used_value(basis).0) + 3000) / 6000)
            }
        },
        _ => Spec::Auto,
    }
}

fn border_box_sizing(style: &ComputedValues) -> bool {
    style.clone_box_sizing() == style::computed_values::box_sizing::T::BorderBox
}

#[derive(Clone, Debug)]
enum CellSrc {
    Node(NodeId),
    Anon(Vec<NodeId>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VAlign {
    Baseline,
    Top,
    Middle,
    Bottom,
}

struct Cell {
    src: CellSrc,
    row: usize,
    col: usize,
    colspan: usize,
    rowspan: usize,
    edges: Edges,
    /// The raw `border-*` widths before the collapsing model halves them.
    spec_w: Spec,
    /// Height asked for, and whether it is a border-box length.
    spec_h: Option<(Au, bool)>,
    min: Au,
    max: Au,
    valign: VAlign,
    hide_empty: bool,
    frag: Option<FragmentId>,
}

struct Row {
    node: Option<NodeId>,
    group: usize,
    cells: Vec<usize>,
    spec_h: Option<Au>,
    frag: Option<FragmentId>,
    height: Au,
}

struct Group {
    node: Option<NodeId>,
    first_row: usize,
    rows: usize,
    frag: Option<FragmentId>,
}

struct ColSpec {
    spec: Spec,
}

/// A `<col>` or `<colgroup>`, for painting its background.
struct ColNode {
    node: NodeId,
    first: usize,
    count: usize,
}

struct Caption {
    node: NodeId,
    top: bool,
    edges: Edges,
    frag: Option<FragmentId>,
}

/// One table, built and measured.
pub(crate) struct Table {
    node: Option<NodeId>,
    style_node: NodeId,
    collapsed: bool,
    spacing: Gap,
    ncols: usize,
    cols: Vec<ColSpec>,
    groups: Vec<Group>,
    rows: Vec<Row>,
    cells: Vec<Cell>,
    captions: Vec<Caption>,
    col_nodes: Vec<ColNode>,
    col_widths: Vec<Au>,
    /// The grid box's own border and padding (margins are the wrapper's).
    edges: Edges,
    specified_height: Option<Au>,
    height_bounds: Bounds,
    wrapper_inline: Au,
    grid_inline: Au,
    borders: Option<borders::Resolved>,
}

/// `border-spacing`: the gap between cells along each axis.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Gap {
    pub(crate) inline: Au,
    pub(crate) block: Au,
}

impl Gap {
    const ZERO: Self = Self {
        inline: Au(0),
        block: Au(0),
    };
}

/// What `plan` hands the block algorithm.
pub(crate) struct Plan {
    pub(crate) table: Table,
    /// The width of the box `Enter` creates (the wrapper if there are captions).
    pub(crate) wrapper_inline: Au,
    /// The edges of that box: margins always; border and padding only when it is
    /// the grid box itself.
    pub(crate) edges: Edges,
    pub(crate) node: Option<NodeId>,
}

// ---------------------------------------------------------------------------
// Structure

#[derive(Clone, Debug)]
enum Tok {
    Cell(NodeId),
    Row(NodeId),
    Group(NodeId, GroupKind),
    Caption(NodeId),
    Col(NodeId),
    ColGroup(NodeId),
    Content(Vec<NodeId>),
}

fn tokenize(dom: px_css::view::Dom<'_>, arena: &Arena, nodes: &[NodeId]) -> Vec<Tok> {
    let mut out: Vec<Tok> = Vec::new();
    let mut run: Vec<NodeId> = Vec::new();
    let flush = |run: &mut Vec<NodeId>, out: &mut Vec<Tok>| {
        let meaningful = run.iter().any(|id| {
            arena.get(*id).is_some_and(|n| {
                n.element_name().is_some() || n.text().is_some_and(|t| !t.trim().is_empty())
            })
        });
        if meaningful {
            out.push(Tok::Content(std::mem::take(run)));
        } else {
            run.clear();
        }
    };
    for &id in nodes {
        let Some(node) = arena.get(id) else { continue };
        if node.element_name().is_none() {
            if node.text().is_some() {
                run.push(id);
            }
            continue;
        }
        let Some(style) = computed_style(dom, id) else {
            continue;
        };
        if is_display_none(&style) {
            continue;
        }
        let tok = match kind_of(&style) {
            Some(Kind::Cell) => Tok::Cell(id),
            Some(Kind::Row) => Tok::Row(id),
            Some(Kind::Group(k)) => Tok::Group(id, k),
            Some(Kind::Caption) => Tok::Caption(id),
            Some(Kind::Column) => Tok::Col(id),
            Some(Kind::ColumnGroup) => Tok::ColGroup(id),
            _ => {
                run.push(id);
                continue;
            }
        };
        flush(&mut run, &mut out);
        out.push(tok);
    }
    flush(&mut run, &mut out);
    out
}

struct RowSrc {
    node: Option<NodeId>,
    cells: Vec<CellSrc>,
}

fn cells_of(toks: &[Tok]) -> Vec<CellSrc> {
    toks.iter()
        .filter_map(|t| match t {
            Tok::Cell(n) => Some(CellSrc::Node(*n)),
            Tok::Content(v) => Some(CellSrc::Anon(v.clone())),
            _ => None,
        })
        .collect()
}

fn rows_from(dom: px_css::view::Dom<'_>, arena: &Arena, toks: &[Tok]) -> Vec<RowSrc> {
    let mut rows = Vec::new();
    let mut loose: Vec<Tok> = Vec::new();
    let end_loose = |loose: &mut Vec<Tok>, rows: &mut Vec<RowSrc>| {
        if !loose.is_empty() {
            rows.push(RowSrc {
                node: None,
                cells: cells_of(loose),
            });
            loose.clear();
        }
    };
    // A row group inside a row group contributes its children as if they were
    // the outer group's own (the inner group's box is dropped), which is what
    // browsers do with `display: table-row-group` nested in one.
    let mut work: Vec<Tok> = toks.iter().rev().cloned().collect();
    while let Some(t) = work.pop() {
        let t = &t;
        match t {
            Tok::Group(n, _) => {
                let kids = tokenize(dom, arena, &children_in_order(arena, *n));
                work.extend(kids.into_iter().rev());
            }
            Tok::Row(n) => {
                end_loose(&mut loose, &mut rows);
                let kids = tokenize(dom, arena, &children_in_order(arena, *n));
                rows.push(RowSrc {
                    node: Some(*n),
                    cells: cells_of(&kids),
                });
            }
            Tok::Cell(_) | Tok::Content(_) => loose.push(t.clone()),
            _ => {}
        }
    }
    end_loose(&mut loose, &mut rows);
    rows
}

fn int_attr(arena: &Arena, id: NodeId, name: &str) -> Option<u32> {
    let v = arena
        .get(id)?
        .attrs()?
        .iter()
        .find(|a| &*a.name.local == name)?
        .value
        .trim()
        .to_owned();
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().ok()
}

/// The cap on grid slots marked occupied, so a hostile `rowspan`/`colspan` pair
/// cannot cost more than a bounded amount of work.
const SLOT_BUDGET: usize = 2_000_000;
const MAX_COLUMNS: usize = 1000;

struct Shape {
    groups: Vec<(Option<NodeId>, GroupKind, Vec<RowSrc>)>,
    captions: Vec<NodeId>,
    columns: Vec<(Option<NodeId>, NodeId, usize)>,
}

fn shape(dom: px_css::view::Dom<'_>, arena: &Arena, nodes: &[NodeId]) -> Shape {
    let toks = tokenize(dom, arena, nodes);
    let mut s = Shape {
        groups: Vec::new(),
        captions: Vec::new(),
        columns: Vec::new(),
    };
    let mut loose: Vec<Tok> = Vec::new();
    let end_loose = |loose: &mut Vec<Tok>, s: &mut Shape| {
        if !loose.is_empty() {
            s.groups
                .push((None, GroupKind::Body, rows_from(dom, arena, loose)));
            loose.clear();
        }
    };
    for t in &toks {
        match t {
            Tok::Group(n, k) => {
                end_loose(&mut loose, &mut s);
                let kids = tokenize(dom, arena, &children_in_order(arena, *n));
                s.groups.push((Some(*n), *k, rows_from(dom, arena, &kids)));
            }
            Tok::Caption(n) => s.captions.push(*n),
            Tok::ColGroup(n) => {
                let kids: Vec<NodeId> = children_in_order(arena, *n)
                    .into_iter()
                    .filter(|k| {
                        computed_style(dom, *k)
                            .is_some_and(|st| matches!(kind_of(&st), Some(Kind::Column)))
                    })
                    .collect();
                if kids.is_empty() {
                    let span = int_attr(arena, *n, "span").unwrap_or(1).clamp(1, 1000) as usize;
                    s.columns.push((Some(*n), *n, span));
                } else {
                    for k in kids {
                        let span = int_attr(arena, k, "span").unwrap_or(1).clamp(1, 1000) as usize;
                        s.columns.push((Some(*n), k, span));
                    }
                }
            }
            Tok::Col(n) => {
                let span = int_attr(arena, *n, "span").unwrap_or(1).clamp(1, 1000) as usize;
                s.columns.push((None, *n, span));
            }
            Tok::Row(_) | Tok::Cell(_) | Tok::Content(_) => loose.push(t.clone()),
        }
    }
    end_loose(&mut loose, &mut s);
    s
}

// ---------------------------------------------------------------------------
// Measuring

thread_local! {
    static DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
/// Nested tables measure their cells recursively (a cell's min-content width is
/// the min-content width of the table inside it). A page can nest them without
/// bound, so past this depth a table measures as empty.
const MAX_NESTING: u32 = 20;

fn collapsed_of(style: &ComputedValues) -> bool {
    style.get_inherited_table().clone_border_collapse()
        == style::computed_values::border_collapse::T::Collapse
}

fn spacing_of(style: &ComputedValues) -> Gap {
    let s = style.get_inherited_table().border_spacing;
    Gap {
        inline: crate::geom::au(s.0.width.0.to_i32_au()).max(Au(0)),
        block: crate::geom::au(s.0.height.0.to_i32_au()).max(Au(0)),
    }
}

fn valign_of(style: &ComputedValues) -> VAlign {
    use crate::inline::VAlign as V;
    match crate::inline::vertical_align_of(style, crate::text::TextStyle::of(style)) {
        V::Top => VAlign::Top,
        V::Bottom => VAlign::Bottom,
        V::Middle => VAlign::Middle,
        _ => VAlign::Baseline,
    }
}

/// Build the structure of a table from its children and measure it.
///
/// `node` is the table element (`None` for an anonymous table), `nodes` the boxes
/// that make it up, `own` the table's own style (`None` for anonymous) and
/// `inherit` the style the inherited table properties come from.
#[allow(clippy::too_many_arguments)]
fn build(
    dom: px_css::view::Dom<'_>,
    arena: &Arena,
    node: Option<NodeId>,
    nodes: &[NodeId],
    style_node: NodeId,
    own: Option<&ComputedValues>,
    inherit: &ComputedValues,
    containing: Au,
) -> Table {
    let shape = shape(dom, arena, nodes);
    let collapsed = collapsed_of(inherit);
    let spacing = if collapsed {
        Gap::ZERO
    } else {
        spacing_of(inherit)
    };

    // Columns declared by <col>.
    let mut cols: Vec<ColSpec> = Vec::new();
    let mut col_nodes = Vec::new();
    for (group, col, span) in &shape.columns {
        let spec = computed_style(dom, *col).map_or(Spec::Auto, |s| width_spec(&s));
        let first = cols.len();
        for _ in 0..(*span).min(MAX_COLUMNS.saturating_sub(cols.len())) {
            cols.push(ColSpec { spec });
        }
        let count = cols.len() - first;
        match group {
            Some(g) => {
                if g != col {
                    col_nodes.push(ColNode {
                        node: *col,
                        first,
                        count,
                    });
                }
                if let Some(c) = col_nodes.iter_mut().find(|c| c.node == *g) {
                    c.count += count;
                } else {
                    col_nodes.push(ColNode {
                        node: *g,
                        first,
                        count,
                    });
                }
            }
            None => col_nodes.push(ColNode {
                node: *col,
                first,
                count,
            }),
        }
    }
    let declared_cols = cols.len();

    // Row groups in visual order: header, bodies, footer (17.2.1 / 17.5.3).
    let mut order: Vec<usize> = Vec::new();
    for want in [GroupKind::Header, GroupKind::Body, GroupKind::Footer] {
        for (i, (_, k, _)) in shape.groups.iter().enumerate() {
            let eff = if want == GroupKind::Header || want == GroupKind::Footer {
                // Only the first header and footer are displaced.
                let first = shape.groups.iter().position(|(_, kk, _)| *kk == want);
                if *k == want && first == Some(i) {
                    want
                } else if *k == want {
                    GroupKind::Body
                } else {
                    *k
                }
            } else {
                // Bodies, and the repeats of header/footer demoted above.
                let first_h = shape
                    .groups
                    .iter()
                    .position(|(_, kk, _)| *kk == GroupKind::Header);
                let first_f = shape
                    .groups
                    .iter()
                    .position(|(_, kk, _)| *kk == GroupKind::Footer);
                match *k {
                    GroupKind::Header if first_h != Some(i) => GroupKind::Body,
                    GroupKind::Footer if first_f != Some(i) => GroupKind::Body,
                    other => other,
                }
            };
            if eff == want {
                order.push(i);
            }
        }
    }

    let mut groups: Vec<Group> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    let mut cells: Vec<Cell> = Vec::new();
    let mut occupied: std::collections::BTreeSet<(usize, usize)> =
        std::collections::BTreeSet::new();
    let mut budget = SLOT_BUDGET;
    let mut max_col = declared_cols;

    for gi in order {
        let (gnode, _, row_srcs) = &shape.groups[gi];
        let group_index = groups.len();
        let first_row = rows.len();
        groups.push(Group {
            node: *gnode,
            first_row,
            rows: row_srcs.len(),
            frag: None,
        });
        for (ri, rs) in row_srcs.iter().enumerate() {
            let row_index = rows.len();
            let spec_h = rs
                .node
                .and_then(|n| computed_style(dom, n))
                .and_then(|s| resolve_block_size(&s));
            rows.push(Row {
                node: rs.node,
                group: group_index,
                cells: Vec::new(),
                spec_h,
                frag: None,
                height: Au(0),
            });
            let mut x = 0usize;
            for src in &rs.cells {
                let (style, cnode) = match src {
                    CellSrc::Node(n) => (computed_style(dom, *n), Some(*n)),
                    CellSrc::Anon(_) => (None, None),
                };
                let colspan = cnode
                    .and_then(|n| int_attr(arena, n, "colspan"))
                    .unwrap_or(1)
                    .clamp(1, 1000) as usize;
                let rowspan_attr = cnode.and_then(|n| int_attr(arena, n, "rowspan"));
                let left_in_group = row_srcs.len() - ri;
                let rowspan = match rowspan_attr {
                    Some(0) => left_in_group,
                    Some(n) => (n as usize).clamp(1, 65_534).min(left_in_group),
                    None => 1,
                };
                while occupied.contains(&(row_index, x)) {
                    x += 1;
                }
                let colspan = colspan.min(MAX_COLUMNS.saturating_sub(x).max(1));
                let rowspan = if colspan * rowspan <= budget {
                    rowspan
                } else {
                    1
                };
                for r in row_index..row_index + rowspan {
                    for c in x..x + colspan {
                        occupied.insert((r, c));
                    }
                }
                budget = budget.saturating_sub(colspan * rowspan);
                let cell_index = cells.len();
                cells.push(Cell {
                    src: src.clone(),
                    row: row_index,
                    col: x,
                    colspan,
                    rowspan,
                    edges: Edges::ZERO,
                    spec_w: style.as_deref().map_or(Spec::Auto, width_spec),
                    spec_h: None,
                    min: Au(0),
                    max: Au(0),
                    valign: style.as_deref().map_or(VAlign::Baseline, valign_of),
                    hide_empty: false,
                    frag: None,
                });
                if let Some(r) = rows.last_mut() {
                    r.cells.push(cell_index);
                }
                x += colspan;
                max_col = max_col.max(x);
            }
        }
    }
    let fixed_table = own.is_some_and(|s| {
        s.get_table().clone_table_layout() == style::computed_values::table_layout::T::Fixed
    });
    // Under `table-layout: fixed` the columns are those the cells reach, but a
    // spanning cell cannot create columns beyond the declared `<col>`s.
    let ncols = if fixed_table && declared_cols > 0 {
        let reach = cells.iter().map(|c| c.col + 1).max().unwrap_or(0);
        for c in &mut cells {
            c.colspan = c.colspan.min(declared_cols.saturating_sub(c.col).max(1));
        }
        let extent = cells
            .iter()
            .map(|c| c.col + c.colspan)
            .max()
            .unwrap_or(reach);
        extent.min(declared_cols).max(reach).min(MAX_COLUMNS)
    } else {
        max_col.min(MAX_COLUMNS)
    };
    cols.truncate(ncols.max(declared_cols.min(ncols)));
    while cols.len() < ncols {
        cols.push(ColSpec { spec: Spec::Auto });
    }

    // Edges and the table's own style.
    let (edges, specified_height, height_bounds) = match own {
        Some(s) => {
            let mut e = resolve_edges(s, containing);
            e.margin = LogicalEdges::ZERO;
            (e, resolve_block_size(s), block_bounds(s))
        }
        None => (Edges::ZERO, None, Bounds::NONE),
    };

    let captions = shape
        .captions
        .iter()
        .filter_map(|n| {
            let s = computed_style(dom, *n)?;
            let top = style_caption_top(&s);
            Some(Caption {
                node: *n,
                top,
                edges: resolve_edges(&s, containing),
                frag: None,
            })
        })
        .collect();

    Table {
        node,
        style_node,
        collapsed,
        spacing,
        ncols,
        cols,
        groups,
        rows,
        cells,
        captions,
        col_nodes,
        col_widths: Vec::new(),
        edges,
        specified_height,
        height_bounds,
        wrapper_inline: Au(0),
        grid_inline: Au(0),
        borders: None,
    }
}

fn style_caption_top(style: &ComputedValues) -> bool {
    use style::computed_values::caption_side::T as S;
    !matches!(style.get_inherited_table().clone_caption_side(), S::Bottom)
}
