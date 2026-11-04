//! Flex layout: CSS Flexible Box Layout Level 1, section 9.
//!
//! # How it fits the block engine
//!
//! `block.rs` is a stack machine that visits each box on the way down (`Enter`,
//! `Children`) and on the way up (`Exit`). Flex layout is split across the same
//! visits so that it stays iterative:
//!
//! - **`Children`** ([`FlexState::begin`]): the container's items are collected and
//!   everything that can be decided *before* the items are laid out is decided.
//!   For a row that is the whole main axis: flex base sizes, line breaking and
//!   resolving flexible lengths (9.2 to 9.7) need only the container's width and
//!   the items' intrinsic widths. For a column it is each item's width (the cross
//!   size). Each item's inline size is recorded and applied when the item is
//!   entered ([`FlexState::inline_size`]).
//! - **`Exit`** ([`FlexState::finish`]): the items have been laid out, so their
//!   heights are known. A row now has its hypothetical cross sizes; a column has
//!   its flex base sizes. Lines are sized and aligned, items are stretched and
//!   positioned, and the container's block size is returned.
//!
//! # What it does not do
//!
//! Each is a named gap, not a silent one.
//!
//! - **Writing modes and `direction`.** Horizontal-tb, left to right only.
//! - **Baseline alignment.** `baseline` is treated as `flex-start`; there is no
//!   font system to take a baseline from before Phase 9.
//! - **Re-layout after stretching.** An item stretched along the cross axis keeps
//!   the content layout it had at its hypothetical size. A stretched row item's
//!   *children* are not re-laid out against the new height (so percentage heights
//!   and nested stretching inside it see the old one), and in a wrapping column a
//!   stretched item's content keeps its fit-content width.
//! - **Percentage heights** resolve only against a container with a definite
//!   `height`; otherwise they behave as `auto`.
//! - **Static position** of an absolutely positioned child is the content-box
//!   origin, not the position it would have had as the container's sole item.
//! - **`inline-flex`** is block-level here (the engine has no atomic inlines): it
//!   shrinks to fit but breaks the line instead of sitting in it.
//! - **`visibility: collapse`**, `aspect-ratio`, replaced elements.
//!
//! # Integers
//!
//! The flex factors are floats in computed style. They are turned into 16.16
//! fixed point the moment they are read and nothing in this file touches a float
//! afterwards, so a repeat run cannot differ in the order operations happened in.
//! Products that can leave `i64` go through `i128`.

use std::collections::HashMap;

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::computed_values::box_sizing::T as BoxSizing;
use style::computed_values::flex_direction::T as Direction;
use style::computed_values::flex_wrap::T as Wrap;
use style::properties::ComputedValues;
use style::values::computed::{MaxSize, Overflow, Size};
use style::values::generics::flex::FlexBasis;
use style::values::generics::length::{GenericMargin, LengthPercentageOrNormal};
use style::values::specified::align::AlignFlags;
use style::values::specified::box_::{DisplayInside, DisplayOutside};

use crate::block::{
    Content, Edges, Intrinsic, children_in_order, computed_style, intrinsic_inline_size_with,
    is_absolutely_positioned, is_block_level, is_display_none, resolve_edges, shrink_to_fit,
    widest_word,
};
use crate::fragment::{FragmentId, FragmentTree};
use crate::geom::au;

/// Flex factors are held in 16.16 fixed point.
const SCALE: i64 = 65_536;
/// Larger factors saturate, keeping every product inside `i128`.
const MAX_FACTOR: i64 = 1 << 32;

fn raw(a: Au) -> i64 {
    i64::from(a.0)
}

fn from_raw(v: i64) -> Au {
    au(i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX }))
}

/// `a * b / c`, exact in `i128`, truncating toward zero.
fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    if c == 0 {
        return 0;
    }
    let v = i128::from(a) * i128::from(b) / i128::from(c);
    i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX })
}

fn factor(v: style::values::CSSFloat) -> i64 {
    ((v * 65_536.0) as i64).clamp(0, MAX_FACTOR)
}

/// An alignment keyword, resolved into the flex frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Al {
    Start,
    End,
    Center,
    Stretch,
    Between,
    Around,
    Evenly,
}

impl Al {
    fn flipped(self) -> Self {
        match self {
            Al::Start => Al::End,
            Al::End => Al::Start,
            other => other,
        }
    }
}

/// Resolve an `AlignFlags` into the flex frame.
///
/// `reversed` says the axis runs against the physical one (`row-reverse`,
/// `wrap-reverse`), which is what makes `start` and `flex-start` different words.
fn resolve_al(flags: AlignFlags, reversed: bool, normal: Al) -> Al {
    let v = flags.value();
    let physical_start = if reversed { Al::End } else { Al::Start };
    if v == AlignFlags::AUTO || v == AlignFlags::NORMAL {
        normal
    } else if v == AlignFlags::START || v == AlignFlags::SELF_START || v == AlignFlags::LEFT {
        physical_start
    } else if v == AlignFlags::END || v == AlignFlags::SELF_END || v == AlignFlags::RIGHT {
        physical_start.flipped()
    } else if v == AlignFlags::FLEX_START || v == AlignFlags::BASELINE {
        Al::Start
    } else if v == AlignFlags::FLEX_END || v == AlignFlags::LAST_BASELINE {
        Al::End
    } else if v == AlignFlags::CENTER {
        Al::Center
    } else if v == AlignFlags::STRETCH {
        Al::Stretch
    } else if v == AlignFlags::SPACE_BETWEEN {
        Al::Between
    } else if v == AlignFlags::SPACE_AROUND {
        Al::Around
    } else if v == AlignFlags::SPACE_EVENLY {
        Al::Evenly
    } else {
        normal
    }
}

/// Where packing starts and how much extra goes between `n` things, given `free`
/// space (8.2 and 8.3). Negative free space falls back as the spec says.
fn distribute(al: Al, free: i64, n: usize) -> (i64, i64) {
    let count = i64::try_from(n).unwrap_or(i64::MAX).max(1);
    match al {
        Al::Start | Al::Stretch => (0, 0),
        Al::End => (free, 0),
        Al::Center => (free / 2, 0),
        Al::Between if free > 0 && count > 1 => (0, free / (count - 1)),
        Al::Between => (0, 0),
        Al::Around if free > 0 => {
            let space = free / count;
            (space / 2, space)
        }
        Al::Evenly if free > 0 => {
            let space = free / (count + 1);
            (space, space)
        }
        Al::Around | Al::Evenly => (free / 2, 0),
    }
}

/// How the flex-basis was stated.
#[derive(Clone, Copy, Debug)]
enum Basis {
    Auto,
    Content,
    Len(Au),
}

/// One axis of an item: 0 is inline (x), 1 is block (y).
#[derive(Clone, Copy, Debug)]
struct Axis {
    /// Start and end margins; `None` is `auto`.
    margin: [Option<Au>; 2],
    /// Border plus padding along this axis.
    bp: Au,
    /// The inner (content-box) size if definite.
    size: Option<Au>,
    /// `min-*` as an inner size; `None` is `auto`.
    min: Option<Au>,
    max: Option<Au>,
}

impl Axis {
    const EMPTY: Self = Self {
        margin: [Some(Au(0)), Some(Au(0))],
        bp: Au(0),
        size: None,
        min: None,
        max: None,
    };

    fn fixed_margins(&self) -> Au {
        self.margin[0].unwrap_or(Au(0)) + self.margin[1].unwrap_or(Au(0))
    }

    fn auto_margins(&self) -> usize {
        self.margin.iter().filter(|m| m.is_none()).count()
    }

    /// Everything outside the inner size that the flex algorithm counts.
    fn extra(&self) -> Au {
        self.bp + self.fixed_margins()
    }
}

#[derive(Clone, Debug)]
struct Item {
    node: Option<NodeId>,
    absolute: bool,
    order: i32,
    grow: i64,
    shrink: i64,
    ax: [Axis; 2],
    basis: Basis,
    align: Al,
    /// `overflow: visible`, which is what makes `min-width: auto` content-based.
    visible: bool,
    /// Intrinsic inner inline sizes.
    min_i: Au,
    max_i: Au,
    base: Au,
    hyp: Au,
    min_main: Au,
    max_main: Option<Au>,
    /// Used inner main and cross sizes.
    main: Au,
    cross: Au,
}

impl Item {
    fn blank() -> Self {
        Self {
            node: None,
            absolute: false,
            order: 0,
            grow: 0,
            shrink: SCALE,
            ax: [Axis::EMPTY; 2],
            basis: Basis::Auto,
            align: Al::Stretch,
            visible: true,
            min_i: Au(0),
            max_i: Au(0),
            base: Au(0),
            hyp: Au(0),
            min_main: Au(0),
            max_main: None,
            main: Au(0),
            cross: Au(0),
        }
    }
}

struct Cont {
    row: bool,
    main_rev: bool,
    wrap: bool,
    cross_rev: bool,
    justify: Al,
    content: Al,
    items_align: Al,
    main_gap: Au,
    cross_gap: Au,
    inner_inline: Au,
    def_h: Option<Au>,
    min_h: Au,
    max_h: Option<Au>,
    font_size: Au,
    items: Vec<Item>,
    /// All items, in order-modified document order.
    ordered: Vec<usize>,
    /// In-flow items only, in the same order.
    flow: Vec<usize>,
    lines: Vec<Vec<usize>>,
}

/// Flex state for one layout run.
pub(crate) struct FlexState {
    /// Inner inline size to give each flex item when it is entered.
    forced: HashMap<NodeId, Au>,
    containers: HashMap<FragmentId, Cont>,
}

/// Whether `style` generates a flex container.
fn is_flex_container(style: &ComputedValues) -> bool {
    let d = style.clone_display();
    d.inside() == DisplayInside::Flex && d.outside() != DisplayOutside::None
}

fn is_inline_flex(style: &ComputedValues) -> bool {
    let d = style.clone_display();
    d.inside() == DisplayInside::Flex && d.outside() == DisplayOutside::Inline
}

/// How a container's items add up in an intrinsic size walk: whether min-content
/// and max-content are sums, and the gap between summed items.
pub(crate) fn intrinsic_mode(style: &ComputedValues) -> (bool, bool, Au) {
    if !is_flex_container(style) {
        return (false, false, Au(0));
    }
    match style.clone_flex_direction() {
        Direction::Row | Direction::RowReverse => {
            let wraps = !matches!(style.clone_flex_wrap(), Wrap::Nowrap);
            let gap = gap_of(&style.clone_column_gap(), None);
            (!wraps, true, gap)
        }
        _ => (false, false, Au(0)),
    }
}

fn gap_of(
    gap: &LengthPercentageOrNormal<style::values::computed::NonNegativeLengthPercentage>,
    basis: Option<Au>,
) -> Au {
    match gap {
        LengthPercentageOrNormal::LengthPercentage(lp) => {
            lp.0.maybe_to_used_value(basis).unwrap_or(Au(0)).max(Au(0))
        }
        LengthPercentageOrNormal::Normal => Au(0),
    }
}

fn size_of(size: &Size, basis: Option<Au>) -> Option<Au> {
    match size {
        Size::LengthPercentage(lp) => lp.0.maybe_to_used_value(basis).map(|v| v.max(Au(0))),
        _ => None,
    }
}

fn max_of(size: &MaxSize, basis: Option<Au>) -> Option<Au> {
    match size {
        MaxSize::LengthPercentage(lp) => lp.0.maybe_to_used_value(basis).map(|v| v.max(Au(0))),
        _ => None,
    }
}

/// An outer size to an inner one, per `box-sizing`.
fn inner(border_box: bool, value: Au, bp: Au) -> Au {
    if border_box {
        (value - bp).max(Au(0))
    } else {
        value
    }
}

fn clamp_au(value: Au, min: Au, max: Option<Au>) -> Au {
    let capped = match max {
        Some(m) if value > m => m,
        _ => value,
    };
    if capped < min { min } else { capped }
}

impl Cont {
    fn read(style: &ComputedValues, inner_inline: Au) -> Self {
        let dir = style.clone_flex_direction();
        let row = matches!(dir, Direction::Row | Direction::RowReverse);
        let main_rev = matches!(dir, Direction::RowReverse | Direction::ColumnReverse);
        let wrap_v = style.clone_flex_wrap();
        let wrap = !matches!(wrap_v, Wrap::Nowrap);
        let cross_rev = matches!(wrap_v, Wrap::WrapReverse);

        let edges = resolve_edges(style, inner_inline);
        let bp_b = edges.border.block_sum() + edges.padding.block_sum();
        let border_box = style.clone_box_sizing() == BoxSizing::BorderBox;
        let def_h = size_of(&style.clone_height(), None).map(|h| inner(border_box, h, bp_b));
        let min_h =
            size_of(&style.clone_min_height(), None).map_or(Au(0), |h| inner(border_box, h, bp_b));
        let max_h = max_of(&style.clone_max_height(), None).map(|h| inner(border_box, h, bp_b));

        let col_gap = gap_of(&style.clone_column_gap(), Some(inner_inline));
        let row_gap = gap_of(&style.clone_row_gap(), def_h);
        let (main_gap, cross_gap) = if row {
            (col_gap, row_gap)
        } else {
            (row_gap, col_gap)
        };

        let items_flags = style.clone_align_items().0;
        let items_align = resolve_al(items_flags, cross_rev, Al::Stretch);
        Self {
            row,
            main_rev,
            wrap,
            cross_rev,
            justify: resolve_al(style.clone_justify_content().primary(), main_rev, Al::Start),
            content: resolve_al(
                style.clone_align_content().primary(),
                cross_rev,
                Al::Stretch,
            ),
            items_align,
            main_gap,
            cross_gap,
            inner_inline,
            def_h,
            min_h,
            max_h,
            font_size: Au::from(style.clone_font_size().computed_size()),
            items: Vec::new(),
            ordered: Vec::new(),
            flow: Vec::new(),
            lines: Vec::new(),
        }
    }

    fn m(&self) -> usize {
        if self.row { 0 } else { 1 }
    }

    fn x(&self) -> usize {
        1 - self.m()
    }
}

fn read_item(
    dom: px_css::view::Dom<'_>,
    arena: &Arena,
    node: NodeId,
    style: &ComputedValues,
    cont: &Cont,
) -> Item {
    let mut it = Item::blank();
    it.node = Some(node);
    it.order = style.clone_order();
    it.grow = factor(style.clone_flex_grow().0);
    it.shrink = factor(style.clone_flex_shrink().0);

    let edges: Edges = resolve_edges(style, cont.inner_inline);
    let margin = style.get_margin();
    let auto = |m: &style::values::computed::Margin| matches!(m, GenericMargin::Auto);
    let border_box = style.clone_box_sizing() == BoxSizing::BorderBox;

    let bp_i = edges.border.inline_sum() + edges.padding.inline_sum();
    let bp_b = edges.border.block_sum() + edges.padding.block_sum();
    let mk = |lo_auto: bool, hi_auto: bool, lo: Au, hi: Au| {
        [(!lo_auto).then_some(lo), (!hi_auto).then_some(hi)]
    };
    it.ax[0].margin = mk(
        auto(&margin.margin_left),
        auto(&margin.margin_right),
        edges.margin.inline_start,
        edges.margin.inline_end,
    );
    it.ax[1].margin = mk(
        auto(&margin.margin_top),
        auto(&margin.margin_bottom),
        edges.margin.block_start,
        edges.margin.block_end,
    );
    it.ax[0].bp = bp_i;
    it.ax[1].bp = bp_b;

    let wb = Some(cont.inner_inline);
    let hb = cont.def_h;
    it.ax[0].size = size_of(&style.clone_width(), wb).map(|v| inner(border_box, v, bp_i));
    it.ax[1].size = size_of(&style.clone_height(), hb).map(|v| inner(border_box, v, bp_b));
    it.ax[0].min = size_of(&style.clone_min_width(), wb).map(|v| inner(border_box, v, bp_i));
    it.ax[1].min = size_of(&style.clone_min_height(), hb).map(|v| inner(border_box, v, bp_b));
    it.ax[0].max = max_of(&style.clone_max_width(), wb).map(|v| inner(border_box, v, bp_i));
    it.ax[1].max = max_of(&style.clone_max_height(), hb).map(|v| inner(border_box, v, bp_b));

    let main_basis = if cont.row { wb } else { hb };
    let main_bp = if cont.row { bp_i } else { bp_b };
    it.basis = match style.clone_flex_basis() {
        FlexBasis::Content => Basis::Content,
        FlexBasis::Size(s) => size_of(&s, main_basis)
            .map_or(Basis::Auto, |v| Basis::Len(inner(border_box, v, main_bp))),
    };
    it.align = resolve_al(style.clone_align_self().0, cont.cross_rev, cont.items_align);
    it.visible = style.clone_overflow_x() == Overflow::Visible
        && style.clone_overflow_y() == Overflow::Visible;

    // Intrinsic inline sizes are needed by a row for every item's base size and
    // content minimum, and by a column only where the width is not stated.
    if cont.row || it.ax[0].size.is_none() {
        let s0 = resolve_edges(style, Au(0)).inline_surround();
        let intr = intrinsic_inline_size_with(dom, arena, node, true);
        it.min_i = (intr.min - s0).max(Au(0));
        it.max_i = (intr.max - s0).max(Au(0));
    }
    let _ = (arena, dom);
    it
}

fn anonymous_item(text: &str, cont: &Cont) -> Item {
    let mut it = Item::blank();
    it.align = cont.items_align;
    it.min_i = widest_word(text, cont.font_size);
    it.max_i = crate::text::measure(text, cont.font_size);
    it
}

impl FlexState {
    pub(crate) fn new() -> Self {
        Self {
            forced: HashMap::new(),
            containers: HashMap::new(),
        }
    }

    pub(crate) fn is_item(&self, node: NodeId) -> bool {
        self.forced.contains_key(&node)
    }

    pub(crate) fn is_container(&self, fragment: FragmentId) -> bool {
        self.containers.contains_key(&fragment)
    }

    /// The inner inline size of an anonymous item at `slot`.
    pub(crate) fn anonymous_width(&self, container: FragmentId, slot: usize) -> Option<Au> {
        let cont = self.containers.get(&container)?;
        let it = cont.items.get(slot)?;
        Some(if cont.row { it.main } else { it.cross })
    }

    /// The inner inline size a box should take: the flex algorithm's for an item,
    /// a shrink-to-fit one for `inline-flex`, otherwise `current`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn inline_size(
        &self,
        dom: px_css::view::Dom<'_>,
        arena: &Arena,
        node: NodeId,
        style: &ComputedValues,
        containing: Au,
        resolved: &Edges,
        current: Au,
    ) -> Au {
        if let Some(w) = self.forced.get(&node) {
            return *w;
        }
        if is_inline_flex(style) && matches!(style.clone_width(), Size::Auto) {
            let s0 = resolve_edges(style, Au(0)).inline_surround();
            let intr = intrinsic_inline_size_with(dom, arena, node, false);
            let available = (containing - resolved.inline_surround()).max(Au(0));
            let fit = shrink_to_fit(
                Intrinsic {
                    min: (intr.min - s0).max(Au(0)),
                    max: (intr.max - s0).max(Au(0)),
                },
                available,
            );
            let min = size_of(&style.clone_min_width(), Some(containing)).unwrap_or(Au(0));
            let max = max_of(&style.clone_max_width(), Some(containing));
            return clamp_au(fit, min, max);
        }
        current
    }

    /// Collect a flex container's items and resolve whatever needs no layout.
    ///
    /// Returns `None` when `node` is not a flex container. The returned contents
    /// are in document order, one per item, so a slot index is an item index.
    pub(crate) fn begin(
        &mut self,
        dom: px_css::view::Dom<'_>,
        arena: &Arena,
        fragment: FragmentId,
        node: NodeId,
        content_inline: Au,
    ) -> Option<Vec<Content>> {
        let style = computed_style(dom, node)?;
        if !is_flex_container(&style) {
            return None;
        }
        let mut cont = Cont::read(&style, content_inline);
        let mut contents: Vec<Content> = Vec::new();

        fn flush(run: &mut String, contents: &mut Vec<Content>, cont: &mut Cont) {
            let collapsed = crate::inline::collapse_whitespace(run);
            run.clear();
            if !collapsed.trim().is_empty() {
                cont.items.push(anonymous_item(&collapsed, cont));
                contents.push(Content::InlineRun(collapsed));
            }
        }

        let mut run = String::new();
        let mut stack = children_in_order(arena, node);
        stack.reverse();
        while let Some(id) = stack.pop() {
            let Some(current) = arena.get(id) else {
                continue;
            };
            if current.element_name().is_some() {
                let Some(s) = computed_style(dom, id) else {
                    continue;
                };
                if is_display_none(&s) {
                    continue;
                }
                if is_absolutely_positioned(&s) {
                    let mut it = read_item(dom, arena, id, &s, &cont);
                    it.absolute = true;
                    cont.items.push(it);
                    contents.push(Content::Absolute(id));
                } else if is_block_level(&s) {
                    flush(&mut run, &mut contents, &mut cont);
                    let it = read_item(dom, arena, id, &s, &cont);
                    cont.items.push(it);
                    contents.push(Content::Block(id));
                } else {
                    // `display: contents` and friends: the children are items.
                    let mut kids = children_in_order(arena, id);
                    kids.reverse();
                    stack.extend(kids);
                }
            } else if let Some(text) = current.text() {
                run.push_str(text);
            }
        }
        flush(&mut run, &mut contents, &mut cont);

        let mut ordered: Vec<usize> = (0..cont.items.len()).collect();
        ordered.sort_by_key(|&i| (cont.items.get(i).map_or(0, |it| it.order), i));
        cont.flow = ordered
            .iter()
            .copied()
            .filter(|&i| cont.items.get(i).is_some_and(|it| !it.absolute))
            .collect();
        cont.ordered = ordered;

        if cont.row {
            row_main_sizes(&mut cont);
            for &i in &cont.flow {
                if let Some(it) = cont.items.get(i)
                    && let Some(n) = it.node
                {
                    self.forced.insert(n, it.main);
                }
            }
        } else {
            column_cross_sizes(&mut cont);
            for &i in &cont.flow {
                if let Some(it) = cont.items.get(i)
                    && let Some(n) = it.node
                {
                    self.forced.insert(n, it.cross);
                }
            }
        }
        self.containers.insert(fragment, cont);
        Some(contents)
    }

    /// Finish a container whose items have been laid out.
    ///
    /// `slots` are the container's content slots (item `i` is slot `i`); `origin`
    /// is the content box's offset inside the border box. Returns the used inner
    /// block size and the container's children in layout order.
    pub(crate) fn finish(
        &mut self,
        tree: &mut FragmentTree,
        fragment: FragmentId,
        slots: &[Option<FragmentId>],
        origin: (Au, Au),
    ) -> Option<(Au, Vec<FragmentId>)> {
        let mut cont = self.containers.remove(&fragment)?;
        let content = place(&mut cont, tree, slots, origin);
        let kids = cont
            .ordered
            .iter()
            .filter_map(|&i| slots.get(i).copied().flatten())
            .collect();
        Some((content, kids))
    }
}

// ---------------------------------------------------------------------------
// 9.2 and 9.3: base sizes, hypothetical sizes, lines.
// ---------------------------------------------------------------------------

/// The content-based minimum (4.5) or the stated `min-*`, as an inner size.
fn min_main_of(it: &Item, m: usize, content_suggestion: Au) -> Au {
    if let Some(min) = it.ax[m].min {
        return min;
    }
    if !it.visible {
        return Au(0);
    }
    let capped = match it.ax[m].max {
        Some(max) if content_suggestion > max => max,
        _ => content_suggestion,
    };
    match it.ax[m].size {
        Some(specified) => {
            let specified = match it.ax[m].max {
                Some(max) if specified > max => max,
                _ => specified,
            };
            specified.min(capped)
        }
        None => capped,
    }
}

/// Fill in base, hypothetical and min/max main sizes. `content` is the item's
/// content size along the main axis.
fn main_params(it: &mut Item, m: usize, content: Au, content_min: Au) {
    it.base = match it.basis {
        Basis::Len(l) => l,
        Basis::Auto => it.ax[m].size.unwrap_or(content),
        Basis::Content => content,
    };
    it.max_main = it.ax[m].max;
    it.min_main = min_main_of(it, m, content_min);
    it.hyp = clamp_au(it.base, it.min_main, it.max_main);
}

fn row_main_sizes(cont: &mut Cont) {
    let avail = cont.inner_inline;
    let flow = cont.flow.clone();
    for &i in &flow {
        if let Some(it) = cont.items.get_mut(i) {
            let (content, content_min) = (it.max_i, it.min_i);
            main_params(it, 0, content, content_min);
        }
    }
    cont.lines = break_lines(cont, Some(avail));
    let lines = cont.lines.clone();
    for line in &lines {
        flex_line(&mut cont.items, line, 0, avail, cont.main_gap);
    }
}

/// 9.3.5: collect items into lines. `avail` of `None` is unbounded.
fn break_lines(cont: &Cont, avail: Option<Au>) -> Vec<Vec<usize>> {
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0i64;
    let m = cont.m();
    for &i in &cont.flow {
        let Some(it) = cont.items.get(i) else {
            continue;
        };
        let outer = raw(it.hyp + it.ax[m].extra());
        let with_gap = if current.is_empty() {
            outer
        } else {
            outer + raw(cont.main_gap)
        };
        let overflows = avail.is_some_and(|a| used + with_gap > raw(a));
        if cont.wrap && !current.is_empty() && overflows {
            lines.push(std::mem::take(&mut current));
            used = outer;
        } else {
            used += with_gap;
        }
        current.push(i);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// 9.7: resolve the flexible lengths of one line on main axis `m`. Sets `main` on
/// each item. `avail` is the container's inner main size.
fn flex_line(items: &mut [Item], line: &[usize], m: usize, avail: Au, gap: Au) {
    struct F {
        idx: usize,
        base: i64,
        hyp: i64,
        min: i64,
        max: Option<i64>,
        extra: i64,
        grow: i64,
        shrink: i64,
        target: i64,
        frozen: bool,
    }
    let mut fs: Vec<F> = Vec::new();
    for &idx in line {
        if let Some(it) = items.get(idx) {
            fs.push(F {
                idx,
                base: raw(it.base),
                hyp: raw(it.hyp),
                min: raw(it.min_main),
                max: it.max_main.map(raw),
                extra: raw(it.ax[m].extra()),
                grow: it.grow,
                shrink: it.shrink,
                target: raw(it.hyp),
                frozen: false,
            });
        }
    }
    if fs.is_empty() {
        return;
    }
    let gaps = raw(gap) * i64::try_from(fs.len() - 1).unwrap_or(0);
    let avail = raw(avail);

    // 1. Which factor applies.
    let sum_hyp: i64 = fs.iter().map(|f| f.hyp + f.extra).sum::<i64>() + gaps;
    let use_grow = sum_hyp < avail;
    // 2. Freeze inflexible items at their hypothetical size.
    for f in &mut fs {
        let flex = if use_grow { f.grow } else { f.shrink };
        if flex == 0 || (use_grow && f.base > f.hyp) || (!use_grow && f.base < f.hyp) {
            f.frozen = true;
            f.target = f.hyp;
        }
    }
    let used = |fs: &[F]| -> i64 {
        fs.iter()
            .map(|f| if f.frozen { f.target } else { f.base } + f.extra)
            .sum::<i64>()
    };
    // 3. Initial free space.
    let initial_free = avail - gaps - used(&fs);

    // 4. Loop.
    for _ in 0..=fs.len() {
        if fs.iter().all(|f| f.frozen) {
            break;
        }
        let mut remaining = avail - gaps - used(&fs);
        let sum_f: i64 = fs
            .iter()
            .filter(|f| !f.frozen)
            .map(|f| if use_grow { f.grow } else { f.shrink })
            .sum();
        if sum_f < SCALE {
            let scaled = mul_div(initial_free, sum_f, SCALE);
            if scaled.abs() < remaining.abs() {
                remaining = scaled;
            }
        }
        if use_grow {
            for f in fs.iter_mut().filter(|f| !f.frozen) {
                f.target = f.base + mul_div(remaining, f.grow, sum_f);
            }
        } else {
            let sum_s: i64 = fs
                .iter()
                .filter(|f| !f.frozen)
                .map(|f| mul_div(f.shrink, f.base, 1))
                .sum();
            for f in fs.iter_mut().filter(|f| !f.frozen) {
                f.target = f.base + mul_div(remaining, mul_div(f.shrink, f.base, 1), sum_s);
            }
        }
        // Fix min/max violations.
        let clamped: Vec<i64> = fs
            .iter()
            .map(|f| {
                let capped = match f.max {
                    Some(max) if f.target > max => max,
                    _ => f.target,
                };
                capped.max(f.min)
            })
            .collect();
        let total: i64 = fs
            .iter()
            .zip(&clamped)
            .filter(|(f, _)| !f.frozen)
            .map(|(f, c)| c - f.target)
            .sum();
        for (f, c) in fs.iter_mut().zip(&clamped) {
            if f.frozen {
                continue;
            }
            let violated = *c - f.target;
            if total == 0 || (total > 0 && violated > 0) || (total < 0 && violated < 0) {
                f.frozen = true;
                f.target = *c;
            }
        }
    }
    for f in &fs {
        if let Some(it) = items.get_mut(f.idx) {
            it.main = from_raw(f.target.max(0));
        }
    }
}

/// A column's cross sizes (widths), which are known before its items are laid out.
fn column_cross_sizes(cont: &mut Cont) {
    let avail = cont.inner_inline;
    let flow = cont.flow.clone();
    for &i in &flow {
        let Some(it) = cont.items.get_mut(i) else {
            continue;
        };
        let ax = it.ax[0];
        let room = (avail - ax.extra()).max(Au(0));
        let width = if let Some(w) = ax.size {
            w
        } else if it.align == Al::Stretch && ax.auto_margins() == 0 {
            room
        } else {
            shrink_to_fit(
                Intrinsic {
                    min: it.min_i,
                    max: it.max_i,
                },
                room,
            )
        };
        it.cross = clamp_au(width, ax.min.unwrap_or(Au(0)), ax.max);
    }
}

// ---------------------------------------------------------------------------
// 9.4 to 9.6: cross sizing and alignment, then positions.
// ---------------------------------------------------------------------------

fn place(
    cont: &mut Cont,
    tree: &mut FragmentTree,
    slots: &[Option<FragmentId>],
    origin: (Au, Au),
) -> Au {
    let (origin_i, origin_b) = origin;
    let m = cont.m();
    let x = cont.x();
    let has_box = |i: usize| slots.get(i).copied().flatten().is_some();

    cont.flow.retain(|&i| has_box(i));
    for line in &mut cont.lines {
        line.retain(|&i| has_box(i));
    }
    cont.lines.retain(|l| !l.is_empty());

    // Content sizes, from the laid-out items.
    let flow = cont.flow.clone();
    for &i in &flow {
        let Some(frag) = slots.get(i).copied().flatten().and_then(|f| tree.get(f)) else {
            continue;
        };
        let content_h = frag.size.block;
        let Some(it) = cont.items.get_mut(i) else {
            continue;
        };
        let content_h = (content_h - it.ax[1].bp).max(Au(0));
        if cont.row {
            let h = it.ax[1].size.unwrap_or(content_h);
            it.cross = clamp_au(h, it.ax[1].min.unwrap_or(Au(0)), it.ax[1].max);
        } else {
            main_params(it, 1, content_h, content_h);
        }
    }

    // The main size, and for a column the lines and flexible lengths.
    let main_size = if cont.row {
        cont.inner_inline
    } else {
        cont.lines = break_lines(cont, cont.def_h.or(cont.max_h));
        let main_gap = raw(cont.main_gap);
        let longest = cont
            .lines
            .iter()
            .map(|l| {
                l.iter()
                    .filter_map(|&i| cont.items.get(i))
                    .map(|it| raw(it.hyp + it.ax[1].extra()))
                    .sum::<i64>()
                    + main_gap * i64::try_from(l.len().saturating_sub(1)).unwrap_or(0)
            })
            .max()
            .unwrap_or(0);
        let m_size = match cont.def_h {
            Some(h) => clamp_au(h, cont.min_h, cont.max_h),
            None => clamp_au(from_raw(longest), cont.min_h, cont.max_h),
        };
        let lines = cont.lines.clone();
        for line in &lines {
            flex_line(&mut cont.items, line, 1, m_size, cont.main_gap);
        }
        m_size
    };

    // 9.4 steps 7-9: line cross sizes.
    let hyp_cross: Vec<i64> = cont
        .lines
        .iter()
        .map(|l| {
            l.iter()
                .filter_map(|&i| cont.items.get(i))
                .map(|it| raw(it.cross + it.ax[x].extra()))
                .max()
                .unwrap_or(0)
        })
        .collect();
    let n_lines = cont.lines.len();
    let cross_gap = raw(cont.cross_gap);
    let gaps_total = cross_gap * i64::try_from(n_lines.saturating_sub(1)).unwrap_or(0);

    let (cross_total, mut line_cross): (i64, Vec<i64>) = if cont.row {
        if !cont.wrap {
            let one = match cont.def_h {
                Some(h) => clamp_au(h, cont.min_h, cont.max_h),
                None => clamp_au(
                    from_raw(hyp_cross.first().copied().unwrap_or(0)),
                    cont.min_h,
                    cont.max_h,
                ),
            };
            (raw(one), vec![raw(one)])
        } else {
            let sum = hyp_cross.iter().sum::<i64>() + gaps_total;
            let total = match cont.def_h {
                Some(h) => clamp_au(h, cont.min_h, cont.max_h),
                None => clamp_au(from_raw(sum), cont.min_h, cont.max_h),
            };
            (raw(total), hyp_cross.clone())
        }
    } else if !cont.wrap {
        (raw(cont.inner_inline), vec![raw(cont.inner_inline)])
    } else {
        (raw(cont.inner_inline), hyp_cross.clone())
    };

    // 9.4 step 9: align-content: stretch.
    let sum_lines = line_cross.iter().sum::<i64>() + gaps_total;
    let mut free = cross_total - sum_lines;
    if cont.wrap && cont.content == Al::Stretch && free > 0 && n_lines > 0 {
        let share = free / i64::try_from(n_lines).unwrap_or(1);
        for lc in &mut line_cross {
            *lc += share;
        }
        free = cross_total - (line_cross.iter().sum::<i64>() + gaps_total);
    }
    let (line_start, line_extra) = if cont.wrap {
        distribute(cont.content, free, n_lines)
    } else {
        (0, 0)
    };

    let main_total = raw(main_size);
    let mut line_pos = line_start;
    let lines = cont.lines.clone();
    for (l, line) in lines.iter().enumerate() {
        let lc = line_cross.get(l).copied().unwrap_or(0);

        // 9.4 steps 11-14 and 9.6: cross sizes and offsets inside the line.
        let line_phys = if cont.cross_rev {
            cross_total - line_pos - lc
        } else {
            line_pos
        };
        let mut cross_off: Vec<i64> = Vec::new();
        for &i in line {
            let Some(it) = cont.items.get_mut(i) else {
                cross_off.push(0);
                continue;
            };
            let ax = it.ax[x];
            if it.align == Al::Stretch && ax.size.is_none() && ax.auto_margins() == 0 {
                it.cross = clamp_au(
                    from_raw((lc - raw(ax.extra())).max(0)),
                    ax.min.unwrap_or(Au(0)),
                    ax.max,
                );
            }
            let outer = raw(it.cross + ax.extra());
            let spare = lc - outer;
            let autos = ax.auto_margins();
            let share = if autos > 0 && spare > 0 {
                spare / i64::try_from(autos).unwrap_or(1)
            } else {
                0
            };
            let lo = raw(ax.margin[0].unwrap_or(from_raw(share)));
            let hi = raw(ax.margin[1].unwrap_or(from_raw(share)));
            let border_box = raw(it.cross + ax.bp);
            // `stretch` that could not stretch behaves as `flex-start`, and
            // `flex-start` is the physical bottom in a `wrap-reverse` container.
            let align = match it.align {
                Al::End => Al::End,
                Al::Center => Al::Center,
                _ => Al::Start,
            };
            let align = if cont.cross_rev {
                align.flipped()
            } else {
                align
            };
            let off = if autos > 0 {
                lo
            } else {
                match align {
                    Al::End => lc - (border_box + hi),
                    Al::Center => spare / 2 + lo,
                    _ => lo,
                }
            };
            cross_off.push(line_phys + off);
        }

        // 9.5: main-axis alignment.
        let mut outer_sum = 0i64;
        let mut autos_total = 0usize;
        for &i in line {
            if let Some(it) = cont.items.get(i) {
                outer_sum += raw(it.main + it.ax[m].extra());
                autos_total += it.ax[m].auto_margins();
            }
        }
        let gap = raw(cont.main_gap);
        let count = line.len();
        let main_free =
            main_total - outer_sum - gap * i64::try_from(count.saturating_sub(1)).unwrap_or(0);
        let (start, extra, auto_share) = if autos_total > 0 && main_free > 0 {
            (0, 0, main_free / i64::try_from(autos_total).unwrap_or(1))
        } else {
            let (s, e) = distribute(cont.justify, main_free, count);
            (s, e, 0)
        };

        let mut pos = start;
        for (k, &i) in line.iter().enumerate() {
            let Some(it) = cont.items.get(i) else {
                continue;
            };
            let ax = it.ax[m];
            let lo = raw(ax.margin[0].unwrap_or(from_raw(auto_share)));
            let hi = raw(ax.margin[1].unwrap_or(from_raw(auto_share)));
            let outer = raw(it.main + ax.bp) + lo + hi;
            let phys_outer = if cont.main_rev {
                main_total - pos - outer
            } else {
                pos
            };
            let main_pos = phys_outer + lo;
            pos += outer + gap + extra;

            let cross_pos = cross_off.get(k).copied().unwrap_or(0);
            let (at_x, at_y) = if cont.row {
                (main_pos, cross_pos)
            } else {
                (cross_pos, main_pos)
            };
            let (w, h) = if cont.row {
                (it.main + it.ax[0].bp, it.cross + it.ax[1].bp)
            } else {
                (it.cross + it.ax[0].bp, it.main + it.ax[1].bp)
            };
            if let Some(frag) = slots
                .get(i)
                .copied()
                .flatten()
                .and_then(|f| tree.get_mut(f))
            {
                frag.inline_offset = origin_i + from_raw(at_x);
                frag.block_offset = origin_b + from_raw(at_y);
                frag.size.inline = w;
                frag.size.block = h;
            }
        }
        line_pos += lc + cross_gap + line_extra;
    }

    // Absolutely positioned children: out of flow, at the content origin plus
    // their own margin (the named gap in the module documentation).
    for (i, it) in cont.items.iter().enumerate() {
        if !it.absolute {
            continue;
        }
        if let Some(frag) = slots
            .get(i)
            .copied()
            .flatten()
            .and_then(|f| tree.get_mut(f))
        {
            frag.inline_offset = origin_i + it.ax[0].margin[0].unwrap_or(Au(0));
            frag.block_offset = origin_b + it.ax[1].margin[0].unwrap_or(Au(0));
        }
    }

    if cont.row {
        from_raw(cross_total)
    } else {
        main_size
    }
}
