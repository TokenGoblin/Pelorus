//! Every built-in page parses, lays out, and stays self-contained and quiet.

use px_pages::{NAMES, PROGRESS, page};

const UA: &str = "html, body, div, p, section, article, header, footer, nav, aside, main, \
     figure, h1, h2, h3, ul, ol, li { display: block } body { margin: 8px } \
     p { margin: 1em 0 } h1 { margin: 0.67em 0; font-size: 2em } h2 { font-size: 1.5em } \
     head, style, script, title, meta, link { display: none }";

fn lay_out(html: &str, dark: bool) -> usize {
    let dom = px_dom::parse(html);
    assert!(!dom.abandoned, "parse abandoned");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;
    let mut engine = px_css::engine::StyleEngine::new_with_color_scheme(800.0, 600.0, quirks, dark);
    engine.add_author_stylesheet(UA, "about:ua");
    for id in arena.descendants(arena.document()) {
        let Some(node) = arena.get(id) else { continue };
        if node
            .element_name()
            .is_some_and(|n| n.local.as_ref() == "style")
        {
            engine
                .add_author_stylesheet(&px_layout::inline::collect_text(&arena, id), "about:page");
        }
    }
    let root = engine.style_root_for(&arena);
    engine.resolve(&arena, &root).expect("style resolves");
    let tree = px_layout::block::layout_document(&arena, &root, px_layout::geom::px(800))
        .expect("layout succeeds");
    tree.in_layout_order().len()
}

fn all() -> Vec<(&'static str, &'static str)> {
    NAMES
        .iter()
        .map(|n| (*n, page(n).expect("every listed name has a page")))
        .collect()
}

#[test]
fn unknown_name_is_none() {
    assert!(page("nope").is_none());
    assert!(page("").is_none());
}

#[test]
fn every_page_lays_out_in_both_schemes() {
    for (name, html) in all() {
        for dark in [false, true] {
            assert!(
                lay_out(html, dark) > 10,
                "{name} (dark={dark}) has too few boxes"
            );
        }
    }
}

#[test]
fn pages_are_self_contained() {
    for (name, html) in all() {
        let lower = html.to_ascii_lowercase();
        for bad in [
            "http:",
            "https:",
            "ftp:",
            "//",
            "<script",
            "<iframe",
            "<img",
            "<link",
            "<object",
            "<embed",
            "url(",
            "@import",
            "javascript:",
            "onclick",
            "onload",
            "src=",
            "<form",
            "<input",
        ] {
            assert!(!lower.contains(bad), "{name} contains {bad:?}");
        }
        // href is allowed only to about: pages.
        for part in lower.split("href=").skip(1) {
            assert!(part.starts_with("\"about:"), "{name}: href outside about:");
        }
        assert!(html.starts_with("<!DOCTYPE html>"), "{name}: no doctype");
        assert!(
            lower.contains("<meta charset=\"utf-8\">"),
            "{name}: no charset"
        );
    }
}

#[test]
fn pages_are_quiet_and_unbranded() {
    for (name, html) in all() {
        let lower = html.to_ascii_lowercase();
        // Exclamation marks are allowed only in the doctype.
        assert_eq!(
            html.matches('!').count(),
            html.matches("<!").count(),
            "{name}: exclamation mark in copy"
        );
        for word in [
            "unhackable",
            "100% secure",
            "military-grade",
            "bulletproof",
            "impenetrable",
            "guaranteed safe",
            "anonymous",
            "shield",
            "padlock",
        ] {
            assert!(!lower.contains(word), "{name} uses {word:?}");
        }
    }
}

#[test]
fn colour_tokens_are_identical_in_every_page() {
    let block = |html: &str| -> String {
        let a = html.find("/* tokens */").expect("tokens start");
        let b = html.find("/* end tokens */").expect("tokens end");
        html[a..b].to_string()
    };
    let first = block(page("newtab").expect("page"));
    for (name, html) in all() {
        assert_eq!(block(html), first, "{name}: tokens drifted");
    }
    // The tokens are the brand palette.
    for hex in [
        "#0B1226", "#E2E8F0", "#9FB0C8", "#5EEAD4", "#60A5FA", "#FBBF24", "#F87171", "#F4F7FB",
        "#475569", "#0F766E", "#1D4ED8", "#B45309", "#B91C1C",
    ] {
        assert!(
            first.contains(hex),
            "token block lacks palette colour {hex}"
        );
    }
}

#[test]
fn version_page_charts_every_progress_row() {
    let html = page("version").expect("page");
    let mut rows = 0;
    for line in PROGRESS
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
    {
        let mut f = line.splitn(3, '|');
        let (n, pct, name) = (
            f.next().unwrap_or(""),
            f.next().unwrap_or(""),
            f.next().unwrap_or(""),
        );
        let name = name.replace('&', "&amp;");
        let row = format!(
            "<div class=\"n\">{n}</div><div class=\"name\">{name}</div><div class=\"track\"><div class=\"fill\" style=\"width: {pct}%\"></div></div><div class=\"pct\">{pct}%</div>"
        );
        assert!(html.contains(&row), "progress row missing or stale: {line}");
        rows += 1;
    }
    assert!(rows >= 23);
    assert_eq!(html.matches("class=\"fill\"").count(), rows, "extra bars");
}

#[test]
fn privacy_page_states_the_invariants() {
    let html = page("privacy").expect("page").to_ascii_lowercase();
    for needle in [
        "no telemetry",
        "partitioned",
        "no jit",
        "sandbox",
        "quiet by default",
    ] {
        assert!(html.contains(needle), "privacy page lacks {needle:?}");
    }
}

#[test]
fn newtab_is_silent() {
    let html = page("newtab").expect("page").to_ascii_lowercase();
    // One field and one line of text: no links, lists or sections.
    for bad in ["<a ", "<ul", "<li", "<section", "<article", "<button"] {
        assert!(!html.contains(bad), "new tab page contains {bad:?}");
    }
}
