//! Display-list construction: fragment tree plus computed style in, paint items out.
//!
//! # Painting order
//!
//! CSS 2.1 Appendix E, for what the engine has. Per stacking context (the root, and
//! any element with `position` other than `static` and a `z-index` other than
//! `auto`): 1 the context root's background and border, 2 child contexts with
//! negative `z-index`, 3 in-flow non-positioned blocks' backgrounds and borders in
//! tree order, 4 non-positioned floats, 5 inline content (text placeholders), 6
//! positioned `z-index: auto` boxes and `z-index: 0` contexts in tree order, 7
//! positive `z-index` contexts in z order, ties in tree order. A float and a
//! positioned `z-index: auto` box are painted atomically as if they were contexts,
//! but their positioned descendants belong to the enclosing real context.
//!
//! `overflow` clips are not bracketed around subtrees (reordering would break the
//! nesting) but opened and closed per phase from each fragment's chain of
//! clipping ancestors; an absolutely positioned box escapes the clips between it
//! and its containing block.
//!
//! Not done, and not approximated: `opacity` contexts, transforms, inline-level
//! backgrounds and borders (layout produces no fragment for an inline box),
//! `outline`, shadows, background images, `border-radius`. `position: sticky`
//! counts as positioned. A page using them paints without them.
//!
//! # What is drawn
//!
//! - Canvas: a white [`Item::FillRect`] over the viewport, then the root element's
//!   background colour when it has one, else the `<body>`'s when the root is
//!   `html` (CSS Backgrounds 3 section 2.11.2). The element whose background
//!   propagated paints none of its own.
//! - `background-color`: one `FillRect` for the box named by `background-clip`
//!   (border box by default; padding box). `content-box` is painted as
//!   padding-box, because padding is not carried in [`px_css::paint::PaintStyle`]
//!   yet. Nothing for a fully transparent colour. Alpha is passed through.
//! - Borders: one [`Item::Border`], sides in top, right, bottom, left order, when
//!   any side is drawn with a width. `none` and `hidden` become width zero and
//!   style `None`. Every other style (dashed, double, ...) is emitted as `Solid`
//!   until the rasterizer has them.
//! - `overflow` other than `visible` (including `clip`): [`Item::PushClip`] at the
//!   padding box after the box's own background and border, [`Item::PopClip`]
//!   after its last descendant.
//! - Text: an [`Item::TextRun`] placeholder per line fragment, in the inherited
//!   `color`, covering the line's extent.
//! - `visibility: hidden | collapse`: the box's own background and border (and,
//!   for lines, text) are skipped; descendants are still visited and paint if
//!   their own computed `visibility` is `visible`.
//!
//! # Iterative
//!
//! One explicit stack, no recursion: a 100,000-deep document builds on a small
//! thread stack, as layout does.

use px_css::paint::{BackgroundClip, PaintStyle, Rgba, paint_style};
use px_css::view::StyleRoot;
use px_dom::{Arena, NodeId};
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::Au;

use crate::display_list::{BorderSide, BorderStyle, Color, DisplayList, Item, Rect};

fn color(c: Rgba) -> Color {
    Color {
        r: c[0],
        g: c[1],
        b: c[2],
        a: c[3],
    }
}

/// What a fragment inherits from the nearest ancestor that has a style.
#[derive(Clone, Copy)]
struct Inherited {
    color: Rgba,
    visible: bool,
}

/// One thing a phase paints.
#[derive(Clone, Copy)]
enum Op {
    /// A box's background and border.
    Boxed(FragmentId),
    /// A line's text placeholder.
    Text(FragmentId),
}

/// A unit of Appendix E painting: a real stacking context, or a *pseudo* one (a
/// float, or a positioned box with `z-index: auto`) painted atomically as if it
/// were one, whose positioned descendants are hoisted into the enclosing real
/// context.
#[derive(Default)]
struct Unit {
    /// The box whose own background and border open the unit (step 1).
    root_box: Option<FragmentId>,
    /// Child contexts with negative `z-index` (step 2), in discovery order.
    negative: Vec<(i32, usize)>,
    /// In-flow non-positioned blocks (step 3).
    blocks: Vec<Op>,
    /// Non-positioned floats (step 4).
    floats: Vec<usize>,
    /// Inline content (step 5).
    inline: Vec<Op>,
    /// Positioned `z-index: auto` boxes and `z-index: 0` contexts (step 6), in
    /// tree order.
    zero: Vec<usize>,
    /// Child contexts with positive `z-index` (step 7).
    positive: Vec<(i32, usize)>,
}

enum Job {
    Unit(usize),
    Ops(Vec<Op>),
}

/// Per-fragment facts gathered once, top-down.
struct Facts {
    style: Vec<Option<PaintStyle>>,
    /// What the fragment inherits from its parent.
    inherited: Vec<Inherited>,
    parent: Vec<Option<FragmentId>>,
    depth: Vec<i64>,
    /// Depth of the nearest positioned strict ancestor, -1 for the initial
    /// containing block.
    cb_depth: Vec<i64>,
}

impl Facts {
    /// The `overflow` clips that apply to `f`, outermost first.
    ///
    /// Every clipping ancestor applies, except that an absolutely positioned box
    /// is not clipped by an `overflow` box *between* it and its containing block
    /// (CSS 2.1 section 11.1.1).
    fn clips(&self, f: FragmentId) -> Vec<FragmentId> {
        let mut out = Vec::new();
        let mut limit = i64::MAX;
        let mut cur = f;
        loop {
            let i = cur.index();
            if self.style[i].is_some_and(|s| s.position_absolute) {
                limit = limit.min(self.cb_depth[i]);
            }
            let Some(p) = self.parent[i] else { break };
            if self.style[p.index()].is_some_and(|s| s.clips_overflow)
                && self.depth[p.index()] <= limit
            {
                out.push(p);
            }
            cur = p;
        }
        out.reverse();
        out
    }
}

/// One top-down pass: styles, inheritance, parents, depths.
fn gather(
    tree: &FragmentTree,
    root: FragmentId,
    style_of: impl Fn(NodeId) -> Option<PaintStyle>,
) -> Facts {
    let n = tree.len();
    let initial = Inherited {
        color: [0, 0, 0, 255],
        visible: true,
    };
    let mut facts = Facts {
        style: vec![None; n],
        inherited: vec![initial; n],
        parent: vec![None; n],
        depth: vec![0; n],
        cb_depth: vec![-1; n],
    };
    let mut stack = vec![(root, None::<FragmentId>, initial, 0i64, -1i64)];
    while let Some((id, parent, inherited, depth, cb)) = stack.pop() {
        let Some(fragment) = tree.get(id) else {
            continue;
        };
        let i = id.index();
        let style = fragment.node.and_then(&style_of);
        facts.style[i] = style;
        facts.inherited[i] = inherited;
        facts.parent[i] = parent;
        facts.depth[i] = depth;
        facts.cb_depth[i] = cb;
        let mut out = inherited;
        let mut child_cb = cb;
        if fragment.kind != FragmentKind::Line
            && let Some(s) = style
        {
            out = Inherited {
                color: s.color,
                visible: s.visible,
            };
            if s.positioned {
                child_cb = depth;
            }
        }
        for kid in tree.children(id).iter().rev() {
            stack.push((*kid, Some(id), out, depth + 1, child_cb));
        }
    }
    facts
}

/// Sort every fragment into the unit and phase Appendix E paints it in.
///
/// One pre-order walk per real stacking context, not entering a nested real
/// context (it gets its own walk, off the work list). Positioned descendants of a
/// pseudo unit are registered with the enclosing real context, so tree order among
/// them survives.
fn build_units(tree: &FragmentTree, facts: &Facts, root: FragmentId) -> Vec<Unit> {
    let mut units: Vec<Unit> = vec![Unit::default()];
    units[0].root_box = facts.style[root.index()].map(|_| root);
    let mut contexts = vec![(0usize, root)];
    while let Some((real, ctx_root)) = contexts.pop() {
        let mut stack: Vec<(FragmentId, usize)> = tree
            .children(ctx_root)
            .iter()
            .rev()
            .map(|k| (*k, real))
            .collect();
        while let Some((id, carried)) = stack.pop() {
            let Some(fragment) = tree.get(id) else {
                continue;
            };
            let mut kids_unit = carried;
            if fragment.kind == FragmentKind::Line {
                units[carried].inline.push(Op::Text(id));
            } else if let Some(s) = facts.style[id.index()] {
                if s.positioned
                    && let Some(z) = s.z_index
                {
                    let u = units.len();
                    units.push(Unit {
                        root_box: Some(id),
                        ..Unit::default()
                    });
                    match z {
                        z if z < 0 => units[real].negative.push((z, u)),
                        0 => units[real].zero.push(u),
                        z => units[real].positive.push((z, u)),
                    }
                    contexts.push((u, id));
                    continue;
                } else if s.positioned || s.floated {
                    let u = units.len();
                    units.push(Unit {
                        root_box: Some(id),
                        ..Unit::default()
                    });
                    if s.positioned {
                        units[real].zero.push(u);
                    } else {
                        units[carried].floats.push(u);
                    }
                    kids_unit = u;
                } else {
                    units[carried].blocks.push(Op::Boxed(id));
                }
            }
            for kid in tree.children(id).iter().rev() {
                stack.push((*kid, kids_unit));
            }
        }
    }
    units
}

/// Walk the units in painting order and emit their items.
fn emit(
    list: &mut DisplayList,
    tree: &FragmentTree,
    facts: &Facts,
    mut units: Vec<Unit>,
    propagated: Option<NodeId>,
) {
    let mut jobs = vec![Job::Unit(0)];
    while let Some(job) = jobs.pop() {
        match job {
            Job::Ops(ops) => emit_ops(list, tree, facts, &ops, propagated),
            Job::Unit(u) => {
                let Some(unit) = units.get_mut(u).map(std::mem::take) else {
                    continue;
                };
                // Pushed last-first: step 7 is popped last.
                let mut positive = unit.positive;
                positive.sort_by_key(|(z, _)| *z);
                for (_, c) in positive.into_iter().rev() {
                    jobs.push(Job::Unit(c));
                }
                for c in unit.zero.into_iter().rev() {
                    jobs.push(Job::Unit(c));
                }
                jobs.push(Job::Ops(unit.inline));
                for c in unit.floats.into_iter().rev() {
                    jobs.push(Job::Unit(c));
                }
                jobs.push(Job::Ops(unit.blocks));
                let mut negative = unit.negative;
                negative.sort_by_key(|(z, _)| *z);
                for (_, c) in negative.into_iter().rev() {
                    jobs.push(Job::Unit(c));
                }
                if let Some(r) = unit.root_box {
                    jobs.push(Job::Ops(vec![Op::Boxed(r)]));
                }
            }
        }
    }
}

/// Emit one phase's operations, opening and closing `overflow` clips as the chain
/// of clipping ancestors changes from one operation to the next.
fn emit_ops(
    list: &mut DisplayList,
    tree: &FragmentTree,
    facts: &Facts,
    ops: &[Op],
    propagated: Option<NodeId>,
) {
    let mut open: Vec<FragmentId> = Vec::new();
    for op in ops {
        let (Op::Boxed(id) | Op::Text(id)) = *op;
        let Some(fragment) = tree.get(id) else {
            continue;
        };
        let want = facts.clips(id);
        let common = open.iter().zip(&want).take_while(|(a, b)| a == b).count();
        while open.len() > common {
            open.pop();
            list.push(Item::PopClip);
        }
        for g in want.iter().skip(common) {
            if let (Some(gf), Some(gs)) = (tree.get(*g), facts.style[g.index()]) {
                list.push(Item::PushClip {
                    rect: padding_box(frag_rect(gf), &gs),
                });
                open.push(*g);
            }
        }
        let rect = frag_rect(fragment);
        match *op {
            Op::Text(_) => {
                let inherited = facts.inherited[id.index()];
                if inherited.visible && inherited.color[3] > 0 {
                    list.push(Item::TextRun {
                        rect,
                        color: color(inherited.color),
                    });
                }
            }
            Op::Boxed(_) => {
                if let Some(style) = facts.style[id.index()]
                    && style.visible
                {
                    let skip_bg = fragment.node.is_some() && fragment.node == propagated;
                    paint_box(list, rect, &style, skip_bg);
                }
            }
        }
    }
    for _ in open {
        list.push(Item::PopClip);
    }
}

fn frag_rect(fragment: &px_layout::fragment::Fragment) -> Rect {
    Rect {
        x: fragment.inline_offset,
        y: fragment.block_offset,
        width: fragment.size.inline,
        height: fragment.size.block,
    }
}

/// Build the display list for a laid-out document.
///
/// The canvas is sized to the initial containing block's fragment; use
/// [`build_with_viewport`] to cover a viewport taller than the content.
#[must_use]
pub fn build(arena: &Arena, style_root: &StyleRoot, tree: &FragmentTree) -> DisplayList {
    build_with_viewport(arena, style_root, tree, Au(0), Au(0))
}

/// As [`build`], with the canvas at least `viewport_width` by `viewport_height`.
#[must_use]
pub fn build_with_viewport(
    arena: &Arena,
    style_root: &StyleRoot,
    tree: &FragmentTree,
    viewport_width: Au,
    viewport_height: Au,
) -> DisplayList {
    let mut list = DisplayList::new();
    let Some(root) = tree.root() else {
        return list;
    };
    px_css::view::with_dom(arena, style_root, |dom| {
        let (canvas_w, canvas_h) = tree
            .get(root)
            .map_or((Au(0), Au(0)), |f| (f.size.inline, f.size.block));
        let canvas = Rect {
            x: Au(0),
            y: Au(0),
            width: canvas_w.max(viewport_width),
            height: canvas_h.max(viewport_height),
        };
        list.push(Item::FillRect {
            rect: canvas,
            color: Color::WHITE,
        });
        let (source, propagated) = canvas_background(arena, |n| paint_style(dom, n));
        if let Some(bg) = source
            && bg[3] > 0
        {
            list.push(Item::FillRect {
                rect: canvas,
                color: color(bg),
            });
        }

        let facts = gather(tree, root, |n| paint_style(dom, n));
        let units = build_units(tree, &facts, root);
        emit(&mut list, tree, &facts, units, propagated);
    });
    list
}

/// Which colour fills the canvas, and which node it was taken from (so that node
/// does not paint it a second time).
fn canvas_background(
    arena: &Arena,
    style_of: impl Fn(NodeId) -> Option<PaintStyle>,
) -> (Option<Rgba>, Option<NodeId>) {
    let name_is = |id: NodeId, name: &str| {
        arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_some_and(|q| q.local.as_ref() == name)
    };
    let is_element = |id: NodeId| arena.get(id).is_some_and(|n| n.element_name().is_some());
    let Some(root) = arena
        .children(arena.document())
        .and_then(|k| k.into_iter().find(|k| is_element(*k)))
    else {
        return (None, None);
    };
    let root_bg = style_of(root).map(|s| s.background);
    if root_bg.is_some_and(|c| c[3] > 0) {
        return (root_bg, Some(root));
    }
    if name_is(root, "html")
        && let Some(body) = arena
            .children(root)
            .and_then(|k| k.into_iter().find(|k| name_is(*k, "body")))
        && let Some(bg) = style_of(body).map(|s| s.background)
        && bg[3] > 0
    {
        return (Some(bg), Some(body));
    }
    (None, None)
}

fn paint_box(list: &mut DisplayList, rect: Rect, style: &PaintStyle, skip_background: bool) {
    if !skip_background && style.background[3] > 0 {
        let filled = match style.background_clip {
            BackgroundClip::BorderBox => rect,
            // Content-box is painted as padding-box; see the module docs.
            BackgroundClip::PaddingBox | BackgroundClip::ContentBox => padding_box(rect, style),
        };
        list.push(Item::FillRect {
            rect: filled,
            color: color(style.background),
        });
    }
    let drawn = |i: usize| style.borders[i].drawn && style.borders[i].width > Au(0);
    if (0..4).any(drawn) {
        let side = |i: usize| {
            let b = style.borders[i];
            if drawn(i) {
                BorderSide {
                    width: b.width,
                    style: BorderStyle::Solid,
                    color: color(b.color),
                }
            } else {
                BorderSide {
                    width: Au(0),
                    style: BorderStyle::None,
                    color: color(b.color),
                }
            }
        };
        list.push(Item::Border {
            rect,
            sides: [side(0), side(1), side(2), side(3)],
        });
    }
}

/// The border box less its borders, never negative.
fn padding_box(rect: Rect, style: &PaintStyle) -> Rect {
    let [top, right, bottom, left] = style.borders.map(|b| b.width);
    Rect {
        x: rect.x + left,
        y: rect.y + top,
        width: (rect.width - left - right).max(Au(0)),
        height: (rect.height - top - bottom).max(Au(0)),
    }
}
