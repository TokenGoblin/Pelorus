//! Grid layout: css-grid-1, built on `block.rs`'s traversal.
//!
//! # How it fits the block traversal
//!
//! Block layout is an explicit stack of `Enter`/`Children`/`Exit` visits. A grid
//! container is a block-level box like any other until its children are
//! scheduled, and then the work splits in two, because the two axes depend on each
//! other in one direction only (css-grid-1 section 11.1):
//!
//! - **`Children`** calls [`Container::begin`]. Placement (section 8) and column
//!   sizing (section 11, inline axis) need only the items' *styles* and their
//!   intrinsic inline sizes, so they run here. The result tells every item the
//!   inline size it will be laid out at ([`Container::item_inline`]), which
//!   `Enter` reads, and then the items are laid out by the ordinary traversal.
//! - **`Exit`** calls [`Container::finish`]. Row sizing needs each item's block
//!   size *at its final column width*, which exists only now. Rows are sized,
//!   the tracks aligned, and every item positioned and stretched.
//!
//! That is the whole interface, and it keeps this module free of the traversal:
//! no recursion, and the nested-grid case is two more entries on the same stack.
//!
//! # Not implemented
//!
//! Subgrid (css-grid-2) and masonry are out of scope (ADR 031). Baseline
//! alignment falls back to `start` because baselines need fonts (Phase 9).
//! Orthogonal writing modes and fragmentation are absent. Items whose inline size
//! depends on their block size (aspect ratios) get no second column pass
//! (section 11.1 step 3). `grid-area` placement of absolutely positioned
//! children is not done: they take their static position.
//!
//! # Bounds
//!
//! At most [`MAX_TRACKS`] tracks per axis and lines clamped to +-10,000
//! ([`place::MAX_LINES`]); every inner loop spends from a [`num::Fuel`] budget. A
//! page can make a grid slow-and-wrong, never unbounded.

mod num;
mod place;
mod props;
mod size;

use app_units::Au;
use px_dom::NodeId;
use style::properties::ComputedValues;

use crate::block::{Content, Edges, Intrinsic, computed_style, resolve_edges};
use crate::fragment::{FragmentId, FragmentTree};
use num::{Fuel, narrow, wide};
use place::ItemPlace;
use props::{
    Align, Aligned, Breadth, ContainerStyle, Content as Distribution, Explicit, ItemStyle,
    MAX_TRACKS, SizeKind, TrackSpec, resolve,
};
use size::{Avail, Limits, MaxFn, MinFn, Track};

/// Whether `style` makes a grid container.
#[must_use]
pub(crate) fn is_grid_container(style: &ComputedValues) -> bool {
    use style::values::specified::box_::DisplayInside;
    matches!(style.clone_display().inside(), DisplayInside::Grid)
}

/// What `begin` needs to know about the container.
pub(crate) struct Inputs<'d, 's> {
    pub dom: px_css::view::Dom<'d>,
    pub style: &'s ComputedValues,
    /// The content-box inline size the container was given; `None` when only the
    /// container's intrinsic sizes are wanted.
    pub content_inline: Option<Au>,
    /// Whether that size is definite (not a shrink-to-fit result).
    pub inline_definite: bool,
    /// The content-box block size if `height` is a length.
    pub definite_block: Option<Au>,
    pub min_block: Au,
    pub max_block: Option<Au>,
    /// The container's text style: what its anonymous text items are measured
    /// with.
    pub text: crate::text::TextStyle,
}

/// What `Enter` needs to lay a grid item out: the inline-axis facts, which depend
/// on the grid area and so cannot be found by the item itself.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ItemInline {
    /// The inline size percentages on the item resolve against: its grid area.
    pub containing: Au,
    /// The item's content-box inline size.
    pub content_inline: Au,
    pub margin_start: Au,
    pub margin_end: Au,
}

/// What an item contributes to track sizing in the inline axis.
#[derive(Clone, Copy, Debug, Default)]
struct Measure {
    /// Border-box min-content and max-content sizes, without margins.
    min: Au,
    max: Au,
    /// Margins that are not auto, percentages as zero.
    margin: Au,
    /// The content-based minimum size (section 6.6), border-box.
    auto_min: Au,
    /// A definite `width`, border-box.
    definite: Option<Au>,
    /// A non-`auto` `min-width`, border-box.
    explicit_min: Option<Au>,
}

struct ItemData {
    style: ItemStyle,
    computed: Option<style::servo_arc::Arc<ComputedValues>>,
    slot: usize,
    /// Start track and span, per axis.
    col: (usize, usize),
    row: (usize, usize),
    measure: Measure,
    inline: Option<ItemInline>,
    /// Left edge of the border box, relative to the content box.
    x: Au,
}

struct Axis {
    specs: Vec<TrackSpec>,
    collapsed: Vec<bool>,
}

/// A grid container between `Children` and `Exit`.
pub(crate) struct Container {
    cs: ContainerStyle,
    items: Vec<ItemData>,
    slot_item: Vec<Option<usize>>,
    cols: Axis,
    rows: Axis,
    col_gap: Au,
    row_gap: Au,
    col_pos: Vec<Au>,
    col_size: Vec<Au>,
    definite_block: Option<Au>,
    min_block: Au,
    max_block: Option<Au>,
    fuel: Fuel,
}

/// What `finish` needs to know about each laid-out item.
pub(crate) struct Kid {
    pub slot: usize,
    pub fragment: FragmentId,
    pub margin_block: (Au, Au),
    pub border_padding_block: Au,
}

fn floor0(a: Au) -> Au {
    a.max(Au(0))
}

/// How many times to repeat an `auto-fill`/`auto-fit` (section 7.2.3.2).
fn repeat_count(
    rep: &props::AutoRepeat,
    gap: Au,
    space: Option<Au>,
    at_least: Option<Au>,
    basis: Option<Au>,
) -> usize {
    let definite = |spec: &TrackSpec| -> Option<Au> {
        let of = |b: &Breadth| match b {
            Breadth::Len(l) => resolve(l, basis).map(floor0),
            _ => None,
        };
        match (of(&spec.max), of(&spec.min)) {
            (Some(max), Some(min)) => Some(max.max(min)),
            (Some(max), None) => Some(max),
            (None, min) => min,
        }
    };
    let one_px = crate::geom::px(1);
    let mut unit_sum = Au(0);
    for spec in &rep.unit {
        match definite(spec) {
            Some(s) => unit_sum += s.max(one_px),
            None => return 1,
        }
    }
    let mut other_sum = Au(0);
    for spec in &rep.others {
        other_sum += definite(spec).unwrap_or(Au(0));
    }
    let k = i64::try_from(rep.unit.len()).unwrap_or(1).max(1);
    let other_n = i64::try_from(rep.others.len()).unwrap_or(0);
    let per = wide(unit_sum) + k * wide(gap);
    if per <= 0 {
        return 1;
    }
    let base = wide(other_sum) + wide(gap) * (other_n - 1);
    let n = if let Some(space) = space {
        (wide(space) - base) / per
    } else if let Some(min) = at_least {
        let need = wide(min) - base;
        if need <= 0 { 1 } else { (need + per - 1) / per }
    } else {
        1
    };
    let cap = (i64::try_from(MAX_TRACKS).unwrap_or(10_000) - other_n) / k;
    usize::try_from(n.clamp(1, cap.max(1))).unwrap_or(1)
}

fn to_track(spec: &TrackSpec, basis: Option<Au>) -> Track {
    let min = match &spec.min {
        Breadth::Len(l) => resolve(l, basis).map_or(MinFn::Auto, |a| MinFn::Fixed(floor0(a))),
        Breadth::MinContent => MinFn::MinContent,
        Breadth::MaxContent => MinFn::MaxContent,
        Breadth::Auto | Breadth::Flex(_) => MinFn::Auto,
    };
    let max = if let Some(arg) = &spec.fit_content {
        resolve(arg, basis).map_or(MaxFn::Auto, |a| MaxFn::FitContent(floor0(a)))
    } else {
        match &spec.max {
            Breadth::Len(l) => resolve(l, basis).map_or(MaxFn::Auto, |a| MaxFn::Fixed(floor0(a))),
            Breadth::Flex(f) => MaxFn::Flex(*f),
            Breadth::MinContent => MaxFn::MinContent,
            Breadth::MaxContent => MaxFn::MaxContent,
            Breadth::Auto => MaxFn::Auto,
        }
    };
    Track::new(min, max)
}

/// Specs for the implicit grid's tracks from `first` to `last` (exclusive).
fn axis_specs(explicit: &Explicit, auto: &[TrackSpec], first: i32, last: i32) -> Vec<TrackSpec> {
    let len = i64::try_from(explicit.tracks.len()).unwrap_or(0);
    let plen = i64::try_from(auto.len().max(1)).unwrap_or(1);
    let count = usize::try_from((last - first).max(0))
        .unwrap_or(0)
        .min(MAX_TRACKS);
    let mut out = Vec::with_capacity(count);
    for k in 0..count {
        let idx0 = i64::from(first) - 1 + i64::try_from(k).unwrap_or(0);
        let spec = if idx0 >= 0 && idx0 < len {
            explicit
                .tracks
                .get(usize::try_from(idx0).unwrap_or(0))
                .cloned()
        } else if idx0 >= len {
            let j = usize::try_from((idx0 - len) % plen).unwrap_or(0);
            auto.get(j).cloned()
        } else {
            let j = usize::try_from((plen - 1) - ((-idx0 - 1) % plen)).unwrap_or(0);
            auto.get(j).cloned()
        };
        out.push(spec.unwrap_or_else(TrackSpec::auto));
    }
    out
}

/// Gutter after each track: none after the last non-collapsed track, none after a
/// collapsed one (its two gutters coincide, section 7.2.3.2).
fn gaps_for(collapsed: &[bool], gap: Au) -> Vec<Au> {
    let mut out = vec![Au(0); collapsed.len()];
    let mut later_active = false;
    for (i, c) in collapsed.iter().enumerate().rev() {
        if !c {
            if later_active && let Some(g) = out.get_mut(i) {
                *g = gap;
            }
            later_active = true;
        }
    }
    out
}

/// Where each track starts, after distributing `free` space (section 10.5).
fn positions(
    sizes: &[Au],
    gaps: &[Au],
    collapsed: &[bool],
    how: Aligned<Distribution>,
    free: Au,
) -> Vec<Au> {
    let active = i64::try_from(collapsed.iter().filter(|c| !**c).count()).unwrap_or(0);
    let f = wide(free);
    let negative = f < 0;
    let (lead, between) = match how.value {
        Distribution::Normal | Distribution::Stretch | Distribution::Start => (0, 0),
        Distribution::End => (f, 0),
        Distribution::Center => (f / 2, 0),
        Distribution::Between if active > 1 && !negative => (0, f / (active - 1)),
        Distribution::Around if active > 1 && !negative => (f / (2 * active), f / active),
        Distribution::Around | Distribution::Evenly if active == 1 && !negative => (f / 2, 0),
        Distribution::Evenly if active > 1 && !negative => (f / (active + 1), f / (active + 1)),
        Distribution::Between | Distribution::Around | Distribution::Evenly => (0, 0),
    };
    let lead = if negative && how.safe { 0 } else { lead };
    let mut out = Vec::with_capacity(sizes.len());
    let mut running = wide(narrow(i128::from(lead)));
    let mut remaining = active;
    for (i, size) in sizes.iter().enumerate() {
        out.push(narrow(i128::from(running)));
        if collapsed.get(i).copied().unwrap_or(false) {
            continue;
        }
        remaining -= 1;
        running += wide(*size) + wide(gaps.get(i).copied().unwrap_or(Au(0)));
        if remaining > 0 {
            running += between;
        }
    }
    out
}

/// The extent of tracks `start..start+span` given their positions and sizes.
fn area(pos: &[Au], size: &[Au], start: usize, span: usize) -> (Au, Au) {
    let first = pos.get(start).copied().unwrap_or(Au(0));
    let last = start
        .saturating_add(span)
        .saturating_sub(1)
        .min(pos.len().saturating_sub(1));
    let end = pos.get(last).copied().unwrap_or(first) + size.get(last).copied().unwrap_or(Au(0));
    (first, (end - first).max(Au(0)))
}

/// Position and margins of a box of `size` in an area of `space`.
///
/// Auto margins take positive free space first and disable the alignment; with
/// none, `align` places the box (section 6.5, css-align-3).
#[allow(clippy::too_many_arguments)]
fn align_in_area(
    space: Au,
    size: Au,
    margin: (Au, Au),
    auto: (bool, bool),
    align: Aligned<Align>,
) -> (Au, Au, Au) {
    let (mut a, mut b) = margin;
    let free = space - size - a - b;
    if (auto.0 || auto.1) && free > Au(0) {
        if auto.0 && auto.1 {
            let half = free / 2;
            a += half;
            b += free - half;
        } else if auto.0 {
            a += free;
        } else {
            b += free;
        }
        return (a, a, b);
    }
    let offset = if free < Au(0) && align.safe {
        Au(0)
    } else {
        match align.value {
            Align::End => free,
            Align::Center => free / 2,
            Align::Normal | Align::Stretch | Align::Start => Au(0),
        }
    };
    (a + offset, a, b)
}

impl Container {
    /// Placement and column sizing. Called from the container's `Children` visit.
    pub(crate) fn begin(
        inp: &Inputs<'_, '_>,
        children: &[Content],
        raw: &mut dyn FnMut(NodeId) -> Intrinsic,
    ) -> Self {
        let mut this = Self::build(inp, children, raw);
        this.size_columns(inp);
        this
    }

    /// The container's min-content and max-content content-box inline sizes
    /// (section 5.1: the sum of the tracks sized under each constraint).
    pub(crate) fn intrinsic(
        inp: &Inputs<'_, '_>,
        children: &[Content],
        raw: &mut dyn FnMut(NodeId) -> Intrinsic,
    ) -> (Au, Au) {
        let mut this = Self::build(inp, children, raw);
        let mut out = [Au(0); 2];
        for (slot, avail) in out.iter_mut().zip([Avail::MinContent, Avail::MaxContent]) {
            let (tracks, gaps) = this.run_columns(None, avail);
            for t in &tracks {
                *slot += t.base;
            }
            for g in &gaps {
                *slot += *g;
            }
        }
        (out[0], out[1])
    }

    /// Everything up to column sizing: styles, placement, the track lists.
    fn build(
        inp: &Inputs<'_, '_>,
        children: &[Content],
        raw: &mut dyn FnMut(NodeId) -> Intrinsic,
    ) -> Self {
        let cs = ContainerStyle::new(inp.style);
        let basis_inline = inp.content_inline;
        let mut fuel = Fuel::new();

        let col_gap = cs
            .column_gap
            .as_ref()
            .and_then(|l| resolve(l, basis_inline))
            .map_or(Au(0), floor0);
        let row_gap = cs
            .row_gap
            .as_ref()
            .and_then(|l| resolve(l, inp.definite_block))
            .map_or(Au(0), floor0);

        // The explicit grid, with auto-repeat counts resolved against the space.
        let col_count = props::auto_repeat_of(&cs.columns).map_or(1, |rep| {
            let space = if inp.inline_definite {
                basis_inline
            } else {
                None
            };
            repeat_count(&rep, col_gap, space, None, basis_inline)
        });
        let row_count = props::auto_repeat_of(&cs.rows).map_or(1, |rep| {
            let at_least = (inp.min_block > Au(0)).then_some(inp.min_block);
            repeat_count(
                &rep,
                row_gap,
                inp.definite_block,
                at_least,
                inp.definite_block,
            )
        });
        let col_explicit = props::expand(&cs.columns, col_count);
        let row_explicit = props::expand(&cs.rows, row_count);
        let ecols = i32::try_from(col_explicit.tracks.len())
            .unwrap_or(0)
            .max(cs.area_columns);
        let erows = i32::try_from(row_explicit.tracks.len())
            .unwrap_or(0)
            .max(cs.area_rows);
        let col_names = cs.names(&col_explicit, false);
        let row_names = cs.names(&row_explicit, true);

        // Items, in order-modified document order (section 6.? `order`).
        let mut items: Vec<ItemData> = Vec::new();
        let mut slot_item = vec![None; children.len()];
        for (slot, content) in children.iter().enumerate() {
            let (st, computed, text) = match content {
                Content::Block(node) | Content::Float(node) => {
                    let Some(computed) = computed_style(inp.dom, *node) else {
                        continue;
                    };
                    (ItemStyle::new(&computed, &cs), Some(computed), None)
                }
                Content::InlineRun(text) => (ItemStyle::anonymous(&cs), None, Some(text.as_str())),
                Content::Absolute(_) => continue,
            };
            let measure = match (&computed, content) {
                (Some(c), Content::Block(node) | Content::Float(node)) => {
                    measure_element(&mut || raw(*node), c, &st)
                }
                _ => {
                    let t = text.unwrap_or("");
                    let (min, max) = crate::text::intrinsic_widths(t, inp.text);
                    Measure {
                        min,
                        max,
                        ..Measure::default()
                    }
                }
            };
            items.push(ItemData {
                style: st,
                computed,
                slot,
                col: (0, 1),
                row: (0, 1),
                measure,
                inline: None,
                x: Au(0),
            });
        }
        items.sort_by_key(|i| (i.style.order, i.slot));
        for (index, item) in items.iter().enumerate() {
            if let Some(entry) = slot_item.get_mut(item.slot) {
                *entry = Some(index);
            }
        }

        // Section 8: placement.
        let places: Vec<ItemPlace> = items
            .iter()
            .map(|i| {
                let c =
                    place::resolve_axis(&i.style.columns.0, &i.style.columns.1, ecols, &col_names);
                let r = place::resolve_axis(&i.style.rows.0, &i.style.rows.1, erows, &row_names);
                if cs.column_flow {
                    ItemPlace { major: c, minor: r }
                } else {
                    ItemPlace { major: r, minor: c }
                }
            })
            .collect();
        let (emajor, eminor) = if cs.column_flow {
            (ecols, erows)
        } else {
            (erows, ecols)
        };
        let placement = place::place(&places, emajor, eminor, cs.dense, &mut fuel);
        let (col_lines, row_lines) = if cs.column_flow {
            (placement.major_lines, placement.minor_lines)
        } else {
            (placement.minor_lines, placement.major_lines)
        };
        let clip = |lines: (i32, i32)| (lines.0, lines.1.min(lines.0.saturating_add(10_000)));
        let (col_lines, row_lines) = (clip(col_lines), clip(row_lines));
        let ncols = usize::try_from(col_lines.1 - col_lines.0).unwrap_or(0);
        let nrows = usize::try_from(row_lines.1 - row_lines.0).unwrap_or(0);

        let index_of = |start: i32, span: i32, first: i32, count: usize| -> (usize, usize) {
            let s = usize::try_from(start - first)
                .unwrap_or(0)
                .min(count.saturating_sub(1));
            let n = usize::try_from(span)
                .unwrap_or(1)
                .max(1)
                .min(count.saturating_sub(s).max(1));
            (s, n)
        };
        for (item, p) in items.iter_mut().zip(&placement.placed) {
            let ((cs0, csp), (rs0, rsp)) = if cs.column_flow {
                ((p.major_start, p.major_span), (p.minor_start, p.minor_span))
            } else {
                ((p.minor_start, p.minor_span), (p.major_start, p.major_span))
            };
            item.col = index_of(cs0, csp, col_lines.0, ncols);
            item.row = index_of(rs0, rsp, row_lines.0, nrows);
        }

        // The implicit grid's tracks.
        let mut cols = Axis {
            specs: axis_specs(&col_explicit, &cs.auto_columns, col_lines.0, col_lines.1),
            collapsed: vec![false; ncols],
        };
        let mut rows = Axis {
            specs: axis_specs(&row_explicit, &cs.auto_rows, row_lines.0, row_lines.1),
            collapsed: vec![false; nrows],
        };
        collapse_empty(
            &mut cols,
            &col_explicit,
            col_lines.0,
            items.iter().map(|i| i.col),
        );
        collapse_empty(
            &mut rows,
            &row_explicit,
            row_lines.0,
            items.iter().map(|i| i.row),
        );

        Self {
            cs,
            items,
            slot_item,
            cols,
            rows,
            col_gap,
            row_gap,
            col_pos: Vec::new(),
            col_size: Vec::new(),
            definite_block: inp.definite_block,
            min_block: inp.min_block,
            max_block: inp.max_block,
            fuel,
        }
    }

    /// Size the columns under `avail`; returns the tracks and their gutters.
    fn run_columns(&mut self, basis: Option<Au>, avail: Avail) -> (Vec<Track>, Vec<Au>) {
        let mut tracks: Vec<Track> = self.cols.specs.iter().map(|s| to_track(s, basis)).collect();
        for (t, c) in tracks.iter_mut().zip(&self.cols.collapsed) {
            if *c {
                *t = Track::new(MinFn::Fixed(Au(0)), MaxFn::Fixed(Au(0)));
            }
        }
        let gaps = gaps_for(&self.cols.collapsed, self.col_gap);

        let sizing: Vec<size::Item> = self
            .items
            .iter()
            .map(|it| {
                let m = &it.measure;
                let spans = tracks.iter().skip(it.col.0).take(it.col.1);
                let auto_min_track = spans.clone().any(|t| matches!(t.min, MinFn::Auto));
                let flex = spans.clone().any(|t| matches!(t.max, MaxFn::Flex(_)));
                let automatic = !it.style.scrolls && auto_min_track && (it.col.1 == 1 || !flex);
                let mut minimum = if let Some(d) = m.definite {
                    d
                } else if let Some(e) = m.explicit_min {
                    e
                } else if automatic {
                    m.auto_min
                } else {
                    Au(0)
                };
                // Clamped to the stretch fit into fixed-only tracks (section 6.6).
                if m.definite.is_none() && m.explicit_min.is_none() && automatic {
                    let mut total = Au(0);
                    let mut all_fixed = true;
                    for t in tracks.iter().skip(it.col.0).take(it.col.1) {
                        match t.max {
                            MaxFn::Fixed(a) => total += a,
                            _ => all_fixed = false,
                        }
                    }
                    if all_fixed {
                        let mut gap = Au(0);
                        for g in gaps.iter().skip(it.col.0).take(it.col.1.saturating_sub(1)) {
                            gap += *g;
                        }
                        minimum = minimum.min(floor0(total + gap - m.margin));
                    }
                }
                size::Item {
                    start: it.col.0,
                    span: it.col.1,
                    min_content: floor0(m.min + m.margin),
                    max_content: floor0(m.max + m.margin),
                    minimum: floor0(minimum + m.margin),
                }
            })
            .collect();

        let stretch = matches!(
            self.cs.justify_content.value,
            Distribution::Normal | Distribution::Stretch
        );
        size::size_tracks(
            &mut tracks,
            &gaps,
            &sizing,
            avail,
            Limits {
                min: Au(0),
                max: None,
            },
            stretch,
            &mut self.fuel,
        );
        (tracks, gaps)
    }

    /// Section 11.1 step 1: column sizes, then each item's inline-axis geometry.
    fn size_columns(&mut self, inp: &Inputs<'_, '_>) {
        let content = inp.content_inline.unwrap_or(Au(0));
        let (tracks, gaps) = self.run_columns(Some(content), Avail::Definite(content));
        self.col_size = tracks.iter().map(|t| t.base).collect();
        let mut total = Au(0);
        for s in &self.col_size {
            total += *s;
        }
        for g in &gaps {
            total += *g;
        }
        self.col_pos = positions(
            &self.col_size,
            &gaps,
            &self.cols.collapsed,
            self.cs.justify_content,
            content - total,
        );

        for it in &mut self.items {
            let (start, width) = area(&self.col_pos, &self.col_size, it.col.0, it.col.1);
            let (x, inline) = inline_geometry(it, start, width);
            it.x = x;
            it.inline = Some(inline);
        }
    }

    /// The inline-axis facts `Enter` needs for the item in `slot`.
    #[must_use]
    pub(crate) fn item_inline(&self, slot: usize) -> Option<ItemInline> {
        let index = self.slot_item.get(slot).copied().flatten()?;
        self.items.get(index)?.inline
    }

    /// Row sizing, alignment and positioning. Called from the container's `Exit`.
    ///
    /// Returns the container's content-box block size and the item fragments in
    /// order-modified order.
    pub(crate) fn finish(
        &mut self,
        tree: &mut FragmentTree,
        kids: &[Kid],
        origin: (Au, Au),
    ) -> (Au, Vec<FragmentId>) {
        let (origin_inline, origin_block) = origin;
        let mut kid_of: Vec<Option<usize>> = vec![None; self.slot_item.len()];
        for (i, k) in kids.iter().enumerate() {
            if let Some(entry) = kid_of.get_mut(k.slot) {
                *entry = Some(i);
            }
        }

        let basis = self.definite_block;
        let mut tracks: Vec<Track> = self.rows.specs.iter().map(|s| to_track(s, basis)).collect();
        for (t, c) in tracks.iter_mut().zip(&self.rows.collapsed) {
            if *c {
                *t = Track::new(MinFn::Fixed(Au(0)), MaxFn::Fixed(Au(0)));
            }
        }
        let gaps = gaps_for(&self.rows.collapsed, self.row_gap);

        // Block-axis contributions: the item's laid-out height.
        let sizing: Vec<size::Item> = self
            .items
            .iter()
            .map(|it| {
                let kid = kid_of
                    .get(it.slot)
                    .copied()
                    .flatten()
                    .and_then(|k| kids.get(k));
                let (natural, margins) = kid.map_or((Au(0), Au(0)), |k| {
                    (tree.get(k.fragment).map_or(Au(0), |f| f.size.block), {
                        let (above, below) = k.margin_block;
                        above + below
                    })
                });
                let bp = kid.map_or(Au(0), |k| k.border_padding_block);
                let adj = |v: Au| {
                    if it.style.border_box {
                        floor0(v).max(bp)
                    } else {
                        floor0(v) + bp
                    }
                };
                let definite = match &it.style.height {
                    SizeKind::Len(l) => resolve(l, None).map(adj),
                    _ => None,
                };
                let outer = floor0(definite.unwrap_or(natural) + margins);
                let spans = tracks.iter().skip(it.row.0).take(it.row.1);
                let auto_min_track = spans.clone().any(|t| matches!(t.min, MinFn::Auto));
                let flex = spans.clone().any(|t| matches!(t.max, MaxFn::Flex(_)));
                let automatic = !it.style.scrolls && auto_min_track && (it.row.1 == 1 || !flex);
                let explicit_min = match &it.style.min_height {
                    SizeKind::Len(l) => resolve(l, None).map(adj),
                    _ => None,
                };
                let minimum = if definite.is_some() {
                    outer
                } else if let Some(e) = explicit_min {
                    floor0(e + margins)
                } else if automatic {
                    outer
                } else {
                    floor0(margins)
                };
                size::Item {
                    start: it.row.0,
                    span: it.row.1,
                    min_content: outer,
                    max_content: outer,
                    minimum,
                }
            })
            .collect();

        let stretch = matches!(
            self.cs.align_content.value,
            Distribution::Normal | Distribution::Stretch
        );
        let (avail, limits) = match self.definite_block {
            Some(h) => (
                Avail::Definite(h),
                Limits {
                    min: h,
                    max: Some(h),
                },
            ),
            None => (
                Avail::MaxContent,
                Limits {
                    min: self.min_block,
                    max: self.max_block,
                },
            ),
        };
        size::size_tracks(
            &mut tracks,
            &gaps,
            &sizing,
            avail,
            limits,
            stretch,
            &mut self.fuel,
        );
        let row_size: Vec<Au> = tracks.iter().map(|t| t.base).collect();
        let mut total = Au(0);
        for s in &row_size {
            total += *s;
        }
        for g in &gaps {
            total += *g;
        }

        let mut height = self.definite_block.unwrap_or(total);
        if self.definite_block.is_none() {
            if let Some(max) = self.max_block {
                height = height.min(max);
            }
            height = height.max(self.min_block);
        }
        // A percentage `row-gap` against an indefinite height is zero while the
        // rows are sized (the percentage is cyclic) and resolves against the
        // resulting height afterwards (css-sizing-3 section 5.2.1): it moves the
        // rows, not the container's height.
        let (gaps, total) = match &self.cs.row_gap {
            Some(l) if self.definite_block.is_none() => {
                let gap = resolve(l, Some(height)).map_or(Au(0), floor0);
                if gap == self.row_gap {
                    (gaps, total)
                } else {
                    let gaps = gaps_for(&self.rows.collapsed, gap);
                    let mut total = Au(0);
                    for s in &row_size {
                        total += *s;
                    }
                    for g in &gaps {
                        total += *g;
                    }
                    (gaps, total)
                }
            }
            _ => (gaps, total),
        };
        let row_pos = positions(
            &row_size,
            &gaps,
            &self.rows.collapsed,
            self.cs.align_content,
            height - total,
        );

        let mut order = Vec::with_capacity(self.items.len());
        for it in &self.items {
            let Some(kid) = kid_of
                .get(it.slot)
                .copied()
                .flatten()
                .and_then(|k| kids.get(k))
            else {
                continue;
            };
            let (top, space) = area(&row_pos, &row_size, it.row.0, it.row.1);
            let natural = tree.get(kid.fragment).map_or(Au(0), |f| f.size.block);
            let bp = kid.border_padding_block;
            let adj = |v: Au| {
                if it.style.border_box {
                    floor0(v).max(bp)
                } else {
                    floor0(v) + bp
                }
            };
            let (ma, mb) = kid.margin_block;
            let (auto_a, auto_b) = it.style.margin_auto_block;
            let stretchable = matches!(it.style.align.value, Align::Normal | Align::Stretch)
                && !auto_a
                && !auto_b;
            let mut block = match &it.style.height {
                SizeKind::Len(l) => resolve(l, Some(space)).map_or(natural, adj),
                SizeKind::Auto if stretchable => floor0(space - ma - mb),
                _ => natural,
            };
            let bound = |k: &SizeKind| match k {
                SizeKind::Len(l) => resolve(l, Some(space)).map(adj),
                _ => None,
            };
            if let Some(max) = bound(&it.style.max_height) {
                block = block.min(max);
            }
            if let Some(min) = bound(&it.style.min_height) {
                block = block.max(min);
            }
            block = block.max(bp);
            let (offset, _, _) =
                align_in_area(space, block, (ma, mb), (auto_a, auto_b), it.style.align);
            if let Some(f) = tree.get_mut(kid.fragment) {
                f.size.block = block;
                f.inline_offset = origin_inline + it.x;
                f.block_offset = origin_block + top + offset;
            }
            order.push(kid.fragment);
        }
        (height, order)
    }
}

/// Auto-fit: tracks an `auto-fit` repeat made that no item touches collapse.
fn collapse_empty(
    axis: &mut Axis,
    explicit: &Explicit,
    first: i32,
    spans: impl Iterator<Item = (usize, usize)>,
) {
    let Some((start, end, true)) = explicit.auto_range else {
        return;
    };
    let n = axis.collapsed.len();
    let mut diff = vec![0i64; n + 1];
    for (s, len) in spans {
        if let Some(d) = diff.get_mut(s) {
            *d += 1;
        }
        if let Some(d) = diff.get_mut((s + len).min(n)) {
            *d -= 1;
        }
    }
    let shift = i64::from(first) - 1;
    let mut running = 0;
    for k in 0..n {
        running += diff.get(k).copied().unwrap_or(0);
        let explicit_index = i64::try_from(k).unwrap_or(0) + shift;
        let in_repeat = explicit_index >= i64::try_from(start).unwrap_or(0)
            && explicit_index < i64::try_from(end).unwrap_or(0);
        if in_repeat
            && running == 0
            && let Some(c) = axis.collapsed.get_mut(k)
        {
            *c = true;
        }
    }
}

/// An element item's intrinsic inline sizes (section 5.2 of css-sizing-3 via
/// `block.rs`'s walker), with its own min/max/definite sizes folded in.
fn measure_element(
    raw: &mut dyn FnMut() -> Intrinsic,
    computed: &ComputedValues,
    st: &ItemStyle,
) -> Measure {
    let edges = resolve_edges(computed, Au(0));
    let bp = edges.border.inline_sum() + edges.padding.inline_sum();
    let margin = edges.margin.inline_sum();
    let adj = |v: Au| {
        if st.border_box {
            floor0(v).max(bp)
        } else {
            floor0(v) + bp
        }
    };
    let definite = match &st.width {
        SizeKind::Len(l) => resolve(l, None).map(adj),
        _ => None,
    };
    let (mut min, mut max) = if let Some(d) = definite {
        (d, d)
    } else {
        let raw = raw();
        let min = floor0(raw.min - margin).max(bp);
        let max = floor0(raw.max - margin).max(bp);
        match &st.width {
            SizeKind::MinContent => (min, min),
            SizeKind::MaxContent => (max, max),
            _ => (min, max),
        }
    };
    let bound = |k: &SizeKind| match k {
        SizeKind::Len(l) => resolve(l, None).map(adj),
        _ => None,
    };
    let explicit_min = bound(&st.min_width);
    let mut auto_min = definite.unwrap_or(min);
    if let Some(m) = bound(&st.max_width) {
        min = min.min(m);
        max = max.min(m);
        auto_min = auto_min.min(m);
    }
    if let Some(m) = explicit_min {
        min = min.max(m);
        max = max.max(m);
    }
    Measure {
        min,
        max,
        margin,
        auto_min,
        definite,
        explicit_min,
    }
}

/// The item's border-box x and inline-axis facts, given its area's start and width.
fn inline_geometry(it: &ItemData, area_start: Au, area_width: Au) -> (Au, ItemInline) {
    let edges = it
        .computed
        .as_ref()
        .map_or(Edges::ZERO, |c| resolve_edges(c, area_width));
    let bp = edges.border.inline_sum() + edges.padding.inline_sum();
    let (ms, me) = (edges.margin.inline_start, edges.margin.inline_end);
    let (auto_s, auto_e) = it.style.margin_auto_inline;
    let avail = floor0(area_width - ms - me);
    let m = &it.measure;
    let adj = |v: Au| {
        if it.style.border_box {
            floor0(v).max(bp)
        } else {
            floor0(v) + bp
        }
    };
    let fit = |a: Au| m.max.min(m.min.max(a));
    let stretchable =
        matches!(it.style.justify.value, Align::Normal | Align::Stretch) && !auto_s && !auto_e;
    let mut width = match &it.style.width {
        SizeKind::Len(l) => resolve(l, Some(area_width)).map_or_else(|| avail, adj),
        SizeKind::MinContent => m.min,
        SizeKind::MaxContent => m.max,
        SizeKind::FitContent(arg) => {
            let a = arg
                .as_ref()
                .and_then(|l| resolve(l, Some(area_width)))
                .map_or(avail, floor0);
            fit(a)
        }
        SizeKind::Auto | SizeKind::None => {
            if stretchable {
                avail
            } else {
                fit(avail)
            }
        }
    };
    let bound = |k: &SizeKind| match k {
        SizeKind::Len(l) => resolve(l, Some(area_width)).map(adj),
        _ => None,
    };
    if let Some(max) = bound(&it.style.max_width) {
        width = width.min(max);
    }
    if let Some(min) = bound(&it.style.min_width) {
        width = width.max(min);
    }
    width = width.max(bp);
    let (offset, ms_used, me_used) = align_in_area(
        area_width,
        width,
        (ms, me),
        (auto_s, auto_e),
        it.style.justify,
    );
    (
        area_start + offset,
        ItemInline {
            containing: area_width,
            content_inline: floor0(width - bp),
            margin_start: ms_used,
            margin_end: me_used,
        },
    )
}
