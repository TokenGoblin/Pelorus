//! Walking the document and drawing it.
//!
//! The walk is recursive, but its depth is bounded: element nesting is capped
//! at parse time ([`super::doc::MAX_DEPTH`]) and `<use>` chains at
//! [`MAX_USE_DEPTH`]. Everything that can multiply work is counted: element
//! visits (a `<use>` bomb stops here), flattened points, edges, and the
//! rasterizer's edge-sample count. Passing any bound fails the whole render.
//!
//! Never done, whatever the document says: scripts, event attributes,
//! `<image>`, `<foreignObject>`, `<animate>`, external `href`s and `url()`s,
//! `@import`. Those elements are simply not rendered.

use super::css::Rule;
use super::doc::{Doc, El};
use super::fixed::Q;
use super::geom::{Mat, p};
use super::paint::{Gradient, Kind, Layer, Spread};
use super::path::{Poly, Seg, ellipse_segs, flatten, parse_path, rect_segs};
use super::raster::{Edges, fill};
use super::stroke::{StrokeStyle, dash, stroke};
use super::style::{self, Paint, Style, numbers};
use super::values::{Color, number_list, parse_fraction, parse_length, parse_transform};
use crate::DecodeError;
use crate::limits::Budget;

const MAX_VISITS: u32 = 200_000;
const MAX_WORK: u64 = 600_000_000;
const MAX_EDGES: usize = 2_000_000;
const MAX_SHAPE_POINTS: usize = 400_000;
const MAX_TOTAL_POINTS: usize = 4_000_000;
const MAX_LAYER_DEPTH: usize = 4;
const MAX_USE_DEPTH: u32 = 12;

/// `1 / sqrt(2)` in Q16.
const FRAC_1_SQRT_2: Q = Q(46_341);

type Vp = (Q, Q);

/// A viewBox: `(min_x, min_y, width, height)`.
pub type ViewBox = (Q, Q, Q, Q);

pub fn view_box(el: &El) -> Option<ViewBox> {
    match numbers(el.attr("viewBox")).as_slice() {
        [x, y, w, h, ..] if w.0 > 0 && h.0 > 0 => Some((*x, *y, *w, *h)),
        _ => None,
    }
}

/// The viewBox-to-viewport transform of SVG 1.1 section 7.8.
pub fn viewport_transform(vb: ViewBox, par: Option<&str>, vw: Q, vh: Q) -> Mat {
    let (minx, miny, bw, bh) = vb;
    let mut align = "xMidYMid";
    let mut slice = false;
    for tok in par.unwrap_or("").split_whitespace() {
        match tok {
            "defer" | "meet" => {}
            "slice" => slice = true,
            t => align = t,
        }
    }
    let (sx, sy) = (vw / bw, vh / bh);
    let to_vb = Mat::translate(-minx, -miny);
    if align == "none" {
        return Mat::scaling(sx, sy).mul(&to_vb);
    }
    let s = if (sx < sy) != slice { sx } else { sy };
    let frac = |c: &str| match c {
        "Min" => Q::ZERO,
        "Max" => Q::ONE,
        _ => Q::ONE.half(),
    };
    let (ax, ay) = (
        align.get(1..4).unwrap_or("Mid"),
        align.get(5..8).unwrap_or("Mid"),
    );
    let tx = frac(ax) * (vw - (bw * s));
    let ty = frac(ay) * (vh - (bh * s));
    Mat::translate(tx, ty).mul(&Mat::scaling(s, s)).mul(&to_vb)
}

fn is_shape(kind: &str) -> bool {
    matches!(
        kind,
        "rect" | "circle" | "ellipse" | "line" | "polyline" | "polygon" | "path"
    )
}

fn len_attr(el: &El, name: &str, basis: Q) -> Q {
    el.attr(name)
        .and_then(parse_length)
        .map_or(Q::ZERO, |l| l.resolve(basis))
}

fn opt_len(el: &El, name: &str, basis: Q) -> Option<Q> {
    el.attr(name)
        .and_then(parse_length)
        .map(|l| l.resolve(basis))
}

fn diag(vp: Vp) -> Q {
    Q::hypot(vp.0, vp.1) * FRAC_1_SQRT_2
}

fn geometry(kind: &str, el: &El, vp: Vp) -> Vec<Seg> {
    let (bx, by, br) = (vp.0, vp.1, diag(vp));
    match kind {
        "rect" => {
            let (w, h) = (len_attr(el, "width", bx), len_attr(el, "height", by));
            if w.0 <= 0 || h.0 <= 0 {
                return Vec::new();
            }
            let (rx, ry) = match (opt_len(el, "rx", bx), opt_len(el, "ry", by)) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => (Q::ZERO, Q::ZERO),
            };
            let rx = if rx > w.half() { w.half() } else { rx };
            let ry = if ry > h.half() { h.half() } else { ry };
            rect_segs(len_attr(el, "x", bx), len_attr(el, "y", by), w, h, rx, ry)
        }
        "circle" => {
            let r = len_attr(el, "r", br);
            if r.0 <= 0 {
                return Vec::new();
            }
            ellipse_segs(len_attr(el, "cx", bx), len_attr(el, "cy", by), r, r)
        }
        "ellipse" => {
            let (rx, ry) = match (opt_len(el, "rx", bx), opt_len(el, "ry", by)) {
                (Some(a), Some(b)) => (a, b),
                (Some(a), None) => (a, a),
                (None, Some(b)) => (b, b),
                (None, None) => return Vec::new(),
            };
            if rx.0 <= 0 || ry.0 <= 0 {
                return Vec::new();
            }
            ellipse_segs(len_attr(el, "cx", bx), len_attr(el, "cy", by), rx, ry)
        }
        "line" => vec![
            Seg::M(p(len_attr(el, "x1", bx), len_attr(el, "y1", by))),
            Seg::L(p(len_attr(el, "x2", bx), len_attr(el, "y2", by))),
        ],
        "polyline" | "polygon" => {
            let nums = number_list(el.attr("points").unwrap_or(""));
            let mut segs: Vec<Seg> = Vec::new();
            for [x, y] in nums.as_chunks::<2>().0 {
                let pt = p(*x, *y);
                segs.push(if segs.is_empty() {
                    Seg::M(pt)
                } else {
                    Seg::L(pt)
                });
            }
            if segs.len() < 2 {
                return Vec::new();
            }
            if kind == "polygon" {
                segs.push(Seg::Z);
            }
            segs
        }
        "path" => parse_path(el.attr("d").unwrap_or("")),
        _ => Vec::new(),
    }
}

fn bbox(polys: &[Poly]) -> Option<(Q, Q, Q, Q)> {
    let mut it = polys.iter().flat_map(|q| q.pts.iter());
    let first = it.next()?;
    let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
    for pt in it {
        x0 = if pt.x < x0 { pt.x } else { x0 };
        y0 = if pt.y < y0 { pt.y } else { y0 };
        x1 = if pt.x > x1 { pt.x } else { x1 };
        y1 = if pt.y > y1 { pt.y } else { y1 };
    }
    Some((x0, y0, x1 - x0, y1 - y0))
}

fn to255(q: Q) -> u32 {
    (q * Q::int(255)).round().clamp(0, 255) as u32
}

enum Src {
    Solid([u8; 4]),
    Grad(Box<Gradient>),
}

#[derive(Clone)]
struct Env {
    ctm: Mat,
    vp: Vp,
    style: Style,
    use_depth: u32,
}

pub struct Renderer<'a> {
    doc: &'a Doc,
    rules: &'a [Rule],
    budget: Budget,
    work: u64,
    visits: u32,
    points: usize,
    layers: usize,
    w: usize,
    h: usize,
}

impl<'a> Renderer<'a> {
    pub fn new(doc: &'a Doc, rules: &'a [Rule], budget: Budget, w: usize, h: usize) -> Self {
        Renderer {
            doc,
            rules,
            budget,
            work: 0,
            visits: 0,
            points: 0,
            layers: 0,
            w,
            h,
        }
    }

    /// Render the whole document onto a fresh canvas. `view` is the root
    /// viewBox (or the intrinsic size when the document has none).
    pub fn render(&mut self, view: ViewBox, color: [u8; 4]) -> Result<Layer, DecodeError> {
        let mut layer = Layer::new(self.w, self.h, &mut self.budget)?;
        let Some(root) = self.doc.get(0) else {
            return Ok(layer);
        };
        let ctm = viewport_transform(
            view,
            root.attr("preserveAspectRatio"),
            Q::int(self.w as i64),
            Q::int(self.h as i64),
        );
        let style = style::compute(self.doc, self.rules, 0, &Style::initial(color));
        if !style.display {
            return Ok(layer);
        }
        let env = Env {
            ctm,
            vp: (view.2, view.3),
            style,
            use_depth: 0,
        };
        self.children(0, &env, &mut layer)?;
        Ok(layer)
    }

    fn children(&mut self, id: u32, env: &Env, layer: &mut Layer) -> Result<(), DecodeError> {
        let doc = self.doc;
        let Some(el) = doc.get(id) else { return Ok(()) };
        for &kid in &el.kids {
            self.element(kid, env, layer)?;
        }
        Ok(())
    }

    fn element(&mut self, id: u32, env: &Env, layer: &mut Layer) -> Result<(), DecodeError> {
        self.visits += 1;
        if self.visits > MAX_VISITS {
            return Err(DecodeError::LimitExceeded("svg element visits"));
        }
        let doc = self.doc;
        let Some(el) = doc.get(id) else { return Ok(()) };
        let kind = el.name.as_str();
        if !(is_shape(kind) || matches!(kind, "g" | "a" | "switch" | "svg" | "use")) {
            return Ok(());
        }
        let style = style::compute(doc, self.rules, id, &env.style);
        if !style.display {
            return Ok(());
        }
        let mut ctm = env.ctm;
        if let Some(m) = el.attr("transform").and_then(parse_transform) {
            ctm = ctm.mul(&m);
        }
        let inner = Env {
            ctm,
            vp: env.vp,
            style: style.clone(),
            use_depth: env.use_depth,
        };
        let layered =
            (style.opacity < Q::ONE || style.clip_path.is_some()) && self.layers < MAX_LAYER_DEPTH;
        if !layered {
            return self.draw(id, el, &inner, layer);
        }
        let mut sub = Layer::new(self.w, self.h, &mut self.budget)?;
        self.layers += 1;
        let drawn = self.draw(id, el, &inner, &mut sub);
        self.layers -= 1;
        let result = drawn.and_then(|()| {
            if let Some(mask) = self.clip_mask(&style, &inner)? {
                sub.mask(&mask);
                self.budget.release(mask.len());
            }
            layer.over(&sub, to255(style.opacity));
            Ok(())
        });
        self.budget.release(sub.bytes());
        result
    }

    fn draw(&mut self, id: u32, el: &El, env: &Env, layer: &mut Layer) -> Result<(), DecodeError> {
        let kind = el.name.as_str();
        match kind {
            "svg" => {
                let (x, y) = (len_attr(el, "x", env.vp.0), len_attr(el, "y", env.vp.1));
                let w = opt_len(el, "width", env.vp.0).unwrap_or(env.vp.0);
                let h = opt_len(el, "height", env.vp.1).unwrap_or(env.vp.1);
                let mut inner = env.clone();
                let mut m = Mat::translate(x, y);
                if let Some(vb) = view_box(el) {
                    m = m.mul(&viewport_transform(
                        vb,
                        el.attr("preserveAspectRatio"),
                        w,
                        h,
                    ));
                    inner.vp = (vb.2, vb.3);
                } else {
                    inner.vp = (w, h);
                }
                inner.ctm = env.ctm.mul(&m);
                self.children(id, &inner, layer)
            }
            "use" => self.use_element(id, el, env, layer),
            k if is_shape(k) => self.shape(el, env, layer),
            _ => self.children(id, env, layer),
        }
    }

    fn use_element(
        &mut self,
        id: u32,
        el: &El,
        env: &Env,
        layer: &mut Layer,
    ) -> Result<(), DecodeError> {
        let doc = self.doc;
        let Some(target) = el
            .attr("href")
            .and_then(|h| h.trim().strip_prefix('#'))
            .and_then(|i| doc.by_id(i))
        else {
            return Ok(());
        };
        if env.use_depth >= MAX_USE_DEPTH {
            return Ok(());
        }
        // A use inside what it references would never end.
        let mut up = Some(id);
        while let Some(a) = up {
            if a == target {
                return Ok(());
            }
            up = doc.get(a).and_then(|e| e.parent);
        }
        let Some(tel) = doc.get(target) else {
            return Ok(());
        };
        let (x, y) = (len_attr(el, "x", env.vp.0), len_attr(el, "y", env.vp.1));
        let mut inner = env.clone();
        inner.ctm = env.ctm.mul(&Mat::translate(x, y));
        inner.use_depth += 1;
        if matches!(tel.name.as_str(), "symbol" | "svg") {
            let w = opt_len(el, "width", env.vp.0).unwrap_or(env.vp.0);
            let h = opt_len(el, "height", env.vp.1).unwrap_or(env.vp.1);
            inner.vp = (w, h);
            if let Some(vb) = view_box(tel) {
                inner.ctm = inner.ctm.mul(&viewport_transform(
                    vb,
                    tel.attr("preserveAspectRatio"),
                    w,
                    h,
                ));
                inner.vp = (vb.2, vb.3);
            }
            inner.style = style::compute(doc, self.rules, target, &env.style);
            if !inner.style.display {
                return Ok(());
            }
            return self.children(target, &inner, layer);
        }
        self.element(target, &inner, layer)
    }

    fn shape(&mut self, el: &El, env: &Env, layer: &mut Layer) -> Result<(), DecodeError> {
        let st = &env.style;
        if !st.visible {
            return Ok(());
        }
        let segs = geometry(el.name.as_str(), el, env.vp);
        if segs.is_empty() {
            return Ok(());
        }
        let scale = env.ctm.max_scale();
        let polys = flatten(&segs, scale, MAX_SHAPE_POINTS)?;
        self.points += polys.iter().map(|q| q.pts.len()).sum::<usize>();
        if self.points > MAX_TOTAL_POINTS {
            return Err(DecodeError::LimitExceeded("svg path points"));
        }
        let bb = bbox(&polys);
        if let Some(src) = self.resolve(&st.fill, st, bb, env) {
            let mut edges = Edges::new(MAX_EDGES);
            for poly in &polys {
                edges.add_poly(&poly.pts, &env.ctm, false)?;
            }
            self.fill_edges(&mut edges, st.fill_even_odd, &src, st.fill_opacity, layer)?;
        }
        let sw = st.stroke_width.resolve(diag(env.vp));
        if sw.0 > 0
            && let Some(src) = self.resolve(&st.stroke, st, bb, env)
        {
            let pattern = self.dash_pattern(st, env.vp);
            let dashed;
            let source: &[Poly] = if pattern.is_empty() {
                &polys
            } else {
                let off = st.dash_offset.resolve(diag(env.vp));
                dashed = dash(&polys, &pattern, off)?;
                &dashed
            };
            let ss = StrokeStyle {
                width: sw,
                cap: st.cap,
                join: st.join,
                miter_limit: st.miter_limit,
            };
            let mut edges = Edges::new(MAX_EDGES);
            stroke(source, &ss, scale, &mut |pts| {
                edges.add_poly(pts, &env.ctm, true)
            })?;
            self.fill_edges(&mut edges, false, &src, st.stroke_opacity, layer)?;
        }
        Ok(())
    }

    fn dash_pattern(&self, st: &Style, vp: Vp) -> Vec<Q> {
        let mut v: Vec<Q> = st.dash.iter().map(|l| l.resolve(diag(vp))).collect();
        if v.is_empty() || v.iter().any(|q| q.0 < 0) || v.iter().all(|q| q.0 == 0) {
            return Vec::new();
        }
        if v.len() % 2 == 1 {
            let again = v.clone();
            v.extend(again);
        }
        v
    }

    fn fill_edges(
        &mut self,
        edges: &mut Edges,
        even_odd: bool,
        src: &Src,
        opacity: Q,
        layer: &mut Layer,
    ) -> Result<(), DecodeError> {
        let alpha = to255(opacity);
        if alpha == 0 {
            return Ok(());
        }
        fill(
            edges,
            even_odd,
            self.w,
            self.h,
            &mut self.work,
            MAX_WORK,
            |y, x0, cov| match src {
                Src::Solid(c) => layer.paint_solid(y, x0, cov, *c, alpha),
                Src::Grad(g) => layer.paint_gradient(y, x0, cov, g, alpha),
            },
        )
    }

    fn resolve(
        &self,
        paint: &Paint,
        st: &Style,
        bb: Option<(Q, Q, Q, Q)>,
        env: &Env,
    ) -> Option<Src> {
        let color = |c: &Color| match c {
            Color::Rgba(v) => Src::Solid(*v),
            Color::Current => Src::Solid(st.color),
        };
        match paint {
            Paint::None => None,
            Paint::Color(c) => Some(color(c)),
            Paint::Url(id, fallback) => self.gradient(id, st, bb, env).or_else(|| match fallback {
                Some(Some(c)) => Some(color(c)),
                _ => None,
            }),
        }
    }

    fn gradient(&self, id: &str, st: &Style, bb: Option<(Q, Q, Q, Q)>, env: &Env) -> Option<Src> {
        let doc = self.doc;
        let mut chain: Vec<u32> = Vec::new();
        let mut cur = doc.by_id(id)?;
        for _ in 0..8 {
            let el = doc.get(cur)?;
            if !matches!(el.name.as_str(), "linearGradient" | "radialGradient") {
                break;
            }
            chain.push(cur);
            match el
                .attr("href")
                .and_then(|h| h.trim().strip_prefix('#'))
                .and_then(|i| doc.by_id(i))
            {
                Some(next) if !chain.contains(&next) => cur = next,
                _ => break,
            }
        }
        let first = doc.get(*chain.first()?)?;
        let attr = |n: &str| {
            chain
                .iter()
                .filter_map(|c| doc.get(*c))
                .find_map(|e| e.attr(n))
        };
        // Stops come from the first gradient in the chain that has any.
        let mut stops: Vec<(Q, [u8; 4])> = Vec::new();
        for c in &chain {
            let el = doc.get(*c)?;
            for &k in &el.kids {
                let Some(kel) = doc.get(k).filter(|e| e.name == "stop") else {
                    continue;
                };
                let s = style::compute(doc, self.rules, k, &Style::initial(st.color));
                let mut off = kel
                    .attr("offset")
                    .and_then(parse_fraction)
                    .unwrap_or(Q::ZERO);
                off = if off < Q::ZERO {
                    Q::ZERO
                } else if off > Q::ONE {
                    Q::ONE
                } else {
                    off
                };
                if let Some((prev, _)) = stops.last()
                    && off < *prev
                {
                    off = *prev;
                }
                let mut rgba = match s.stop_color {
                    Color::Rgba(v) => v,
                    Color::Current => st.color,
                };
                rgba[3] = (to255(s.stop_opacity) * u32::from(rgba[3]) / 255) as u8;
                stops.push((off, rgba));
                if stops.len() >= 256 {
                    break;
                }
            }
            if !stops.is_empty() {
                break;
            }
        }
        match stops.as_slice() {
            [] => return None,
            [(_, c)] => return Some(Src::Solid(*c)),
            _ => {}
        }
        let bbox_units = attr("gradientUnits") != Some("userSpaceOnUse");
        let mut m = env.ctm;
        if bbox_units {
            let (x, y, w, h) = bb?;
            if w.0 <= 0 || h.0 <= 0 {
                return None;
            }
            m = m.mul(&Mat::translate(x, y)).mul(&Mat::scaling(w, h));
        }
        if let Some(t) = attr("gradientTransform").and_then(parse_transform) {
            m = m.mul(&t);
        }
        let inv = m.invert()?;
        let get = |name: &str, default_pct: i64, basis: Q| -> Q {
            let l = attr(name).and_then(parse_length);
            match (l, bbox_units) {
                (Some(l), true) => {
                    if l.pct {
                        l.v.scale(1, 100)
                    } else {
                        l.v
                    }
                }
                (Some(l), false) => l.resolve(basis),
                (None, true) => Q::int(default_pct).scale(1, 100),
                (None, false) => (Q::int(default_pct) * basis).scale(1, 100),
            }
        };
        let (bx, by, br) = (env.vp.0, env.vp.1, diag(env.vp));
        let kind = if first.name == "linearGradient" {
            Kind::Linear {
                x1: get("x1", 0, bx),
                y1: get("y1", 0, by),
                x2: get("x2", 100, bx),
                y2: get("y2", 0, by),
            }
        } else {
            Kind::Radial {
                cx: get("cx", 50, bx),
                cy: get("cy", 50, by),
                r: get("r", 50, br),
            }
        };
        let spread = match attr("spreadMethod") {
            Some("reflect") => Spread::Reflect,
            Some("repeat") => Spread::Repeat,
            _ => Spread::Pad,
        };
        Some(Src::Grad(Box::new(Gradient {
            kind,
            stops,
            spread,
            inv,
        })))
    }

    /// The coverage mask of the element's `clip-path`, if it names a usable
    /// `<clipPath>` in user-space units.
    fn clip_mask(&mut self, st: &Style, env: &Env) -> Result<Option<Vec<u8>>, DecodeError> {
        let doc = self.doc;
        let Some(cp) = st
            .clip_path
            .as_deref()
            .and_then(|i| doc.by_id(i))
            .and_then(|i| doc.get(i))
            .filter(|e| {
                e.name == "clipPath" && e.attr("clipPathUnits") != Some("objectBoundingBox")
            })
        else {
            return Ok(None);
        };
        let mut mask = self.budget.zeroed(self.w * self.h)?;
        let mut base = env.ctm;
        if let Some(t) = cp.attr("transform").and_then(parse_transform) {
            base = base.mul(&t);
        }
        let w = self.w;
        for &k in &cp.kids {
            let Some(kel) = doc.get(k).filter(|e| is_shape(&e.name)) else {
                continue;
            };
            self.visits += 1;
            if self.visits > MAX_VISITS {
                self.budget.release(mask.len());
                return Err(DecodeError::LimitExceeded("svg element visits"));
            }
            let ks = style::compute(doc, self.rules, k, &Style::initial(st.color));
            if !ks.display {
                continue;
            }
            let mut m = base;
            if let Some(t) = kel.attr("transform").and_then(parse_transform) {
                m = m.mul(&t);
            }
            let segs = geometry(kel.name.as_str(), kel, env.vp);
            let polys = flatten(&segs, m.max_scale(), MAX_SHAPE_POINTS)?;
            let mut edges = Edges::new(MAX_EDGES);
            for poly in &polys {
                edges.add_poly(&poly.pts, &m, false)?;
            }
            let r = fill(
                &mut edges,
                ks.clip_even_odd,
                self.w,
                self.h,
                &mut self.work,
                MAX_WORK,
                |y, x0, cov| {
                    let start = (y * w) + x0;
                    if let Some(row) = mask.get_mut(start..start + cov.len()) {
                        for (d, c) in row.iter_mut().zip(cov) {
                            let (a, b) = (u32::from(*d), u32::from(*c));
                            *d = (a + b - super::paint::div255(a * b)).min(255) as u8;
                        }
                    }
                },
            );
            if let Err(e) = r {
                self.budget.release(mask.len());
                return Err(e);
            }
        }
        Ok(Some(mask))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn _pt(x: i64, y: i64) -> super::super::geom::P {
        p(Q::int(x), Q::int(y))
    }

    #[test]
    fn viewport_math() {
        let vb = (Q::ZERO, Q::ZERO, Q::int(10), Q::int(10));
        // Meet, centred: a 10x10 box in 40x20 is scaled 2 and shifted 10.
        let m = viewport_transform(vb, None, Q::int(40), Q::int(20));
        let o = m.apply(_pt(0, 0));
        assert_eq!((o.x, o.y), (Q::int(10), Q::ZERO));
        assert_eq!(m.apply(_pt(10, 10)).x, Q::int(30));
        // Slice scales up to cover.
        let m = viewport_transform(vb, Some("xMinYMin slice"), Q::int(40), Q::int(20));
        assert_eq!(m.apply(_pt(10, 10)).x, Q::int(40));
        assert_eq!(m.apply(_pt(10, 10)).y, Q::int(40));
        // None stretches.
        let m = viewport_transform(vb, Some("none"), Q::int(40), Q::int(20));
        assert_eq!(m.apply(_pt(10, 10)), _pt(40, 20));
        // xMax aligns right.
        let m = viewport_transform(vb, Some("xMaxYMid meet"), Q::int(40), Q::int(20));
        assert_eq!(m.apply(_pt(0, 0)).x, Q::int(20));
    }
}
