//! Scheduling a table's boxes and placing them once the cells are laid out.

use super::*;

/// What a cell or caption is told about itself before it is entered.
#[derive(Clone, Copy)]
pub(crate) struct Forced {
    /// Whether `height` and `min-height` apply to the box itself (a caption)
    /// rather than to the row it sits in (a cell).
    pub honour_height: bool,
    /// Content-box inline size.
    pub content_inline: Au,
    /// Border, padding and margin (margin is zero for a cell).
    pub edges: Edges,
}

/// A box the block algorithm must visit on behalf of a table.
pub(crate) enum Sched {
    /// A real cell or caption: `Enter` it as a child of `parent` in `slot`.
    Real {
        node: NodeId,
        parent: FragmentId,
        slot: usize,
    },
    /// An anonymous cell, already created: lay out `nodes` in it.
    Anon {
        fragment: FragmentId,
        container: NodeId,
        nodes: Vec<NodeId>,
        width: Au,
    },
}

/// Tables between their `Children` and `Exit` visits, and the cells' forced sizes.
#[derive(Default)]
pub(crate) struct State {
    tables: BTreeMap<usize, Table>,
    forced: BTreeMap<(usize, usize), Forced>,
    /// `(parent fragment, slot)` of each cell/caption, to find its fragment.
    slots: BTreeMap<(usize, usize), (usize, SlotRole)>,
}

#[derive(Clone, Copy)]
enum SlotRole {
    Cell(usize),
    Caption(usize),
}

impl State {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register(&mut self, fragment: FragmentId, table: Table) {
        self.tables.insert(fragment.index(), table);
    }

    pub(crate) fn is_table(&self, fragment: FragmentId) -> bool {
        self.tables.contains_key(&fragment.index())
    }

    pub(crate) fn forced(&self, parent: FragmentId, slot: usize) -> Option<Forced> {
        self.forced.get(&(parent.index(), slot)).copied()
    }

    /// `Enter` created `fragment` for the cell or caption at `(parent, slot)`.
    pub(crate) fn entered(&mut self, parent: FragmentId, slot: usize, fragment: FragmentId) {
        let Some((table, role)) = self.slots.get(&(parent.index(), slot)).copied() else {
            return;
        };
        if let Some(t) = self.tables.get_mut(&table) {
            match role {
                SlotRole::Cell(i) => {
                    if let Some(c) = t.cells.get_mut(i) {
                        c.frag = Some(fragment);
                    }
                }
                SlotRole::Caption(i) => {
                    if let Some(c) = t.captions.get_mut(i) {
                        c.frag = Some(fragment);
                    }
                }
            }
        }
    }

    /// Create the structural fragments of the table at `fragment` and say which
    /// boxes inside it must now be laid out.
    ///
    /// `grid` is the fragment of the grid box: `fragment` itself, or a child of it
    /// when there are captions (created here).
    pub(crate) fn schedule(&mut self, tree: &mut FragmentTree, fragment: FragmentId) -> Vec<Sched> {
        let Some(t) = self.tables.get_mut(&fragment.index()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let key = fragment.index();

        // Captions are children of the wrapper (`fragment`) when there is one.
        let mut forced_new: Vec<((usize, usize), Forced)> = Vec::new();
        let mut slot_new: Vec<((usize, usize), (usize, SlotRole))> = Vec::new();
        if !t.captions.is_empty() {
            for (i, c) in t.captions.iter().enumerate() {
                let width = (t.wrapper_inline - c.edges.margin.inline_sum()).max(Au(0));
                forced_new.push((
                    (key, i),
                    Forced {
                        honour_height: true,
                        content_inline: (width - c.edges.border.inline_sum() - c.edges.padding.inline_sum())
                            .max(Au(0)),
                        edges: c.edges,
                    },
                ));
                slot_new.push(((key, i), (key, SlotRole::Caption(i))));
                out.push(Sched::Real {
                    node: c.node,
                    parent: fragment,
                    slot: i,
                });
            }
        }

        // Column fragments, then groups, rows.
        for g in 0..t.groups.len() {
            let f = tree.push(Fragment::new(FragmentKind::Block, LogicalSize::ZERO));
            if let Some(fr) = tree.get_mut(f) {
                fr.node = t.groups[g].node;
                if fr.node.is_none() {
                    fr.kind = FragmentKind::AnonymousBlock;
                }
            }
            t.groups[g].frag = Some(f);
        }
        for r in 0..t.rows.len() {
            let f = tree.push(Fragment::new(FragmentKind::Block, LogicalSize::ZERO));
            if let Some(fr) = tree.get_mut(f) {
                fr.node = t.rows[r].node;
                if fr.node.is_none() {
                    fr.kind = FragmentKind::AnonymousBlock;
                }
            }
            t.rows[r].frag = Some(f);
        }
        for r in 0..t.rows.len() {
            let Some(row_frag) = t.rows[r].frag else {
                continue;
            };
            for (slot, &ci) in t.rows[r].cells.iter().enumerate() {
                let cell = &t.cells[ci];
                let end = (cell.col + cell.colspan).min(t.col_widths.len());
                let mut width: Au = t.col_widths[cell.col.min(end)..end].iter().copied().sum();
                width += t.spacing.0 * (end.saturating_sub(cell.col) as i32 - 1).max(0);
                match &cell.src {
                    CellSrc::Node(n) => {
                        let e = cell.edges;
                        forced_new.push((
                            (row_frag.index(), slot),
                            Forced {
                                honour_height: false,
                                content_inline: (width - e.border.inline_sum() - e.padding.inline_sum())
                                    .max(Au(0)),
                                edges: e,
                            },
                        ));
                        slot_new.push(((row_frag.index(), slot), (key, SlotRole::Cell(ci))));
                        out.push(Sched::Real {
                            node: *n,
                            parent: row_frag,
                            slot,
                        });
                    }
                    CellSrc::Anon(nodes) => {
                        let f = tree.push(Fragment::new(
                            FragmentKind::AnonymousBlock,
                            LogicalSize::new(width, Au(0)),
                        ));
                        out.push(Sched::Anon {
                            fragment: f,
                            container: t.style_node,
                            nodes: nodes.clone(),
                            width,
                        });
                        // The row's fragment list is assembled in `finish`; the
                        // anonymous cell is known by its slot in the row.
                        t.cells[ci].frag = Some(f);
                    }
                }
            }
        }
        for (k, v) in forced_new {
            self.forced.insert(k, v);
        }
        for (k, v) in slot_new {
            self.slots.insert(k, v);
        }
        out
    }

    /// Place everything. Returns `true` if `fragment` was a table.
    pub(crate) fn finish(&mut self, tree: &mut FragmentTree, fragment: FragmentId) -> bool {
        let Some(mut t) = self.tables.remove(&fragment.index()) else {
            return false;
        };
        t.place(tree, fragment);
        true
    }
}

fn first_baseline(tree: &FragmentTree, root: FragmentId) -> Option<Au> {
    let mut stack = vec![(root, Au(0))];
    while let Some((id, top)) = stack.pop() {
        let f = tree.get(id)?;
        let here = if id == root { top } else { top + f.block_offset };
        match f.kind {
            FragmentKind::Line => return Some(here + f.baseline),
            FragmentKind::Block | FragmentKind::AnonymousBlock => {
                for kid in tree.children(id).iter().rev() {
                    stack.push((*kid, here));
                }
            }
            FragmentKind::Inline { .. } => {}
        }
    }
    None
}

fn shift_content(tree: &mut FragmentTree, cell: FragmentId, delta: Au) {
    if delta == Au(0) {
        return;
    }
    let kids: Vec<FragmentId> = tree.children(cell).to_vec();
    for k in kids {
        if let Some(f) = tree.get_mut(k) {
            f.block_offset += delta;
        }
    }
}

impl Table {
    fn place(&mut self, tree: &mut FragmentTree, fragment: FragmentId) {
        let sp = self.spacing;
        let nrows = self.rows.len();
        let has_caption = !self.captions.is_empty();

        // Natural cell heights and baselines.
        let mut natural: Vec<Au> = vec![Au(0); self.cells.len()];
        let mut baseline: Vec<Au> = vec![Au(0); self.cells.len()];
        for (i, c) in self.cells.iter().enumerate() {
            let Some(f) = c.frag else { continue };
            let h = tree.get(f).map_or(Au(0), |f| f.size.block);
            natural[i] = h.max(c.spec_h.map_or(Au(0), |(h, _)| h));
            baseline[i] = first_baseline(tree, f).unwrap_or_else(|| {
                (h - c.edges.border.block_end - c.edges.padding.block_end).max(Au(0))
            });
        }

        // Row heights from rowspan-1 cells.
        let mut row_base: Vec<Au> = vec![Au(0); nrows];
        for (r, row) in self.rows.iter().enumerate() {
            let mut h = row.spec_h.unwrap_or(Au(0));
            let (mut asc, mut desc) = (Au(0), Au(0));
            for &ci in &row.cells {
                let c = &self.cells[ci];
                if c.rowspan != 1 {
                    continue;
                }
                if c.valign == VAlign::Baseline {
                    asc = asc.max(baseline[ci]);
                    desc = desc.max((natural[ci] - baseline[ci]).max(Au(0)));
                } else {
                    h = h.max(natural[ci]);
                }
            }
            row_base[r] = h.max(asc + desc);
        }
        // Spanning cells: whatever they need beyond their rows is spread equally.
        for (ci, c) in self.cells.iter().enumerate() {
            if c.rowspan <= 1 {
                continue;
            }
            let end = (c.row + c.rowspan).min(nrows);
            let have: Au = row_base[c.row..end].iter().copied().sum::<Au>()
                + sp.1 * (end - c.row - 1) as i32;
            if natural[ci] > have {
                let ones = vec![1i64; end - c.row];
                for (k, d) in columns::split(natural[ci] - have, &ones).into_iter().enumerate() {
                    row_base[c.row + k] += d;
                }
            }
        }

        // The table's height: stated height and min/max-height add to the rows.
        let rows_sum: Au = row_base.iter().copied().sum();
        let spacing_total = if nrows == 0 { Au(0) } else { sp.1 * (nrows as i32 + 1) };
        let frame = self.edges.border.block_sum() + self.edges.padding.block_sum();
        let base = frame + spacing_total + rows_sum;
        let wanted = self.height_bounds.clamp(self.specified_height.unwrap_or(base));
        let target = base.max(wanted);
        if target > base && nrows > 0 {
            let weights: Vec<i64> = row_base.iter().map(|h| i64::from(h.0)).collect();
            for (h, d) in row_base.iter_mut().zip(columns::split(target - base, &weights)) {
                *h += d;
            }
        }
        for (r, row) in self.rows.iter_mut().enumerate() {
            row.height = row_base[r];
        }

        // Positions, in the grid box's coordinates.
        let x0 = self.edges.border.inline_start + self.edges.padding.inline_start;
        let y0 = self.edges.border.block_start + self.edges.padding.block_start;
        let mut col_x: Vec<Au> = Vec::with_capacity(self.ncols + 1);
        let mut x = x0 + sp.0;
        for w in &self.col_widths {
            col_x.push(x);
            x += *w + sp.0;
        }
        col_x.push(x);
        let grid_w: Au = self.col_widths.iter().copied().sum::<Au>()
            + sp.0 * (self.ncols as i32 - 1).max(0);
        let grid_x = x0 + sp.0;

        let mut row_y: Vec<Au> = vec![Au(0); nrows];
        let mut y = y0 + if nrows > 0 { sp.1 } else { Au(0) };
        for r in 0..nrows {
            row_y[r] = y;
            y += self.rows[r].height + sp.1;
        }
        let rows_end = if nrows > 0 { y - sp.1 } else { y0 };
        let grid_h = if nrows > 0 { rows_end - (y0 + sp.1) } else { Au(0) };

        // Groups.
        let mut group_frags: Vec<FragmentId> = Vec::new();
        for g in &self.groups {
            let Some(gf) = g.frag else { continue };
            if g.rows == 0 {
                continue;
            }
            let (first, last) = (g.first_row, g.first_row + g.rows - 1);
            let top = row_y[first];
            let bottom = row_y[last] + self.rows[last].height;
            if let Some(f) = tree.get_mut(gf) {
                f.inline_offset = grid_x;
                f.block_offset = top;
                f.size = LogicalSize::new(grid_w, bottom - top);
            }
            let mut row_frags = Vec::new();
            for r in first..=last {
                let Some(rf) = self.rows[r].frag else { continue };
                if let Some(f) = tree.get_mut(rf) {
                    f.inline_offset = Au(0);
                    f.block_offset = row_y[r] - top;
                    f.size = LogicalSize::new(grid_w, self.rows[r].height);
                }
                let mut cell_frags = Vec::new();
                for &ci in &self.rows[r].cells {
                    let c = &self.cells[ci];
                    let Some(cf) = c.frag else { continue };
                    let cend = (c.col + c.colspan).min(self.ncols);
                    let cw = col_x[cend] - col_x[c.col] - sp.0;
                    let rend = (c.row + c.rowspan).min(nrows);
                    let ch: Au = (c.row..rend).map(|k| self.rows[k].height).sum::<Au>()
                        + sp.1 * (rend - c.row - 1) as i32;
                    let natural_h = natural[ci].min(ch);
                    let extra = (ch - tree.get(cf).map_or(Au(0), |f| f.size.block)).max(Au(0));
                    let shift = match c.valign {
                        VAlign::Top => Au(0),
                        VAlign::Bottom => extra,
                        VAlign::Middle => extra / 2,
                        VAlign::Baseline => {
                            if c.rowspan == 1 {
                                let asc = self.rows[r]
                                    .cells
                                    .iter()
                                    .filter(|k| {
                                        self.cells[**k].rowspan == 1
                                            && self.cells[**k].valign == VAlign::Baseline
                                    })
                                    .map(|k| baseline[*k])
                                    .max()
                                    .unwrap_or(Au(0));
                                (asc - baseline[ci]).max(Au(0))
                            } else {
                                Au(0)
                            }
                        }
                    };
                    let _ = natural_h;
                    shift_content(tree, cf, shift.min(extra));
                    let empty = tree.children(cf).is_empty();
                    if let Some(f) = tree.get_mut(cf) {
                        f.inline_offset = col_x[c.col] - grid_x;
                        f.block_offset = Au(0);
                        f.size = LogicalSize::new(cw, ch);
                        if c.hide_empty && empty && !self.collapsed {
                            f.node = None;
                        }
                    }
                    cell_frags.push(cf);
                }
                tree.set_children(rf, &cell_frags);
                row_frags.push(rf);
            }
            tree.set_children(gf, &row_frags);
            group_frags.push(gf);
        }

        // Columns and column groups paint under the cells.
        let mut grid_kids: Vec<FragmentId> = Vec::new();
        let (cols_top, cols_h) = (y0 + sp.1.min(grid_h), grid_h);
        for cn in &self.col_nodes {
            if cn.count == 0 || cn.first >= self.ncols {
                continue;
            }
            let end = (cn.first + cn.count).min(self.ncols);
            let mut f = Fragment::new(
                FragmentKind::Block,
                LogicalSize::new(col_x[end] - col_x[cn.first] - sp.0, cols_h),
            );
            f.node = Some(cn.node);
            f.inline_offset = col_x[cn.first];
            f.block_offset = cols_top;
            grid_kids.push(tree.push(f));
        }
        grid_kids.extend(group_frags);

        // Collapsed borders paint as filled rectangles over the cells.
        if let Some(b) = self.borders.take() {
            grid_kids.extend(b.fills(tree, &col_x, &row_y, &self.rows_heights(), sp));
        }

        let height = (frame + spacing_total + rows_sum).max(target);
        let grid_box = if has_caption {
            let mut f = Fragment::new(FragmentKind::Block, LogicalSize::new(self.grid_inline, height));
            f.node = self.node;
            tree.push(f)
        } else {
            fragment
        };
        tree.set_children(grid_box, &grid_kids);
        if let Some(f) = tree.get_mut(grid_box) {
            f.size = LogicalSize::new(self.grid_inline, height);
        }

        if !has_caption {
            return;
        }
        // Captions above and below the grid box.
        let mut kids: Vec<FragmentId> = Vec::new();
        let mut y = Au(0);
        for want_top in [true, false] {
            if !want_top {
                if let Some(f) = tree.get_mut(grid_box) {
                    f.block_offset = y;
                    f.inline_offset = Au(0);
                }
                kids.push(grid_box);
                y += height;
            }
            for c in self.captions.iter().filter(|c| c.top == want_top) {
                let Some(cf) = c.frag else { continue };
                let h = tree.get(cf).map_or(Au(0), |f| f.size.block);
                y += c.edges.margin.block_start;
                if let Some(f) = tree.get_mut(cf) {
                    f.block_offset = y;
                    f.inline_offset = c.edges.margin.inline_start;
                }
                y += h + c.edges.margin.block_end;
                kids.push(cf);
            }
        }
        tree.set_children(fragment, &kids);
        if let Some(f) = tree.get_mut(fragment) {
            f.size = LogicalSize::new(self.wrapper_inline, y);
        }
    }

    fn rows_heights(&self) -> Vec<Au> {
        self.rows.iter().map(|r| r.height).collect()
    }
}
