//! One page through the engine: parse, style, layout, display list, and a
//! viewport raster at a scroll offset. Also the link boxes, for hit testing.
//!
//! Everything here is in-process (see this crate's CLAUDE.md). Layout never
//! fetches anything: a local page reads relative stylesheets next to its file (a
//! scheme, host or `..` is skipped); a web page's stylesheets were fetched by
//! [`crate::web`] through `px-net` beforehand and arrive in [`Resources`].

use std::collections::{BTreeMap, HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use px_layout::geom::{Au, px};
use px_paint::raster::Canvas;
use px_paint::{DisplayList, Item};

use crate::fonts::LayoutFonts;

const AU_PER_PX: i64 = 60;

/// Enough of a user-agent stylesheet for block layout, plus a default face and
/// size so an ordinary page reads as text and not as Ahem squares.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     form, fieldset, pre, hr, dl, dd, dt { display: block } \
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
     li { display: list-item } \
     b, strong, th { font-weight: bolder } \
     i, em, cite, dfn, var { font-style: italic } \
     small { font-size: smaller } \
     code, kbd, samp, tt { font-family: monospace } \
     hr { margin: 0.5em auto; border: 1px inset; color: gray } \
     [hidden] { display: none } \
     head, style, script, template, title, meta, link, source, track { display: none }";

/// Parse a page the way a browser with scripting disabled does (HTML, "scripting
/// flag"): this shell never runs a script, so a `<noscript>` element's contents
/// are markup, not a single text node, and they render (the user-agent
/// stylesheet of HTML section 15 hides `noscript` only when scripting is enabled).
fn parse_page(source: &str) -> px_dom::Dom {
    px_dom::parse_with(source, px_dom::ParseOptions { scripting: false })
}

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
    /// Decoded pictures by key (see [`Resources::key`]): what the image loader
    /// fetched and `px-image` decoded. A picture that failed is simply absent.
    pub images: HashMap<String, Arc<px_paint::ImageData>>,
    /// The page's `@font-face` fonts that loaded (see [`crate::fonts`]); empty
    /// when it declares none or none arrived. Layout reads them through
    /// `px_layout::fontset::with_page_fonts`.
    pub fonts: Arc<px_layout::fontset::PageFonts>,
}

/// Percent-decode `s` (bytes that are not valid UTF-8 after decoding make it
/// `None`).
fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b.get(i) == Some(&b'%')
            && let Some(h) = s.get(i + 1..i + 3)
            && let Ok(v) = u8::from_str_radix(h, 16)
        {
            out.push(v);
            i += 3;
        } else {
            out.push(*b.get(i)?);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// `file://` + an absolute path, as a URL a CSS engine can resolve against.
/// On Windows the path is `C:\dir\p.html`, and `file://C:\dir\p.html` is a
/// host `C` plus garbage on any parser that does not carry the drive-letter
/// quirk; the URL is `file:///C:/dir/p.html`.
fn file_url(path: &str) -> String {
    let slashed = path.replace('\\', "/");
    if slashed.starts_with('/') {
        format!("file://{slashed}")
    } else {
        format!("file:///{slashed}")
    }
}

/// The path in a `file:` URL, with the leading `/` of `/C:/dir` dropped: that
/// slash belongs to the URL, and `/C:/dir` is not a path on Windows.
fn drive_path(path: &str) -> String {
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        path[1..].to_string()
    } else {
        path.to_string()
    }
}

impl Resources {
    /// The key a reference to an image goes by: the absolute URL (fragment
    /// dropped) for a web page; for a local page the path of a file at or below
    /// the page's directory, as a string. `None` for anything else: a local page
    /// cannot reach the web, a web page cannot reach `file:`, `data:` and the
    /// like are not fetched, and `..` does not leave the directory.
    #[must_use]
    pub fn key(&self, raw: &str) -> Option<String> {
        if let Some(base) = &self.base {
            let abs = px_net::browse::resolve(base, raw)?;
            return Some(abs.split('#').next().unwrap_or("").to_string());
        }
        let page = self.file.as_deref()?;
        let dir = std::path::absolute(page).ok()?.parent()?.to_path_buf();
        let raw = raw.trim();
        let path = if let Some(rest) = raw.strip_prefix("file://") {
            PathBuf::from(drive_path(&percent_decode(rest.split(['#', '?']).next()?)?))
        } else {
            let r = raw.split(['#', '?']).next()?;
            if r.is_empty() || r.contains(':') || r.starts_with('/') || r.contains("..") {
                return None;
            }
            dir.join(percent_decode(r)?)
        };
        let abs = std::path::absolute(&path).ok()?;
        if abs
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
            || !abs.starts_with(&dir)
        {
            return None;
        }
        Some(abs.to_string_lossy().into_owned())
    }

    /// The URL relative references in an inline `<style>` resolve against: the
    /// document's URL on the web, `file://` + the page's path for a local page.
    fn sheet_base(&self, _own: Option<&str>) -> String {
        if let Some(b) = &self.base {
            return b.clone();
        }
        match self
            .file
            .as_deref()
            .and_then(|p| std::path::absolute(p).ok())
        {
            Some(p) => file_url(&p.to_string_lossy()),
            None => "about:page".to_string(),
        }
    }
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
    let dom = parse_page(source);
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

/// One stylesheet to apply: its text, the URL its relative references resolve
/// against, and the `media` attribute of its `<style>` or `<link>` (empty when
/// absent, which matches always).
struct Sheet {
    css: String,
    base: String,
    media: String,
}

fn add_sheets(engine: &mut px_css::engine::StyleEngine, res: &Resources, arena: &px_dom::Arena) {
    for sheet in stylesheets(res, arena) {
        engine.add_author_stylesheet_for_media(&sheet.css, &sheet.base, &sheet.media);
    }
}

fn stylesheets(res: &Resources, arena: &px_dom::Arena) -> Vec<Sheet> {
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
                    out.push(Sheet {
                        css: text,
                        base: res.sheet_base(None),
                        media: attr(node, "media").unwrap_or("").to_string(),
                    });
                }
            }
            "link" => {
                let Some(href) = stylesheet_href(node) else {
                    continue;
                };
                let css = if let Some(base) = &res.base {
                    px_net::browse::resolve(base, href)
                        .and_then(|u| res.fetched.get(&u).cloned().map(|c| (c, u)))
                } else {
                    res.file
                        .as_deref()
                        .and_then(|p| resolve_css(p, href))
                        .and_then(|p| {
                            let base = std::path::absolute(&p).map_or_else(
                                |_| res.sheet_base(None),
                                |a| file_url(&a.to_string_lossy()),
                            );
                            std::fs::read_to_string(&p).ok().map(|c| (c, base))
                        })
                };
                let media = attr(node, "media").unwrap_or("");
                out.extend(css.map(|(css, base)| Sheet {
                    css,
                    base,
                    media: media.to_string(),
                }));
            }
            _ => {}
        }
    }
    out
}

fn title_of(arena: &px_dom::Arena) -> String {
    for id in arena.descendants(arena.document()) {
        if let Some(n) = arena.get(id)
            && n.element_name()
                .is_some_and(|q| &*q.local == "title" && &*q.ns == "http://www.w3.org/1999/xhtml")
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
    let fonts = page.fonts.clone();
    catch_unwind(AssertUnwindSafe(|| {
        px_layout::fontset::with_page_fonts(fonts, || layout_inner(source, page, width, height))
    }))
    .map_err(|_| "the engine panicked on this page".to_string())?
}

fn layout_inner(source: &str, page: &Resources, width: u32, height: u32) -> Result<Laid, String> {
    let width = width.max(1);
    let height = height.max(1);
    let dom = parse_page(source);
    if dom.abandoned {
        return Err("parse abandoned".into());
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new(width as f32, height as f32, quirks);
    engine.add_user_agent_stylesheet(UA_STYLESHEET, "about:ua");
    add_sheets(&mut engine, page, &arena);
    let style_root = engine
        .style_document(&mut arena)
        .ok_or("style resolve failed")?;
    let (images, sizes) = image_tables(&arena, &style_root, page, width);
    let tree = px_layout::replaced::layout_document_with_images(
        &arena,
        &style_root,
        px(width as i32),
        Some(px(height as i32)),
        &sizes,
    )
    .ok_or("layout failed")?;

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

    let list = px_paint::build_with_images(
        &arena,
        &style_root,
        &tree,
        px(width as i32),
        px(doc_px.max(height as i32)),
        &images,
    );
    Ok(Laid {
        width,
        height: doc_px,
        title: title_of(&arena),
        links,
        list,
    })
}

/// The distinct `background-image` URLs the styled document uses, in document
/// order, as the style engine resolved them.
fn background_urls(arena: &px_dom::Arena, root: &px_css::view::StyleRoot) -> Vec<String> {
    px_css::view::with_dom(arena, root, |dom| {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for id in arena.descendants(arena.document()) {
            for layer in px_css::image_style::background_layers(dom, id) {
                if seen.insert(layer.url.clone()) {
                    out.push(layer.url);
                }
            }
        }
        out
    })
    .unwrap_or_default()
}

/// What paint and layout get to know about the page's pictures: each `<img>`
/// that has one (the first of its candidates, in preference order, that was
/// fetched and decoded) and each background URL that has one.
fn image_tables(
    arena: &px_dom::Arena,
    root: &px_css::view::StyleRoot,
    res: &Resources,
    width: u32,
) -> (px_paint::ImageSet, BTreeMap<px_dom::NodeId, (u32, u32)>) {
    let mut set = px_paint::ImageSet::default();
    let mut sizes = BTreeMap::new();
    // Inline `<svg>` elements are pictures drawn from their own markup.
    let inline = px_css::view::with_dom(arena, root, |dom| {
        px_paint::svg::inline_images(arena, &|n| px_css::image_style::text_color(dom, n))
    })
    .unwrap_or_default();
    for (id, img) in inline {
        sizes.insert(id, (img.width, img.height));
        set.elements.insert(id, img);
    }
    if res.images.is_empty() {
        return (set, sizes);
    }
    for id in arena.descendants(arena.document()) {
        if arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_none_or(|q| &*q.local != "img")
        {
            continue;
        }
        let found = crate::images::candidates(arena, id, width)
            .into_iter()
            .find_map(|raw| res.key(&raw).and_then(|k| res.images.get(&k)));
        if let Some(img) = found {
            sizes.insert(id, (img.width, img.height));
            set.elements.insert(id, img.clone());
        }
    }
    for url in background_urls(arena, root) {
        if let Some(img) = res.key(&url).and_then(|k| res.images.get(&k)) {
            set.urls.insert(url, img.clone());
        }
    }
    (set, sizes)
}

/// The images a page asks for, as keys (see [`Resources::key`]) in request
/// order: each `<img>`'s preferred candidate, then each `background-image`
/// URL, de-duplicated and capped at [`crate::images::MAX_IMAGES`]. Needs the
/// stylesheets in `res` already, because backgrounds come from computed style.
#[must_use]
pub fn image_plan(source: &str, res: &Resources) -> Vec<String> {
    let dom = parse_page(source);
    if dom.abandoned {
        return Vec::new();
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine =
        px_css::engine::StyleEngine::new(crate::images::PLAN_WIDTH as f32, 768.0, quirks);
    engine.add_user_agent_stylesheet(UA_STYLESHEET, "about:ua");
    add_sheets(&mut engine, res, &arena);
    let style_root = engine.style_root_for(&arena);
    if engine.resolve(&arena, &style_root).is_none() {
        return Vec::new();
    }
    let mut wanted = crate::images::Wanted::default();
    for id in arena.descendants(arena.document()) {
        let is_img = arena
            .get(id)
            .and_then(|n| n.element_name())
            .is_some_and(|q| &*q.local == "img");
        if is_img
            && let Some(key) = crate::images::candidates(&arena, id, crate::images::PLAN_WIDTH)
                .into_iter()
                .find_map(|raw| res.key(&raw))
        {
            wanted.add(key);
        }
    }
    for url in background_urls(&arena, &style_root) {
        if let Some(key) = res.key(&url) {
            wanted.add(key);
        }
    }
    wanted.keys
}

/// The `@font-face` rules the page's stylesheets declare that have a usable
/// `url()` source, in document order. Needs the stylesheets in `res` already.
#[must_use]
pub fn font_plan(source: &str, res: &Resources) -> Vec<px_css::font_face::FontFaceDecl> {
    let dom = parse_page(source);
    if dom.abandoned {
        return Vec::new();
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine =
        px_css::engine::StyleEngine::new(crate::images::PLAN_WIDTH as f32, 768.0, quirks);
    engine.add_user_agent_stylesheet(UA_STYLESHEET, "about:ua");
    add_sheets(&mut engine, res, &arena);
    engine.font_faces()
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
                | Item::Image { rect, .. }
                | Item::PushClip { rect } => rect.y = shift(rect.y),
                Item::FillRoundedRect { shape, .. }
                | Item::RoundedBorder { shape, .. }
                | Item::PushRoundedClip { shape } => shape.rect.y = shift(shape.rect.y),
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
    use super::{drive_path, file_url};

    #[test]
    fn file_urls_and_paths_round_trip_for_both_path_styles() {
        assert_eq!(file_url("/tmp/a/p.html"), "file:///tmp/a/p.html");
        assert_eq!(file_url(r"C:\Users\u\p.html"), "file:///C:/Users/u/p.html");
        assert_eq!(drive_path("/C:/Users/u/b.png"), "C:/Users/u/b.png");
        assert_eq!(drive_path("/tmp/a/b.png"), "/tmp/a/b.png");
        assert_eq!(drive_path("/c/d.png"), "/c/d.png");
    }

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

    fn pixel_of(src: &str, x: u32, y: u32) -> Option<[u8; 4]> {
        let l = layout(src, &Resources::default(), 200, 100).unwrap();
        l.render(0, 200, 100).unwrap().pixel(x, y)
    }

    const RED: Option<[u8; 4]> = Some([255, 0, 0, 255]);
    const WHITE: Option<[u8; 4]> = Some([255, 255, 255, 255]);

    #[test]
    fn noscript_content_renders_because_no_script_runs() {
        let src = "<!doctype html><body style='margin:0'><noscript>\
            <div style='height:50px;background:#f00'></div></noscript>";
        assert_eq!(pixel_of(src, 10, 10), RED);
    }

    #[test]
    fn layered_author_rules_beat_the_user_agent_sheet() {
        // The user-agent sheet says `div { display: block }` and `body { margin: 8px }`;
        // author rules win whatever their layer.
        let src = "<!doctype html><style>@layer a { body { margin: 0 } \
            .r { display: flex } .r > i { display: block; width: 20px; height: 20px; \
            background: #f00 } }</style><div class=r><i></i><i></i></div>";
        assert_eq!(
            pixel_of(src, 30, 10),
            RED,
            "second item is beside the first"
        );
    }

    #[test]
    fn the_media_attribute_of_a_sheet_is_evaluated() {
        let page = |media: &str| {
            format!(
                "<!doctype html><body style='margin:0'><style media='{media}'>\
                 div {{ height: 50px; background: #f00 }}</style><div></div>"
            )
        };
        assert_eq!(pixel_of(&page("(min-width: 100px)"), 5, 5), RED);
        assert_eq!(pixel_of(&page("(min-width: 900px)"), 5, 5), WHITE);
        assert_eq!(pixel_of(&page("print"), 5, 5), WHITE);
        assert_eq!(pixel_of(&page("screen"), 5, 5), RED);
        assert_eq!(pixel_of(&page(""), 5, 5), RED);
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
