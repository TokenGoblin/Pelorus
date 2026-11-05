//! Measuring a table and resolving its widths (the Enter-time half).

use super::*;

/// What a cell's own box adds around its content, along the inline axis.
fn surround(e: &Edges) -> Au {
    e.border.inline_sum() + e.padding.inline_sum()
}

impl Table {
    /// Measure every cell and resolve the collapsed borders, so that `min`/`max`
    /// of each cell are final border-box widths.
    fn measure_cells(&mut self, dom: px_css::view::Dom<'_>, arena: &Arena, containing: Au) {
        let mut inner: Vec<(Au, Au)> = Vec::with_capacity(self.cells.len());
        for i in 0..self.cells.len() {
            let (min_max, edges, spec_h, hide) = match &self.cells[i].src {
                CellSrc::Node(n) => {
                    let Some(style) = computed_style(dom, *n) else {
                        inner.push((Au(0), Au(0)));
                        continue;
                    };
                    let mut e = resolve_edges(&style, containing);
                    e.margin = LogicalEdges::ZERO;
                    let raw = crate::block::intrinsic_inline_size_with(dom, arena, *n, true);
                    let s0 = resolve_edges(&style, Au(0));
                    let sur = s0.border.inline_sum() + s0.padding.inline_sum();
                    let mm = ((raw.min - sur).max(Au(0)), (raw.max - sur).max(Au(0)));
                    let h = resolve_block_size(&style, None, Au(0))
                        .map(|h| (h, border_box_sizing(&style)));
                    let hide = style.get_inherited_table().clone_empty_cells()
                        == style::computed_values::empty_cells::T::Hide;
                    (mm, e, h, hide)
                }
                CellSrc::Anon(nodes) => {
                    let raw = crate::block::intrinsic_of_nodes(dom, arena, self.style_node, nodes);
                    ((raw.min, raw.max), Edges::ZERO, None, false)
                }
            };
            inner.push(min_max);
            let c = &mut self.cells[i];
            c.edges = edges;
            c.spec_h = spec_h;
            c.hide_empty = hide;
        }

        if self.collapsed {
            self.resolve_collapsed_borders(dom);
        }

        for (i, (imin, imax)) in inner.into_iter().enumerate() {
            let sur = surround(&self.cells[i].edges);
            let spec = match (&self.cells[i].src, self.cells[i].spec_w) {
                (CellSrc::Node(n), Spec::Length(l)) => {
                    let bb = computed_style(dom, *n).is_some_and(|s| border_box_sizing(&s));
                    Spec::Length(if bb { l } else { l + sur })
                }
                (_, s) => s,
            };
            let c = &mut self.cells[i];
            c.spec_h = c.spec_h.map(|(h, bb)| {
                if bb {
                    (h, true)
                } else {
                    (
                        h + c.edges.border.block_sum() + c.edges.padding.block_sum(),
                        true,
                    )
                }
            });
            c.min = imin + sur;
            c.max = (imax + sur).max(c.min);
            c.spec_w = spec;
        }
    }

    /// Column constraints from the cells and `<col>`s.
    fn column_constraints(&self) -> (Vec<Col>, Vec<Span>) {
        let mut cols = vec![Col::default(); self.ncols];
        let mut spans = Vec::new();
        for (i, spec) in self.cols.iter().enumerate() {
            if let Some(c) = cols.get_mut(i) {
                match spec.spec {
                    Spec::Length(l) => {
                        c.fixed = Some(l);
                        c.max = c.max.max(l);
                    }
                    Spec::Percent(p) => c.pct = c.pct.max(p),
                    Spec::Auto => {}
                }
            }
        }
        for cell in &self.cells {
            let (mut fixed, mut pct) = (None, 0);
            let mut max = cell.max;
            match cell.spec_w {
                Spec::Length(l) => {
                    let l = l.max(cell.min);
                    fixed = Some(l);
                    max = l;
                }
                Spec::Percent(p) => pct = p,
                Spec::Auto => {}
            }
            if cell.colspan == 1 {
                if let Some(c) = cols.get_mut(cell.col) {
                    c.min = c.min.max(cell.min);
                    c.max = c.max.max(max).max(c.min);
                    if let Some(f) = fixed {
                        c.fixed = Some(c.fixed.map_or(f, |x| x.max(f)));
                    }
                    c.pct = c.pct.max(pct);
                }
            } else {
                spans.push(Span {
                    start: cell.col,
                    count: cell.colspan,
                    min: cell.min,
                    max,
                    fixed,
                    pct,
                });
            }
        }
        (cols, spans)
    }

    /// Total of everything in the table's inline axis that is not a column.
    fn inline_extra(&self) -> Au {
        let spacing = if self.ncols == 0 {
            Au(0)
        } else {
            self.spacing.inline * (self.ncols as i32 + 1)
        };
        self.edges.border.inline_sum() + self.edges.padding.inline_sum() + spacing
    }

    /// The table's min-content and max-content (border-box) widths.
    fn intrinsic_widths(&self) -> (Au, Au) {
        let (mut cols, spans) = self.column_constraints();
        columns::apply_spans(&mut cols, &spans, self.spacing.inline);
        columns::table_intrinsic(&cols, self.inline_extra())
    }
}

fn depth_guard() -> Option<DepthToken> {
    DEPTH.with(|d| {
        if d.get() >= MAX_NESTING {
            None
        } else {
            d.set(d.get() + 1);
            Some(DepthToken)
        }
    })
}

struct DepthToken;

impl Drop for DepthToken {
    fn drop(&mut self) {
        DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

fn empty_table(style_node: NodeId, inherit: &ComputedValues) -> Table {
    Table {
        node: None,
        style_node,
        collapsed: false,
        spacing: spacing_of(inherit),
        ncols: 0,
        cols: Vec::new(),
        groups: Vec::new(),
        rows: Vec::new(),
        cells: Vec::new(),
        captions: Vec::new(),
        col_nodes: Vec::new(),
        col_widths: Vec::new(),
        edges: Edges::ZERO,
        specified_height: None,
        height_bounds: Bounds::NONE,
        wrapper_inline: Au(0),
        grid_inline: Au(0),
        borders: None,
    }
}

fn style_node_of(arena: &Arena, node: Option<NodeId>, run: &[NodeId]) -> Option<NodeId> {
    match node {
        Some(n) => Some(n),
        None => {
            let first = *run.first()?;
            arena.get(first)?.parent().or(Some(first))
        }
    }
}

/// The intrinsic border-box widths of the table (real or anonymous) at `node`.
pub(crate) fn intrinsic(
    dom: px_css::view::Dom<'_>,
    arena: &Arena,
    node: NodeId,
    style: &ComputedValues,
) -> Intrinsic {
    let real = is_table(style);
    let run;
    let (nodes, tnode): (Vec<NodeId>, Option<NodeId>) = if real {
        (children_in_order(arena, node), Some(node))
    } else {
        run = internal_run(dom, arena, node);
        (run.clone(), None)
    };
    let Some(style_node) = style_node_of(arena, tnode, &nodes) else {
        return Intrinsic::ZERO;
    };
    let Some(_guard) = depth_guard() else {
        return Intrinsic::ZERO;
    };
    let mut t = build(
        dom,
        arena,
        tnode,
        &nodes,
        style_node,
        real.then_some(style),
        style,
        Au(0),
    );
    t.measure_cells(dom, arena, Au(0));
    let (min, max) = t.intrinsic_widths();
    if !real {
        return Intrinsic { min, max };
    }
    let (mut min, mut max) = (min, max);
    if let Spec::Length(l) = width_spec(style) {
        {
            let l = if border_box_sizing(style) {
                l
            } else {
                l + t.edges.border.inline_sum() + t.edges.padding.inline_sum()
            };
            let w = l.max(min);
            min = w;
            max = w;
        }
    }
    // The caller adds the table's margin, as for any other box.
    let m = resolve_edges(style, Au(0)).margin.inline_sum();
    for c in &t.captions {
        let cm = crate::block::intrinsic_inline_size_with(dom, arena, c.node, false);
        min = min.max(cm.min);
        max = max.max(cm.min);
    }
    Intrinsic {
        min: min + m,
        max: max + m,
    }
}

/// Resolve a table's structure and widths against `containing`.
///
/// `style` is the table's own style for a real table and the enclosing box's for
/// an anonymous one (from which only the inherited table properties are read).
/// `margins` are the table's margins, already resolved with `auto` as zero.
#[allow(clippy::too_many_arguments)]
pub(crate) fn plan(
    dom: px_css::view::Dom<'_>,
    arena: &Arena,
    node: NodeId,
    style: &ComputedValues,
    containing: Au,
    mut margins: LogicalEdges,
    centre_auto: bool,
) -> Plan {
    let real = is_table(style);
    let run;
    let (nodes, tnode): (Vec<NodeId>, Option<NodeId>) = if real {
        (children_in_order(arena, node), Some(node))
    } else {
        run = internal_run(dom, arena, node);
        (run.clone(), None)
    };
    let style_node = style_node_of(arena, tnode, &nodes).unwrap_or(node);
    let guard = depth_guard();
    let mut table = if guard.is_some() {
        build(
            dom,
            arena,
            tnode,
            &nodes,
            style_node,
            real.then_some(style),
            style,
            containing,
        )
    } else {
        empty_table(style_node, style)
    };
    table.node = tnode;
    table.measure_cells(dom, arena, containing);

    let extra = table.inline_extra();
    let (mut cols, spans) = table.column_constraints();
    columns::apply_spans(&mut cols, &spans, table.spacing.inline);
    let (tmin, tmax) = columns::table_intrinsic(&cols, extra);

    let avail = (containing - margins.inline_sum()).max(Au(0));
    let spec = if real { width_spec(style) } else { Spec::Auto };
    let spec_bb = match spec {
        Spec::Length(l) => Some(if border_box_sizing(style) {
            l
        } else {
            l + table.edges.border.inline_sum() + table.edges.padding.inline_sum()
        }),
        Spec::Percent(p) => {
            let w = columns::au64(i64::from(containing.0) * p / 10_000);
            Some(if border_box_sizing(style) {
                w
            } else {
                w + table.edges.border.inline_sum() + table.edges.padding.inline_sum()
            })
        }
        Spec::Auto => None,
    };
    let fixed_layout = real
        && spec_bb.is_some()
        && style.get_table().clone_table_layout() == style::computed_values::table_layout::T::Fixed;

    let mut width = match spec_bb {
        Some(w) => w.max(if fixed_layout { Au(0) } else { tmin }),
        None => {
            let lower = if tmin > avail { tmin } else { avail };
            if lower < tmax { lower } else { tmax }
        }
    };
    if real {
        let b = inline_bounds(style, containing, Au(0));
        let bb_extra = if border_box_sizing(style) {
            Au(0)
        } else {
            table.edges.border.inline_sum() + table.edges.padding.inline_sum()
        };
        let clamped = Bounds {
            min: b.min + bb_extra,
            max: b.max.map(|m| m + bb_extra),
        }
        .clamp(width);
        width = if fixed_layout {
            clamped
        } else {
            clamped.max(tmin)
        };
    }

    let avail_cols = (width - extra).max(Au(0));
    let widths = if fixed_layout {
        let mut known: Vec<Option<Au>> = vec![None; table.ncols];
        for (i, c) in table.cols.iter().enumerate() {
            known[i] = match c.spec {
                Spec::Length(l) => Some(l),
                Spec::Percent(p) => Some(columns::au64(i64::from(avail_cols.0) * p / 10_000)),
                Spec::Auto => None,
            };
        }
        if let Some(first) = table.rows.first() {
            for &ci in &first.cells {
                let cell = &table.cells[ci];
                let w = match cell.spec_w {
                    Spec::Length(l) => Some(l),
                    Spec::Percent(p) => Some(columns::au64(i64::from(avail_cols.0) * p / 10_000)),
                    Spec::Auto => None,
                };
                if let Some(w) = w {
                    let share = (w - table.spacing.inline * (cell.colspan as i32 - 1)).max(Au(0));
                    let ones = vec![1i64; cell.colspan];
                    for (k, part) in columns::split(share, &ones).into_iter().enumerate() {
                        if let Some(slot) = known.get_mut(cell.col + k)
                            && slot.is_none()
                        {
                            *slot = Some(part);
                        }
                    }
                }
            }
        }
        columns::fixed_layout(&known, avail_cols)
    } else {
        columns::distribute(&cols, avail_cols)
    };
    let sum: Au = widths.iter().copied().sum();
    let grid_inline = (sum + extra).max(width.min(sum + extra));
    let grid_inline = if fixed_layout {
        grid_inline.max(width)
    } else {
        grid_inline
    };
    table.col_widths = widths;
    table.grid_inline = grid_inline;

    // The wrapper is as wide as the table, or its widest caption if that is more.
    let mut wrapper = grid_inline;
    for c in &table.captions {
        let m = crate::block::intrinsic_inline_size_with(dom, arena, c.node, false);
        wrapper = wrapper.max(m.min);
    }
    table.wrapper_inline = wrapper;

    if centre_auto && real {
        centre_margins(style, containing, wrapper, &mut margins);
    }
    let has_captions = !table.captions.is_empty();
    let edges = Edges {
        margin: margins,
        border: if has_captions {
            LogicalEdges::ZERO
        } else {
            table.edges.border
        },
        padding: if has_captions {
            LogicalEdges::ZERO
        } else {
            table.edges.padding
        },
    };
    Plan {
        wrapper_inline: wrapper,
        edges,
        node: if has_captions { None } else { tnode },
        table,
    }
}

/// Auto margins on a table split the leftover width, whatever `width` is.
fn centre_margins(style: &ComputedValues, containing: Au, width: Au, m: &mut LogicalEdges) {
    use style::values::generics::length::GenericMargin;
    let margin = style.get_margin();
    let start_auto = matches!(margin.margin_left, GenericMargin::Auto);
    let end_auto = matches!(margin.margin_right, GenericMargin::Auto);
    if !start_auto && !end_auto {
        return;
    }
    let leftover = containing - width - m.inline_sum();
    if leftover <= Au(0) {
        return;
    }
    if start_auto && end_auto {
        let half = leftover / 2;
        m.inline_start = half;
        m.inline_end = leftover - half;
    } else if start_auto {
        m.inline_start = leftover;
    } else {
        m.inline_end = leftover;
    }
}
