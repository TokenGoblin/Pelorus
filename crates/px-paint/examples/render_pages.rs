//! The render corpus' renderer: `cargo xtask render` runs this to turn HTML pages
//! into PNGs through the product path (parse, style, layout, display list,
//! software raster, PNG encode). It lives here and not in `px-xtask` because the
//! xtask crate is std-only and must not link the style engine.
//!
//! ```text
//! render_pages [--twice] <out_dir> <root> <id>=<source> ...
//! ```
//!
//! `source` is a path relative to `root` (the repository root), optionally
//! suffixed `#dark` to render with `prefers-color-scheme: dark`. For each page it
//! writes `<out_dir>/<id>.png` and prints one line on stdout:
//!
//! ```text
//! OK <id> <width> <height> <ink_pixels>
//! FAIL <id> <reason>
//! ```
//!
//! `ink_pixels` is the number of pixels that differ from the most common colour of
//! the bitmap, so a page that paints one flat colour has zero ink. With `--twice`
//! each page is rendered a second time and a differing byte is a `FAIL`.
//!
//! Pictures: each `<img src>` and `background-image: url()` that names a file next
//! to the page (no scheme, no leading `/`) is decoded with `px-image`; a missing or
//! undecodable one is simply absent, so the page shows its `alt` text. Only pages
//! under `tests/render/` get pictures; vendored WPT pages render without, as they
//! always have (see `render`).
//!
//! Fonts: each `@font-face` `src: url(@fonts/NAME)` of a page under
//! `tests/render/` is read from `tests/fonts/NAME` (TTF, WOFF or WOFF2), through
//! the same loader the browser uses (`px_layout::fontset::build_page_fonts`), so
//! `font-family`, weight, style and `unicode-range` matching are the product's.
//! A `<meta name="px-test-generics" content="serif=NAME;monospace=NAME">` in the
//! page maps generic families to files in `tests/fonts` for that page only (the
//! browser maps them to the machine's installed fonts, which a deterministic
//! corpus must not read). Both are harness conventions; a browser ignores them.
//!
//! The canvas is 800 px wide and `max(600, document height)` tall, capped at 2400
//! (ADR 033 (a)).

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use px_layout::geom::px;
use px_paint::FontId;
use px_paint::glyph::{PathCmd, Pt};
use px_paint::raster::GlyphSource;

const WIDTH: u32 = 800;
const MIN_HEIGHT: u32 = 600;
const MAX_HEIGHT: u32 = 2400;
/// Layout units per CSS pixel.
const AU_PER_PX: i32 = 60;

/// Enough of a user-agent stylesheet for block layout: the `display` values
/// without which nothing lays out, and the default margins.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     form, fieldset, pre, hr { display: block } \
     body { margin: 8px } \
     p, blockquote, figure { margin: 1em 0 } \
     h1 { margin: 0.67em 0; font-size: 2em } \
     head, style, script, title, meta, link { display: none }";

fn attr<'a>(node: &'a px_dom::Node, local: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|a| &*a.name.local == local)
        .map(|a| &*a.value)
}

/// Resolve a stylesheet href: relative to the page, or `/css/<module>/...` into the
/// vendored WPT tree.
fn resolve(root: &Path, from: &Path, href: &str) -> Option<PathBuf> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    if href.is_empty() || href.contains("://") || href.starts_with("//") {
        return None;
    }
    if let Some(rest) = href.strip_prefix('/') {
        let rest = rest.strip_prefix("css/")?;
        return Some(root.join("tests/wpt").join(rest));
    }
    Some(from.parent()?.join(href))
}

fn stylesheets(root: &Path, path: &Path, arena: &px_dom::Arena) -> Vec<String> {
    let mut out = Vec::new();
    let document = arena.document();
    for id in arena.descendants(document) {
        let Some(node) = arena.get(id) else { continue };
        let Some(name) = node.element_name() else {
            continue;
        };
        if name.local.as_ref() == "style" {
            out.push(px_layout::inline::collect_text(arena, id));
        } else if name.local.as_ref() == "link" {
            let rel = attr(node, "rel").unwrap_or("").to_ascii_lowercase();
            if !rel.split_ascii_whitespace().any(|w| w == "stylesheet") {
                continue;
            }
            let href = attr(node, "href").unwrap_or("");
            if let Some(css) =
                resolve(root, path, href).and_then(|p| std::fs::read_to_string(p).ok())
            {
                out.push(css);
            }
        }
    }
    out
}

/// Decode the file at `rel` next to the page; a scheme, a host or an absolute
/// path is not read.
fn picture(page: &Path, rel: &str) -> Option<std::sync::Arc<px_paint::ImageData>> {
    let rel = rel.split(['#', '?']).next().unwrap_or("");
    if rel.is_empty() || rel.contains(':') || rel.starts_with('/') {
        return None;
    }
    let bytes = std::fs::read(page.parent()?.join(rel)).ok()?;
    let decoded = px_image::decode(&bytes, &px_image::Limits::DEFAULT).ok()?;
    let img = decoded.first_image()?;
    Some(std::sync::Arc::new(px_paint::ImageData {
        width: img.width,
        height: img.height,
        rgba: img.rgba.clone(),
    }))
}

/// The pictures of a page: each `<img src>` and each `background-image` URL (the
/// style sheets are given the base `https://corpus.invalid/`, so a relative URL
/// resolves to a path under it, read from the page's directory).
fn pictures(
    page: &Path,
    arena: &px_dom::Arena,
    style_root: &px_css::view::StyleRoot,
) -> (
    px_paint::ImageSet,
    std::collections::BTreeMap<px_dom::NodeId, (u32, u32)>,
) {
    let mut set = px_paint::ImageSet::default();
    let mut sizes = std::collections::BTreeMap::new();
    for id in arena.descendants(arena.document()) {
        let Some(node) = arena.get(id) else { continue };
        if node.element_name().is_some_and(|n| &*n.local == "img")
            && let Some(img) = attr(node, "src").and_then(|s| picture(page, s))
        {
            sizes.insert(id, (img.width, img.height));
            set.elements.insert(id, img);
        }
    }
    px_css::view::with_dom(arena, style_root, |dom| {
        for id in arena.descendants(arena.document()) {
            for layer in px_css::image_style::background_layers(dom, id) {
                if let Some(rel) = layer.url.strip_prefix("https://corpus.invalid/")
                    && let Some(img) = picture(page, rel)
                {
                    set.urls.insert(layer.url, img);
                }
            }
        }
    });
    (set, sizes)
}

/// The fonts of a corpus page: its `@font-face` rules (files under `tests/fonts`
/// named by `@fonts/NAME`) and its test generics.
fn page_fonts(
    root: &Path,
    arena: &px_dom::Arena,
    engine: &px_css::engine::StyleEngine,
) -> px_layout::fontset::PageFonts {
    let read = |name: &str| -> Option<Vec<u8>> {
        if name.is_empty() || name.contains(['/', '\\', ':']) || name.starts_with('.') {
            return None;
        }
        std::fs::read(root.join("tests/fonts").join(name)).ok()
    };
    let decls = engine.font_faces();
    let mut fetch = |_: &px_css::font_face::FontFaceDecl,
                     src: &px_css::font_face::FontSource| {
        read(src.url.strip_prefix("https://corpus.invalid/@fonts/")?)
    };
    let mut fonts = px_layout::fontset::build_page_fonts(&decls, &mut fetch);
    for id in arena.descendants(arena.document()) {
        let Some(node) = arena.get(id) else { continue };
        if node.element_name().is_none_or(|n| &*n.local != "meta")
            || attr(node, "name") != Some("px-test-generics")
        {
            continue;
        }
        for pair in attr(node, "content").unwrap_or("").split(';') {
            let Some((kw, file)) = pair.split_once('=') else {
                continue;
            };
            if let Some(g) = px_text::sysfonts::GenericFamily::from_keyword(kw.trim())
                && let Some(bytes) = read(file.trim())
                && let Ok(fid) = px_layout::fontset::register_font(&bytes)
            {
                fonts.set_generic(g, fid);
            }
        }
    }
    fonts
}

fn render(root: &Path, path: &Path, dark: bool) -> Result<px_paint::Canvas, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let dom = px_dom::parse(&source);
    if dom.abandoned {
        return Err("parse abandoned".into());
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let mut arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new_with_color_scheme(
        WIDTH as f32,
        MIN_HEIGHT as f32,
        quirks,
        dark,
    );
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");
    for css in stylesheets(root, path, &arena) {
        engine.add_author_stylesheet(&css, "https://corpus.invalid/page.css");
    }
    // Fonts are read before the cascade is resolved and stay installed (on this
    // thread) until the page is rasterized. Vendored WPT pages get none.
    let own = path.starts_with(root.join("tests/render"));
    let _fonts = px_layout::fontset::enter_page_fonts(std::sync::Arc::new(if own {
        page_fonts(root, &arena, &engine)
    } else {
        Default::default()
    }));
    let style_root = engine
        .style_document(&mut arena)
        .ok_or("style resolve failed")?;
    // Only the corpus' own pages get pictures. A vendored WPT page that names an
    // image (`grid-abspos-staticpos-align-self-img-001`) tests the layout mode
    // around it, which does not place those boxes yet; it rendered picture-less
    // (test and reference alike) before pictures existed, and still does.
    let (images, sizes) = if own {
        pictures(path, &arena, &style_root)
    } else {
        Default::default()
    };
    let tree = px_layout::replaced::layout_document_with_images(
        &arena,
        &style_root,
        px(WIDTH as i32),
        Some(px(MIN_HEIGHT as i32)),
        &sizes,
    )
    .ok_or("layout failed")?;

    // Document height: the lowest fragment edge.
    let mut bottom: i64 = 0;
    for (_, id) in tree.in_layout_order() {
        if let Some(f) = tree.get(id) {
            bottom = bottom.max(f.block_offset.0 as i64 + f.size.block.0 as i64);
        }
    }
    let doc_px = ((bottom + AU_PER_PX as i64 - 1) / AU_PER_PX as i64).max(0) as u32;
    let height = doc_px.clamp(MIN_HEIGHT, MAX_HEIGHT);

    let list = px_paint::build_with_images(
        &arena,
        &style_root,
        &tree,
        px(WIDTH as i32),
        px(height as i32),
        &images,
    );
    px_paint::raster::rasterize_with(&list, WIDTH, height, &LayoutFonts)
        .map_err(|e| format!("raster: {e}"))
}

/// Glyph outlines from the registry layout shaped with (`FontRegistry::with_ahem`
/// behind `px_layout::text::font_of`). Layout's `FontId` numbers are the display
/// list's, so asking layout's own registry cannot disagree with the ids the
/// builder wrote. `px-paint` stays free of any font crate; this example (a
/// dev-dependency user of `px-text`) is the one place the two meet for the
/// render corpus.
struct LayoutFonts;

impl GlyphSource for LayoutFonts {
    fn units_per_em(&self, font: FontId) -> Option<u32> {
        px_layout::text::font_of(px_text::FontId(font.0)).map(|f| u32::from(f.units_per_em()))
    }

    fn outline(&self, font: FontId, glyph: u32) -> Option<Vec<PathCmd>> {
        let f = px_layout::text::font_of(px_text::FontId(font.0))?;
        let o = f.outline(u16::try_from(glyph).ok()?).ok()?;
        let p = |p: px_text::Pt| Pt { x: p.x, y: p.y };
        Some(
            o.commands
                .iter()
                .map(|c| match *c {
                    px_text::PathCmd::MoveTo(a) => PathCmd::MoveTo(p(a)),
                    px_text::PathCmd::LineTo(a) => PathCmd::LineTo(p(a)),
                    px_text::PathCmd::QuadTo(a, b) => PathCmd::QuadTo(p(a), p(b)),
                    px_text::PathCmd::Close => PathCmd::Close,
                })
                .collect(),
        )
    }
}

fn png_of(root: &Path, path: &Path, dark: bool) -> Result<(px_paint::Canvas, Vec<u8>), String> {
    let canvas = catch_unwind(AssertUnwindSafe(|| render(root, path, dark)))
        .map_err(|_| "panicked".to_string())??;
    let png = canvas.to_png().map_err(|e| format!("png: {e}"))?;
    Ok((canvas, png))
}

fn ink(canvas: &px_paint::Canvas) -> usize {
    let mut counts: HashMap<[u8; 4], usize> = HashMap::new();
    for p in canvas.pixels.as_chunks::<4>().0 {
        *counts.entry([p[0], p[1], p[2], p[3]]).or_insert(0) += 1;
    }
    let total = canvas.pixels.len() / 4;
    total - counts.values().copied().max().unwrap_or(0)
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let twice = args.first().is_some_and(|a| a == "--twice");
    if twice {
        args.remove(0);
    }
    if args.len() < 2 {
        eprintln!("usage: render_pages [--twice] <out_dir> <root> <id>=<source> ...");
        std::process::exit(2);
    }
    let out_dir = PathBuf::from(&args[0]);
    let root = PathBuf::from(&args[1]);
    if let Err(e) = std::fs::create_dir_all(&out_dir) {
        eprintln!("cannot create {}: {e}", out_dir.display());
        std::process::exit(2);
    }
    for spec in &args[2..] {
        let Some((id, source)) = spec.split_once('=') else {
            eprintln!("bad spec {spec}");
            std::process::exit(2);
        };
        // A `#dark` suffix renders with prefers-color-scheme: dark.
        let (source, dark) = match source.strip_suffix("#dark") {
            Some(s) => (s, true),
            None => (source, false),
        };
        let path = root.join(source);
        match png_of(&root, &path, dark) {
            Err(e) => println!("FAIL {id} {e}"),
            Ok((canvas, png)) => {
                if twice {
                    match png_of(&root, &path, dark) {
                        Ok((_, again)) if again == png => {}
                        _ => {
                            println!("FAIL {id} nondeterministic: second render differs");
                            continue;
                        }
                    }
                }
                if let Err(e) = std::fs::write(out_dir.join(format!("{id}.png")), &png) {
                    println!("FAIL {id} write: {e}");
                    continue;
                }
                println!(
                    "OK {id} {} {} {}",
                    canvas.width,
                    canvas.height,
                    ink(&canvas)
                );
            }
        }
    }
}
