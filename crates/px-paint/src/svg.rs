//! Inline `<svg>` elements as pictures (ADR 049).
//!
//! An `<svg>` element in an HTML document is a replaced element: layout gives
//! it a box from its `width` and `height` (attributes or CSS) and its
//! intrinsic size, and paint draws a bitmap in it. The bitmap is made here: the
//! element and its subtree are copied into `px-image`'s SVG document model and
//! rendered at the intrinsic size, with `currentColor` the element's computed
//! `color`. The result goes into [`crate::ImageSet::elements`] like an
//! `<img>`'s picture, and the document goes into [`crate::ImageSet::vectors`]
//! so paint renders it again at the size it is drawn (ADR 051).
//!
//! The subtree's own markup is used: its attributes, its `style=""`, its
//! `<style>` element. The page's author CSS reaches inside too, as in a
//! browser (`.icon path { fill: red }`), through the selector subset
//! `px-image` understands: type, class and id compounds with descendant and
//! child combinators, matched against the element's HTML ancestors as well.
//! Rules using anything else are dropped. Nothing inside is scripted or
//! fetched. The same limits as an SVG image apply, plus [`MAX_INLINE`] and
//! [`MAX_INLINE_BYTES`] per page.

use std::sync::Arc;

use px_dom::{Arena, NodeId};
use px_image::svg::{Builder, Document};

use crate::display_list::ImageData;
use crate::imgpaint::Vector;

/// Most inline SVG elements drawn per page.
pub const MAX_INLINE: usize = 256;
/// Most pixel bytes of inline SVG per page.
pub const MAX_INLINE_BYTES: usize = 64 << 20;

const SVG_NS: &str = "http://www.w3.org/2000/svg";

fn limits() -> px_image::Limits {
    px_image::Limits {
        max_pixels: 1 << 22,
        max_frames: 1,
        max_total_bytes: 64 << 20,
    }
}

fn is_svg(arena: &Arena, id: NodeId) -> bool {
    arena
        .get(id)
        .and_then(|n| n.element_name())
        .is_some_and(|q| &*q.ns == SVG_NS && &*q.local == "svg")
}

/// Copy the subtree at `root` into a document builder. Iterative: the DOM may
/// be deeper than the builder allows, which is then an error, not a stack.
fn build(arena: &Arena, root: NodeId) -> Option<Document> {
    enum Step {
        Enter(NodeId),
        Exit,
    }
    let mut b = Builder::new();
    let mut stack = vec![Step::Enter(root)];
    while let Some(step) = stack.pop() {
        match step {
            Step::Exit => b.close(),
            Step::Enter(id) => {
                let node = arena.get(id)?;
                if let Some(q) = node.element_name() {
                    let attrs = node
                        .attrs()
                        .unwrap_or_default()
                        .iter()
                        .map(|a| (a.name.local.to_string(), a.value.to_string()))
                        .collect();
                    b.open(&q.local, &*q.ns != SVG_NS, attrs).ok()?;
                    stack.push(Step::Exit);
                    let kids: Vec<NodeId> = arena.child_ids(id).collect();
                    stack.extend(kids.into_iter().rev().map(Step::Enter));
                } else if let Some(t) = node.text() {
                    b.text(t);
                }
            }
        }
    }
    Document::from_builder(b).ok()
}

/// One inline `<svg>` that has a picture.
#[derive(Clone, Debug)]
pub struct Inline {
    pub node: NodeId,
    /// The picture at the intrinsic size.
    pub image: Arc<ImageData>,
    /// The document, for rendering at the used size.
    pub vector: Vector,
    /// A `viewBox` and no absolute size: fills the width it is given.
    pub ratio_only: bool,
}

/// The `(tag, id, class)` of the ancestors of `id`, innermost first.
fn chain_of(arena: &Arena, id: NodeId) -> Vec<(String, String, String)> {
    arena
        .ancestors(id)
        .filter_map(|a| {
            let n = arena.get(a)?;
            let q = n.element_name()?;
            let attr = |name: &str| {
                n.attrs()
                    .and_then(|v| v.iter().find(|x| &*x.name.local == name))
                    .map(|x| x.value.to_string())
                    .unwrap_or_default()
            };
            Some((q.local.to_string(), attr("id"), attr("class")))
        })
        .take(32)
        .collect()
}

/// The picture of each outermost inline `<svg>` in `arena`. `color_of` gives
/// the computed `color` of an element (what `currentColor` means in it);
/// `page_css` is the page's author CSS text, applied inside the element.
/// An element that fails to render (limits, nothing drawable) has no picture
/// and so is not a replaced element: it lays out as an empty inline.
#[must_use]
pub fn inline_images(
    arena: &Arena,
    color_of: &dyn Fn(NodeId) -> Option<[u8; 4]>,
    page_css: &str,
) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut bytes = 0usize;
    for id in arena.descendants(arena.document()) {
        if out.len() >= MAX_INLINE || !is_svg(arena, id) {
            continue;
        }
        if arena.ancestors(id).any(|a| is_svg(arena, a)) {
            continue;
        }
        let Some(mut doc) = build(arena, id) else {
            continue;
        };
        if !page_css.is_empty() {
            doc.add_page_style(page_css, &chain_of(arena, id));
        }
        let (w, h) = doc.intrinsic_size();
        let need = (w as usize).saturating_mul(h as usize).saturating_mul(4);
        if bytes.saturating_add(need) > MAX_INLINE_BYTES {
            continue;
        }
        let color = color_of(id).unwrap_or([0, 0, 0, 255]);
        let Ok(img) = doc.render(w, h, color, &limits()) else {
            continue;
        };
        bytes += img.rgba.len();
        let data = ImageData {
            width: img.width,
            height: img.height,
            rgba: img.rgba,
        };
        if data.is_well_formed() {
            let ratio_only = doc.is_ratio_only();
            out.push(Inline {
                node: id,
                image: Arc::new(data),
                vector: Vector {
                    doc: Arc::new(doc),
                    color,
                },
                ratio_only,
            });
        }
    }
    out
}
