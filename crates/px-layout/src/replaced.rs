//! Replaced elements: `<img>` with a decoded picture.
//!
//! Layout never decodes anything. The embedder decodes (or fails to) before
//! layout and hands over the **intrinsic size in CSS pixels** of each `<img>`
//! that has a picture ([`layout_document_with_images`]); an `<img>` without an
//! entry is not replaced and lays out as the inline element it is, with its
//! `alt` text as its content (HTML "rendering": an image that is not available
//! is represented by its alternative text).
//!
//! # Used size (CSS 2.1 section 10.3.2 and 10.6.2, CSS Sizing 3 section 5)
//!
//! A replaced element has an intrinsic width, height and ratio (the picture's;
//! the ratio is `width / height`). The *specified* width and height are the
//! computed `width` and `height` when they are not `auto`, else the element's
//! `width` and `height` attributes (HTML maps them to presentational hints of
//! zero specificity, so any author `width` wins; an integer or a decimal means
//! pixels, `N%` a percentage of the containing block's width, which for the
//! height needs a definite containing height and so counts as `auto`).
//!
//! - both specified: used as given;
//! - one specified: the other follows the intrinsic ratio;
//! - neither: the intrinsic size.
//!
//! `min-width`, `max-width`, `min-height` and `max-height` then apply to an
//! `auto` axis, the other axis following the ratio again. This is a simplified
//! form of the CSS 2.1 section 10.4 constraint table: it agrees with it
//! whenever at most one bound binds, which is every case that has a stated
//! answer in the specification's own examples. `box-sizing: border-box` has the
//! element's padding and border taken out of a stated size.
//!
//! `object-fit` and `object-position` do not change the box, only the picture
//! inside it; they are paint's.
//!
//! # How the table reaches the box walk
//!
//! A thread-local set for the duration of [`layout_document_with_images`] and
//! cleared by a guard (also when layout unwinds). Layout itself is single
//! threaded; the table is read-only while it is installed. The alternative,
//! threading a parameter through every step of `block.rs`, would touch
//! every signature of a file others are editing.

use std::cell::RefCell;
use std::collections::BTreeMap;

use app_units::Au;
use px_dom::{Arena, Node, NodeId};
use style::properties::ComputedValues;

use crate::geom::au;

use crate::fragment::FragmentTree;

const AU_PER_PX: i64 = 60;

/// The largest used width or height, in px. A page cannot make a box that
/// overflows `Au`.
const MAX_PX: i64 = 1_000_000;

thread_local! {
    static TABLE: RefCell<Option<BTreeMap<NodeId, (u32, u32)>>> = const { RefCell::new(None) };
}

struct Installed;

impl Drop for Installed {
    fn drop(&mut self) {
        TABLE.with(|t| *t.borrow_mut() = None);
    }
}

/// [`crate::block::layout_document_in`] with the intrinsic sizes, in CSS px, of the
/// `<img>` elements that have a picture. An entry with a zero side is ignored.
#[must_use]
pub fn layout_document_with_images(
    arena: &Arena,
    root: &px_css::view::StyleRoot,
    viewport_inline_size: Au,
    viewport_block_size: Option<Au>,
    images: &BTreeMap<NodeId, (u32, u32)>,
) -> Option<FragmentTree> {
    let table: BTreeMap<NodeId, (u32, u32)> = images
        .iter()
        .filter(|(_, (w, h))| *w > 0 && *h > 0)
        .map(|(k, v)| (*k, *v))
        .collect();
    TABLE.with(|t| *t.borrow_mut() = Some(table));
    let _guard = Installed;
    crate::block::layout_document_in(arena, root, viewport_inline_size, viewport_block_size)
}

/// The intrinsic size of `node`'s picture, if it has one.
pub(crate) fn intrinsic(node: NodeId) -> Option<(u32, u32)> {
    TABLE.with(|t| t.borrow().as_ref().and_then(|m| m.get(&node).copied()))
}

/// Whether `node` is a replaced element with a picture.
pub(crate) fn has_picture(node: NodeId) -> bool {
    intrinsic(node).is_some()
}

/// A replaced element whose `display` is inline-level: it sits on a line as one
/// atomic inline, exactly like an `inline-block`.
pub(crate) fn is_atomic_inline(node: NodeId, style: &ComputedValues) -> bool {
    use style::values::specified::box_::DisplayOutside;
    has_picture(node) && matches!(style.clone_display().outside(), DisplayOutside::Inline)
}

fn attr<'a>(node: &'a Node, local: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|a| &*a.name.local == local)
        .map(|a| &*a.value)
}

/// The text an `<img>` shows when its picture is not available: its `alt`,
/// whitespace as written (the inline run collapses it). `None` for another
/// element, for an `<img>` that has a picture, and for an empty or missing `alt`.
pub(crate) fn alt_text(node_id: NodeId, node: &Node) -> Option<String> {
    if node.element_name()?.local.as_ref() != "img" || has_picture(node_id) {
        return None;
    }
    let alt = attr(node, "alt")?;
    (!alt.trim().is_empty()).then(|| alt.to_string())
}

/// An HTML "rules for parsing dimension values", reduced: optional leading
/// whitespace, digits with an optional fraction, then an optional `%`. Anything
/// after that is ignored; no digits is `None`.
fn dimension(value: &str) -> Option<(i64, bool)> {
    let v = value.trim_start();
    let end = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(v.len());
    let number = v.get(..end)?;
    if number.is_empty() || number == "." {
        return None;
    }
    let (whole, frac) = number.split_once('.').unwrap_or((number, ""));
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    // Thousandths, truncated, so no float is involved.
    let mut milli = 0i64;
    for (i, c) in frac.chars().take(3).enumerate() {
        milli += i64::from(c.to_digit(10)?) * [100, 10, 1][i];
    }
    let thousandths = whole.checked_mul(1000)?.checked_add(milli)?;
    let percent = v.get(end..).is_some_and(|r| r.starts_with('%'));
    Some((thousandths, percent))
}

fn px_to_au(px: i64) -> Au {
    au((px.clamp(0, MAX_PX) * AU_PER_PX) as i32)
}

/// The inputs the used-size computation needs from the box walk.
pub(crate) struct Inputs<'a> {
    pub node_id: NodeId,
    pub node: &'a Node,
    pub style: &'a ComputedValues,
    /// The containing block's inline size.
    pub containing: Au,
    /// Border plus padding along each axis (inline, block).
    pub surround: (Au, Au),
    /// `min-width`, `max-width` (content box).
    pub width_bounds: (Au, Option<Au>),
    /// `min-height`, `max-height` (content box).
    pub height_bounds: (Au, Option<Au>),
}

/// The used content-box size of a replaced element with a picture.
pub(crate) fn used_size(i: &Inputs<'_>) -> Option<(Au, Au)> {
    use style::computed_values::box_sizing::T as BoxSizing;
    use style::values::computed::Size;

    let (iw, ih) = intrinsic(i.node_id)?;
    let (iw, ih) = (i64::from(iw), i64::from(ih));
    let border_box = i.style.clone_box_sizing() == BoxSizing::BorderBox;
    let content = |v: Au, surround: Au| {
        if border_box {
            (v - surround).max(Au(0))
        } else {
            v
        }
    };

    let mut w: Option<Au> = match i.style.clone_width() {
        Size::LengthPercentage(ref lp) => {
            lp.0.maybe_to_used_value(Some(i.containing))
                .map(|v| content(v, i.surround.0))
        }
        _ => None,
    };
    let mut h: Option<Au> = match i.style.clone_height() {
        Size::LengthPercentage(ref lp) => {
            lp.0.maybe_to_used_value(None)
                .map(|v| content(v, i.surround.1))
        }
        _ => None,
    };
    if w.is_none()
        && let Some((n, pct)) = attr(i.node, "width").and_then(dimension)
    {
        let v = if pct {
            au(((i64::from(i.containing.0) * n) / 100_000).clamp(0, i64::from(i32::MAX)) as i32)
        } else {
            au((n * AU_PER_PX / 1000).clamp(0, MAX_PX * AU_PER_PX) as i32)
        };
        w = Some(v);
    }
    if h.is_none()
        && let Some((n, false)) = attr(i.node, "height").and_then(dimension)
    {
        h = Some(au(
            (n * AU_PER_PX / 1000).clamp(0, MAX_PX * AU_PER_PX) as i32
        ));
    }

    // Following the ratio, rounding to the nearest Au.
    let by_ratio_h =
        |w: Au| au(((i64::from(w.0) * ih + iw / 2) / iw).clamp(0, i64::MAX >> 33) as i32);
    let by_ratio_w =
        |h: Au| au(((i64::from(h.0) * iw + ih / 2) / ih).clamp(0, i64::MAX >> 33) as i32);
    let clamp = |v: Au, (min, max): (Au, Option<Au>)| {
        let capped = max.map_or(v, |m| v.min(m));
        capped.max(min)
    };
    let (wu, hu) = match (w, h) {
        (Some(w), Some(h)) => (clamp(w, i.width_bounds), clamp(h, i.height_bounds)),
        (Some(w), None) => {
            let wu = clamp(w, i.width_bounds);
            (wu, clamp(by_ratio_h(wu), i.height_bounds))
        }
        (None, Some(h)) => {
            let hu = clamp(h, i.height_bounds);
            (clamp(by_ratio_w(hu), i.width_bounds), hu)
        }
        (None, None) => {
            let (w0, h0) = (px_to_au(iw), px_to_au(ih));
            let wc = clamp(w0, i.width_bounds);
            let h1 = if wc == w0 { h0 } else { by_ratio_h(wc) };
            let hc = clamp(h1, i.height_bounds);
            if hc == h1 {
                (wc, hc)
            } else {
                (clamp(by_ratio_w(hc), i.width_bounds), hc)
            }
        }
    };
    let cap = au((MAX_PX * AU_PER_PX) as i32);
    Some((wu.min(cap).max(Au(0)), hu.min(cap).max(Au(0))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_values_parse_like_html() {
        assert_eq!(dimension("100"), Some((100_000, false)));
        assert_eq!(dimension(" 12px"), Some((12_000, false)));
        assert_eq!(dimension("50%"), Some((50_000, true)));
        assert_eq!(dimension("1.5"), Some((1_500, false)));
        assert_eq!(dimension(".25"), Some((250, false)));
        assert_eq!(dimension(""), None);
        assert_eq!(dimension("auto"), None);
        assert_eq!(dimension("-3"), None);
        assert_eq!(dimension("."), None);
        // Absurd values saturate rather than overflow.
        assert!(dimension("99999999999999999999").is_none());
    }
}
