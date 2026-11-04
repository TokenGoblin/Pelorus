//! Display-list construction: fragment tree plus computed style in, paint items out.
//!
//! # Painting order
//!
//! **Not yet CSS 2.1 Appendix E in full.** What is implemented is the normal-flow
//! case: the canvas background first, then every fragment in tree order, each
//! painting its own background, then its border, then (inside the box's overflow
//! clip, when it has one) its descendants. For boxes in normal flow that is
//! Appendix E steps 4-7 collapsed into one pass, which is correct while nothing
//! floats or is positioned.
//!
//! Not done, and not approximated: stacking contexts and `z-index`, floats
//! (painted in tree order here, not in their own layer above block backgrounds),
//! positioned boxes (likewise), inline-level backgrounds and borders (layout
//! produces no fragment for an inline box), `outline`, shadows, background
//! images, `border-radius`, `opacity`, transforms. A page using them paints in
//! tree order, which is wrong for overlapping content and right for everything
//! else. That is a known gap, not a silent one.
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

enum Task {
    Enter(FragmentId, Inherited),
    PopClip,
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

        let mut stack = vec![Task::Enter(
            root,
            Inherited {
                color: [0, 0, 0, 255],
                visible: true,
            },
        )];
        while let Some(task) = stack.pop() {
            let (id, mut inherited) = match task {
                Task::PopClip => {
                    list.push(Item::PopClip);
                    continue;
                }
                Task::Enter(id, inherited) => (id, inherited),
            };
            let Some(fragment) = tree.get(id) else {
                continue;
            };
            let rect = Rect {
                x: fragment.inline_offset,
                y: fragment.block_offset,
                width: fragment.size.inline,
                height: fragment.size.block,
            };
            let style = fragment.node.and_then(|n| paint_style(dom, n));
            let mut clip = false;

            match (fragment.kind, style) {
                (FragmentKind::Line, _) => {
                    if inherited.visible && inherited.color[3] > 0 {
                        list.push(Item::TextRun {
                            rect,
                            color: color(inherited.color),
                        });
                    }
                }
                (_, Some(style)) => {
                    inherited = Inherited {
                        color: style.color,
                        visible: style.visible,
                    };
                    if style.visible {
                        let skip_bg = fragment.node.is_some() && fragment.node == propagated;
                        paint_box(&mut list, rect, &style, skip_bg);
                    }
                    if style.clips_overflow {
                        list.push(Item::PushClip {
                            rect: padding_box(rect, &style),
                        });
                        clip = true;
                    }
                }
                // The initial containing block and anonymous blocks: no style of
                // their own, nothing to paint, children inherit.
                (_, None) => {}
            }

            if clip {
                stack.push(Task::PopClip);
            }
            for kid in tree.children(id).iter().rev() {
                stack.push(Task::Enter(*kid, inherited));
            }
        }
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
