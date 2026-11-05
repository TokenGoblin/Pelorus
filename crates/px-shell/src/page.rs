//! One page through the engine: parse, style, layout, display list, and a
//! viewport raster at a scroll offset. Also the link boxes, for hit testing.
//!
//! Everything here is in-process (see this crate's CLAUDE.md). Layout never
//! fetches anything: a local page reads relative stylesheets next to its file (a
//! scheme, host or `..` is skipped); a web page's stylesheets were fetched by
//! [`crate::web`] through `px-net` beforehand and arrive in [`Resources`].

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use px_layout::block::layout_document;
use px_layout::geom::{Au, px};
use px_paint::raster::Canvas;
use px_paint::{DisplayList, Item};

use crate::fonts::LayoutFonts;

const AU_PER_PX: i64 = 60;

/// Enough of a user-agent stylesheet for block layout, plus a default face and
/// size so an ordinary page reads as text and not as Ahem squares.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     table, form, fieldset, pre, hr, dl, dd, dt { display: block } \
     html { font-family: \"Noto Sans\"; font-size: 16px; line-height: 1.4; color: #1b1b1f } \
     body { margin: 8px } \
     p, blockquote, figure { margin: 1em 0 } \
     h1 { margin: 0.67em 0; font-size: 2em } \
     h2 { margin: 0.83em 0; font-size: 1.5em } \
     h3 { margin: 1em 0; font-size: 1.17em } \
     ul, ol { margin: 1em 0; padding-left: 40px } \
     h4 { margin: 1.33em 0 } \
     h5 { margin: 1.67em 0; font-size: 0.83em } \
     h6 { margin: 2.33em 0; font-size: 0.67em } \
     pre { margin: 1em 0; white-space: pre } \
     dl { margin: 1em 0 } dd { margin-left: 40px } \
     a { color: #0b57d0 } \
     head, style, script, noscript, template, title, meta, link { display: none }";

/// A clickable region of the document, in CSS pixels from its top-left.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkBox {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub href: String,
}

impl LinkBox {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A laid-out document at one viewport width.
pub struct Laid {
    pub width: u32,
    /// Document height in CSS px (the lowest fragment edge).
    pub height: i32,
    pub title: String,
    pub links: Vec<LinkBox>,
    list: DisplayList,
}

fn attr<'a>(node: &'a px_dom::Node, local: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|a| &*a.name.local == local)
        .map(|a| &*a.value)
}

/// A relative stylesheet reference next to the page; anything with a scheme,
/// host or absolute path is refused (no network, no wandering the filesystem).
fn resolve_css(page: &Path, href: &str) -> Option<PathBuf> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    if href.is_empty() || href.contains(':') || href.starts_with('/') || href.contains("..") {
        return None;
    }
    Some(page.parent()?.join(href))
}

/// What a page's subresources resolved to, fetched before layout.
///
/// A local page has `file` (relative stylesheets are read next to it). A web
/// page has `base` (its final URL) and `fetched`, the stylesheets the network
/// loader returned keyed by absolute URL; nothing is fetched during layout.
/// `blocked_inline` holds the text of inline `<style>` elements the page's
/// Content-Security-Policy refuses.
#[derive(Clone, Debug, Default)]
pub struct Resources {
    pub file: Option<PathBuf>,
    pub base: Option<String>,
    pub fetched: HashMap<String, String>,
    pub blocked_inline: HashSet<String>,
}

/// A `<link rel=stylesheet>` the engine should apply: not `alternate`, not
/// print-only. Returns its `href`.
fn stylesheet_href(node: &px_dom::Node) -> Option<&str> {
    let rel = attr(node, "rel").unwrap_or("").to_ascii_lowercase();
    let mut words = rel.split_ascii_whitespace();
    if !words.clone().any(|w| w == "stylesheet") || words.any(|w| w == "alternate") {
        return None;
    }
    if attr(node, "media").is_some_and(|m| m.trim().eq_ignore_ascii_case("print")) {
        return None;
    }
    if attr(node, "disabled").is_some() {
        return None;
    }
    attr(node, "href")
}

/// The stylesheet links and inline style texts of a source, in document order:
/// what the network loader must fetch and what the CSP must vet.
#[must_use]
pub fn subresource_plan(source: &str) -> (Vec<String>, Vec<String>) {
    let dom = px_dom::parse(source);
    let arena = dom.arena;
    let (mut links, mut styles) = (Vec::new(), Vec::new());
    for id in arena.descendants(arena.document()) {
        let Some(node) = arena.get(id) else { continue };
        match node.element_name().map(|n| n.local.as_ref()) {
            Some("style") => styles.push(px_layout::inline::collect_text(&arena, id)),
            Some("link") => {
                if let Some(h) = stylesheet_href(node) {
                    links.push(h.to_string());
                }
            }
            _ => {}
        }
    }
    (links, styles)
}

fn stylesheets(res: &Resources, arena: &px_dom::Arena) -> Vec<String> {
    let mut out = Vec::new();
    for id in arena.descendants(arena.document()) {
        let Some(node) = arena.get(id) else { continue };
        let Some(name) = node.element_name() else {
            continue;
        };
        match name.local.as_ref() {
            "style" => {
                let text = px_layout::inline::collect_text(arena, id);
                if !res.blocked_inline.contains(&text) {
                    out.push(text);
                }
            }
            "link" => {
                let Some(href) = stylesheet_href(node) else {
                    continue;
                };
                let css = if let Some(base) = &res.base {
                    px_net::browse::resolve(base, href).and_then(|u| res.fetched.get(&u).cloned())
                } else {
                    res.file
                        .as_deref()
                        .and_then(|p| resolve_css(p, href))
                        .and_then(|p| std::fs::read_to_string(p).ok())
                };
                out.extend(css);
            }
            _ => {}
        }
    }
    out
}

fn title_of(arena: &px_dom::Arena) -> String {
    for id in arena.descendants(arena.document()) {
        if let Some(n) = arena.get(id)
            && n.element_name().is_some_and(|q| &*q.local == "title")
        {
            let t = px_layout::inline::collect_text(arena, id);
            return t.split_whitespace().collect::<Vec<_>>().join(" ");
        }
    }
    String::new()
}

/// The `href` of the nearest `<a href>` at or above `node`.
fn link_of(arena: &px_dom::Arena, node: px_dom::NodeId) -> Option<String> {
    let of = |id: px_dom::NodeId| -> Option<String> {
        let n = arena.get(id)?;
        if n.element_name()?.local.as_ref() == "a" {
            attr(n, "href").map(str::to_string)
        } else {
            None
        }
    };
    of(node).or_else(|| arena.ancestors(node).find_map(of))
}

fn px_of(a: Au) -> i32 {
    // Floor for origins, matching the rasterizer's snapping closely enough for
    // a hit box.
    (i64::from(a.0)).div_euclid(AU_PER_PX) as i32
}

fn ceil_px(a: Au) -> i32 {
    ((i64::from(a.0) + AU_PER_PX - 1).div_euclid(AU_PER_PX)) as i32
}

/// Run the pipeline. `page` is the file the source came from (for relative
/// stylesheets); `None` for a built-in page. Never panics: an engine panic is
/// reported as an error.
pub fn layout(source: &str, page: &Resources, width: u32, height: u32) -> Result<Laid, String> {
    catch_unwind(AssertUnwindSafe(|| {
        layout_inner(source, page, width, height)
    }))
    .map_err(|_| "the engine panicked on this page".to_string())?
}

fn layout_inner(source: &str, page: &Resources, width: u32, height: u32) -> Result<Laid, String> {
    let width = width.max(1);
    let height = height.max(1);
    let dom = px_dom::parse(source);
    if dom.abandoned {
        return Err("parse abandoned".into());
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(width as f32, height as f32, quirks);
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");
    for css in stylesheets(page, &arena) {
        engine.add_author_stylesheet(&css, "about:page");
    }
    let style_root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &style_root)
        .ok_or("style resolve failed")?;
    let tree = layout_document(&arena, &style_root, px(width as i32)).ok_or("layout failed")?;

    let mut bottom: i64 = 0;
    let mut links = Vec::new();
    for (_, id) in tree.in_layout_order() {
        let Some(f) = tree.get(id) else { continue };
        bottom = bottom.max(i64::from(f.block_offset.0) + i64::from(f.size.block.0));
        if let Some(n) = f.node
            && !matches!(f.kind, px_layout::fragment::FragmentKind::Line)
            && let Some(href) = link_of(&arena, n)
        {
            let (x, y) = (px_of(f.inline_offset), px_of(f.block_offset));
            links.push(LinkBox {
                x,
                y,
                w: ceil_px(Au(f.inline_offset.0.saturating_add(f.size.inline.0))) - x,
                h: ceil_px(Au(f.block_offset.0.saturating_add(f.size.block.0))) - y,
                href,
            });
        }
        for run in tree.text_runs(id) {
            if let Some(n) = run.node
                && let Some(href) = link_of(&arena, n)
            {
                let x0 = f.inline_offset.0.saturating_add(run.inline_offset.0);
                let x = px_of(Au(x0));
                let y = px_of(f.block_offset);
                links.push(LinkBox {
                    x,
                    y,
                    w: ceil_px(Au(x0.saturating_add(run.width.0))) - x,
                    h: ceil_px(Au(f.block_offset.0.saturating_add(f.size.block.0))) - y,
                    href,
                });
            }
        }
    }
    let doc_px = ((bottom + AU_PER_PX - 1) / AU_PER_PX).clamp(0, i64::from(i32::MAX)) as i32;

    let list = px_paint::build_with_viewport(
        &arena,
        &style_root,
        &tree,
        px(width as i32),
        px(doc_px.max(height as i32)),
    );
    Ok(Laid {
        width,
        height: doc_px,
        title: title_of(&arena),
        links,
        list,
    })
}

impl Laid {
    /// The link under a document point (topmost wins).
    pub fn link_at(&self, x: i32, y: i32) -> Option<&str> {
        self.links
            .iter()
            .rev()
            .find(|l| l.contains(x, y))
            .map(|l| l.href.as_str())
    }

    /// The largest useful scroll offset for a viewport `view_h` tall.
    pub fn max_scroll(&self, view_h: i32) -> i32 {
        (self.height - view_h).max(0)
    }

    /// Rasterize the viewport: the display list shifted up by `scroll` px, drawn
    /// on a `width` by `view_h` canvas, with an overlay scrollbar when the
    /// document is taller than the view.
    pub fn render(&self, scroll: i32, width: u32, view_h: u32) -> Result<Canvas, String> {
        let dy = Au(i32::try_from(i64::from(scroll) * AU_PER_PX).unwrap_or(i32::MAX));
        let shift = |a: Au| Au(a.0.saturating_sub(dy.0));
        let mut list = self.list.clone();
        for item in &mut list.items {
            match item {
                Item::FillRect { rect, .. }
                | Item::TextRun { rect, .. }
                | Item::Border { rect, .. }
                | Item::PushClip { rect } => rect.y = shift(rect.y),
                Item::GlyphRun { .. } | Item::PopClip => {}
            }
        }
        for g in &mut list.glyphs {
            g.y = shift(g.y);
        }
        let mut cv = px_paint::raster::rasterize_with(&list, width, view_h, &LayoutFonts)
            .map_err(|e| format!("raster: {e}"))?;
        if self.height > view_h as i32 {
            scrollbar(&mut cv, scroll, self.height);
        }
        Ok(cv)
    }
}

/// A thin overlay thumb on the right edge, 35% black over whatever is there.
fn scrollbar(cv: &mut Canvas, scroll: i32, doc_h: i32) {
    let (w, h) = (cv.width as i32, cv.height as i32);
    let thumb_h =
        ((i64::from(h) * i64::from(h)) / i64::from(doc_h.max(1))).clamp(24, i64::from(h)) as i32;
    let travel = (h - thumb_h).max(0);
    let range = (doc_h - h).max(1);
    let top = (i64::from(scroll.clamp(0, range)) * i64::from(travel) / i64::from(range)) as i32;
    for y in top..(top + thumb_h).min(h) {
        for x in (w - 8).max(0)..(w - 2).max(0) {
            let i = (y as usize * w as usize + x as usize) * 4;
            if let Some(px4) = cv.pixels.get_mut(i..i + 3) {
                for c in px4 {
                    *c = (u16::from(*c) * 65 / 100) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<!doctype html><title> Hi  there </title><body style='margin:0'>\
        <p style='margin:0;height:40px'><a href='next.html'>go</a></p>\
        <div style='height:2000px'></div>";

    #[test]
    fn title_height_and_links() {
        let l = layout(PAGE, &Resources::default(), 400, 300).unwrap();
        assert_eq!(l.title, "Hi there");
        assert_eq!(l.height, 2040);
        assert_eq!(l.width, 400);
        assert_eq!(l.link_at(5, 5), Some("next.html"));
        assert_eq!(l.link_at(300, 5), None);
        assert_eq!(l.link_at(5, 900), None);
        assert_eq!(l.max_scroll(300), 1740);
    }

    #[test]
    fn the_viewport_follows_the_scroll_offset() {
        let src = "<!doctype html><body style='margin:0'>\
            <div style='height:100px;background:#f00'></div>\
            <div style='height:100px;background:#00f'></div>";
        let l = layout(src, &Resources::default(), 50, 100).unwrap();
        let top = l.render(0, 50, 100).unwrap();
        assert_eq!(top.pixel(10, 10), Some([255, 0, 0, 255]));
        let down = l.render(100, 50, 100).unwrap();
        assert_eq!(down.pixel(10, 10), Some([0, 0, 255, 255]));
    }

    #[test]
    fn a_width_change_relayouts() {
        let src = "<!doctype html><body style='margin:0'><p style='margin:0'>\
            word word word word word word word word word word word word word word</p>";
        let wide = layout(src, &Resources::default(), 900, 300).unwrap();
        let narrow = layout(src, &Resources::default(), 120, 300).unwrap();
        assert!(narrow.height > wide.height);
    }

    #[test]
    fn stylesheet_references_stay_local() {
        assert!(resolve_css(Path::new("/a/b.html"), "http://x/y.css").is_none());
        assert!(resolve_css(Path::new("/a/b.html"), "//x/y.css").is_none());
        assert!(resolve_css(Path::new("/a/b.html"), "/etc/passwd").is_none());
        assert!(resolve_css(Path::new("/a/b.html"), "../c.css").is_none());
        assert_eq!(
            resolve_css(Path::new("/a/b.html"), "s/c.css"),
            Some(PathBuf::from("/a/s/c.css"))
        );
    }
}
