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
//! Not done, and not approximated: `opacity` contexts, transforms, `outline`,
//! `text-decoration`, shadows, background images. `position:
//! sticky` counts as positioned. A page using them paints without them.
//!
//! # Inline content
//!
//! A line fragment's children are the pieces of the inline boxes drawn on it
//! (`FragmentKind::Inline`, with a border or a background) and the inline-blocks
//! sitting on it. Per line, in the inline step: each piece's background and border,
//! outermost first (a piece owns only the side edges its kind says), then the
//! line's text, each run in the `color` and `visibility` of its own element; the
//! inline-blocks are painted after, each atomically as if it were a stacking
//! context (its own background, blocks, floats and text), so an inline-block that
//! overlaps text paints over all of the line's text, not only the text before it.
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
//! - `border-radius` and `corner-shape` (CSS Backgrounds 3 section 5, CSS Borders
//!   4): a box with a rounded corner emits [`Item::FillRoundedRect`] for its
//!   background, [`Item::RoundedBorder`] for its border and
//!   [`Item::PushRoundedClip`] for its `overflow` clip, in place of the plain
//!   items; radii are resolved against the border box (percentages included),
//!   clamped so adjacent curves never overlap, and reduced by the border widths
//!   for the padding edge (a `padding-box` background, the clip). A box without a
//!   rounded corner emits exactly the plain items. A piece of an inline box does
//!   not round the edges it does not own.
//! - `overflow` other than `visible` (including `clip`): [`Item::PushClip`] at the
//!   padding box after the box's own background and border, [`Item::PopClip`]
//!   after its last descendant.
//! - Text: one [`Item::GlyphRun`] per text run of each line fragment, pen positions
//!   in page space on the baseline (see `emit_line`), in the `color` of the run's
//!   element. [`Item::TextRun`] is no longer emitted; the enum variant stays
//!   because the IPC wire still carries it. The font id is the layout registry's
//!   `px_text::FontId` number: whoever rasterizes must supply the same fonts under
//!   the same ids.
//! - `visibility: hidden | collapse`: the box's own background and border (and,
//!   for lines, text) are skipped; descendants are still visited and paint if
//!   their own computed `visibility` is `visible`.
//!
//! # Iterative
//!
//! One explicit stack, no recursion: a 100,000-deep document builds on a small
//! thread stack, as layout does.

use px_css::paint::{
    BackgroundClip, CornerKind, PaintCorner, PaintStyle, RadiusLen, Rgba, paint_style,
};
use px_css::view::StyleRoot;
use px_dom::{Arena, NodeId};
use px_layout::fragment::{FragmentId, FragmentKind, FragmentTree};
use px_layout::geom::Au;

use crate::imgpaint::{self, ImageSet};
use crate::display_list::{
    BorderSide, BorderStyle, Color, Corner, CornerShape, DisplayList, FontId, Item,
    PositionedGlyph, Rect, RoundedRect,
};

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
    /// Inline content (step 5): per line, the inline boxes' backgrounds and
    /// borders outermost first, then the line's text.
    inline: Vec<Op>,
    /// Inline-blocks (step 5, after the line's text): each painted atomically as
    /// if it were a stacking context (CSS 2.1 Appendix E, 7.2.1).
    atomics: Vec<usize>,
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
        // The flag marks an inline-block: a block-level fragment that is a child
        // of a line.
        let mut stack: Vec<(FragmentId, usize, bool)> = tree
            .children(ctx_root)
            .iter()
            .rev()
            .map(|k| (*k, real, false))
            .collect();
        while let Some((id, carried, atomic)) = stack.pop() {
            let Some(fragment) = tree.get(id) else {
                continue;
            };
            let mut kids_unit = carried;
            if fragment.kind == FragmentKind::Line {
                // Inline boxes first, so their backgrounds and borders are under
                // the text; they have no children of their own.
                for kid in tree.children(id) {
                    if tree
                        .get(*kid)
                        .is_some_and(|k| matches!(k.kind, FragmentKind::Inline { .. }))
                    {
                        units[carried].inline.push(Op::Boxed(*kid));
                    }
                }
                units[carried].inline.push(Op::Text(id));
                for kid in tree.children(id).iter().rev() {
                    if tree
                        .get(*kid)
                        .is_some_and(|k| !matches!(k.kind, FragmentKind::Inline { .. }))
                    {
                        stack.push((*kid, carried, true));
                    }
                }
                continue;
            } else if atomic {
                let u = units.len();
                units.push(Unit {
                    root_box: facts.style[id.index()].map(|_| id),
                    ..Unit::default()
                });
                units[carried].atomics.push(u);
                for kid in tree.children(id).iter().rev() {
                    stack.push((*kid, u, false));
                }
                continue;
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
                stack.push((*kid, kids_unit, false));
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
    styles: &Styles<'_, '_>,
) {
    let mut jobs = vec![Job::Unit(0)];
    while let Some(job) = jobs.pop() {
        match job {
            Job::Ops(ops) => emit_ops(list, tree, facts, &ops, propagated, styles),
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
                for c in unit.atomics.into_iter().rev() {
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
    styles: &Styles<'_, '_>,
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
                let border_box = frag_rect(gf);
                match border_shape(border_box, &gs, (true, true)) {
                    Some(shape) => list.push(Item::PushRoundedClip {
                        shape: shape.inset(border_widths(&gs)),
                    }),
                    None => list.push(Item::PushClip {
                        rect: padding_box(border_box, &gs),
                    }),
                }
                open.push(*g);
            }
        }
        let rect = frag_rect(fragment);
        match *op {
            Op::Text(_) => emit_line(list, tree, facts, id, styles),
            Op::Boxed(_) => {
                if let Some(style) = facts.style[id.index()]
                    && style.visible
                {
                    let skip_bg = fragment.node.is_some() && fragment.node == propagated;
                    // A piece of an inline box owns only the side edges it was
                    // given (`box-decoration-break: slice`).
                    let (left, right) = match fragment.kind {
                        FragmentKind::Inline { left, right } => (left, right),
                        _ => (true, true),
                    };
                    let extras = image_items(list, fragment, rect, &style, skip_bg, styles);
                    paint_box(list, rect, &style, skip_bg, (left, right), &extras.0);
                    list.items.extend(extras.1);
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

fn border_widths(style: &PaintStyle) -> [Au; 4] {
    style.borders.map(|b| b.width)
}

fn resolve_radius(r: RadiusLen, basis: Au) -> Au {
    let pct = (i64::from(r.pct) * i64::from(basis.0.max(0)) + (1 << 19)) >> 20;
    let total = i64::from(r.abs.0).saturating_add(pct);
    Au(i32::try_from(total).unwrap_or(i32::MAX))
}

fn corner(c: &PaintCorner, rect: Rect) -> Corner {
    Corner {
        rx: resolve_radius(c.x, rect.width),
        ry: resolve_radius(c.y, rect.height),
        shape: match c.kind {
            CornerKind::Convex(n) => CornerShape::Convex(n),
            CornerKind::Concave(n) => CornerShape::Concave(n),
        },
    }
}

/// The border box as a rounded rectangle (`border-radius`, `corner-shape`,
/// percentages against the box, radii clamped by CSS Backgrounds 3 section 5.5),
/// or `None` when no corner is rounded, so that an unrounded box emits exactly
/// the items it always did. A piece of an inline box that does not own an edge
/// (`box-decoration-break: slice`) has square corners on that edge.
fn border_shape(
    rect: Rect,
    style: &PaintStyle,
    (left, right): (bool, bool),
) -> Option<RoundedRect> {
    let mut corners = style.corners.map(|c| corner(&c, rect));
    for (i, owned) in [(0, left), (1, right), (2, right), (3, left)] {
        if !owned {
            corners[i] = Corner::SQUARE_CORNER;
        }
    }
    let shape = RoundedRect::new(rect, corners);
    shape.is_rounded().then_some(shape)
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
    build_with_images(
        arena,
        style_root,
        tree,
        viewport_width,
        viewport_height,
        &ImageSet::default(),
    )
}

/// As [`build_with_viewport`], with the decoded pictures of the page: each
/// `<img>` that has one paints it (see [`crate::imgpaint`]) and every
/// `background-image: url(...)` whose URL is in `images` paints its layer. An
/// empty set builds exactly what [`build_with_viewport`] builds.
#[must_use]
pub fn build_with_images(
    arena: &Arena,
    style_root: &StyleRoot,
    tree: &FragmentTree,
    viewport_width: Au,
    viewport_height: Au,
    images: &ImageSet,
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
        // The propagated element's image layers cover the whole canvas.
        let canvas_images = if images.urls.is_empty() {
            None
        } else {
            canvas_image_source(arena, |n| px_css::image_style::background_layers(dom, n))
        };
        if let Some((_, layers)) = &canvas_images {
            let items = imgpaint::canvas_items(&mut list, canvas, layers, images);
            list.items.extend(items);
        }

        let facts = gather(tree, root, |n| paint_style(dom, n));
        let units = build_units(tree, &facts, root);
        let style_of = |n| paint_style(dom, n);
        let styles = Styles::new(
            &style_of,
            dom,
            images,
            canvas_images.as_ref().map(|(n, _)| *n),
        );
        emit(&mut list, tree, &facts, units, propagated, &styles);
    });
    list
}

/// The element whose `background-image` layers paint against the canvas, and its
/// layers: the root element's when it has any, else the `<body>`'s when the root
/// is `html` (the same rule as the colour, CSS Backgrounds 3 section 2.11.2).
fn canvas_image_source(
    arena: &Arena,
    layers_of: impl Fn(NodeId) -> Vec<px_css::image_style::BgLayer>,
) -> Option<(NodeId, Vec<px_css::image_style::BgLayer>)> {
    let name_is = |id: NodeId, name: &str| {
        arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_some_and(|q| q.local.as_ref() == name)
    };
    let is_element = |id: NodeId| arena.get(id).is_some_and(|n| n.element_name().is_some());
    let root = arena
        .children(arena.document())
        .and_then(|k| k.into_iter().find(|k| is_element(*k)))?;
    let l = layers_of(root);
    if !l.is_empty() {
        return Some((root, l));
    }
    if name_is(root, "html") {
        let body = arena
            .children(root)
            .and_then(|k| k.into_iter().find(|k| name_is(*k, "body")))?;
        let l = layers_of(body);
        if !l.is_empty() {
            return Some((body, l));
        }
    }
    None
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

fn paint_box(
    list: &mut DisplayList,
    rect: Rect,
    style: &PaintStyle,
    skip_background: bool,
    (left, right): (bool, bool),
    background_images: &[Item],
) {
    let shape = border_shape(rect, style, (left, right));
    if !skip_background && style.background[3] > 0 {
        let border_box = matches!(style.background_clip, BackgroundClip::BorderBox);
        // Content-box is painted as padding-box; see the module docs.
        match shape {
            Some(shape) => list.push(Item::FillRoundedRect {
                shape: if border_box {
                    shape
                } else {
                    shape.inset(border_widths(style))
                },
                color: color(style.background),
            }),
            None => list.push(Item::FillRect {
                rect: if border_box {
                    rect
                } else {
                    padding_box(rect, style)
                },
                color: color(style.background),
            }),
        }
    }
    list.items.extend_from_slice(background_images);
    let drawn = |i: usize| {
        style.borders[i].drawn
            && style.borders[i].width > Au(0)
            && !(i == 3 && !left)
            && !(i == 1 && !right)
    };
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
        let sides = [side(0), side(1), side(2), side(3)];
        match shape {
            Some(shape) => list.push(Item::RoundedBorder { shape, sides }),
            None => list.push(Item::Border { rect, sides }),
        }
    }
}

/// The image items of a box: `(background layers, element content)`. The layers
/// go between the background colour and the border, the content after the border.
fn image_items(
    list: &mut DisplayList,
    fragment: &px_layout::fragment::Fragment,
    rect: Rect,
    style: &PaintStyle,
    skip_background: bool,
    styles: &Styles<'_, '_>,
) -> (Vec<Item>, Vec<Item>) {
    let images = styles.images;
    let Some(node) = fragment.node.filter(|_| !images.is_empty()) else {
        return (Vec::new(), Vec::new());
    };
    let owns_edges = !matches!(fragment.kind, FragmentKind::Inline { .. });
    let shape = border_shape(rect, style, (true, true)).filter(|_| owns_edges);
    let borders = border_widths(style);
    let layers = if skip_background
        || images.urls.is_empty()
        || styles.canvas_images == Some(node)
    {
        Vec::new()
    } else {
        let l = px_css::image_style::background_layers(styles.dom, node);
        imgpaint::background_items(list, rect, shape, borders, &l, images)
    };
    let content = match images.elements.get(&node) {
        Some(img) if fragment.kind == FragmentKind::Block => {
            match px_css::image_style::replaced_style(styles.dom, node) {
                Some(rs) => imgpaint::content_items(list, rect, shape, borders, &rs, img),
                None => Vec::new(),
            }
        }
        _ => Vec::new(),
    };
    (layers, content)
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

/// Computed paint styles by DOM node, memoised: a run names the element whose
/// `color` paints it, and many runs share one element.
struct Styles<'a, 'd> {
    dom: px_css::view::Dom<'d>,
    images: &'a ImageSet,
    /// The element whose image layers were painted against the canvas.
    canvas_images: Option<NodeId>,
    of: &'a dyn Fn(NodeId) -> Option<PaintStyle>,
    memo: std::cell::RefCell<std::collections::BTreeMap<NodeId, Option<PaintStyle>>>,
}

impl<'a, 'd> Styles<'a, 'd> {
    fn new(
        of: &'a dyn Fn(NodeId) -> Option<PaintStyle>,
        dom: px_css::view::Dom<'d>,
        images: &'a ImageSet,
        canvas_images: Option<NodeId>,
    ) -> Self {
        Self {
            dom,
            images,
            canvas_images,
            of,
            memo: std::cell::RefCell::new(std::collections::BTreeMap::new()),
        }
    }

    fn get(&self, node: NodeId) -> Option<PaintStyle> {
        *self
            .memo
            .borrow_mut()
            .entry(node)
            .or_insert_with(|| (self.of)(node))
    }
}

fn add(a: Au, b: Au) -> Au {
    Au(a.0.saturating_add(b.0))
}

/// Emit one [`Item::GlyphRun`] per text run of a line fragment.
///
/// A glyph's pen position is the line fragment's origin plus the run's
/// `inline_offset` plus the glyph's `x` (horizontal), and the line's origin plus
/// the run's `baseline` plus the glyph's `y` (vertical, downward from the
/// baseline). Layout hands glyphs over in visual order, left to right, so a
/// right-to-left run needs no special handling here. The colour and visibility
/// are those of the run's `node`; a run without a styled node takes what the line
/// inherits.
fn emit_line(
    list: &mut DisplayList,
    tree: &FragmentTree,
    facts: &Facts,
    line: FragmentId,
    styles: &Styles<'_, '_>,
) {
    let Some(fragment) = tree.get(line) else {
        return;
    };
    let inherited = facts.inherited[line.index()];
    let mut glyphs = Vec::new();
    for run in tree.text_runs(line) {
        let (rgba, visible) = match run.node.and_then(|n| styles.get(n)) {
            Some(s) => (s.color, s.visible),
            None => (inherited.color, inherited.visible),
        };
        if !visible || rgba[3] == 0 {
            continue;
        }
        let origin_x = add(fragment.inline_offset, run.inline_offset);
        let origin_y = add(fragment.block_offset, run.baseline);
        glyphs.clear();
        glyphs.extend(tree.glyphs(run).iter().map(|g| PositionedGlyph {
            glyph_id: g.glyph_id,
            x: add(origin_x, g.x),
            y: add(origin_y, g.y),
        }));
        if glyphs.is_empty() {
            continue;
        }
        // Underline and overline go under the glyphs, line-through over them
        // (CSS Text Decoration 3, section 3.2). A line is a rectangle as wide as
        // the run, in its declaring element's `text-decoration-color`.
        let line = |list: &mut DisplayList, node: Option<NodeId>, top: Au| {
            let Some(c) = node.and_then(|n| styles.get(n)).map(|s| s.decoration_color) else {
                return;
            };
            if c[3] == 0 || run.deco.thickness <= Au(0) {
                return;
            }
            list.push(Item::FillRect {
                rect: Rect {
                    x: origin_x,
                    y: add(origin_y, top),
                    width: run.width,
                    height: run.deco.thickness,
                },
                color: color(c),
            });
        };
        line(list, run.deco.under, run.deco.under_top);
        line(list, run.deco.over, run.deco.over_top);
        // A run beyond the list's bounds is dropped, not truncated.
        let _ = list.push_glyph_run(FontId(run.font.0), run.font_size, color(rgba), &glyphs);
        line(list, run.deco.through, run.deco.through_top);
    }
}
