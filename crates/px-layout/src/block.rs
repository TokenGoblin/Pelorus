//! Block layout: the normal flow, laid out iteratively.
//!
//! Takes a styled `px-dom` document and produces a [`FragmentTree`].
//!
//! # Two passes, one explicit stack, no recursion
//!
//! Block layout is naturally two-phase and the phases run in opposite directions:
//!
//! - **Down**, a box's inline size is resolved against its containing block,
//!   because CSS 2.1 §10.3.3 makes width depend on the *parent's* width.
//! - **Up**, a box's block size is resolved from its children, because
//!   `height: auto` means "as tall as the content" (§10.6.3).
//!
//! Written recursively that is eight lines and a stack overflow on a page that
//! nests a hundred thousand divs — §9 Phase 6's third gate item, and the same
//! failure Phase 4 built its nesting gate around. So the traversal is an explicit
//! stack of [`Step`]s: a box is pushed once to be entered and once to be exited,
//! and the "up" phase happens on the exit visit. The shape is uglier than
//! recursion and it is the whole point.
//!
//! # Three steps, not two
//!
//! [`Step::Children`] sits between them and does one thing: partition a block
//! container's content into runs of inline content and block-level boxes, in
//! document order (§9.2.1.1). It is separate from `Enter` because the initial
//! containing block needs it too — the ICB is a block container whose content is
//! the document's — and because the partition is where anonymous block boxes are
//! generated, which is a different job from resolving a box's width.
//!
//! # What this does not do yet
//!
//! Floats, positioned boxes, and real text shaping ([`crate::text`] is a stub
//! metric with its limits written down). Margin collapsing runs between siblings
//! but not *through* a parent. Inline boxes generate no fragment of their own, so
//! a `<span>`'s border and padding are not drawn; its text is laid out in the run
//! around it. Each is a named gap rather than a silent one, and the tests below
//! assert what is implemented rather than asserting around what is not.

use app_units::Au;
use px_dom::{Arena, NodeId};
use style::properties::ComputedValues;

use crate::fragment::{Fragment, FragmentId, FragmentKind, FragmentTree};
use crate::geom::{LogicalEdges, LogicalSize};

/// One visit in the iterative traversal.
///
/// The two-visit pattern that replaces recursion: `Enter` does the work that
/// needs the parent's geometry, `Exit` does the work that needs the children's.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// Resolve this box's inline size and create its fragment.
    ///
    /// `slot` is where this box's fragment belongs among its parent's children.
    /// Reserved rather than appended, because a parent's children are a mix of
    /// boxes created now (anonymous blocks, during [`Step::Children`]) and boxes
    /// created later (block children, on their own `Enter`). Appending would order
    /// them by creation and put every anonymous block before every real one.
    Enter {
        node: NodeId,
        parent: FragmentId,
        slot: usize,
    },
    /// Partition this container's content and schedule it.
    ///
    /// Separate from `Enter` so the initial containing block can use it too: the
    /// ICB is a block container whose children come from the document, and
    /// without this it needed its own copy of the partitioning.
    Children { fragment: FragmentId, node: NodeId },
    /// Resolve this box's block size from the fragments its children produced.
    Exit { fragment: FragmentId },
}

/// One item of a block container's content, in document order.
///
/// CSS 2.1 §9.2.1.1: a block container holds either only inline content or only
/// block-level content. When markup gives it both, the inline runs are wrapped in
/// **anonymous block boxes** so the container's children are uniformly
/// block-level.
///
/// This is the partition that produces them, and it is most of what the CSS2
/// reftest corpus exercises — 42 of the subset's failures were `block-in-inline`
/// before it existed.
#[derive(Debug)]
enum Content {
    /// A run of inline-level content: text, and the text of inline descendants.
    InlineRun(String),
    /// A block-level child, which becomes a box of its own.
    Block(NodeId),
    /// A floated child (§9.5), which becomes a box of its own but stays *inside*
    /// the run it appears in.
    ///
    /// §9.2.1.1's anonymous box rule is about in-flow block-level boxes: an
    /// out-of-flow one does not separate the inline content around it, because the
    /// content flows past it rather than below it. Treating a float as
    /// `Content::Block` split every `text <span style="float:left">…</span> text`
    /// into two anonymous blocks and stacked them, which is the opposite of what a
    /// float is for.
    Float(NodeId),
}

/// Per-fragment layout state, indexed by fragment index.
///
/// A side table rather than fields on [`Fragment`], because none of it survives
/// layout: the fragment tree is the output, and carrying a box's specified height
/// or its margins into it would invite a consumer to depend on them.
///
/// They grow in lockstep, so a fragment index is a valid index into every one of
/// them — which is what [`Tables::grow_to`] is for, and why it is one method
/// rather than the five parallel `push` calls it replaced at three call sites.
struct Tables {
    /// Children accumulated per fragment, as reserved slots.
    ///
    /// `Option`, because a slot is reserved when a container's content is
    /// partitioned and filled when the child's own `Enter` creates its fragment —
    /// which happens later, after any *anonymous* siblings have already been
    /// created. Appending instead of reserving ordered them by creation and put
    /// every anonymous block before every real one.
    ///
    /// A slot that is still `None` at `Exit` is a child that generated no box.
    pending: Vec<Vec<Option<FragmentId>>>,
    /// The content-box inline size, which children resolve percentages against.
    inline_sizes: Vec<Au>,
    /// `None` means `height: auto`, which is resolved from content on the way up.
    specified_block_sizes: Vec<Option<Au>>,
    /// Whether this fragment establishes a block formatting context (§9.4.1).
    ///
    /// Read on the way up, where it decides whether the box's `height: auto`
    /// stretches to contain its own floats (§10.6.7).
    formatting_context_roots: Vec<bool>,
    /// Which floats this fragment must clear, if any (§9.5.2).
    clear_sides: Vec<Option<crate::float::ClearSide>>,
    /// Whether this fragment is a float, and if so which edge it takes.
    ///
    /// A side table rather than a [`FragmentKind`], because "float" is a statement
    /// about a box's *flow*, not about what kind of box it is — a float is an
    /// ordinary block box that happens to be out of flow, and the distinction
    /// matters again for `position: absolute` in a later phase.
    float_sides: Vec<Option<crate::float::FloatSide>>,
    /// The inline content a box holds directly, waiting to be laid out.
    ///
    /// Recorded when the container is partitioned and used on the way *up*.
    /// A container has at most one of these: `partition_content` ends a run only
    /// at a block-level sibling, and a container with one of those wraps its runs
    /// in anonymous blocks that each hold exactly one.
    inline_runs: Vec<Option<PendingRun>>,
    /// Resolved margins, borders and padding.
    edges: Vec<Edges>,
}

impl Tables {
    /// One row, for the initial containing block.
    fn new(viewport_inline_size: Au) -> Self {
        Self {
            pending: vec![Vec::new()],
            inline_sizes: vec![viewport_inline_size],
            specified_block_sizes: vec![None],
            // The initial containing block is a formatting context root: there
            // is no outer context for a float in it to escape into.
            formatting_context_roots: vec![true],
            clear_sides: vec![None],
            float_sides: vec![None],
            inline_runs: vec![None],
            edges: vec![Edges::ZERO],
        }
    }

    /// Extend every table so that `index` is addressable in all of them.
    fn grow_to(&mut self, index: usize) {
        while self.pending.len() <= index {
            self.pending.push(Vec::new());
            self.inline_sizes.push(Au(0));
            self.specified_block_sizes.push(None);
            self.formatting_context_roots.push(false);
            self.clear_sides.push(None);
            self.float_sides.push(None);
            self.inline_runs.push(None);
            self.edges.push(Edges::ZERO);
        }
    }
}

/// Inline content recorded on the way down and laid out on the way up.
///
/// # Why the deferral
///
/// Inline layout needs two things: the container's content inline size, known on
/// the way *down*, and the floats that shorten its line boxes, known on the way
/// *up* — a float's own size is resolved at its `Exit`, which happens after its
/// container's `Children`. Laying the lines out at `Children` meant they were
/// measured against a container that had not yet met its own floats.
///
/// So the text and the metrics it will be measured with are recorded here and the
/// lines are built at `Exit`, once every float in the container has been placed.
/// Nothing else about the result changes, which is the point: this deferral landed
/// on its own, with the reftest count held fixed, so that the float work after it
/// could not hide a regression inside a restructure.
struct PendingRun {
    text: String,
    font_size: Au,
    line_height: Au,
    /// Where this run sits among its container's content items.
    ///
    /// Only meaningful for a run held directly by a block container — one wrapped
    /// in an anonymous block is laid out when its wrapper's slot comes round. It
    /// exists so that a float *before* the text is placed before the lines are
    /// measured, and a float after it is not.
    slot: usize,
}

/// The resolved box-model edges of one box, in app units.
struct Edges {
    margin: LogicalEdges,
    border: LogicalEdges,
    padding: LogicalEdges,
}

impl Edges {
    /// No margin, border or padding — what an anonymous block box has (§9.2.1.1).
    const ZERO: Self = Self {
        margin: LogicalEdges::ZERO,
        border: LogicalEdges::ZERO,
        padding: LogicalEdges::ZERO,
    };

    /// Everything outside the content box, along the inline axis.
    fn inline_surround(&self) -> Au {
        self.margin.inline_sum() + self.border.inline_sum() + self.padding.inline_sum()
    }

    /// Everything outside the content box, along the block axis.
    fn block_surround(&self) -> Au {
        self.border.block_sum() + self.padding.block_sum()
    }
}

/// Lay out `arena` against a viewport `inline_size` wide.
///
/// Returns the fragment tree, or `None` if the document has no root element —
/// which an empty parse produces and which is not a layout failure.
///
/// The viewport's block size is deliberately not an input. `height: auto` on the
/// root resolves from content, and a viewport height would only matter for
/// percentage heights and viewport units, neither of which this phase resolves.
/// Taking a parameter it did not use would suggest otherwise.
#[must_use]
pub fn layout_document(
    arena: &Arena,
    root: &px_css::view::StyleRoot,
    viewport_inline_size: Au,
) -> Option<FragmentTree> {
    let mut tree = FragmentTree::new();

    // The initial containing block. One fragment, viewport-wide, whose block size
    // the exit pass fills in from its children.
    let icb = tree.push(Fragment::new(
        FragmentKind::Block,
        LogicalSize::new(viewport_inline_size, Au(0)),
    ));
    tree.set_root(icb);

    // Not the traversal's entry point -- the document node is, below -- but an
    // empty parse must still be distinguishable from a laid-out document, and
    // "has a root element" is the distinction the return type promises.
    first_element(arena)?;

    let mut tables = Tables::new(viewport_inline_size);

    // The ICB is entered as a block container whose content is the document's.
    // Starting from the document node rather than from the root element is what
    // lets `Step::Children` be the only place content is partitioned: a root
    // element that is not block-level is flattened through by the same rule that
    // flattens an inline span, rather than by a second copy of it in `Enter`.
    let mut stack = vec![
        Step::Exit { fragment: icb },
        Step::Children {
            fragment: icb,
            node: arena.document(),
        },
    ];

    px_css::view::with_dom(arena, root, |dom| {
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter { node, parent, slot } => {
                    let Some(style) = computed_style(dom, node) else {
                        continue;
                    };
                    // Only block-level nodes are scheduled as `Enter` steps --
                    // `partition_content` classifies with the same predicate and
                    // flattens through everything else. This is the belt to that
                    // braces: a box whose formatting context this phase does not
                    // implement (flex, grid, inline-block, table) generates no
                    // fragment, because laying it out as a block would be
                    // confidently wrong geometry rather than none. Its slot stays
                    // empty and `Exit` drops it.
                    if !is_block_level(&style) {
                        continue;
                    }

                    let containing = tables
                        .inline_sizes
                        .get(parent.index())
                        .copied()
                        .unwrap_or(Au(0));
                    let mut resolved = resolve_edges(&style, containing);
                    let side = float_side(&style);

                    // §10.3.5: a float with `width: auto` shrinks to fit rather
                    // than filling its containing block, which is why floats need
                    // intrinsic sizes at all. An in-flow block with `width: auto`
                    // fills, and getting these the wrong way round makes every
                    // float the full width of the page — indistinguishable from
                    // "floats are not implemented" in a rendering.
                    let content_inline = match side {
                        Some(_) if definite_inline_size(&style).is_none() => {
                            let available = containing - resolved.inline_surround();
                            let available = available.max(Au(0));
                            let intrinsic = intrinsic_inline_size(dom, arena, node);
                            // The intrinsic figures are border-box, and this box's
                            // own surround is already counted in `available`.
                            let surround =
                                resolved.border.inline_sum() + resolved.padding.inline_sum();
                            shrink_to_fit(
                                Intrinsic {
                                    min: (intrinsic.min - surround).max(Au(0)),
                                    max: (intrinsic.max - surround).max(Au(0)),
                                },
                                available,
                            )
                        }
                        _ => resolve_inline_size(&style, containing, &resolved),
                    };

                    // §9.5.1 rule 9: `auto` margins on a float are zero, not
                    // centring. A float is shifted to an edge; there is nothing
                    // for the leftover space to be shared between.
                    if side.is_none() {
                        centre_if_auto_margins(&style, containing, content_inline, &mut resolved);
                    }

                    let fragment = tree.push(Fragment::new(
                        FragmentKind::Block,
                        LogicalSize::new(
                            content_inline
                                + resolved.border.inline_sum()
                                + resolved.padding.inline_sum(),
                            Au(0),
                        ),
                    ));

                    tables.grow_to(fragment.index());
                    tables.inline_sizes[fragment.index()] = content_inline;
                    tables.edges[fragment.index()] = resolved;
                    tables.float_sides[fragment.index()] = side;
                    tables.clear_sides[fragment.index()] = clear_side(&style);
                    tables.formatting_context_roots[fragment.index()] =
                        establishes_formatting_context(&style);
                    tables.specified_block_sizes[fragment.index()] = resolve_block_size(&style);
                    if let Some(reserved) = tables
                        .pending
                        .get_mut(parent.index())
                        .and_then(|kids| kids.get_mut(slot))
                    {
                        *reserved = Some(fragment);
                    }

                    // Popped in the reverse order: `Children` schedules the
                    // subtree, and `Exit` runs once all of it has.
                    stack.push(Step::Exit { fragment });
                    stack.push(Step::Children { fragment, node });
                }

                Step::Children { fragment, node } => {
                    let items = partition_content(dom, arena, node);
                    // Only *in-flow* block-level content forces anonymous boxes
                    // (§9.2.1.1); a float is block-level and does not.
                    let has_block = items.iter().any(|i| matches!(i, Content::Block(_)));
                    let available = tables
                        .inline_sizes
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(Au(0));
                    // An anonymous box inherits from its enclosing non-anonymous
                    // box (§9.2.1.1), and the container's own style is what the
                    // inline content is measured with in either branch below.
                    // Flattening through inline elements loses their fonts, which
                    // is a limitation of `crate::text`'s single-metric stub rather
                    // than of this partition.
                    let style = computed_style(dom, node);
                    let font_size = style.as_ref().map_or_else(
                        || crate::geom::px(16),
                        |s| Au::from(s.clone_font_size().computed_size()),
                    );
                    // The fallback is `normal` computed by hand, because the
                    // document node has no computed style and is the container
                    // the ICB partitions. Reaching it means the markup put text
                    // outside `<html>`, which the tree builder normally moves in.
                    let line_height = style
                        .as_ref()
                        .map_or(font_size * 6 / 5, |s| crate::inline::line_height_of(s));

                    // A slot per content item, always -- floats included, and
                    // whether or not anonymous boxes are generated. `Exit` walks
                    // the slots in order, so this is what keeps a float placed
                    // before the text that flows past it and after the text that
                    // does not.
                    tables.grow_to(fragment.index());
                    tables.pending[fragment.index()] = vec![None; items.len()];

                    for (slot, item) in items.into_iter().enumerate() {
                        match item {
                            Content::Block(child) | Content::Float(child) => {
                                stack.push(Step::Enter {
                                    node: child,
                                    parent: fragment,
                                    slot,
                                });
                            }
                            Content::InlineRun(text) if !has_block => {
                                // §9.2.1.1: inline content with no in-flow
                                // block-level sibling is not wrapped. The lines
                                // hang directly off this container, and the slot
                                // stays empty -- `PendingRun::slot` is what tells
                                // `Exit` where in the sequence to build them.
                                tables.inline_runs[fragment.index()] = Some(PendingRun {
                                    text,
                                    font_size,
                                    line_height,
                                    slot,
                                });
                            }
                            Content::InlineRun(text) => {
                                let anonymous = tree.push(Fragment::new(
                                    FragmentKind::AnonymousBlock,
                                    LogicalSize::new(available, Au(0)),
                                ));
                                tables.grow_to(anonymous.index());
                                tables.inline_sizes[anonymous.index()] = available;
                                if let Some(reserved) = tables
                                    .pending
                                    .get_mut(fragment.index())
                                    .and_then(|kids| kids.get_mut(slot))
                                {
                                    *reserved = Some(anonymous);
                                }
                                tables.inline_runs[anonymous.index()] = Some(PendingRun {
                                    text,
                                    font_size,
                                    line_height,
                                    slot,
                                });
                                // No `Children` step, because an anonymous block
                                // has no children beyond that run -- and no `Exit`
                                // either. Its container lays the run out when the
                                // slot comes round, because that is the only point
                                // at which the floats the lines must avoid have
                                // been placed.
                            }
                        }
                    }
                }

                Step::Exit { fragment } => {
                    // The content slots reserved when this container was
                    // partitioned, walked in document order. Three things happen
                    // in this loop and the order between them is the whole point:
                    // floats are placed, inline runs are broken into lines around
                    // the floats already placed, and in-flow boxes are stacked.
                    let slots = tables
                        .pending
                        .get(fragment.index())
                        .cloned()
                        .unwrap_or_default();

                    let available = tables
                        .inline_sizes
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(Au(0));
                    let surround = tables
                        .edges
                        .get(fragment.index())
                        .map_or(Au(0), Edges::block_surround);
                    // The content-box origin, relative to this box's own *border*
                    // box. Its margin is deliberately not included: a margin is
                    // outside the border box, and this box's own offset already
                    // accounts for it. Adding it here counted it twice, which is
                    // what a review found -- a child of a 30px-margin parent
                    // inside an 8px-margin body came out at 35px, which is neither
                    // absolute (43) nor parent-relative (5).
                    let content_inline_start = tables
                        .edges
                        .get(fragment.index())
                        .map_or(Au(0), |e| e.border.inline_start + e.padding.inline_start);
                    // The block axis needs the same origin and once did not have
                    // it at all: children were placed at the border-box top,
                    // inside this box's own top padding, while the inline axis did
                    // include padding. The two axes disagreed.
                    let content_block_start = tables
                        .edges
                        .get(fragment.index())
                        .map_or(Au(0), |e| e.border.block_start + e.padding.block_start);

                    // This container's floats, in its content box's coordinates.
                    // A float in an *ancestor* context does not reach here, which
                    // is the named gap in `crate::float`'s module documentation.
                    let mut floats = crate::float::FloatContext::new();
                    let mut own_run = tables
                        .inline_runs
                        .get_mut(fragment.index())
                        .and_then(Option::take);

                    let mut kids: Vec<FragmentId> = Vec::new();
                    let mut cursor = Au(0);
                    // The margin left over from the previous sibling's bottom
                    // edge, waiting to collapse with the next one's top.
                    let mut pending_margin = Au(0);
                    let mut first_in_flow = true;

                    for slot in 0..=slots.len() {
                        // This container's own inline content, at its position in
                        // the sequence rather than always first -- so a float
                        // before the text narrows it and a float after it does
                        // not. The `..=` above is what lets a run that comes after
                        // every other item still be reached.
                        if let Some(run) = own_run.take_if(|run| run.slot == slot) {
                            let height = place_run(
                                &mut tree,
                                &mut kids,
                                &run,
                                &floats,
                                available,
                                cursor,
                                content_inline_start,
                                content_block_start,
                            );
                            cursor += height;
                        }

                        let Some(Some(kid)) = slots.get(slot).copied() else {
                            continue;
                        };

                        // A float is out of flow: it is positioned by the float
                        // context and does not move the cursor, so the in-flow
                        // content after it sits where it would have been anyway
                        // and only the *lines* flow around it (§9.5).
                        if let Some(side) = tables.float_sides.get(kid.index()).copied().flatten() {
                            let margin = tables
                                .edges
                                .get(kid.index())
                                .map_or(LogicalEdges::ZERO, |e| e.margin);
                            let border_box = tree.get(kid).map_or(LogicalSize::ZERO, |f| f.size);
                            let margin_box = LogicalSize::new(
                                border_box.inline + margin.inline_sum(),
                                border_box.block + margin.block_sum(),
                            );
                            // §9.5.2: `clear` on a float raises the floor the
                            // placement search starts from, rather than moving the
                            // float afterwards -- the float still has to fit
                            // beside whatever is at the new position.
                            let floor = match tables.clear_sides.get(kid.index()).copied().flatten()
                            {
                                Some(clear) => cursor.max(floats.clearance(clear)),
                                None => cursor,
                            };
                            let placed = floats.place(side, margin_box, floor, available);
                            if let Some(float_fragment) = tree.get_mut(kid) {
                                float_fragment.inline_offset = content_inline_start
                                    + placed.inline_start
                                    + margin.inline_start;
                                float_fragment.block_offset =
                                    content_block_start + placed.block_start + margin.block_start;
                            }
                            kids.push(kid);
                            continue;
                        }

                        // An anonymous block box, whose run is laid out here
                        // rather than at a visit of its own -- this is the only
                        // point at which the floats its lines must avoid have
                        // been placed.
                        if let Some(run) = tables
                            .inline_runs
                            .get_mut(kid.index())
                            .and_then(Option::take)
                        {
                            let mut wrapped = Vec::new();
                            let height = place_run(
                                &mut tree,
                                &mut wrapped,
                                &run,
                                &floats,
                                available,
                                Au(0),
                                Au(0),
                                Au(0),
                            );
                            tree.set_children(kid, &wrapped);
                            if let Some(anonymous) = tree.get_mut(kid) {
                                anonymous.size.block = height;
                            }
                        }

                        let kid_margin = tables
                            .edges
                            .get(kid.index())
                            .map_or(LogicalEdges::ZERO, |e| e.margin);

                        // Margin collapsing between siblings, per CSS 2.1 §8.3.1.
                        // Two adjoining vertical margins collapse into one whose
                        // size is the larger of the two -- or, when they have
                        // opposite signs, the sum of the most positive and the
                        // most negative. "Adjoining" here means only
                        // sibling-to-sibling: collapsing *through* a parent, which
                        // happens when no border or padding separates a parent
                        // from its first or last child, is not implemented and is
                        // still a named gap.
                        //
                        // The first in-flow child's top margin has nothing to
                        // collapse against here, because collapsing it with this
                        // box's own is that unimplemented case.
                        cursor += if first_in_flow {
                            kid_margin.block_start
                        } else {
                            collapse(pending_margin, kid_margin.block_start)
                        };
                        first_in_flow = false;

                        // §9.5.2: clearance is introduced *after* the margin, and
                        // pushes the box down rather than replacing where it was
                        // going. A box that is already below the floats it clears
                        // does not move at all, which is what the `max` says.
                        if let Some(clear) = tables.clear_sides.get(kid.index()).copied().flatten()
                        {
                            cursor = cursor.max(floats.clearance(clear));
                        }

                        if let Some(kid_fragment) = tree.get_mut(kid) {
                            // Relative to this box's border box. A final pass
                            // converts the whole tree to absolute once every
                            // parent's own offset is known -- which it is not
                            // here, because a parent is positioned by *its*
                            // parent's exit visit, which happens later.
                            kid_fragment.block_offset = content_block_start + cursor;
                            kid_fragment.inline_offset =
                                content_inline_start + kid_margin.inline_start;
                            cursor += kid_fragment.size.block;
                        }
                        pending_margin = kid_margin.block_end;
                        kids.push(kid);
                    }
                    // The last child's bottom margin is inside this box's content
                    // height. Collapsing it out through the parent is the case
                    // above that is not implemented.
                    cursor += pending_margin;

                    // §10.6.7: a box that establishes a block formatting context
                    // and has `height: auto` stretches to contain its own floats.
                    // A box that does *not* establish one leaves them to overflow,
                    // which looks like a bug in every rendering and is the rule --
                    // it is why `display: flow-root` exists, and why the clearfix
                    // hack existed before it did.
                    if tables
                        .formatting_context_roots
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(false)
                    {
                        cursor = cursor.max(floats.lowest_edge(None));
                    }

                    tree.set_children(fragment, &kids);
                    let specified = tables
                        .specified_block_sizes
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(None);
                    if let Some(f) = tree.get_mut(fragment) {
                        // A specified `height` wins; `auto` takes the content's
                        // block size (§10.6.3). Either way the box's own border
                        // and padding are added, because the fragment records a
                        // border box.
                        //
                        let content = specified.unwrap_or(cursor);
                        f.size.block = content + surround;
                    }
                }
            }
        }
    })?;

    make_offsets_absolute(&mut tree);
    Some(tree)
}

/// Turn parent-relative offsets into offsets from the fragment tree's origin.
///
/// Layout places a child relative to its parent's border box, because a parent's
/// own position is not known until *its* parent's exit visit, which happens after
/// the child has been placed. One top-down pass afterwards resolves that, and it
/// is the pass that makes [`Fragment`]'s documented "offset from the fragment
/// tree's origin" true.
///
/// Iterative, like every other walk here.
fn make_offsets_absolute(tree: &mut FragmentTree) {
    let Some(root) = tree.root() else { return };
    let mut stack = vec![(root, Au(0), Au(0))];

    while let Some((id, base_inline, base_block)) = stack.pop() {
        let Some(fragment) = tree.get_mut(id) else {
            continue;
        };
        fragment.inline_offset += base_inline;
        fragment.block_offset += base_block;
        let (inline, block) = (fragment.inline_offset, fragment.block_offset);

        for kid in tree.children(id).to_vec() {
            stack.push((kid, inline, block));
        }
    }
}

/// The first element in the document, which is `<html>` for a parsed page.
fn first_element(arena: &Arena) -> Option<NodeId> {
    let document = arena.document();
    core::iter::once(document)
        .chain(arena.descendants(document))
        .find(|id| arena.get(*id).is_some_and(|n| n.element_name().is_some()))
}

/// Lay `run` out into line fragments, position them, and return their height.
///
/// The lines are appended to `into` and offset into the container's content box
/// by `content_inline_start` / `content_block_start`, starting at `block_origin`
/// along the block axis. `floats` is the container's float context, which is what
/// shortens them (§9.5).
///
/// Called from `Exit`, once per inline run, at the point in the container's
/// content sequence where that run sits — see [`PendingRun`] for why it is not
/// called on the way down, and the `Exit` arm for why it is not called all at
/// once.
#[expect(
    clippy::too_many_arguments,
    reason = "every argument is a distinct piece of the container's geometry; \
              bundling them into a struct used at exactly two call sites would \
              name the same values twice"
)]
fn place_run(
    tree: &mut FragmentTree,
    into: &mut Vec<FragmentId>,
    run: &PendingRun,
    floats: &crate::float::FloatContext,
    available: Au,
    block_origin: Au,
    content_inline_start: Au,
    content_block_start: Au,
) -> Au {
    let context = crate::inline::InlineContext {
        text: &run.text,
        font_size: run.font_size,
        line_height: run.line_height,
        available,
        floats: if floats.is_empty() {
            None
        } else {
            Some(floats)
        },
        block_origin,
    };
    let (lines, height) = crate::inline::layout_lines(tree, &context);

    for line in &lines {
        // Shifted into the content box rather than repositioned: `layout_lines`
        // has already placed each line against the floats and stacked it, and
        // moving it again is the double-height bug an earlier version had, where
        // every text block came out exactly twice as tall with its single line
        // sitting one line-height below the top.
        if let Some(fragment) = tree.get_mut(*line) {
            fragment.inline_offset += content_inline_start;
            fragment.block_offset += content_block_start;
        }
    }
    into.extend_from_slice(&lines);
    height
}

/// Which floats this box must clear, if any (§9.5.2).
fn clear_side(style: &ComputedValues) -> Option<crate::float::ClearSide> {
    use style::computed_values::clear::T as Clear;
    match style.clone_clear() {
        // The logical spellings collapse onto the physical ones for the same
        // reason they do in `float_side`: this phase's inline axis is
        // left-to-right everywhere.
        Clear::Left | Clear::InlineStart => Some(crate::float::ClearSide::Start),
        Clear::Right | Clear::InlineEnd => Some(crate::float::ClearSide::End),
        Clear::Both => Some(crate::float::ClearSide::Both),
        Clear::None => None,
    }
}

/// Which edge this box floats to, if it floats at all (§9.5).
fn float_side(style: &ComputedValues) -> Option<crate::float::FloatSide> {
    use style::computed_values::float::T as Float;
    match style.clone_float() {
        // The logical spellings map straight across because this phase treats the
        // inline axis as left-to-right everywhere -- `crate::geom`'s whole
        // vocabulary does. Under `direction: rtl` `left` and `inline-start` are
        // opposite edges and this is wrong for both; bidi is Phase 9's, and
        // collapsing them here is the same simplification the rest of the crate
        // already makes rather than a new one.
        Float::Left | Float::InlineStart => Some(crate::float::FloatSide::Start),
        Float::Right | Float::InlineEnd => Some(crate::float::FloatSide::End),
        Float::None => None,
    }
}

/// Partition `node`'s content into runs of inline content and block-level boxes.
///
/// CSS 2.1 §9.2.1.1, and the one walk that used to be two: an earlier version
/// collected *all* of a container's inline text into a single run laid out before
/// *all* of its block children, so `<div>one<p>two</p>three</div>` laid out
/// "onethree" above the paragraph. Document order was lost, and no anonymous box
/// was generated because nothing recorded that the content was mixed.
///
/// Flattens through non-block-level elements: an inline `<span>`'s text belongs to
/// the run it sits in, and a block-level element *inside* that span ends the run
/// and becomes an item of its own. That is the block-in-inline case, and it is
/// where most of the vendored CSS2 reftests that exercise anonymous boxes live.
///
/// Iterative, and a pre-order walk so the items come out in document order.
fn partition_content(dom: px_css::view::Dom<'_>, arena: &Arena, node: NodeId) -> Vec<Content> {
    let mut out = Vec::new();
    let mut run = String::new();
    let mut stack: Vec<NodeId> = children_in_order(arena, node);
    stack.reverse();

    while let Some(id) = stack.pop() {
        let Some(current) = arena.get(id) else {
            continue;
        };

        if current.element_name().is_some() {
            let style = computed_style(dom, id);
            // `display: none` generates no box and no *content* -- neither the
            // element nor its descendants (§9.2.4). Flattening through it the way
            // an inline element is flattened through put the UA sheet's
            // `title { display: none }` text into an anonymous block at the top of
            // every document in the reftest corpus, which is how this was found:
            // a test whose title was longer than its reference's laid out a wider
            // first line and failed on a string neither document renders.
            if style.as_ref().is_some_and(|s| is_display_none(s)) {
                continue;
            }
            // A float is block-level (stylo blockifies it, §9.7) but out of
            // flow, so it neither ends the run nor forces an anonymous box.
            if let Some(style) = style.as_ref().filter(|s| float_side(s).is_some()) {
                let _ = style;
                out.push(Content::Float(id));
                continue;
            }
            if style.is_some_and(|s| is_block_level(&s)) {
                flush_run(&mut run, &mut out);
                out.push(Content::Block(id));
            } else {
                let mut kids = children_in_order(arena, id);
                kids.reverse();
                stack.extend(kids);
            }
            continue;
        }

        if let Some(text) = current.text() {
            run.push_str(text);
        }
    }
    flush_run(&mut run, &mut out);
    out
}

/// End the run being accumulated, dropping it if it collapses to nothing.
///
/// Whitespace is collapsed here rather than at the point of use so that a run of
/// only whitespace between two block boxes -- the newline in
/// `<div><p>a</p>\n<p>b</p></div>`, which every hand-written document has -- does
/// not generate an empty anonymous block between them. §9.2.1.1 says as much:
/// white space that would collapse away generates no anonymous box.
fn flush_run(run: &mut String, out: &mut Vec<Content>) {
    let collapsed = crate::inline::collapse_whitespace(run);
    run.clear();
    if !collapsed.trim().is_empty() {
        out.push(Content::InlineRun(collapsed));
    }
}

/// Every child of `node`, elements and text alike, in document order.
fn children_in_order(arena: &Arena, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(parent) = arena.get(node) else {
        return out;
    };
    let mut next = parent.first_child();
    while let Some(id) = next {
        let Some(child) = arena.get(id) else { break };
        out.push(id);
        next = child.next_sibling();
    }
    out
}

/// A box's intrinsic inline sizes: CSS 2.1 §10.3.5's two "preferred" widths.
///
/// - `max` is the **preferred width**: the width the box would take if nothing
///   ever wrapped. For text, the whole run on one line.
/// - `min` is the **preferred minimum width**: the narrowest the box can be
///   without its content overflowing. For text, the widest single word, because
///   §9.4.2 says a word wider than its line overflows rather than breaking.
///
/// Both are border-box sizes — margins, borders and padding included — because
/// that is what shrink-to-fit compares against the available width.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Intrinsic {
    min: Au,
    max: Au,
}

impl Intrinsic {
    const ZERO: Self = Self {
        min: Au(0),
        max: Au(0),
    };
}

/// A block container part-way through having its intrinsic sizes computed.
struct IntrinsicFrame {
    /// Everything outside the content box along the inline axis.
    surround: Au,
    /// The widest contribution seen so far, from text or from a child.
    min: Au,
    max: Au,
}

/// One visit in the intrinsic walk.
enum IntrinsicStep {
    /// Measure this box's own text and schedule its block-level children.
    Descend(NodeId),
    /// Every child has contributed; fold the frame into its parent.
    Combine,
}

/// CSS 2.1 §10.3.5: shrink-to-fit.
///
/// > `min(max(preferred minimum width, available width), preferred width)`
///
/// Used for a float with `width: auto`, which is the only box in this phase that
/// gets it — absolutely positioned boxes and inline-blocks are the other two and
/// neither is implemented.
fn shrink_to_fit(intrinsic: Intrinsic, available: Au) -> Au {
    let lower = if intrinsic.min > available {
        intrinsic.min
    } else {
        available
    };
    if lower < intrinsic.max {
        lower
    } else {
        intrinsic.max
    }
}

/// The intrinsic inline sizes of the box `node` generates.
///
/// A block container's children do not *sum* along the inline axis — they stack —
/// so both figures are the widest child's rather than the total. That is the one
/// thing about this algorithm that surprises people coming from inline layout,
/// where the opposite is true.
///
/// Percentages resolve against zero. CSS 2.1 leaves intrinsic sizing undefined,
/// and treating a percentage width or margin as zero is what CSS-SIZING-3 §5.2
/// settled on: the alternative is a circularity, because the containing block's
/// width is what is being computed.
///
/// Iterative, like every other walk in this crate, over an explicit stack of
/// [`IntrinsicStep`]s. A frame is pushed when a container is descended into and
/// popped when its children have all contributed, so `frames.last_mut()` is
/// always the parent of whatever just finished.
fn intrinsic_inline_size(dom: px_css::view::Dom<'_>, arena: &Arena, root: NodeId) -> Intrinsic {
    let mut frames: Vec<IntrinsicFrame> = Vec::new();
    let mut stack = vec![IntrinsicStep::Descend(root)];
    let mut result = Intrinsic::ZERO;

    /// Fold a finished box's sizes into its parent, or into the result.
    fn contribute(frames: &mut [IntrinsicFrame], result: &mut Intrinsic, value: Intrinsic) {
        let target = match frames.last_mut() {
            Some(frame) => {
                frame.min = frame.min.max(value.min);
                frame.max = frame.max.max(value.max);
                return;
            }
            None => result,
        };
        *target = value;
    }

    while let Some(step) = stack.pop() {
        match step {
            IntrinsicStep::Descend(node) => {
                let Some(style) = computed_style(dom, node) else {
                    contribute(&mut frames, &mut result, Intrinsic::ZERO);
                    continue;
                };
                if is_display_none(&style) {
                    contribute(&mut frames, &mut result, Intrinsic::ZERO);
                    continue;
                }

                let edges = resolve_edges(&style, Au(0));
                let surround = edges.inline_surround();

                // A definite width makes both figures the same and stops the
                // walk: nothing inside can widen a box whose width is stated.
                if let Some(width) = definite_inline_size(&style) {
                    let total = width + surround;
                    contribute(
                        &mut frames,
                        &mut result,
                        Intrinsic {
                            min: total,
                            max: total,
                        },
                    );
                    continue;
                }

                let font_size = Au::from(style.clone_font_size().computed_size());
                let mut frame = IntrinsicFrame {
                    surround,
                    min: Au(0),
                    max: Au(0),
                };
                let mut blocks = Vec::new();
                for item in partition_content(dom, arena, node) {
                    match item {
                        // A float contributes to its container's preferred
                        // *minimum* width -- content cannot be narrower than a
                        // float it contains -- but not to its preferred width,
                        // because text flows beside a float rather than after it.
                        // Both are approximated by the block rule here, which
                        // overstates the preferred width of a container whose only
                        // wide thing is a float.
                        Content::Block(child) | Content::Float(child) => blocks.push(child),
                        Content::InlineRun(text) => {
                            frame.max = frame.max.max(crate::text::measure(&text, font_size));
                            frame.min = frame.min.max(widest_word(&text, font_size));
                        }
                    }
                }

                frames.push(frame);
                stack.push(IntrinsicStep::Combine);
                for child in blocks.into_iter().rev() {
                    stack.push(IntrinsicStep::Descend(child));
                }
            }

            IntrinsicStep::Combine => {
                let Some(frame) = frames.pop() else { continue };
                let value = Intrinsic {
                    min: frame.min + frame.surround,
                    max: frame.max + frame.surround,
                };
                contribute(&mut frames, &mut result, value);
            }
        }
    }
    result
}

/// The width of the widest word in `text`.
///
/// The preferred minimum width of a run of text. Trailing spaces are excluded
/// because §16.6.1 removes them at a break, so they never contribute to how narrow
/// a line can be.
fn widest_word(text: &str, font_size: Au) -> Au {
    crate::text::words(text, font_size)
        .into_iter()
        .map(|(word, _)| crate::text::width_without_trailing_spaces(word, font_size))
        .max()
        .unwrap_or(Au(0))
}

/// The used `width`, if it is a length this phase can resolve without a context.
///
/// `auto` and percentages both return `None`, which the intrinsic walk reads as
/// "look inside". They mean different things in general and the same thing here.
fn definite_inline_size(style: &ComputedValues) -> Option<Au> {
    use style::values::computed::Size;
    match style.clone_width() {
        Size::LengthPercentage(ref lp) => lp.0.maybe_to_used_value(None),
        _ => None,
    }
}

/// The computed style of `node`, if it is a styled element.
fn computed_style(
    dom: px_css::view::Dom<'_>,
    node: NodeId,
) -> Option<style::servo_arc::Arc<ComputedValues>> {
    let view = dom.node(node)?;
    let element = px_css::dom::StyleElement::new(view)?;
    let data = style::dom::TElement::borrow_data(&element)?;
    Some(data.styles.primary().clone())
}

/// Whether this element generates no box at all.
///
/// Distinct from "not block-level", which this phase also declines to lay out:
/// an unimplemented formatting context still has *content*, and its text belongs
/// to the inline run it sits in. `display: none` has none, and the difference is
/// the whole of §9.2.4.
fn is_display_none(style: &ComputedValues) -> bool {
    use style::values::specified::box_::{DisplayInside, DisplayOutside};
    let display = style.clone_display();
    // Both halves, because `display: contents` is also outside `none` and is the
    // opposite instruction: it generates no box *and keeps its children*, which is
    // what flattening through a non-block-level element already does for it.
    matches!(display.outside(), DisplayOutside::None)
        && matches!(display.inside(), DisplayInside::None)
}

/// Whether this box participates in block layout: a **block container** whose
/// outer role is block-level.
///
/// `display` is two independent halves (CSS Display 3 §2.1, and stylo stores it
/// that way): the *outside* role says how the box behaves in its parent, and the
/// *inside* type says what layout it runs for its children. Block layout is
/// outside `block` and inside `flow` or `flow-root` — and nothing else, because
/// anything else is a formatting context this phase does not implement, and
/// laying one out as a block would be confidently wrong geometry rather than
/// none.
///
/// Reading the two halves rather than comparing against `Display::Block` is what
/// admits `display: flow-root`. That matters more than it sounds: a `flow-root` is
/// a block container in every respect and differs from a plain block only in
/// establishing a formatting context, so treating it as unimplemented threw away
/// the box entirely — which in the reftest corpus meant a
/// `<div style="width: 150px; display: flow-root">` full of floats vanished and
/// its floats were placed against the body's 784px instead. It is also the only
/// spelling `Display::FlowRoot` has here, because stylo puts that constant behind
/// a Gecko feature this build does not enable.
///
/// The content of a box that is *not* block-level is still visited, flattened into
/// the run around it by [`partition_content`]; only `display: none` is dropped.
fn is_block_level(style: &ComputedValues) -> bool {
    use style::values::specified::box_::{DisplayInside, DisplayOutside};
    let display = style.clone_display();
    matches!(display.outside(), DisplayOutside::Block)
        && matches!(
            display.inside(),
            DisplayInside::Flow | DisplayInside::FlowRoot
        )
}

/// Whether this box establishes a block formatting context (§9.4.1).
///
/// A BFC root contains its own floats (§10.6.7) and its own margin collapsing.
/// `display: flow-root` exists to ask for exactly this and nothing else, which is
/// why it is the clearest case: inside `flow-root`, outside anything.
///
/// Floats and absolutely positioned boxes are BFC roots too, and so is anything
/// with `overflow` other than `visible`. The first is here; the other two are not,
/// and are named gaps rather than silent ones.
fn establishes_formatting_context(style: &ComputedValues) -> bool {
    use style::values::specified::box_::DisplayInside;
    matches!(style.clone_display().inside(), DisplayInside::FlowRoot) || float_side(style).is_some()
}

/// Resolve margins, borders and padding against the containing block's inline/// Resolve margins, borders and padding against the containing block's inline
/// size.
///
/// Percentages on *all* of these resolve against the containing block's **inline**
/// size, including the block-axis ones — §8.3 and §8.4, and it is the rule that
/// surprises people: `margin-top: 10%` is ten percent of the *width*.
fn resolve_edges(style: &ComputedValues, containing_inline: Au) -> Edges {
    let margin = style.get_margin();
    let padding = style.get_padding();
    let border = style.get_border();

    Edges {
        margin: LogicalEdges {
            block_start: used_margin(&margin.margin_top, containing_inline),
            block_end: used_margin(&margin.margin_bottom, containing_inline),
            inline_start: used_margin(&margin.margin_left, containing_inline),
            inline_end: used_margin(&margin.margin_right, containing_inline),
        },
        // `border-*-width` computes to zero whenever the corresponding
        // `border-*-style` is `none` or `hidden` (CSS 2.1 §8.5.1), and **the
        // style struct does not apply that for you**.
        //
        // Phase 5's property sweep saw `border-top-width: 9px` serialise as
        // "0px" with no style set, and I concluded from that stylo folds the rule
        // in at computed-value time. It does not: the folding happens on the way
        // out, in `computed_value_to_string`. Reading `border_top_width` directly
        // gives `medium` — 3px — for every element that never mentioned a border.
        //
        // The symptom was every box in the tree being 6px narrower than its
        // containing block and offset 3px into it, compounding once per level. It
        // looked like an inset, which is exactly what it was.
        border: LogicalEdges {
            block_start: used_border(&border.border_top_width, border.border_top_style),
            block_end: used_border(&border.border_bottom_width, border.border_bottom_style),
            inline_start: used_border(&border.border_left_width, border.border_left_style),
            inline_end: used_border(&border.border_right_width, border.border_right_style),
        },
        padding: LogicalEdges {
            block_start: padding.padding_top.to_used_value(containing_inline),
            block_end: padding.padding_bottom.to_used_value(containing_inline),
            inline_start: padding.padding_left.to_used_value(containing_inline),
            inline_end: padding.padding_right.to_used_value(containing_inline),
        },
    }
}

/// Collapse two adjoining vertical margins into one, per CSS 2.1 §8.3.1.
///
/// The larger of the two when both have the same sign; the sum of the most
/// positive and the most negative when they differ. Two negative margins collapse
/// to the more negative, which the "maximum of the absolute values" phrasing gets
/// wrong and which `min` here gets right.
fn collapse(a: Au, b: Au) -> Au {
    if a >= Au(0) && b >= Au(0) {
        if a > b { a } else { b }
    } else if a <= Au(0) && b <= Au(0) {
        if a < b { a } else { b }
    } else {
        a + b
    }
}

/// A border width, after CSS 2.1 §8.5.1's rule about `none` and `hidden`.
///
/// Both keywords force the used width to zero regardless of what `border-width`
/// says. The style struct keeps the two independent, so this is the consumer's
/// job — and getting it wrong insets every box by 3px per side, per level.
fn used_border(
    width: &style::values::computed::BorderSideWidth,
    border_style: style::values::computed::BorderStyle,
) -> Au {
    use style::values::computed::BorderStyle;
    match border_style {
        BorderStyle::None | BorderStyle::Hidden => Au(0),
        _ => width.0,
    }
}

/// The specified block size, or `None` for `auto`.
///
/// A percentage height resolves against the *containing block's* height, which is
/// itself usually `auto` and therefore not yet known — §10.5 makes that case
/// compute to `auto` too. Treated as `auto` here, which is right for the common
/// case and wrong when an ancestor has a definite height. Named rather than
/// silently approximated; it wants the containing block's resolved height threaded
/// down, which is a change to the Enter pass rather than a patch here.
fn resolve_block_size(style: &ComputedValues) -> Option<Au> {
    use style::values::computed::Size;
    match style.clone_height() {
        // `maybe_to_used_value(None)` is exactly this function's semantics: a
        // definite length converts, and a percentage — which has no basis while
        // the containing block's height is auto — returns None, which is the
        // `auto` this function already means. It also keeps floats out of
        // px-layout, which ci/gate-layout.sh enforces, where `Au::from_f32_px`
        // would not.
        Size::LengthPercentage(ref lp) => lp.0.maybe_to_used_value(None),
        _ => None,
    }
}

/// One margin's used value, with `auto` resolving to zero here.
///
/// `auto` is *not* zero in general — §10.3.3 makes two auto inline margins split
/// the leftover space, which is how a fixed-width block centres. That is resolved
/// in [`centre_if_auto_margins`], after the inline size is known, because the
/// leftover space is what it divides and that does not exist yet at this point.
/// Zero is the right answer for block-axis margins, where `auto` computes to zero
/// (§10.6.3).
fn used_margin(margin: &style::values::computed::Margin, containing: Au) -> Au {
    use style::values::generics::length::GenericMargin;
    match margin {
        GenericMargin::LengthPercentage(lp) => lp.to_used_value(containing),
        GenericMargin::Auto => Au(0),
        // `anchor-size()` and friends need an anchor this phase has none of.
        _ => Au(0),
    }
}

/// Split the leftover inline space between `auto` margins — §10.3.3's centring.
///
/// Only applies when the width is *not* `auto`. With `width: auto` the box already
/// fills the containing block, so §10.3.3 sets any auto margin to zero rather than
/// leaving space for it to take — which is why `margin: 0 auto` on a full-width
/// block does nothing, a rule that reliably surprises people.
fn centre_if_auto_margins(
    style: &ComputedValues,
    containing: Au,
    content_inline: Au,
    edges: &mut Edges,
) {
    use style::values::computed::Size;
    use style::values::generics::length::GenericMargin;

    if matches!(style.clone_width(), Size::Auto) {
        return;
    }

    let margin = style.get_margin();
    let start_auto = matches!(margin.margin_left, GenericMargin::Auto);
    let end_auto = matches!(margin.margin_right, GenericMargin::Auto);
    if !start_auto && !end_auto {
        return;
    }

    let used = content_inline + edges.border.inline_sum() + edges.padding.inline_sum();
    let leftover = containing - used - edges.margin.inline_sum();
    if leftover <= Au(0) {
        return;
    }

    if start_auto && end_auto {
        // Halved in app units, so the result is exact rather than rounded twice.
        let half = leftover / 2;
        edges.margin.inline_start = half;
        edges.margin.inline_end = leftover - half;
    } else if start_auto {
        edges.margin.inline_start = leftover;
    } else {
        edges.margin.inline_end = leftover;
    }
}

/// The content-box inline size, per CSS 2.1 §10.3.3.
///
/// `width: auto` fills what the containing block leaves after margins, borders
/// and padding. A specified width is used as given, and the box is allowed to
/// overflow — §10.3.3's over-constrained case adjusts `margin-right`, which
/// changes where siblings sit but not this box's width.
///
/// Saturating at zero rather than going negative: a content box narrower than
/// nothing is not a thing, and a negative `Au` here would propagate into every
/// descendant's containing block.
fn resolve_inline_size(style: &ComputedValues, containing: Au, edges: &Edges) -> Au {
    use style::values::computed::Size;

    let available = containing - edges.inline_surround();
    let available = if available < Au(0) { Au(0) } else { available };

    match style.clone_width() {
        Size::Auto => available,
        Size::LengthPercentage(ref lp) => {
            let used = lp.0.to_used_value(containing);
            if used < Au(0) { Au(0) } else { used }
        }
        // Intrinsic keywords (`min-content`, `max-content`, `fit-content`) need
        // intrinsic sizing, which needs inline layout. Treated as `auto` and
        // named here rather than silently falling through a `_` arm.
        _ => available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    /// Parse, style and lay out `html` with `css`, at an 800px viewport.
    fn layout(html: &str, css: &str) -> (FragmentTree, Arena) {
        let dom = px_dom::parse(html);
        assert!(!dom.abandoned, "the fixture must parse");
        let quirks = px_css::engine::quirks_mode_of(&dom);
        let arena = dom.arena;

        let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
        engine.add_author_stylesheet(css, "https://example.invalid/a.css");
        let style_root = engine.style_root_for(&arena);
        engine
            .resolve(&arena, &style_root)
            .expect("the document has a root element");

        let tree = layout_document(&arena, &style_root, px(800)).expect("a root element exists");
        (tree, arena)
    }

    /// Every fragment kind in the tree, in layout order.
    fn kinds(tree: &FragmentTree) -> Vec<FragmentKind> {
        tree.in_layout_order()
            .into_iter()
            .filter_map(|(_, id)| tree.get(id).map(|f| f.kind))
            .collect()
    }

    /// The block-axis offset and height of every fragment of `kind`.
    fn bands(tree: &FragmentTree, kind: FragmentKind) -> Vec<(Au, Au)> {
        tree.in_layout_order()
            .into_iter()
            .filter_map(|(_, id)| tree.get(id))
            .filter(|f| f.kind == kind)
            .map(|f| (f.block_offset, f.size.block))
            .collect()
    }

    const FLAT: &str = "html, body, div, p { display: block; margin: 0; padding: 0 } \
         * { font-size: 10px; line-height: 20px }";

    /// Every fragment's rectangle, in layout order.
    fn boxes(tree: &FragmentTree) -> Vec<(FragmentKind, Au, Au, Au, Au)> {
        tree.in_layout_order()
            .into_iter()
            .filter_map(|(_, id)| tree.get(id))
            .map(|f| {
                (
                    f.kind,
                    f.inline_offset,
                    f.block_offset,
                    f.size.inline,
                    f.size.block,
                )
            })
            .collect()
    }

    /// The rectangles of every fragment of `kind`.
    fn rects_of(tree: &FragmentTree, kind: FragmentKind) -> Vec<(Au, Au, Au, Au)> {
        boxes(tree)
            .into_iter()
            .filter(|b| b.0 == kind)
            .map(|b| (b.1, b.2, b.3, b.4))
            .collect()
    }

    /// §9.5: a float is out of flow, so the block after it does not move down.
    ///
    /// The defining property, and the one that is invisible in a rendering when it
    /// is wrong in the other direction — a float laid out in flow just looks like a
    /// block, which is exactly how it looked before this existed.
    #[test]
    fn block_a_float_does_not_push_the_next_block_down() {
        let (tree, _) = layout(
            "<html><body><div id=f></div><div id=b></div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             #f { float: left; width: 50px; height: 40px } \
             #b { height: 30px }",
        );
        let blocks = rects_of(&tree, FragmentKind::Block);
        assert!(
            blocks.contains(&(px(0), px(0), px(50), px(40))),
            "the float is at the top-start corner: {blocks:?}"
        );
        assert!(
            blocks.contains(&(px(0), px(0), px(800), px(30))),
            "the in-flow block starts at the top too, beneath the float: {blocks:?}"
        );
    }

    /// §9.5: line boxes shorten and shift to make room for a float.
    #[test]
    fn block_text_flows_beside_a_float() {
        let (tree, _) = layout(
            "<html><body><div><span id=f></span>aaaa bbbb cccc</div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             div { width: 100px; font-size: 10px; line-height: 20px } \
             #f { float: left; width: 40px; height: 20px }",
        );
        let lines = rects_of(&tree, FragmentKind::Line);
        assert!(!lines.is_empty(), "the text laid out at all");
        assert_eq!(
            lines[0].0,
            px(40),
            "the first line starts past the float: {lines:?}"
        );
        assert_eq!(
            lines[1].0,
            px(0),
            "the second line is below the float and starts at the edge: {lines:?}"
        );
    }

    /// A float does not force an anonymous block around the text it interrupts.
    ///
    /// §9.2.1.1 is about in-flow block-level boxes. Treating a float as one split
    /// `text<float>text` into two stacked anonymous blocks, which is the opposite
    /// of what a float is for.
    #[test]
    fn block_a_float_does_not_split_the_run_around_it() {
        let (tree, _) = layout(
            "<html><body><div>before<span id=f></span>after</div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             div { font-size: 10px; line-height: 20px } \
             #f { float: left; width: 10px; height: 10px }",
        );
        assert!(
            !boxes(&tree)
                .iter()
                .any(|b| b.0 == FragmentKind::AnonymousBlock),
            "a float is out of flow and generates no anonymous siblings: {:?}",
            boxes(&tree)
        );
        assert_eq!(
            rects_of(&tree, FragmentKind::Line).len(),
            1,
            "the text on both sides of the float is one run, so one line"
        );
    }

    /// §10.3.5: a float with `width: auto` shrinks to fit rather than filling.
    #[test]
    fn block_a_float_shrinks_to_fit_its_content() {
        let (tree, _) = layout(
            "<html><body><div id=f>abcd</div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             #f { float: left; font-size: 10px; line-height: 20px }",
        );
        let floats: Vec<_> = rects_of(&tree, FragmentKind::Block)
            .into_iter()
            .filter(|r| r.2 < px(800))
            .collect();
        // Four characters at 5px each. An in-flow block would be 800 wide.
        assert_eq!(
            floats,
            vec![(px(0), px(0), px(20), px(20))],
            "the float is as wide as its text, not as wide as the page"
        );
    }

    /// §9.5.2: `clear` pushes a box below the floats it names.
    #[test]
    fn block_clear_pushes_a_block_below_the_float() {
        let (tree, _) = layout(
            "<html><body><div id=f></div><div id=b></div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             #f { float: left; width: 50px; height: 40px } \
             #b { clear: left; height: 30px }",
        );
        assert!(
            rects_of(&tree, FragmentKind::Block).contains(&(px(0), px(40), px(800), px(30))),
            "the cleared block starts below the float's bottom edge: {:?}",
            rects_of(&tree, FragmentKind::Block)
        );
    }

    /// `clear` on the other side does not move the box.
    ///
    /// The control for the test above: a `clear` implemented as "always drop below
    /// every float" would pass that one and fail this.
    #[test]
    fn block_clear_on_the_other_side_does_nothing() {
        let (tree, _) = layout(
            "<html><body><div id=f></div><div id=b></div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             #f { float: left; width: 50px; height: 40px } \
             #b { clear: right; height: 30px }",
        );
        assert!(
            rects_of(&tree, FragmentKind::Block).contains(&(px(0), px(0), px(800), px(30))),
            "there are no end-side floats to clear: {:?}",
            rects_of(&tree, FragmentKind::Block)
        );
    }

    /// §10.6.7: a formatting context root stretches to contain its own floats.
    #[test]
    fn block_a_flow_root_contains_its_floats() {
        let (tree, _) = layout(
            "<html><body><div id=r><div id=f></div></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             #r { display: flow-root } \
             #f { float: left; width: 50px; height: 40px }",
        );
        assert!(
            rects_of(&tree, FragmentKind::Block).contains(&(px(0), px(0), px(800), px(40))),
            "the flow-root is as tall as the float inside it: {:?}",
            rects_of(&tree, FragmentKind::Block)
        );
    }

    /// A plain block container does *not* contain its floats.
    ///
    /// The rule that looks like a bug in every rendering, and the reason
    /// `display: flow-root` exists at all. Asserted because the tempting fix is to
    /// make every container contain its floats, which would break far more pages
    /// than it fixed.
    #[test]
    fn block_a_plain_block_does_not_contain_its_floats() {
        let (tree, _) = layout(
            "<html><body><div id=r><div id=f></div></div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0 } \
             #f { float: left; width: 50px; height: 40px }",
        );
        assert!(
            rects_of(&tree, FragmentKind::Block).contains(&(px(0), px(0), px(800), px(0))),
            "the container has no in-flow content, so it is zero-high: {:?}",
            rects_of(&tree, FragmentKind::Block)
        );
    }

    /// `display: flow-root` is a block container, not an unimplemented context.
    ///
    /// Found by the reftest corpus rather than reasoned about: a
    /// `<div style="width: 150px; display: flow-root">` was being thrown away
    /// whole, and the floats inside it were placed against the body's 784px.
    #[test]
    fn block_flow_root_lays_out_as_a_block_container() {
        let (tree, _) = layout(
            "<html><body><div id=r></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             #r { display: flow-root; width: 150px; height: 20px }",
        );
        assert!(
            rects_of(&tree, FragmentKind::Block).contains(&(px(0), px(0), px(150), px(20))),
            "flow-root generates a box of its own: {:?}",
            rects_of(&tree, FragmentKind::Block)
        );
    }

    /// §9.2.1.1: inline content with no block-level siblings needs no wrapper.
    ///
    /// The negative half of the rule, and the one a naive implementation gets
    /// wrong by wrapping unconditionally — which costs a level of nesting on every
    /// paragraph on the web and makes the reftest comparison disagree with every
    /// reference that spells the structure out.
    #[test]
    fn block_inline_only_content_generates_no_anonymous_box() {
        let (tree, _) = layout("<html><body><div>just text</div></body></html>", FLAT);
        assert!(
            !kinds(&tree).contains(&FragmentKind::AnonymousBlock),
            "a container of only inline content holds its lines directly: {:?}",
            kinds(&tree)
        );
    }

    /// §9.2.1.1: block-level content with no inline siblings needs no wrapper.
    #[test]
    fn block_block_only_content_generates_no_anonymous_box() {
        let (tree, _) = layout(
            "<html><body><div><p>one</p><p>two</p></div></body></html>",
            FLAT,
        );
        assert!(
            !kinds(&tree).contains(&FragmentKind::AnonymousBlock),
            "a container of only block content generates no wrapper: {:?}",
            kinds(&tree)
        );
    }

    /// Mixed content wraps each inline run, and *only* the inline runs.
    ///
    /// The canonical example from §9.2.1.1. Three items — text, a block, more text
    /// — become three block-level children, two of them anonymous.
    #[test]
    fn block_mixed_content_wraps_each_inline_run() {
        let (tree, _) = layout(
            "<html><body><div>before<p>middle</p>after</div></body></html>",
            FLAT,
        );
        let anonymous = bands(&tree, FragmentKind::AnonymousBlock);
        assert_eq!(
            anonymous.len(),
            2,
            "one anonymous box per inline run, got {:?}",
            kinds(&tree)
        );
    }

    /// The anonymous boxes sit *around* the block, not before it.
    ///
    /// This is the property the restructure exists for. The previous
    /// implementation collected all of a container's inline text into one run laid
    /// out before all of its block children, so "before" and "after" both rendered
    /// above the paragraph. Asserting the count alone would not have caught that;
    /// asserting the sibling sequence is what does.
    #[test]
    fn block_mixed_content_keeps_document_order() {
        let (tree, _) = layout(
            "<html><body><div>before<p>middle</p>after</div></body></html>",
            FLAT,
        );

        // The <div>'s children, by construction: the only fragment in this
        // document with three of them.
        let container = tree
            .in_layout_order()
            .into_iter()
            .map(|(_, id)| id)
            .find(|id| tree.children(*id).len() == 3)
            .expect("the div has three block-level children");

        let sequence: Vec<(FragmentKind, Au, Au)> = tree
            .children(container)
            .iter()
            .filter_map(|id| tree.get(*id))
            .map(|f| (f.kind, f.block_offset, f.size.block))
            .collect();

        // Three 20px bands, stacked, with the real block between the two
        // anonymous ones. Both the kinds and the offsets are asserted: the kinds
        // alone would pass if all three were laid out at the same place, and the
        // offsets alone would pass if the wrapper were the <p>.
        assert_eq!(
            sequence,
            vec![
                (FragmentKind::AnonymousBlock, px(0), px(20)),
                (FragmentKind::Block, px(20), px(20)),
                (FragmentKind::AnonymousBlock, px(40), px(20)),
            ]
        );
    }

    /// A block inside an inline splits the run around it (§9.2.1.1).
    ///
    /// `block-in-inline`, which is what most of the vendored `box-display` and
    /// `visuren` reftests exercise. The `<span>` is not a box this phase lays out,
    /// but its *content* is, and the block-level child inside it still has to end
    /// the run it interrupts rather than being hoisted past it.
    #[test]
    fn block_a_block_inside_an_inline_splits_the_run() {
        let (tree, _) = layout(
            "<html><body><div><span>before<p>middle</p>after</span></div></body></html>",
            FLAT,
        );
        assert_eq!(
            bands(&tree, FragmentKind::AnonymousBlock).len(),
            2,
            "the block ends the run inside the span: {:?}",
            kinds(&tree)
        );
    }

    /// Whitespace between two blocks generates no anonymous box (§9.2.1.1).
    ///
    /// Every hand-written document has a newline between its block elements, and
    /// wrapping each one would put an empty line box between every pair of
    /// paragraphs on the web — visible as extra height, not as nothing.
    #[test]
    fn block_whitespace_between_blocks_generates_no_anonymous_box() {
        let (tree, _) = layout(
            "<html><body><div>\n  <p>one</p>\n  <p>two</p>\n</div></body></html>",
            FLAT,
        );
        assert!(
            !kinds(&tree).contains(&FragmentKind::AnonymousBlock),
            "collapsible white space generates no box: {:?}",
            kinds(&tree)
        );
    }

    /// `display: none` contributes no content, not even text (§9.2.4).
    ///
    /// Distinct from the formatting contexts this phase declines to lay out, whose
    /// text *does* belong to the run around them. Getting the two confused put the
    /// UA stylesheet's `title { display: none }` into an anonymous block at the top
    /// of every document in the reftest corpus.
    #[test]
    fn block_display_none_contributes_no_text() {
        let css = "html, body, div { display: block; margin: 0; padding: 0 } \
             .hidden { display: none } * { font-size: 10px; line-height: 20px }";
        let (tree, _) = layout(
            "<html><body><div><span class=hidden>invisible</span></div></body></html>",
            css,
        );
        assert!(
            !kinds(&tree).contains(&FragmentKind::Line),
            "a display:none subtree produces no line boxes: {:?}",
            kinds(&tree)
        );
    }

    #[test]
    fn block_auto_width_fills_the_containing_block() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0; border: 0 }",
        );
        // The initial containing block, then html, body, div.
        let order = tree.in_layout_order();
        assert_eq!(order.len(), 4, "icb + html + body + div");
        for (_, id) in &order {
            let fragment = tree.get(*id).expect("every id resolves");
            assert_eq!(
                fragment.size.inline,
                px(800),
                "auto width fills the containing block all the way down"
            );
        }
    }

    #[test]
    fn block_margins_and_padding_reduce_the_content_width() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             div { display: block; margin: 0 10px; padding: 0 20px; border: 0 }",
        );
        let order = tree.in_layout_order();
        let div = order.last().expect("the div is deepest").1;
        let fragment = tree.get(div).expect("resolves");
        // Border box = content + padding. 800 - 2*10 margin = 780 border box.
        assert_eq!(
            fragment.size.inline,
            px(780),
            "margins come out of the containing block before the border box"
        );
        assert_eq!(
            fragment.inline_offset,
            px(10),
            "the box sits inside its own left margin"
        );
    }

    #[test]
    fn block_heights_stack_along_the_block_axis() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div><div id=b></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             div { display: block; height: 50px; margin: 0; padding: 0; border: 0 }",
        );
        let order = tree.in_layout_order();
        let divs: Vec<_> = order.iter().skip(3).collect();
        assert_eq!(divs.len(), 2, "icb + html + body, then two divs");

        let first = tree.get(divs[0].1).expect("resolves");
        let second = tree.get(divs[1].1).expect("resolves");
        assert_eq!(first.block_offset, px(0));
        assert_eq!(
            second.block_offset,
            px(50),
            "the second box starts where the first ends"
        );
    }

    /// `height: auto` is the content's height, summed up the tree.
    #[test]
    fn block_auto_height_comes_from_the_children() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div><div id=b></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             div { display: block; height: 30px; margin: 0; padding: 0; border: 0 }",
        );
        let root = tree.root().expect("a root fragment");
        let icb = tree.get(root).expect("resolves");
        assert_eq!(
            icb.size.block,
            px(60),
            "two 30px children make a 60px auto-height ancestor"
        );
    }

    /// An over-wide box does not produce a negative content width.
    #[test]
    fn block_over_constrained_width_saturates_at_zero() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 } \
             div { display: block; margin: 0 1000px; padding: 0; border: 0 }",
        );
        let order = tree.in_layout_order();
        let div = order.last().expect("the div is deepest").1;
        let fragment = tree.get(div).expect("resolves");
        assert_eq!(
            fragment.size.inline,
            px(0),
            "margins wider than the containing block leave no content, not a negative width"
        );
    }
    /// Text gives its container a height, which is what `height: auto` means for
    /// a block whose content is inline.
    ///
    /// Before inline layout existed this returned zero — wrong rather than
    /// incomplete, and the reason the reftest gate item could not be claimed.
    #[test]
    fn block_text_content_gives_the_container_a_height() {
        let (tree, _arena) = layout(
            "<html><body><div id=a>hello world</div></body></html>",
            "html, body, div { display: block; margin: 0; padding: 0;              font-size: 16px; line-height: 20px }",
        );
        let order = tree.in_layout_order();
        let div = order
            .iter()
            .find(|(depth, _)| *depth == 3)
            .expect("icb > html > body > div");
        let fragment = tree.get(div.1).expect("resolves");
        assert_eq!(
            fragment.size.block,
            px(20),
            "one line of text at line-height 20px makes a 20px tall block"
        );
        assert_eq!(
            tree.children(div.1).len(),
            1,
            "the text produced exactly one line fragment"
        );
    }

    /// Text wider than its container wraps, and each line adds height.
    #[test]
    fn block_text_wraps_and_each_line_adds_height() {
        let (tree, _arena) = layout(
            "<html><body><div id=a>aaa bbb ccc ddd</div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 }              div { display: block; width: 60px; margin: 0; padding: 0;              font-size: 16px; line-height: 20px }",
        );
        let order = tree.in_layout_order();
        let div = order
            .iter()
            .find(|(depth, _)| *depth == 3)
            .expect("icb > html > body > div");
        let lines = tree.children(div.1).len();
        assert!(
            lines > 1,
            "four words in a 60px box must wrap, got {lines} line(s)"
        );
        let fragment = tree.get(div.1).expect("resolves");
        assert_eq!(
            fragment.size.block,
            px(20) * i32::try_from(lines).expect("few lines"),
            "the block is as tall as its lines"
        );
    }
    /// Adjoining sibling margins collapse to the larger, not the sum (§8.3.1).
    #[test]
    fn block_adjoining_sibling_margins_collapse_to_the_larger() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div><div id=b></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 }              div { display: block; height: 20px; padding: 0; border: 0 }              #a { margin-bottom: 30px } #b { margin-top: 10px }",
        );
        let order = tree.in_layout_order();
        let second = order
            .iter()
            .filter(|(depth, _)| *depth == 3)
            .nth(1)
            .expect("two divs at depth 3");
        let fragment = tree.get(second.1).expect("resolves");
        assert_eq!(
            fragment.block_offset,
            px(50),
            "20px box + max(30, 10) collapsed margin, not 20 + 30 + 10"
        );
    }

    /// A positive and a negative margin add rather than taking a maximum.
    #[test]
    fn block_opposite_sign_margins_are_summed() {
        let (tree, _arena) = layout(
            "<html><body><div id=a></div><div id=b></div></body></html>",
            "html, body { display: block; margin: 0; padding: 0 }              div { display: block; height: 20px; padding: 0; border: 0 }              #a { margin-bottom: 30px } #b { margin-top: -10px }",
        );
        let order = tree.in_layout_order();
        let second = order
            .iter()
            .filter(|(depth, _)| *depth == 3)
            .nth(1)
            .expect("two divs at depth 3");
        let fragment = tree.get(second.1).expect("resolves");
        assert_eq!(
            fragment.block_offset,
            px(40),
            "20px box + (30 + -10), because the signs differ"
        );
    }

    /// Two negative margins collapse to the *more* negative.
    ///
    /// The "maximum of the absolute values" phrasing gets this wrong; §8.3.1 says
    /// the most negative wins, and this is the case that distinguishes them.
    #[test]
    fn block_two_negative_margins_collapse_to_the_more_negative() {
        assert_eq!(collapse(px(-10), px(-30)), px(-30));
        assert_eq!(collapse(px(-30), px(-10)), px(-30));
        assert_eq!(collapse(px(10), px(30)), px(30));
        assert_eq!(collapse(px(30), px(-10)), px(20));
    }
}
