#![forbid(unsafe_code)]

//! `px-fetch` — fetch a real URL and run it through the whole engine.
//!
//! **A test-only diagnostic, not a product.** Behind `--features testing` for the
//! reason `/CLAUDE.md` gives for every test-only capability: it must not exist in
//! a shipping binary, and §14.4's release scan checks that absence rather than
//! trusting a flag to be off.
//!
//! # Why it exists
//!
//! Phase 6's gate compares 105 vendored reftest pairs against each other. That is
//! a good test of *self-consistency* and a poor test of whether the engine can
//! survive the actual web, where markup is malformed, stylesheets are enormous,
//! and no document looks like a test case. Laying out one real page exercises
//! more of `px-dom`, `px-css` and `px-layout` together than the whole corpus does
//! separately.
//!
//! It is also the first time anything in this project has been pointed at a live
//! website. Everything until now — including the compat suite, by ADR 002 — has
//! run against fixtures.
//!
//! # What it does not do
//!
//! No JavaScript (Phase 10), no images (Phase 15), no paint (Phase 8), no real
//! text shaping (Phase 9), and no external stylesheets — only `<style>` elements
//! in the document itself. A page whose layout depends on any of those will come
//! out wrong, and the point of this tool is to see *how* wrong.
//!
//! ```text
//! px-fetch https://example.com
//! px-fetch https://example.com --find "domain"
//! px-fetch https://example.com --tree
//! ```

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(url) = args.first() else {
        eprintln!("usage: px-fetch <url> [--find TEXT] [--tree]");
        return ExitCode::FAILURE;
    };

    let find = flag_value(&args, "--find");
    let show_tree = args.iter().any(|a| a == "--tree");

    match run(url, find.as_deref(), show_tree) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("px-fetch: {message}");
            ExitCode::FAILURE
        }
    }
}

/// The value following `name`, if present.
fn flag_value(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|a| a == name)?;
    args.get(index + 1).cloned()
}

fn run(url: &str, find: Option<&str>, show_tree: bool) -> Result<(), String> {
    let page = fetch_page(url)?;

    println!("== {} ==", page.final_url);
    println!("status  {}", page.status);
    println!("bytes   {}", page.body.len());

    let dom = px_dom::parse(&page.body);
    if dom.abandoned {
        return Err("the parser abandoned this document as pathological".to_owned());
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let elements = core::iter::once(arena.document())
        .chain(arena.descendants(arena.document()))
        .filter(|id| arena.get(*id).is_some_and(|n| n.element_name().is_some()))
        .count();
    println!("quirks  {quirks:?}");
    println!(
        "parsed  {elements} elements, {} nodes, {} parse errors",
        arena.len(),
        dom.error_count
    );

    // Style. The document's own <style> elements only -- external sheets need a
    // fetch per URL and a base-URL resolution this tool does not do, so a page
    // that keeps its CSS in a file will lay out as if unstyled. Named here
    // because it is the difference that will surprise somebody first.
    let mut engine = px_css::engine::StyleEngine::new(800.0, 600.0, quirks);
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");
    let mut sheets = 0usize;

    // External stylesheets first, then inline ones, which is document order for
    // the common case and matters because the cascade breaks ties by order. A
    // page that relies on an inline rule overriding a linked one gets the right
    // answer; one that interleaves them does not, and that is a real limitation
    // of fetching by kind rather than walking the document once.
    for href in linked_stylesheets(&arena) {
        let Ok(absolute) = resolve_redirect(&page.final_url, &href) else {
            continue;
        };
        match fetch_page(&absolute) {
            Ok(sheet) => {
                engine.add_author_stylesheet(&sheet.body, &absolute);
                sheets += 1;
            }
            // A stylesheet that will not load is not a failure of the page. A
            // browser renders what it has, which is the behaviour worth copying
            // here -- reporting it rather than aborting is the whole point of a
            // diagnostic.
            Err(why) => println!("  (stylesheet {absolute} failed: {why})"),
        }
    }

    for css in inline_stylesheets(&arena) {
        engine.add_author_stylesheet(&css, &page.final_url);
        sheets += 1;
    }
    let style_root = engine.style_root_for(&arena);
    let styled = engine
        .resolve(&arena, &style_root)
        .ok_or("the document has no root element to style")?;
    println!("styled  {styled} elements, from {sheets} inline stylesheet(s)");

    let tree = px_layout::block::layout_document(&arena, &style_root, px_layout::geom::px(800))
        .ok_or("layout produced nothing")?;
    let order = tree.in_layout_order();
    let deepest = order.iter().map(|(d, _)| *d).max().unwrap_or(0);
    let tallest = order
        .iter()
        .filter_map(|(_, id)| tree.get(*id))
        .map(|f| f.size.block)
        .max()
        .unwrap_or(app_units::Au(0));
    println!(
        "laid out {} fragments, {} deep, tallest {:?}",
        tree.len(),
        deepest,
        tallest
    );

    // The distribution of computed `display`, because a real page can lay out to
    // almost nothing and the number alone does not say why.
    //
    // px-layout generates a box only for `display: block`. Everything else --
    // flex, grid, inline-block, table -- establishes a formatting context Phase 6
    // does not implement, and is skipped rather than laid out as a block, because
    // wrong geometry is worse than none. On a modern page that is most of the
    // document: rust-lang.org collapsed from 176 fragments to 2 the moment its
    // real stylesheets were loaded, which is this line's reason for existing.
    report_display_values(&arena, &style_root);

    if let Some(title) = document_title(&arena) {
        println!("title   {title}");
    }

    let text = visible_text(&arena);
    println!("text    {} characters", text.len());

    if let Some(needle) = find {
        search(&text, needle);
    } else {
        let preview: String = text.chars().take(400).collect();
        println!("\n-- first 400 characters of visible text --\n{preview}");
    }

    if show_tree {
        println!("\n-- fragment tree (first 40) --");
        for (depth, id) in order.iter().take(40) {
            if let Some(f) = tree.get(*id) {
                println!(
                    "{:indent$}{:?} {:?}x{:?} at ({:?},{:?})",
                    "",
                    f.kind,
                    f.size.inline,
                    f.size.block,
                    f.inline_offset,
                    f.block_offset,
                    indent = depth * 2
                );
            }
        }
    }

    Ok(())
}

/// Report every occurrence of `needle`, case-insensitively, with context.
///
/// This is the "search a website" the tool is for. Matching on the *visible*
/// text rather than the source means a match is something a reader would see —
/// a word split across markup still matches, and a word that appears only in an
/// attribute or a comment does not.
fn search(text: &str, needle: &str) {
    let haystack = text.to_lowercase();
    let lowered = needle.to_lowercase();

    let mut hits = 0usize;
    let mut from = 0usize;
    while let Some(at) = haystack.get(from..).and_then(|s| s.find(&lowered)) {
        let start = from + at;
        hits += 1;
        let context_start = start.saturating_sub(40);
        let context_end = (start + lowered.len() + 40).min(text.len());
        if let Some(context) = text.get(context_start..context_end) {
            println!("  {}: ...{}...", hits, context.replace('\n', " "));
        }
        from = start + lowered.len().max(1);
        if hits >= 20 {
            println!("  (stopping at 20)");
            break;
        }
    }
    println!("\nfound   {hits} occurrence(s) of {needle:?}");
}

/// A fetched page, after redirects.
struct Page {
    final_url: String,
    status: u16,
    body: String,
}

/// Fetch `url`, following redirects.
///
/// px-net returns a redirect rather than following it — deliberately, so the
/// decision is the caller's — so the loop is here. Capped, because a redirect
/// cycle is a real thing on the real web and this is the first code in the
/// project to meet one.
fn fetch_page(url: &str) -> Result<Page, String> {
    const MAX_REDIRECTS: usize = 5;

    let psl_text = std::fs::read_to_string(psl_path())
        .map_err(|e| format!("cannot read the public suffix list: {e}"))?;
    let psl = px_net::psl::PublicSuffixList::parse(&psl_text)
        .map_err(|e| format!("the public suffix list will not parse: {e:?}"))?;

    let mut current = url.to_owned();
    for hop in 0..=MAX_REDIRECTS {
        let (key, path) = key_and_path(&psl, &current)?;
        let response =
            px_net::fetch::fetch(&key, &path).map_err(|e| format!("fetch failed: {e}"))?;

        if let Some(target) = response.redirect_target() {
            if hop == MAX_REDIRECTS {
                return Err(format!("more than {MAX_REDIRECTS} redirects"));
            }
            let next = resolve_redirect(&current, target)?;
            println!("redirect {} -> {}", response.head.status, next);
            current = next;
            continue;
        }

        return Ok(Page {
            final_url: current,
            status: response.head.status,
            body: String::from_utf8_lossy(&response.body).into_owned(),
        });
    }
    Err("redirect loop".to_owned())
}

/// Split a URL into the partition key and the request path.
///
/// The top-level site is the URL's own host, because this tool is always the
/// thing being navigated to rather than a subresource of something else. A real
/// navigation makes the same choice; a subresource would not.
fn key_and_path(
    psl: &px_net::psl::PublicSuffixList,
    url: &str,
) -> Result<(px_net::partition::PartitionKey, String), String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("bad url: {e}"))?;
    let host = parsed.host_str().ok_or("the url has no host")?.to_owned();
    let scheme = parsed.scheme().to_owned();
    let port = parsed
        .port_or_known_default()
        .ok_or("the url has no port and no default for its scheme")?;

    let origin = px_net::partition::Origin::new(&scheme, &host, port)
        .ok_or("that scheme and host do not make a valid origin")?;
    let key = px_net::partition::PartitionKey::new(psl, &host, origin)
        .ok_or("the host has no registrable domain, so it cannot be partitioned")?;

    let mut path = parsed.path().to_owned();
    if path.is_empty() {
        path.push('/');
    }
    if let Some(query) = parsed.query() {
        path.push('?');
        path.push_str(query);
    }
    Ok((key, path))
}

/// Resolve a `Location` against the URL it came from.
fn resolve_redirect(from: &str, target: &str) -> Result<String, String> {
    let base = url::Url::parse(from).map_err(|e| format!("bad base url: {e}"))?;
    base.join(target)
        .map(|u| u.to_string())
        .map_err(|e| format!("bad redirect target: {e}"))
}

/// Where the committed public suffix list lives.
fn psl_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/public_suffix_list.dat")
}

/// The `<title>`, if the document has one.
fn document_title(arena: &px_dom::Arena) -> Option<String> {
    let document = arena.document();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let node = arena.get(id)?;
        if node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("title"))
        {
            let text = px_layout::inline::collect_text(arena, id);
            let collapsed = px_layout::inline::collapse_whitespace(&text);
            if !collapsed.is_empty() {
                return Some(collapsed);
            }
        }
    }
    None
}

/// The document's visible text, with `<script>` and `<style>` contents removed.
///
/// Without the exclusion a page's JavaScript is "text on the page", which makes
/// every search match its own source and every character count meaningless.
fn visible_text(arena: &px_dom::Arena) -> String {
    let document = arena.document();
    let mut out = String::new();

    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        let Some(text) = node.text() else { continue };

        let inside_invisible = arena.ancestors(id).any(|ancestor| {
            arena.get(ancestor).is_some_and(|n| {
                n.element_name().is_some_and(|q| {
                    q.local == html5ever::local_name!("script")
                        || q.local == html5ever::local_name!("style")
                        || q.local == html5ever::local_name!("head")
                })
            })
        });
        if inside_invisible {
            continue;
        }

        out.push_str(text);
        out.push(' ');
    }

    px_layout::inline::collapse_whitespace(&out)
}

/// Count the computed `display` of every element and print the distribution.
///
/// A diagnostic rather than a check. It turns "laid out 2 fragments" from a
/// mystery into a measurement: if 180 elements computed `flex`, the page did not
/// fail to lay out, it laid out exactly as much as Phase 6 can.
fn report_display_values(arena: &px_dom::Arena, root: &px_css::view::StyleRoot) {
    use std::collections::BTreeMap;

    let Some(display_longhand) = style::properties::PropertyId::parse_unchecked("display", None)
        .ok()
        .and_then(|id| id.longhand_id())
    else {
        return;
    };

    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    px_css::view::with_dom(arena, root, |dom| {
        for id in core::iter::once(arena.document()).chain(arena.descendants(arena.document())) {
            let Some(node) = dom.node(id) else { continue };
            let Some(element) = px_css::dom::StyleElement::new(node) else {
                continue;
            };
            let Some(data) = style::dom::TElement::borrow_data(&element) else {
                continue;
            };
            // Serialised, not `Debug`-formatted. stylo's `Display` is a
            // bitfield whose `Debug` prints `Display(514)`, which is true and
            // useless; `computed_value_to_string` gives the CSS keyword.
            let display = data.styles.primary().computed_value_to_string(
                style::properties::PropertyDeclarationId::Longhand(display_longhand),
            );
            *counts.entry(display).or_default() += 1;
        }
    });

    let mut ranked: Vec<(String, usize)> = counts.into_iter().collect();
    ranked.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    let shown: Vec<String> = ranked
        .iter()
        .take(6)
        .map(|(name, n)| format!("{name} {n}"))
        .collect();
    if !shown.is_empty() {
        println!("display {}", shown.join(", "));
    }
}

/// The `href` of every `<link rel="stylesheet">`, in document order.
///
/// `rel` is matched case-insensitively and as a whitespace-separated token list,
/// because `rel="stylesheet alternate"` and `rel="StyleSheet"` are both real and
/// both appear on the web.
fn linked_stylesheets(arena: &px_dom::Arena) -> Vec<String> {
    let document = arena.document();
    let mut out = Vec::new();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        if !node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("link"))
        {
            continue;
        }
        let Some(attrs) = node.attrs() else { continue };

        let is_stylesheet = attrs.iter().any(|a| {
            a.name.local == html5ever::local_name!("rel")
                && a.value
                    .split_ascii_whitespace()
                    .any(|token| token.eq_ignore_ascii_case("stylesheet"))
        });
        if !is_stylesheet {
            continue;
        }
        if let Some(href) = attrs
            .iter()
            .find(|a| a.name.local == html5ever::local_name!("href"))
        {
            out.push(href.value.to_string());
        }
    }
    out
}

/// The text of every `<style>` element, in document order.
fn inline_stylesheets(arena: &px_dom::Arena) -> Vec<String> {
    let document = arena.document();
    let mut out = Vec::new();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        if node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("style"))
        {
            out.push(px_layout::inline::collect_text(arena, id));
        }
    }
    out
}

/// Enough of a user-agent stylesheet that a real page lays out at all.
///
/// The same one the reftest harness uses, and the same caveat: not the real UA
/// sheet, which is Phase 8's and mostly about paint. Without it every element
/// computes `display: inline` and the page has no block structure whatsoever.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, dl, dt, dd, \
     table, form, fieldset, pre, hr, address, details, summary { display: block } \
     body { margin: 8px } \
     p, blockquote, figure, ul, ol, dl, pre { margin: 1em 0 } \
     h1 { margin: 0.67em 0; font-size: 2em } \
     h2 { margin: 0.83em 0; font-size: 1.5em } \
     h3 { margin: 1em 0; font-size: 1.17em } \
     head, style, script, title, meta, link, base { display: none }";
