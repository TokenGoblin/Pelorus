//! Block layout: widths (CSS2 §10.3.3), heights (§10.6.3) and margin collapsing
//! (§8.3.1) for in-flow block boxes.
//!
//! Iterative: one explicit frame stack, never recursion over tree depth.
//!
//! # Margin collapsing, as implemented
//!
//! A single accumulator, `pending`, holds the margins adjoining the current
//! insertion point. Entering a block pushes its top margin; leaving one pushes
//! its bottom margin. A margin is *resolved* (turned into a position) only when
//! something that separates it from the next margin is placed: a child with
//! border or padding on top, a box that establishes a BFC, or a box whose height
//! or bottom padding/border stops its bottom margin from reaching its parent.
//!
//! A *chain* is a run of nested boxes whose top edges are adjoining: no
//! border/padding on top and not a BFC. The chain's top edge is not known until
//! the first margin-separating thing is placed, at which point every frame in
//! the chain is positioned at once. That is why `chain_base` exists: it is the
//! y at which the outermost chain frame's parent had placed content up to.
//!
//! Not implemented yet: clearance, floats, min/max sizes, `Percent` heights.

use crate::au::Au;
use crate::style::{Edges, Length};
use crate::tree::{BoxId, Geometry, LayoutTree};

#[derive(Clone, Copy, Default)]
struct Margins {
    pos: Au,
    neg: Au,
}

impl Margins {
    fn push(&mut self, m: Au) {
        if m >= Au::ZERO {
            self.pos = self.pos.max(m);
        } else {
            self.neg = self.neg.min(m);
        }
    }
    fn resolve(self) -> Au {
        self.pos + self.neg
    }
}

struct Frame {
    id: Option<BoxId>, // None: the virtual parent of the root
    next_child: usize,
    content_x: Au,
    content_width: Au,
    margin: Edges<Au>,
    padding: Edges<Au>,
    border: Edges<Au>,
    border_box_x: Au,
    border_box_width: Au,
    explicit_height: Option<Au>,
    top_chain: bool,
    collapses_bottom: bool,
    border_top: Au,
    content_top: Au,
    cursor: Au,
}

/// Lay out the tree in a viewport of `viewport_width`. Positions are relative
/// to the viewport's top-left. Idempotent.
pub fn layout(tree: &mut LayoutTree, viewport_width: Au) {
    let Some(root) = tree.root() else { return };

    let mut stack: Vec<Frame> = vec![Frame {
        id: None,
        next_child: 0,
        content_x: Au::ZERO,
        content_width: viewport_width,
        margin: Edges::default(),
        padding: Edges::default(),
        border: Edges::default(),
        border_box_x: Au::ZERO,
        border_box_width: viewport_width,
        explicit_height: None,
        top_chain: false,
        collapses_bottom: false,
        border_top: Au::ZERO,
        content_top: Au::ZERO,
        cursor: Au::ZERO,
    }];
    let mut pending = Margins::default();
    let mut chain_base = Au::ZERO;
    let mut to_enter: Option<BoxId> = Some(root);

    loop {
        // Enter the next box, if one is queued.
        if let Some(child) = to_enter.take() {
            let Some(parent) = stack.last() else { return };
            let cb_width = parent.content_width;
            let parent_content_x = parent.content_x;
            let Some(b) = tree.get(child) else { return };
            let style = b.style;

            let padding = Edges {
                top: style.padding.top.resolve(cb_width).unwrap_or(Au::ZERO),
                right: style.padding.right.resolve(cb_width).unwrap_or(Au::ZERO),
                bottom: style.padding.bottom.resolve(cb_width).unwrap_or(Au::ZERO),
                left: style.padding.left.resolve(cb_width).unwrap_or(Au::ZERO),
            };
            let border = style.border;
            let (ml, mr, width) = used_width(
                cb_width,
                style.margin.left,
                style.margin.right,
                style.width,
                padding.left + padding.right + border.left + border.right,
            );
            let margin_top = style.margin.top.resolve(cb_width).unwrap_or(Au::ZERO);
            let margin_bottom = style.margin.bottom.resolve(cb_width).unwrap_or(Au::ZERO);
            let border_box_width =
                width + padding.left + padding.right + border.left + border.right;
            let border_box_x = parent_content_x + ml;
            let explicit_height = match style.height {
                Length::Px(h) => Some(h.non_negative()),
                _ => None,
            };

            let is_root = parent_is_virtual(&stack);
            let joins_chain = border.top == Au::ZERO
                && padding.top == Au::ZERO
                && !style.establishes_bfc
                && !is_root;
            let collapses_bottom = explicit_height.is_none()
                && border.bottom == Au::ZERO
                && padding.bottom == Au::ZERO
                && !style.establishes_bfc
                && !is_root;

            pending.push(margin_top);

            let mut frame = Frame {
                id: Some(child),
                next_child: 0,
                content_x: border_box_x + border.left + padding.left,
                content_width: width,
                margin: Edges {
                    top: margin_top,
                    right: mr,
                    bottom: margin_bottom,
                    left: ml,
                },
                padding,
                border,
                border_box_x,
                border_box_width,
                explicit_height,
                top_chain: false,
                collapses_bottom,
                border_top: Au::ZERO,
                content_top: Au::ZERO,
                cursor: Au::ZERO,
            };

            if joins_chain {
                let parent_in_chain = stack.last().map(|f| f.top_chain).unwrap_or(false);
                if !parent_in_chain {
                    chain_base = stack.last().map(|f| f.cursor).unwrap_or(Au::ZERO);
                }
                frame.top_chain = true;
            } else {
                // Margin-separating: position it, and everything above it in
                // an open chain, now.
                let base = match stack.last() {
                    Some(f) if f.top_chain => chain_base,
                    Some(f) => f.cursor,
                    None => Au::ZERO,
                };
                let y = base + pending.resolve();
                pending = Margins::default();
                close_chain(&mut stack, y);
                frame.border_top = y;
                frame.content_top = y + border.top + padding.top;
                frame.cursor = frame.content_top;
            }
            stack.push(frame);
            continue;
        }

        // Advance the top frame: enter its next in-flow child, or exit it.
        let next = {
            let Some(top) = stack.last_mut() else { return };
            match top.id {
                Some(id) => {
                    let child = tree.children(id).get(top.next_child).copied();
                    if child.is_some() {
                        top.next_child += 1;
                    }
                    child
                }
                None => None,
            }
        };
        if next.is_some() {
            to_enter = next;
            continue;
        }

        // Exit the top frame.
        let Some(mut f) = stack.pop() else { return };
        let Some(id) = f.id else { return };

        let mut through = false;
        if f.top_chain {
            // Nothing inside separated its margins from ours.
            let y = chain_base + pending.resolve();
            if f.collapses_bottom {
                // Empty and adjoining: top and bottom margins collapse through
                // the box. Leave `pending` open, but give the box a position.
                f.border_top = y;
                f.content_top = y;
                f.cursor = y;
                through = true;
            } else {
                pending = Margins::default();
                close_chain(&mut stack, y);
                f.border_top = y;
                f.content_top = y + f.border.top + f.padding.top;
                f.cursor = f.content_top;
                f.top_chain = false;
            }
        } else if !f.collapses_bottom && f.explicit_height.is_none() {
            // Children's trailing margin stays inside a box that doesn't let
            // it escape.
            f.cursor += pending.resolve();
            pending = Margins::default();
        } else if !f.collapses_bottom {
            pending = Margins::default();
        }

        let content_height = f
            .explicit_height
            .unwrap_or((f.cursor - f.content_top).non_negative());
        let border_box_height =
            content_height + f.padding.top + f.padding.bottom + f.border.top + f.border.bottom;
        let border_bottom = f.border_top + border_box_height;

        pending.push(f.margin.bottom);

        if let Some(b) = tree.get_mut(id) {
            b.geometry = Geometry {
                x: f.border_box_x,
                y: f.border_top,
                width: f.border_box_width,
                height: border_box_height,
                margin: f.margin,
                padding: f.padding,
                border: f.border,
            };
        }
        // A box the margins collapsed through takes no space, so it must not
        // move the parent's insertion point: the margin is still pending.
        if !through && let Some(parent) = stack.last_mut() {
            parent.cursor = border_bottom;
        }
    }
}

fn parent_is_virtual(stack: &[Frame]) -> bool {
    stack.last().map(|f| f.id.is_none()).unwrap_or(true)
}

/// Position every frame in an open top chain at `y`.
fn close_chain(stack: &mut [Frame], y: Au) {
    for f in stack.iter_mut().rev() {
        if !f.top_chain {
            break;
        }
        f.top_chain = false;
        f.border_top = y;
        f.content_top = y;
        f.cursor = y;
    }
}

/// CSS2 §10.3.3: returns (margin-left, margin-right, content width).
fn used_width(
    cb: Au,
    margin_left: Length,
    margin_right: Length,
    width: Length,
    horizontal_extras: Au,
) -> (Au, Au, Au) {
    let ml = margin_left.resolve(cb);
    let mr = margin_right.resolve(cb);
    match width.resolve(cb) {
        None => {
            let ml = ml.unwrap_or(Au::ZERO);
            let mr = mr.unwrap_or(Au::ZERO);
            let w = (cb - ml - mr - horizontal_extras).non_negative();
            (ml, mr, w)
        }
        Some(w) => {
            let w = w.non_negative();
            let remaining = cb - w - horizontal_extras;
            match (ml, mr) {
                (None, None) => {
                    if remaining < Au::ZERO {
                        (Au::ZERO, remaining, w)
                    } else {
                        let left = remaining / 2;
                        (left, remaining - left, w)
                    }
                }
                (None, Some(mr)) => (remaining - mr, mr, w),
                (Some(ml), None) => (ml, remaining - ml, w),
                // Over-constrained: the right margin gives way (ltr).
                (Some(ml), Some(_)) => (ml, remaining - ml, w),
            }
        }
    }
}
