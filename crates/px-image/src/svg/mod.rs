//! Static SVG rendering (ADR 049).
//!
//! An SVG used as an image is a picture, not a program: this module parses
//! the document, resolves its style, and draws it to RGBA8 at a given size.
//! It never runs script, never reads an event attribute, and never loads
//! anything: `<image>`, `<foreignObject>`, `<script>`, `<animate>` and
//! friends are not rendered; `href` and `url()` are followed only when they
//! name an element of the same document (`#id`); stylesheet at-rules are
//! skipped, so `@import` fetches nothing. See [`xml`] for the parser's entity
//! rules and [`render`] for the work bounds.
//!
//! # Supported
//!
//! `svg` (nested too), `g`, `a`, `switch`, `defs`, `symbol`, `use`, `rect`,
//! `circle`, `ellipse`, `line`, `polyline`, `polygon`, `path` (every command,
//! arcs included); `fill`, `fill-opacity`, `fill-rule`, `stroke` and its
//! width, cap, join, miter limit, dash array and offset; `opacity` (as a
//! group); `transform`; `viewBox` with `preserveAspectRatio`; solid colours
//! (hex, `rgb()`, `hsl()`, names), `currentColor`; `linearGradient` and
//! `radialGradient` (units, transform, spread, `href` inheritance; the focal
//! point is the centre); `clip-path` to a user-space `<clipPath>`;
//! presentation attributes, `style=""` and a `<style>` element with simple
//! selectors; `display` and `visibility`.
//!
//! # Not (yet)
//!
//! `text`, `image`, `mask`, `pattern`, `marker`, `filter`, `foreignObject`,
//! `clipPath` in bounding-box units, nested-`svg` overflow clipping, animation.
//! Those are skipped, not approximated.
//!
//! # Numbers
//!
//! No floating point. Lengths and matrices are Q16.16 in an `i64`
//! ([`fixed::Q`]) that saturates, angles are Q30 radians from a CORDIC, and
//! the rasterizer samples 16 scanlines per pixel with exact 1/256 pixel
//! spans. The same document gives the same pixels everywhere.

mod css;
mod doc;
mod fixed;
mod geom;
mod paint;
mod path;
mod raster;
mod render;
mod stroke;
mod style;
mod values;
mod xml;

pub use doc::{Builder, MAX_DEPTH, MAX_ELEMENTS};

use crate::limits::Budget;
use crate::{DecodeError, Image, Limits};
use fixed::Q;
use render::{Renderer, ViewBox, view_box};
use values::parse_length;

/// Largest SVG source accepted, in bytes.
pub const MAX_SOURCE_BYTES: usize = 16 << 20;

/// The size used when a document gives none (the CSS default object size).
const DEFAULT_SIZE: (i64, i64) = (300, 150);

/// A parsed SVG document, ready to render at any size.
#[derive(Debug)]
pub struct Document {
    doc: doc::Doc,
    rules: Vec<css::Rule>,
}

/// `<svg ...>` in the first bytes, after optional BOM, whitespace, comments,
/// an XML declaration or a doctype. A cheap check; [`Document::parse`] is the
/// real test.
pub fn looks_like_svg(data: &[u8]) -> bool {
    let d = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    let d = match d.iter().position(|b| !b.is_ascii_whitespace()) {
        Some(i) => d.get(i..).unwrap_or(&[]),
        None => return false,
    };
    if !d.starts_with(b"<") {
        return false;
    }
    let head = d.get(..d.len().min(4096)).unwrap_or(d);
    head.windows(4).any(|w| w == b"<svg")
}

impl Document {
    /// Parse SVG source bytes (UTF-8).
    pub fn parse(data: &[u8]) -> Result<Document, DecodeError> {
        if data.len() > MAX_SOURCE_BYTES {
            return Err(DecodeError::LimitExceeded("svg source"));
        }
        let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
        if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
            return Err(DecodeError::Unsupported("UTF-16 SVG"));
        }
        let text = String::from_utf8_lossy(data);
        Document::from_doc(xml::parse(&text)?)
    }

    /// Finish a document built by a [`Builder`] (the HTML DOM's inline SVG).
    pub fn from_builder(b: Builder) -> Result<Document, DecodeError> {
        Document::from_doc(b.finish()?)
    }

    fn from_doc(doc: doc::Doc) -> Result<Document, DecodeError> {
        let mut rules = Vec::new();
        for el in &doc.els {
            if el.name == "style" && matches!(el.attr("type"), None | Some("" | "text/css")) {
                css::parse_sheet(&el.text, &mut rules);
            }
        }
        Ok(Document { doc, rules })
    }

    fn root(&self) -> Option<&doc::El> {
        self.doc.get(0)
    }

    /// Whether the document has a `viewBox` but no absolute `width` or
    /// `height`: it has an aspect ratio and no size, so a replaced element
    /// showing it fills the width it is given (CSS Sizing 3 section 5.1)
    /// instead of taking [`Document::intrinsic_size`].
    pub fn is_ratio_only(&self) -> bool {
        let Some(root) = self.root() else {
            return false;
        };
        let abs = |n: &str| {
            root.attr(n)
                .and_then(parse_length)
                .is_some_and(|l| !l.pct && l.v.0 > 0)
        };
        !abs("width") && !abs("height") && view_box(root).is_some()
    }

    /// Let page style sheets reach into this document, as they reach into an
    /// inline `<svg>` in a browser. `css` is the page's author CSS text and
    /// `chain` the HTML ancestors of the `<svg>` element, innermost first, as
    /// `(tag, id, class)`. Only the selector subset of [`css`] is understood
    /// (at-rules, pseudo-classes and attribute selectors drop their rule), and
    /// only rules that match something in this document are kept. A rule from
    /// the page ranks below the document's own `<style>` rules at equal
    /// specificity, and above presentation attributes, as in CSS.
    pub fn add_page_style(&mut self, css: &str, chain: &[(String, String, String)]) {
        /// Page CSS scanned, bytes.
        const MAX_PAGE_CSS: usize = 2 << 20;
        /// Page rules parsed before filtering.
        const MAX_PAGE_RULES: usize = 50_000;
        /// Rule-by-element match tests allowed while filtering.
        const MAX_MATCH_WORK: usize = 4_000_000;
        self.doc.set_outer(chain);
        let mut end = css.len().min(MAX_PAGE_CSS);
        while !css.is_char_boundary(end) {
            end -= 1;
        }
        let mut page = Vec::new();
        css::parse_sheet_max(css.get(..end).unwrap_or(""), &mut page, MAX_PAGE_RULES);
        let n = u32::try_from(self.doc.els.len()).unwrap_or(u32::MAX);
        if page.len().saturating_mul(n as usize) > MAX_MATCH_WORK {
            return;
        }
        page.retain(|r| (0..n).any(|e| r.matches(&self.doc, e)));
        page.truncate(css::MAX_RULES);
        for (i, r) in page.iter_mut().enumerate() {
            r.order = i;
        }
        let k = page.len();
        for r in &mut self.rules {
            r.order = r.order.saturating_add(k);
        }
        page.append(&mut self.rules);
        self.rules = page;
    }

    /// The intrinsic size in CSS pixels: the root's `width` and `height` when
    /// absolute lengths, else derived from the `viewBox` ratio, else 300x150.
    pub fn intrinsic_size(&self) -> (u32, u32) {
        let (w, h) = self.intrinsic_q();
        let px = |q: Q| u32::try_from(q.round().clamp(1, 1 << 16)).unwrap_or(1);
        (px(w), px(h))
    }

    fn intrinsic_q(&self) -> (Q, Q) {
        let Some(root) = self.root() else {
            return (Q::int(DEFAULT_SIZE.0), Q::int(DEFAULT_SIZE.1));
        };
        let abs = |n: &str| {
            root.attr(n)
                .and_then(parse_length)
                .filter(|l| !l.pct && l.v.0 > 0)
                .map(|l| l.v)
        };
        let (w, h, vb) = (abs("width"), abs("height"), view_box(root));
        match (w, h, vb) {
            (Some(w), Some(h), _) => (w, h),
            (Some(w), None, Some((_, _, bw, bh))) => (w, w * bh / bw),
            (None, Some(h), Some((_, _, bw, bh))) => (h * bw / bh, h),
            (Some(w), None, None) => (w, Q::int(DEFAULT_SIZE.1)),
            (None, Some(h), None) => (Q::int(DEFAULT_SIZE.0), h),
            (None, None, Some((_, _, bw, bh))) => {
                // Only a ratio: fit inside the default object size.
                let (dw, dh) = (Q::int(DEFAULT_SIZE.0), Q::int(DEFAULT_SIZE.1));
                if dw * bh <= dh * bw {
                    (dw, dw * bh / bw)
                } else {
                    (dh * bw / bh, dh)
                }
            }
            (None, None, None) => (dw_q(), dh_q()),
        }
    }

    /// Render at `width` by `height` pixels. `current_color` is what
    /// `currentColor` means at the root.
    pub fn render(
        &self,
        width: u32,
        height: u32,
        current_color: [u8; 4],
        limits: &Limits,
    ) -> Result<Image, DecodeError> {
        let budget = Budget::new(limits);
        budget.check_dims(u64::from(width), u64::from(height))?;
        let (iw, ih) = self.intrinsic_q();
        let view: ViewBox = self
            .root()
            .and_then(view_box)
            .unwrap_or((Q::ZERO, Q::ZERO, iw, ih));
        let (w, h) = (width as usize, height as usize);
        let mut r = Renderer::new(&self.doc, &self.rules, budget, w, h);
        let layer = r.render(view, current_color)?;
        Ok(Image {
            width,
            height,
            rgba: layer.unpremultiplied(),
        })
    }
}

fn dw_q() -> Q {
    Q::int(DEFAULT_SIZE.0)
}
fn dh_q() -> Q {
    Q::int(DEFAULT_SIZE.1)
}

/// Parse and render at the intrinsic size, `currentColor` black.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Image, DecodeError> {
    let doc = Document::parse(data)?;
    let (w, h) = doc.intrinsic_size();
    doc.render(w, h, [0, 0, 0, 255], limits)
}
