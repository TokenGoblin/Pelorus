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
//! # What this does not do yet
//!
//! Floats, positioned boxes, and inline/text layout. A box containing text
//! currently produces no line fragments at all, so its `height: auto` resolves to
//! zero — which is *wrong*, not merely incomplete, and is why the reftest gate
//! item is not yet claimed. Margin collapsing is likewise absent. Each is a named
//! gap rather than a silent one, and the tests below assert what is implemented
//! rather than asserting around what is not.

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
    Enter { node: NodeId, parent: FragmentId },
    /// Resolve this box's block size from the fragments its children produced.
    Exit { fragment: FragmentId },
}

/// The resolved box-model edges of one box, in app units.
struct Edges {
    margin: LogicalEdges,
    border: LogicalEdges,
    padding: LogicalEdges,
}

impl Edges {
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

    let root_element = first_element(arena)?;

    // Children accumulated per fragment, keyed by fragment index. A side table
    // rather than building the tree top-down, because `set_children` needs the
    // whole list at once and the list is only complete on the exit visit.
    let mut pending: Vec<Vec<FragmentId>> = vec![Vec::new()];
    let mut inline_sizes: Vec<Au> = vec![viewport_inline_size];
    // `None` means `height: auto`, which is resolved from content on the way up.
    let mut specified_block_sizes: Vec<Option<Au>> = vec![None];
    // The block size this box's own inline content occupies, if any. Separate
    // from the children cursor because line fragments are positioned by
    // `layout_lines` rather than by the block stacking pass.
    let mut inline_content_heights: Vec<Au> = vec![Au(0)];
    let mut edges: Vec<Edges> = vec![Edges {
        margin: LogicalEdges::ZERO,
        border: LogicalEdges::ZERO,
        padding: LogicalEdges::ZERO,
    }];

    let mut stack = vec![
        Step::Exit { fragment: icb },
        Step::Enter {
            node: root_element,
            parent: icb,
        },
    ];

    px_css::view::with_dom(arena, root, |dom| {
        while let Some(step) = stack.pop() {
            match step {
                Step::Enter { node, parent } => {
                    let Some(style) = computed_style(dom, node) else {
                        continue;
                    };
                    // A box whose formatting context this phase does not
                    // implement -- flex, grid, inline-block, table -- generates no
                    // fragment of its own, because laying it out as a block would
                    // be confidently wrong geometry rather than none.
                    //
                    // Its **children are still visited**, attached to the same
                    // parent. Skipping them too was a review finding and a bad
                    // one: a single `display: flex` wrapper near the top of a page
                    // deleted everything beneath it, which is why rust-lang.org
                    // laid out to 2 fragments while 112 of its elements computed
                    // `display: block`. Dropping a subtree is not the
                    // conservative choice it looks like.
                    if !is_block_level(&style) {
                        for child in children_of(arena, node).into_iter().rev() {
                            stack.push(Step::Enter {
                                node: child,
                                parent,
                            });
                        }
                        continue;
                    }

                    let containing = inline_sizes.get(parent.index()).copied().unwrap_or(Au(0));
                    let mut resolved = resolve_edges(&style, containing);
                    let content_inline = resolve_inline_size(&style, containing, &resolved);
                    centre_if_auto_margins(&style, containing, content_inline, &mut resolved);

                    let fragment = tree.push(Fragment::new(
                        FragmentKind::Block,
                        LogicalSize::new(
                            content_inline
                                + resolved.border.inline_sum()
                                + resolved.padding.inline_sum(),
                            Au(0),
                        ),
                    ));

                    // Grow the side tables in lockstep with the tree, so every
                    // fragment index is a valid index into each of them.
                    while pending.len() <= fragment.index() {
                        pending.push(Vec::new());
                        inline_sizes.push(Au(0));
                        specified_block_sizes.push(None);
                        inline_content_heights.push(Au(0));
                        edges.push(Edges {
                            margin: LogicalEdges::ZERO,
                            border: LogicalEdges::ZERO,
                            padding: LogicalEdges::ZERO,
                        });
                    }
                    inline_sizes[fragment.index()] = content_inline;
                    edges[fragment.index()] = resolved;
                    specified_block_sizes[fragment.index()] = resolve_block_size(&style);
                    if let Some(kids) = pending.get_mut(parent.index()) {
                        kids.push(fragment);
                    }

                    stack.push(Step::Exit { fragment });

                    let element_kids = children_of(arena, node);
                    {
                        // The box's own inline-level text: its text nodes, plus
                        // the text of any non-block descendants, stopping at every
                        // block-level one. Laid out on the way down, because the
                        // available inline size is known here and the lines' total
                        // height is what the Exit pass wants.
                        //
                        // The first version only did this for a box with *no*
                        // element children, so `<div>hello <b>world</b></div>`
                        // dropped its text entirely and resolved to zero height.
                        // Stopping at block-level descendants rather than at any
                        // element is what makes that work without also giving
                        // `<body>` the text of every block inside it.
                        let raw = collect_inline_text(dom, arena, node);
                        let collapsed = crate::inline::collapse_whitespace(&raw);
                        if !collapsed.is_empty() {
                            let font_size = Au::from(style.clone_font_size().computed_size());
                            let context = crate::inline::InlineContext {
                                text: &collapsed,
                                font_size,
                                line_height: crate::inline::line_height_of(&style),
                                available: content_inline,
                            };
                            let (lines, height) = crate::inline::layout_lines(&mut tree, &context);
                            while pending.len() <= tree.len() {
                                pending.push(Vec::new());
                                inline_sizes.push(Au(0));
                                specified_block_sizes.push(None);
                                inline_content_heights.push(Au(0));
                                edges.push(Edges {
                                    margin: LogicalEdges::ZERO,
                                    border: LogicalEdges::ZERO,
                                    padding: LogicalEdges::ZERO,
                                });
                            }
                            if let Some(kids) = pending.get_mut(fragment.index()) {
                                kids.extend_from_slice(&lines);
                            }
                            inline_content_heights[fragment.index()] = height;
                        }
                    }

                    // Children pushed in reverse so they are entered in document
                    // order once popped.
                    for child in element_kids.into_iter().rev() {
                        stack.push(Step::Enter {
                            node: child,
                            parent: fragment,
                        });
                    }
                }

                Step::Exit { fragment } => {
                    let kids = pending.get(fragment.index()).cloned().unwrap_or_default();

                    // Stack the children along the block axis. No margin
                    // Margin collapsing between siblings, per CSS 2.1 §8.3.1.
                    //
                    // Two adjoining vertical margins collapse into one whose size
                    // is the larger of the two — or, when they have opposite
                    // signs, the sum of the most positive and the most negative.
                    // "Adjoining" here means only sibling-to-sibling: collapsing
                    // *through* a parent, which happens when no border or padding
                    // separates a parent from its first or last child, is not
                    // implemented and is still a named gap.
                    //
                    // Inline content sits above any block children, and the line
                    // fragments were already positioned relative to it.
                    let mut cursor = inline_content_heights
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(Au(0));
                    let surround = edges
                        .get(fragment.index())
                        .map_or(Au(0), Edges::block_surround);
                    // The parent's content-box origin, relative to its own
                    // *border* box. Its margin is deliberately not included: a
                    // margin is outside the border box, and the parent's own
                    // offset already accounts for it. Adding it here counted it
                    // twice, which is what a review found -- a child of a
                    // 30px-margin parent inside an 8px-margin body came out at
                    // 35px, which is neither absolute (43) nor parent-relative (5).
                    let content_inline_start = edges
                        .get(fragment.index())
                        .map_or(Au(0), |e| e.border.inline_start + e.padding.inline_start);
                    // The block axis needs the same origin and did not have it at
                    // all: children were placed at the parent's border-box top,
                    // inside its own top padding, while the inline axis did
                    // include padding. The two axes disagreed.
                    let content_block_start = edges
                        .get(fragment.index())
                        .map_or(Au(0), |e| e.border.block_start + e.padding.block_start);

                    // The margin left over from the previous sibling's bottom
                    // edge, waiting to collapse with the next one's top.
                    let mut pending_margin = Au(0);
                    let mut first_in_flow = true;

                    for kid in &kids {
                        // Line fragments were already positioned by
                        // `layout_lines`, and their height is already in `cursor`
                        // via `inline_content_heights`. Stacking them again moves
                        // them down by their own height and counts it twice --
                        // which presented as every text block coming out exactly
                        // double height, with its single line sitting one
                        // line-height below the top.
                        if tree.get(*kid).is_some_and(|f| f.kind == FragmentKind::Line) {
                            // Shifted into the content box rather than
                            // repositioned: `layout_lines` already stacked them
                            // relative to the content origin, and moving them
                            // again is the double-height bug from earlier.
                            if let Some(line) = tree.get_mut(*kid) {
                                line.inline_offset += content_inline_start;
                                line.block_offset += content_block_start;
                            }
                            continue;
                        }
                        let kid_margin = edges
                            .get(kid.index())
                            .map_or(LogicalEdges::ZERO, |e| e.margin);

                        // The first in-flow child's top margin has nothing to
                        // collapse against here, because collapsing it with the
                        // parent's is the through-the-parent case this does not
                        // implement.
                        cursor += if first_in_flow {
                            kid_margin.block_start
                        } else {
                            collapse(pending_margin, kid_margin.block_start)
                        };
                        first_in_flow = false;

                        if let Some(kid_fragment) = tree.get_mut(*kid) {
                            // Relative to this parent's border box. A final pass
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
                    }
                    // The last child's bottom margin is inside this box's content
                    // height. Collapsing it out through the parent is the case
                    // above that is not implemented.
                    cursor += pending_margin;

                    tree.set_children(fragment, &kids);
                    let specified = specified_block_sizes
                        .get(fragment.index())
                        .copied()
                        .unwrap_or(None);
                    if let Some(f) = tree.get_mut(fragment) {
                        // A specified `height` wins; `auto` takes the content's
                        // block size (§10.6.3). Either way the box's own border
                        // and padding are added, because the fragment records a
                        // border box.
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

/// The inline-level text belonging to `node`, stopping at block-level children.
///
/// A block container's inline content is its own text nodes and the text of any
/// inline descendants — not the text of block descendants, which form their own
/// boxes and lay out their own lines. Without the stop, `<body>` would collect
/// every word on the page and lay it out again as body's own lines.
///
/// Iterative, and a pre-order walk so the text comes out in document order.
fn collect_inline_text(dom: px_css::view::Dom<'_>, arena: &Arena, node: NodeId) -> String {
    let mut out = String::new();
    let mut stack: Vec<NodeId> = children_in_order(arena, node);
    stack.reverse();

    while let Some(id) = stack.pop() {
        let Some(current) = arena.get(id) else {
            continue;
        };

        if current.element_name().is_some() {
            // A block-level element is a box of its own; its text is not ours.
            let is_block = computed_style(dom, id).is_some_and(|s| is_block_level(&s));
            if is_block {
                continue;
            }
            let mut kids = children_in_order(arena, id);
            kids.reverse();
            stack.extend(kids);
            continue;
        }

        if let Some(text) = current.text() {
            out.push_str(text);
        }
    }
    out
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

/// The element children of `node`, in document order.
fn children_of(arena: &Arena, node: NodeId) -> Vec<NodeId> {
    let mut out = Vec::new();
    let Some(parent) = arena.get(node) else {
        return out;
    };
    let mut next = parent.first_child();
    while let Some(id) = next {
        let Some(child) = arena.get(id) else { break };
        if child.element_name().is_some() {
            out.push(id);
        }
        next = child.next_sibling();
    }
    out
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

/// Whether this box participates in block layout.
///
/// Only `display: block` for now. `inline-block`, `flex` and `grid` are
/// block-level too, and each establishes a formatting context this phase does not
/// implement — laying them out as plain blocks would produce confidently wrong
/// geometry rather than none.
fn is_block_level(style: &ComputedValues) -> bool {
    use style::values::computed::Display;
    style.clone_display() == Display::Block
}

/// Resolve margins, borders and padding against the containing block's inline
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
