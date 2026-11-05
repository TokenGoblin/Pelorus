//! Raster images in the display list: an `<img>`'s content, and `background-image`.
//!
//! Everything here is integer geometry in [`Au`] (1/60 px); the rasterizer
//! snaps each destination rectangle once (see [`crate::raster`]). No float.
//!
//! # `<img>` (CSS Images 3 section 5, `object-fit` and `object-position`)
//!
//! The picture is drawn into the element's **content box** (border box less
//! border and padding). `fill` stretches it over the box; `contain` and `cover`
//! scale it uniformly to fit inside or to cover the box; `none` keeps its
//! intrinsic size; `scale-down` is the smaller of `none` and `contain`. The
//! scaled picture is placed by `object-position` (default `50% 50%`, a
//! percentage of the space left over). When it does not lie inside the content
//! box it is clipped to it; when the box has rounded corners, the picture is
//! clipped to the content box rounded by the radii less the border and padding
//! widths, as `overflow: clip` would.
//!
//! # `background-image` (CSS Backgrounds 3 sections 3.4 to 3.10)
//!
//! Layers paint bottom (last) to top (first), above the background colour and
//! below the border. A layer's *positioning area* is the `background-origin` box
//! and its *painting area* the `background-clip` box; `content-box` is treated
//! as the padding box (padding is not carried in `PaintStyle`), the same
//! approximation the colour layer already makes. The image is sized by
//! `background-size` (`auto` keeps the intrinsic size or the ratio; `contain` and
//! `cover` scale to the positioning area; lengths and percentages are against
//! the positioning area), positioned by `background-position` (a percentage is of
//! the area less the image), and tiled by `background-repeat`: `repeat` fills the
//! painting area, `no-repeat` draws once, `space` spreads whole tiles with equal
//! gaps (one tile that fits is placed at the start), `round` rescales the tile so
//! a whole number fits. Tiles are separate [`Item::Image`]s inside one clip, so
//! each snaps on its own and neighbours share an edge. A layer that would need
//! more than [`MAX_TILES`] tiles paints nothing. Gradients and `none` layers are
//! absent from the layer list (see `px_css::image_style`).
//!
//! The canvas background (the root's, or the `<body>`'s when it propagates) paints
//! its images against the whole canvas.

use std::collections::BTreeMap;
use std::sync::Arc;

use px_css::image_style::{
    BgLayer, BgSize, LenPct, ObjectFit, RefBox, Repeat, ReplacedStyle, SizeComp,
};
use px_dom::NodeId;
use px_layout::geom::Au;

use crate::display_list::{
    DisplayList, ImageData, ImageFilter, Item, Rect, RoundedRect,
};

/// Most tiles one background layer may expand to.
pub const MAX_TILES: usize = 20_000;

/// The decoded pictures a page has, by what asked for them.
#[derive(Clone, Debug, Default)]
pub struct ImageSet {
    /// The picture of each `<img>` element that has one.
    pub elements: BTreeMap<NodeId, Arc<ImageData>>,
    /// The picture at each absolute `background-image` URL.
    pub urls: BTreeMap<String, Arc<ImageData>>,
}

impl ImageSet {
    /// Whether there is nothing to paint.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty() && self.urls.is_empty()
    }
}

fn filter(nearest: bool) -> ImageFilter {
    if nearest {
        ImageFilter::Nearest
    } else {
        ImageFilter::Smooth
    }
}

fn au(v: i64) -> Au {
    Au(i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX }))
}

/// `a * b / c` rounded to nearest, `c > 0`.
fn mul_div(a: i64, b: i64, c: i64) -> i64 {
    (a * b + c / 2).div_euclid(c.max(1))
}

fn rect_of(x: i64, y: i64, w: i64, h: i64) -> Rect {
    Rect {
        x: au(x),
        y: au(y),
        width: au(w.max(0)),
        height: au(h.max(0)),
    }
}

fn r64(r: Rect) -> (i64, i64, i64, i64) {
    (
        i64::from(r.x.0),
        i64::from(r.y.0),
        i64::from(r.width.0),
        i64::from(r.height.0),
    )
}

/// The size `object-fit` gives a picture of intrinsic size `(iw, ih)` px in a
/// box `(bw, bh)` (Au).
fn fitted(fit: ObjectFit, (iw, ih): (i64, i64), (bw, bh): (i64, i64)) -> (i64, i64) {
    let intrinsic = (iw * 60, ih * 60);
    let contain = || {
        if bw * ih <= bh * iw {
            (bw, mul_div(bw, ih, iw))
        } else {
            (mul_div(bh, iw, ih), bh)
        }
    };
    let cover = || {
        if bw * ih <= bh * iw {
            (mul_div(bh, iw, ih), bh)
        } else {
            (bw, mul_div(bw, ih, iw))
        }
    };
    match fit {
        ObjectFit::Fill => (bw, bh),
        ObjectFit::Contain => contain(),
        ObjectFit::Cover => cover(),
        ObjectFit::None => intrinsic,
        ObjectFit::ScaleDown => {
            let c = contain();
            if c.0 < intrinsic.0 || c.1 < intrinsic.1 {
                c
            } else {
                intrinsic
            }
        }
    }
}

fn clip_item(shape: Option<RoundedRect>, rect: Rect) -> Item {
    match shape {
        Some(s) => Item::PushRoundedClip { shape: s },
        None => Item::PushClip { rect },
    }
}

/// The items for an `<img>`'s picture. `border_box` is the element's border box,
/// `shape` its rounded border box (`None` when no corner is rounded), `borders`
/// its border widths `[top, right, bottom, left]`.
pub(crate) fn content_items(
    list: &mut DisplayList,
    border_box: Rect,
    shape: Option<RoundedRect>,
    borders: [Au; 4],
    rs: &ReplacedStyle,
    img: &Arc<ImageData>,
) -> Vec<Item> {
    let Some(idx) = list.add_image(img) else {
        return Vec::new();
    };
    let inset = [
        Au(borders[0].0.saturating_add(rs.padding[0].0)),
        Au(borders[1].0.saturating_add(rs.padding[1].0)),
        Au(borders[2].0.saturating_add(rs.padding[2].0)),
        Au(borders[3].0.saturating_add(rs.padding[3].0)),
    ];
    let (x, y, w, h) = r64(border_box);
    let (t, r, b, l) = (
        i64::from(inset[0].0),
        i64::from(inset[1].0),
        i64::from(inset[2].0),
        i64::from(inset[3].0),
    );
    let (cx, cy, cw, ch) = (x + l, y + t, (w - l - r).max(0), (h - t - b).max(0));
    if cw == 0 || ch == 0 || img.width == 0 || img.height == 0 {
        return Vec::new();
    }
    let content = rect_of(cx, cy, cw, ch);
    let (dw, dh) = fitted(
        rs.fit,
        (i64::from(img.width), i64::from(img.height)),
        (cw, ch),
    );
    let dx = cx + rs.pos_x.resolve(au(cw - dw)).0 as i64;
    let dy = cy + rs.pos_y.resolve(au(ch - dh)).0 as i64;
    let dest = rect_of(dx, dy, dw, dh);
    let content_shape = shape.map(|s| s.inset(inset));
    let inside = dx >= cx && dy >= cy && dx + dw <= cx + cw && dy + dh <= cy + ch;
    let mut out = Vec::new();
    let clipped = content_shape.is_some() || !inside;
    if clipped {
        out.push(clip_item(content_shape, content));
    }
    out.push(Item::Image {
        rect: dest,
        image: idx,
        filter: filter(rs.nearest),
    });
    if clipped {
        out.push(Item::PopClip);
    }
    out
}

/// Tile origins along one axis: positions of the tiles' near edges that
/// intersect `[c0, c1)`, and the (possibly adjusted) tile size.
fn tiles_1d(
    mode: Repeat,
    (p0, plen): (i64, i64),
    (c0, c1): (i64, i64),
    pos: i64,
    size: i64,
) -> (Vec<i64>, i64) {
    if size <= 0 {
        return (Vec::new(), size);
    }
    match mode {
        Repeat::NoRepeat => (vec![pos], size),
        Repeat::Repeat | Repeat::Round => {
            let size = if mode == Repeat::Round {
                let n = ((plen + size / 2) / size).max(1);
                (plen / n).max(1)
            } else {
                size
            };
            // The first tile whose far edge is past c0.
            let k0 = (c0 - pos).div_euclid(size);
            let mut v = Vec::new();
            let mut at = pos + k0 * size;
            while at < c1 && v.len() <= MAX_TILES {
                if at + size > c0 {
                    v.push(at);
                }
                at += size;
            }
            (v, size)
        }
        Repeat::Space => {
            let n = plen / size;
            if n <= 1 {
                return (vec![p0], size);
            }
            let mut v = Vec::new();
            for i in 0..n {
                let at = p0 + mul_div(i, plen - size, n - 1);
                if at < c1 && at + size > c0 {
                    v.push(at);
                }
            }
            (v, size)
        }
    }
}

/// The size of the tile: CSS Backgrounds 3 section 3.9, with the intrinsic size
/// `(iw, ih)` in px.
fn layer_size(layer: &BgLayer, (iw, ih): (i64, i64), (pw, ph): (i64, i64)) -> (i64, i64) {
    match layer.size {
        BgSize::Contain => fitted(ObjectFit::Contain, (iw, ih), (pw, ph)),
        BgSize::Cover => fitted(ObjectFit::Cover, (iw, ih), (pw, ph)),
        BgSize::Explicit(wc, hc) => {
            let comp = |c: SizeComp, basis: i64| match c {
                SizeComp::Auto => None,
                SizeComp::Len(lp) => Some(i64::from(lp.resolve(au(basis)).0).max(0)),
            };
            match (comp(wc, pw), comp(hc, ph)) {
                (Some(w), Some(h)) => (w, h),
                (Some(w), None) => (w, mul_div(w, ih, iw)),
                (None, Some(h)) => (mul_div(h, iw, ih), h),
                (None, None) => (iw * 60, ih * 60),
            }
        }
    }
}

/// The items of one element's `background-image` layers. `border_box` and
/// `shape` as for [`content_items`]; `borders` its border widths.
pub(crate) fn background_items(
    list: &mut DisplayList,
    border_box: Rect,
    shape: Option<RoundedRect>,
    borders: [Au; 4],
    layers: &[BgLayer],
    images: &ImageSet,
) -> Vec<Item> {
    let mut out = Vec::new();
    // Bottom layer first.
    for layer in layers.iter().rev() {
        let Some(img) = images.urls.get(&layer.url) else {
            continue;
        };
        layer_items(&mut out, list, border_box, shape, borders, layer, img);
    }
    out
}

fn ref_box(
    which: RefBox,
    border_box: Rect,
    shape: Option<RoundedRect>,
    borders: [Au; 4],
) -> (Rect, Option<RoundedRect>) {
    match which {
        RefBox::Border => (border_box, shape),
        // Content-box is the padding box here (see the module docs).
        RefBox::Padding | RefBox::Content => {
            let (x, y, w, h) = r64(border_box);
            let [t, r, b, l] = borders.map(|a| i64::from(a.0.max(0)));
            (
                rect_of(x + l, y + t, (w - l - r).max(0), (h - t - b).max(0)),
                shape.map(|s| s.inset(borders)),
            )
        }
    }
}

fn layer_items(
    out: &mut Vec<Item>,
    list: &mut DisplayList,
    border_box: Rect,
    shape: Option<RoundedRect>,
    borders: [Au; 4],
    layer: &BgLayer,
    img: &Arc<ImageData>,
) {
    if img.width == 0 || img.height == 0 {
        return;
    }
    let (pos_rect, _) = ref_box(layer.origin, border_box, shape, borders);
    let (paint_rect, paint_shape) = ref_box(layer.clip, border_box, shape, borders);
    let (px0, py0, pw, ph) = r64(pos_rect);
    let (cx0, cy0, cw, ch) = r64(paint_rect);
    if pw <= 0 || ph <= 0 || cw <= 0 || ch <= 0 {
        return;
    }
    let (iw, ih) = (i64::from(img.width), i64::from(img.height));
    let (w, h) = layer_size(layer, (iw, ih), (pw, ph));
    if w <= 0 || h <= 0 {
        return;
    }
    let x = px0 + LenPct::resolve(layer.pos_x, au(pw - w)).0 as i64;
    let y = py0 + LenPct::resolve(layer.pos_y, au(ph - h)).0 as i64;
    let (xs, w) = tiles_1d(
        layer.repeat_x,
        (px0, pw),
        (cx0, cx0 + cw),
        x,
        w,
    );
    // `round` in one axis with `auto` in the other keeps the ratio.
    let h = if layer.repeat_x == Repeat::Round
        && matches!(layer.size, BgSize::Explicit(_, SizeComp::Auto))
        && layer.repeat_y != Repeat::Round
    {
        mul_div(w, ih, iw)
    } else {
        h
    };
    let (ys, h) = tiles_1d(
        layer.repeat_y,
        (py0, ph),
        (cy0, cy0 + ch),
        y,
        h,
    );
    if xs.is_empty() || ys.is_empty() || xs.len().saturating_mul(ys.len()) > MAX_TILES {
        return;
    }
    let Some(idx) = list.add_image(img) else {
        return;
    };
    out.push(clip_item(paint_shape, paint_rect));
    for ty in &ys {
        for tx in &xs {
            out.push(Item::Image {
                rect: rect_of(*tx, *ty, w, h),
                image: idx,
                filter: filter(layer.nearest),
            });
        }
    }
    out.push(Item::PopClip);
}

/// The canvas background's image layers, against the whole canvas.
pub(crate) fn canvas_items(
    list: &mut DisplayList,
    canvas: Rect,
    layers: &[BgLayer],
    images: &ImageSet,
) -> Vec<Item> {
    background_items(list, canvas, None, [Au(0); 4], layers, images)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_fit_sizes() {
        // A 40x20 picture in a 100x100 px box (Au).
        let b = (6000, 6000);
        let i = (40, 20);
        assert_eq!(fitted(ObjectFit::Fill, i, b), (6000, 6000));
        assert_eq!(fitted(ObjectFit::Contain, i, b), (6000, 3000));
        assert_eq!(fitted(ObjectFit::Cover, i, b), (12000, 6000));
        assert_eq!(fitted(ObjectFit::None, i, b), (2400, 1200));
        // Smaller than the box: scale-down keeps the intrinsic size.
        assert_eq!(fitted(ObjectFit::ScaleDown, i, b), (2400, 1200));
        // Larger than the box: it behaves as contain.
        assert_eq!(fitted(ObjectFit::ScaleDown, (400, 200), b), (6000, 3000));
    }

    #[test]
    fn repeat_tiles_cover_the_painting_area_exactly() {
        // Tile 10, origin 3, area [0, 25): tiles at -7, 3, 13, 23.
        let (v, s) = tiles_1d(Repeat::Repeat, (0, 25), (0, 25), 3, 10);
        assert_eq!((v, s), (vec![-7, 3, 13, 23], 10));
        let (v, _) = tiles_1d(Repeat::NoRepeat, (0, 25), (0, 25), 3, 10);
        assert_eq!(v, vec![3]);
    }

    #[test]
    fn space_spreads_whole_tiles_and_round_rescales() {
        // 25 wide, tile 10: two fit; the first at 0, the last ending at 25.
        let (v, s) = tiles_1d(Repeat::Space, (0, 25), (0, 25), 0, 10);
        assert_eq!((v, s), (vec![0, 15], 10));
        // Only one fits: placed at the start.
        let (v, _) = tiles_1d(Repeat::Space, (0, 15), (0, 15), 4, 10);
        assert_eq!(v, vec![0]);
        // 25 wide, tile 10: round(2.5) = 3 tiles of 8 (floor of 25/3).
        let (v, s) = tiles_1d(Repeat::Round, (0, 25), (0, 25), 0, 10);
        assert_eq!(s, 8);
        assert_eq!(v, vec![0, 8, 16, 24]);
    }
}
