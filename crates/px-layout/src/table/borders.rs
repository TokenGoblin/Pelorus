//! The collapsing border model (CSS 2.1 section 17.6.2).
//!
//! Every grid edge has several candidate borders (the cells either side, the
//! row, row group, column and column group the edge belongs to, and the table);
//! conflict resolution picks one per edge segment. Layout then gives each cell
//! half of the winning width on each side, and the table half of the outer ones,
//! and paint draws the winners as filled rectangles (the boxes' own borders are
//! suppressed in `px_css::paint`).

use super::*;
use px_css::paint::{CollapsedSide, collapsed_sides};

/// Where a candidate border came from, in ascending precedence.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Table,
    ColumnGroup,
    Column,
    RowGroup,
    Row,
    Cell,
}

#[derive(Clone, Copy)]
struct Seg {
    side: CollapsedSide,
    source: Source,
}

/// Whether `a` beats `b` (17.6.2.1). Earlier candidates win ties, so callers add
/// them in the spec's start-before-end order.
fn beats(a: &Seg, b: &Seg) -> bool {
    if a.side.hidden != b.side.hidden {
        return a.side.hidden;
    }
    if a.side.hidden {
        return false;
    }
    if a.side.width != b.side.width {
        return a.side.width > b.side.width;
    }
    if a.side.style != b.side.style {
        return a.side.style > b.side.style;
    }
    a.source > b.source
}

fn push(slot: &mut Option<Seg>, cand: Seg) {
    match slot {
        Some(cur) if !beats(&cand, cur) => {}
        _ => *slot = Some(cand),
    }
}

/// The resolved edges, kept for painting.
pub(super) struct Resolved {
    /// `h[line][col]`: the border on horizontal line `line` above column `col`.
    h: Vec<Vec<Option<Seg>>>,
    /// `v[row][line]`: the border on vertical line `line` beside row `row`.
    v: Vec<Vec<Option<Seg>>>,
}

fn drawn(s: &Option<Seg>) -> Option<(Au, [u8; 4])> {
    let seg = (*s)?;
    (!seg.side.hidden && seg.side.width > Au(0) && seg.side.style > 0)
        .then_some((seg.side.width, seg.side.color))
}

fn width_of(s: &Option<Seg>) -> Au {
    drawn(s).map_or(Au(0), |d| d.0)
}

impl Table {
    pub(super) fn resolve_collapsed_borders(&mut self, dom: px_css::view::Dom<'_>) {
        let nrows = self.rows.len();
        let ncols = self.ncols;
        if nrows == 0 || ncols == 0 {
            // No grid: the table's own border is just its own.
            return;
        }
        let sides = |n: Option<NodeId>| -> Option<[CollapsedSide; 4]> {
            let s = computed_style(dom, n?)?;
            Some(collapsed_sides(&s))
        };
        let table_sides = self.node.and_then(|n| sides(Some(n)));

        // Who owns each grid slot.
        let mut owner: BTreeMap<(usize, usize), usize> = BTreeMap::new();
        for (ci, c) in self.cells.iter().enumerate() {
            for r in c.row..(c.row + c.rowspan).min(nrows) {
                for k in c.col..(c.col + c.colspan).min(ncols) {
                    owner.insert((r, k), ci);
                }
            }
        }
        let cell_sides: Vec<Option<[CollapsedSide; 4]>> = self
            .cells
            .iter()
            .map(|c| match &c.src {
                CellSrc::Node(n) => sides(Some(*n)),
                CellSrc::Anon(_) => None,
            })
            .collect();
        let row_sides: Vec<Option<[CollapsedSide; 4]>> =
            self.rows.iter().map(|r| sides(r.node)).collect();
        let group_sides: Vec<Option<[CollapsedSide; 4]>> =
            self.groups.iter().map(|g| sides(g.node)).collect();
        // Column and column group per grid column.
        let mut col_side: Vec<Option<[CollapsedSide; 4]>> = vec![None; ncols];
        let mut colgroup_side: Vec<Option<[CollapsedSide; 4]>> = vec![None; ncols];
        for cn in &self.col_nodes {
            let Some(style) = computed_style(dom, cn.node) else {
                continue;
            };
            let is_group = matches!(kind_of(&style), Some(Kind::ColumnGroup));
            let s = Some(collapsed_sides(&style));
            for k in cn.first..(cn.first + cn.count).min(ncols) {
                if is_group {
                    colgroup_side[k] = s;
                } else {
                    col_side[k] = s;
                }
            }
        }
        let group_of_row: Vec<usize> = self.rows.iter().map(|r| r.group).collect();

        let seg = |s: &CollapsedSide, source| Seg { side: *s, source };

        // Horizontal lines 0..=nrows.
        let mut h: Vec<Vec<Option<Seg>>> = vec![vec![None; ncols]; nrows + 1];
        for line in 0..=nrows {
            for k in 0..ncols {
                let slot = &mut h[line][k];
                // Above the line first (its bottom side), then below (its top).
                if line > 0
                    && let Some(&ci) = owner.get(&(line - 1, k))
                {
                    let c = &self.cells[ci];
                    if c.row + c.rowspan == line
                        && let Some(s) = &cell_sides[ci]
                    {
                        push(slot, seg(&s[2], Source::Cell));
                    }
                }
                if line < nrows
                    && let Some(&ci) = owner.get(&(line, k))
                    && self.cells[ci].row == line
                    && let Some(s) = &cell_sides[ci]
                {
                    push(slot, seg(&s[0], Source::Cell));
                }
                if line > 0
                    && let Some(s) = &row_sides[line - 1]
                {
                    push(slot, seg(&s[2], Source::Row));
                }
                if line < nrows
                    && let Some(s) = &row_sides[line]
                {
                    push(slot, seg(&s[0], Source::Row));
                }
                for (gi, g) in self.groups.iter().enumerate() {
                    if g.rows == 0 {
                        continue;
                    }
                    if g.first_row + g.rows == line
                        && let Some(s) = &group_sides[gi]
                    {
                        push(slot, seg(&s[2], Source::RowGroup));
                    }
                    if g.first_row == line
                        && let Some(s) = &group_sides[gi]
                    {
                        push(slot, seg(&s[0], Source::RowGroup));
                    }
                }
                if line == 0 || line == nrows {
                    let idx = if line == 0 { 0 } else { 2 };
                    if let Some(s) = &col_side[k] {
                        push(slot, seg(&s[idx], Source::Column));
                    }
                    if let Some(s) = &colgroup_side[k] {
                        push(slot, seg(&s[idx], Source::ColumnGroup));
                    }
                    if let Some(s) = &table_sides {
                        push(slot, seg(&s[idx], Source::Table));
                    }
                }
            }
        }

        // Vertical lines 0..=ncols, per row.
        let mut v: Vec<Vec<Option<Seg>>> = vec![vec![None; ncols + 1]; nrows];
        for r in 0..nrows {
            for line in 0..=ncols {
                let slot = &mut v[r][line];
                if line > 0
                    && let Some(&ci) = owner.get(&(r, line - 1))
                {
                    let c = &self.cells[ci];
                    if c.col + c.colspan == line
                        && let Some(s) = &cell_sides[ci]
                    {
                        push(slot, seg(&s[1], Source::Cell));
                    }
                }
                if line < ncols
                    && let Some(&ci) = owner.get(&(r, line))
                    && self.cells[ci].col == line
                    && let Some(s) = &cell_sides[ci]
                {
                    push(slot, seg(&s[3], Source::Cell));
                }
                if line == 0
                    && let Some(s) = &row_sides[r]
                {
                    push(slot, seg(&s[3], Source::Row));
                }
                if line == ncols
                    && let Some(s) = &row_sides[r]
                {
                    push(slot, seg(&s[1], Source::Row));
                }
                if let Some(s) = &group_sides[group_of_row[r]] {
                    if line == 0 {
                        push(slot, seg(&s[3], Source::RowGroup));
                    }
                    if line == ncols {
                        push(slot, seg(&s[1], Source::RowGroup));
                    }
                }
                if line < ncols {
                    if let Some(s) = &col_side[line] {
                        push(slot, seg(&s[3], Source::Column));
                    }
                    if let Some(s) = &colgroup_side[line] {
                        push(slot, seg(&s[3], Source::ColumnGroup));
                    }
                }
                if line > 0 {
                    if let Some(s) = &col_side[line - 1] {
                        push(slot, seg(&s[1], Source::Column));
                    }
                    if let Some(s) = &colgroup_side[line - 1] {
                        push(slot, seg(&s[1], Source::ColumnGroup));
                    }
                }
                if let Some(s) = &table_sides {
                    if line == 0 {
                        push(slot, seg(&s[3], Source::Table));
                    }
                    if line == ncols {
                        push(slot, seg(&s[1], Source::Table));
                    }
                }
            }
        }

        // Cell border widths: half of the widest winner along each side.
        for (ci, c) in self.cells.iter_mut().enumerate() {
            let end_col = (c.col + c.colspan).min(ncols);
            let end_row = (c.row + c.rowspan).min(nrows);
            let top = (c.col..end_col)
                .map(|k| width_of(&h[c.row][k]))
                .max()
                .unwrap_or(Au(0));
            let bottom = (c.col..end_col)
                .map(|k| width_of(&h[end_row][k]))
                .max()
                .unwrap_or(Au(0));
            let left = (c.row..end_row)
                .map(|r| width_of(&v[r][c.col]))
                .max()
                .unwrap_or(Au(0));
            let right = (c.row..end_row)
                .map(|r| width_of(&v[r][end_col]))
                .max()
                .unwrap_or(Au(0));
            let _ = ci;
            c.edges.border = LogicalEdges {
                block_start: top - top / 2,
                block_end: bottom / 2,
                inline_start: left - left / 2,
                inline_end: right / 2,
            };
        }
        // The table's own border: the outer halves.
        let top = h[0].iter().map(width_of).max().unwrap_or(Au(0));
        let bottom = h[nrows].iter().map(width_of).max().unwrap_or(Au(0));
        let left = v.iter().map(|r| width_of(&r[0])).max().unwrap_or(Au(0));
        let right = v.iter().map(|r| width_of(&r[ncols])).max().unwrap_or(Au(0));
        self.edges.border = LogicalEdges {
            block_start: top / 2,
            block_end: bottom - bottom / 2,
            inline_start: left / 2,
            inline_end: right - right / 2,
        };
        self.borders = Some(Resolved { h, v });
    }
}

impl Resolved {
    /// One filled rectangle per drawn segment, in the grid box's coordinates.
    pub(super) fn fills(
        &self,
        tree: &mut FragmentTree,
        col_x: &[Au],
        row_y: &[Au],
        row_h: &[Au],
        sp: (Au, Au),
    ) -> Vec<FragmentId> {
        let _ = sp;
        let nrows = row_h.len();
        let ncols = col_x.len().saturating_sub(1);
        let line_y = |line: usize| -> Au {
            if line < nrows {
                row_y[line]
            } else {
                row_y[nrows - 1] + row_h[nrows - 1]
            }
        };
        let mut out = Vec::new();
        let mut rect = |tree: &mut FragmentTree, x: Au, y: Au, w: Au, hgt: Au, color: [u8; 4]| {
            let mut f = Fragment::new(FragmentKind::Block, LogicalSize::new(w, hgt));
            f.inline_offset = x;
            f.block_offset = y;
            f.fill = Some(color);
            out.push(tree.push(f));
        };
        for (line, segs) in self.h.iter().enumerate() {
            let y = line_y(line);
            for (k, s) in segs.iter().enumerate() {
                let Some((w, color)) = drawn(s) else { continue };
                rect(tree, col_x[k], y - w / 2, col_x[k + 1] - col_x[k], w, color);
            }
        }
        for (r, segs) in self.v.iter().enumerate() {
            let (top, bottom) = (line_y(r), line_y(r) + row_h[r]);
            for (line, s) in segs.iter().enumerate() {
                let Some((w, color)) = drawn(s) else { continue };
                let around = |l: usize| -> Au {
                    let a = if line > 0 {
                        width_of(&self.h[l][line - 1])
                    } else {
                        Au(0)
                    };
                    let b = if line < ncols {
                        width_of(&self.h[l][line])
                    } else {
                        Au(0)
                    };
                    a.max(b)
                };
                let up = around(r) / 2;
                let down = around(r + 1) / 2;
                rect(
                    tree,
                    col_x[line] - w / 2,
                    top - up,
                    w,
                    bottom - top + up + down,
                    color,
                );
            }
        }
        out
    }
}
