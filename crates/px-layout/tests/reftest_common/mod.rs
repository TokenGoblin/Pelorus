//! The reftest machinery shared by `flex_reftest.rs` and `grid_reftest.rs`.
//!
//! **Duplicated from `reftest.rs` on purpose, not shared with it.** Cargo builds one
//! binary per file in `tests/`, and Phase 6's gate (`cargo xtask layout`) is pinned to
//! that file's names and contents; making it depend on a module the Phase 7 harness
//! also edits would let a Phase 7 change move a Phase 6 number. The cost is that a
//! fix to the instrument has to be made in both places, so the comments that explain
//! *why* the comparison is shaped as it is live in `reftest.rs` (ADR 029) and are
//! not repeated here beyond a sentence.
//!
//! The two things that differ from Phase 6:
//!
//! - **Pairs are found by reading `<link rel="match" href>`**, not by filename
//!   (ADR 031 item 1). Nearly half of WPT's flexbox hrefs leave the test's
//!   directory, and references are shared between tests.
//! - **Linked stylesheets are loaded** when the file is vendored. A great many
//!   flexbox tests get their layout from `support/flexbox.css`; ignoring the link
//!   would lay out a different document from the one the test is about.
//!
//! The comparison is ADR 029's: the set of absolute rectangles of every fragment,
//! sorted and deduplicated. Its known blind spot (a reference that draws the same
//! picture with fewer boxes) carries over.

use std::collections::BTreeMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use app_units::Au;
use px_layout::block::layout_document;
use px_layout::fragment::FragmentTree;
use px_layout::geom::px;

/// The viewport every pair is laid out against. WPT's own default.
pub const VIEWPORT_PX: i32 = 800;

/// A layout with no more fragments than this has no content in it: the initial
/// containing block, `<html>` and `<body>`. A pair that agrees at this size agrees
/// about nothing (see `reftest.rs`, where 39 of 210 files did exactly that).
pub const TRIVIAL_FRAGMENTS: usize = 3;

/// One vendored suite.
pub struct Suite {
    /// Directory name under `tests/wpt/`, and the module name in WPT's URLs.
    pub dir: &'static str,
    /// Printed in the report.
    pub label: &'static str,
    /// Pairs the comparison method gets wrong, each with the reason. Enforced in
    /// both directions by the stale-exclusion test.
    pub expected_failures: &'static [(&'static str, &'static str)],
}

/// One test and its reference.
pub struct Pair {
    pub name: String,
    pub test: PathBuf,
    pub reference: PathBuf,
}

/// Why a pair is not matching. Ordered by how much it says about the engine:
/// only [`Category::Layout`] is a claim about layout.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Category {
    /// The test (or reference) is changed by script before the comparison.
    NeedsScriptHost,
    /// Listed in `expected_failures`: the geometry comparison is the wrong oracle.
    ComparisonMethod,
    /// A linked stylesheet is not in the pinned sparse checkout, so the document
    /// laid out is not the document the test describes.
    SupportNotVendored,
    /// Parse, style or layout returned nothing for one side.
    DidNotLayOut,
    /// Layout panicked on one side. A defect, never a feature gap.
    Panicked,
    /// Both sides laid out and the geometry differs: an unimplemented feature or
    /// a defect. Not further classified, because classifying hundreds of pairs by
    /// filename would be a guess dressed as documentation.
    Layout,
}

/// The outcome of one pair.
pub struct Outcome {
    pub name: String,
    pub matched: bool,
    /// The geometry agrees, whether or not the pair is excluded. An excluded pair
    /// that agrees is a stale exclusion.
    pub agrees: bool,
    /// `None` when matched.
    pub category: Option<Category>,
}

/// One fragment's geometry, as compared. The fragment kind is excluded.
pub type Geometry = (Au, Au, Au, Au);

/// What one laid-out file contributes to a comparison.
pub struct Laid {
    pub rectangles: Vec<Geometry>,
    pub fragments: usize,
    pub scripted: bool,
    pub missing_support: bool,
}

/// The `Suite`'s root directory.
pub fn root(suite: &Suite) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/wpt")
        .join(suite.dir)
}

/// The value of attribute `local` on an element node.
fn attr<'a>(node: &'a px_dom::Node, local: &str) -> Option<&'a str> {
    node.attrs()?
        .iter()
        .find(|a| &*a.name.local == local)
        .map(|a| &*a.value)
}

/// Every `<link rel=match|mismatch>` target and every `<link rel=stylesheet>` href
/// of a document, in document order.
struct Links {
    matches: Vec<String>,
    mismatch: bool,
    stylesheets: Vec<String>,
}

fn links_of(arena: &px_dom::Arena) -> Links {
    let mut out = Links {
        matches: Vec::new(),
        mismatch: false,
        stylesheets: Vec::new(),
    };
    let document = arena.document();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        let is_link = node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("link"));
        if !is_link {
            continue;
        }
        let rel = attr(node, "rel").unwrap_or("").to_ascii_lowercase();
        let href = attr(node, "href").unwrap_or("").to_owned();
        let mut words = rel.split_ascii_whitespace();
        if words.clone().any(|w| w == "mismatch") {
            out.mismatch = true;
        } else if words.clone().any(|w| w == "match") {
            out.matches.push(href);
        } else if words.any(|w| w == "stylesheet") {
            out.stylesheets.push(href);
        }
    }
    out
}

/// Resolve `href` as WPT does, against `from`'s directory, with an absolute
/// `/css/<module>/...` href resolving inside the vendored tree. `None` when the
/// target is not a path we could have vendored.
fn resolve(suite: &Suite, from: &Path, href: &str) -> Option<PathBuf> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    if href.is_empty() || href.contains("://") || href.starts_with("//") {
        return None;
    }
    if let Some(rest) = href.strip_prefix('/') {
        let prefix = format!("css/{}/", suite.dir);
        return rest.strip_prefix(&prefix).map(|r| root(suite).join(r));
    }
    Some(from.parent()?.join(href))
}

/// Every test/reference pair in the vendored subset, found by `rel="match"`.
///
/// A file that is itself some other pair's reference is not a test, so a
/// reference that links onward does not count twice.
pub fn pairs(suite: &Suite) -> Vec<Pair> {
    let base = root(suite);
    let mut files = Vec::new();
    let mut stack = vec![base.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("html" | "htm")
            ) {
                files.push(path);
            }
        }
    }
    files.sort();

    let mut found = Vec::new();
    let mut referenced = std::collections::BTreeSet::new();
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let dom = px_dom::parse(&source);
        let links = links_of(&dom.arena);
        if links.mismatch || links.matches.len() != 1 {
            continue;
        }
        let Some(reference) = links.matches.first().and_then(|h| resolve(suite, &path, h)) else {
            continue;
        };
        if !reference.exists() {
            continue;
        }
        let reference = normalise(&reference);
        referenced.insert(reference.clone());
        let name = relative_name(&base, &path);
        found.push(Pair {
            name,
            test: path,
            reference,
        });
    }
    found.retain(|p| !referenced.contains(&normalise(&p.test)));
    found.sort_by(|a, b| a.name.cmp(&b.name));
    found
}

/// Collapse `..` components so two spellings of one file compare equal.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn relative_name(base: &Path, path: &Path) -> String {
    normalise(path)
        .strip_prefix(normalise(base))
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Whether this document's meaning depends on script this engine cannot run: a
/// `<script>` element or any `on*` attribute. Same test as `reftest.rs`.
fn needs_script(arena: &px_dom::Arena) -> bool {
    let document = arena.document();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        let Some(name) = node.element_name() else {
            continue;
        };
        if name.local == html5ever::local_name!("script") {
            return true;
        }
        if node
            .attrs()
            .is_some_and(|attrs| attrs.iter().any(|a| a.name.local.starts_with("on")))
        {
            return true;
        }
    }
    false
}

/// Enough of a user-agent stylesheet for block layout. Not the real one (Phase 8's):
/// the `display` values without which nothing lays out, plus `body` and `p` margins.
/// `display: flex` and `grid` are deliberately not here; the documents ask for them.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     table, form, fieldset, pre, hr { display: block } \
     body { margin: 8px } \
     p, blockquote, figure { margin: 1em 0 } \
     h1 { margin: 0.67em 0; font-size: 2em } \
     head, style, script, title, meta, link { display: none }";

/// What WPT's `/fonts/ahem.css` contains.
const AHEM_CSS: &str = "@font-face { font-family: Ahem; src: url(\"/fonts/Ahem.ttf\") }";

/// Every absolute rectangle, sorted and deduplicated (ADR 029).
fn geometry(tree: &FragmentTree) -> Vec<Geometry> {
    let mut rects: Vec<Geometry> = tree
        .in_layout_order()
        .into_iter()
        .filter_map(|(_, id)| {
            let f = tree.get(id)?;
            Some((f.inline_offset, f.block_offset, f.size.inline, f.size.block))
        })
        .collect();
    rects.sort();
    rects.dedup();
    rects
}

/// The author stylesheets of a document in document order: `<style>` text and the
/// contents of linked stylesheets that were vendored. The bool is true when a
/// linked stylesheet could not be loaded.
fn stylesheets(suite: &Suite, path: &Path, arena: &px_dom::Arena) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut missing = false;
    let document = arena.document();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        let Some(name) = node.element_name() else {
            continue;
        };
        if name.local == html5ever::local_name!("style") {
            out.push(px_layout::inline::collect_text(arena, id));
        } else if name.local == html5ever::local_name!("link") {
            let rel = attr(node, "rel").unwrap_or("").to_ascii_lowercase();
            if !rel.split_ascii_whitespace().any(|w| w == "stylesheet") {
                continue;
            }
            let href = attr(node, "href").unwrap_or("");
            // WPT's own `/fonts/ahem.css`, which lives outside the vendored
            // `css/<module>` trees, is a single `@font-face` for Ahem. Layout
            // resolves Ahem from its `FontRegistry` whether or not an
            // `@font-face` names it (Phase 9 unit 3, `px_layout::text`), so the
            // sheet is satisfied: a pair that links only this one is a document
            // the engine can now lay out as written, and is graded.
            if href == "/fonts/ahem.css" {
                out.push(AHEM_CSS.to_owned());
                continue;
            }
            match resolve(suite, path, href).and_then(|p| std::fs::read_to_string(p).ok()) {
                Some(css) => out.push(css),
                None => missing = true,
            }
        }
    }
    (out, missing)
}

/// Parse, style and lay out one file. `Err(true)` means layout panicked.
pub fn layout_file(suite: &Suite, path: &Path) -> Result<Option<Laid>, bool> {
    catch_unwind(AssertUnwindSafe(|| layout_file_inner(suite, path))).map_err(|_| true)
}

fn layout_file_inner(suite: &Suite, path: &Path) -> Option<Laid> {
    let source = std::fs::read_to_string(path).ok()?;
    let dom = px_dom::parse(&source);
    if dom.abandoned {
        return None;
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = px_css::engine::StyleEngine::new(VIEWPORT_PX as f32, 600.0, quirks);
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");
    let (sheets, missing_support) = stylesheets(suite, path, &arena);
    for css in sheets {
        engine.add_author_stylesheet(&css, "https://wpt.invalid/test.css");
    }

    let style_root = engine.style_root_for(&arena);
    engine.resolve(&arena, &style_root)?;
    let tree = layout_document(&arena, &style_root, px(VIEWPORT_PX))?;
    Some(Laid {
        scripted: needs_script(&arena),
        rectangles: geometry(&tree),
        fragments: tree.len(),
        missing_support,
    })
}

/// Lay out both halves of `pair` and classify the result.
pub fn evaluate_pair(suite: &Suite, pair: &Pair) -> Outcome {
    let excluded = suite.expected_failures.iter().any(|(n, _)| *n == pair.name);
    let (test, reference) = (
        layout_file(suite, &pair.test),
        layout_file(suite, &pair.reference),
    );
    let mut agrees = false;
    let category = match (test, reference) {
        (Err(_), _) | (_, Err(_)) => Some(Category::Panicked),
        (Ok(None), _) | (_, Ok(None)) => Some(Category::DidNotLayOut),
        (Ok(Some(test)), Ok(Some(reference))) => {
            let equal =
                test.fragments > TRIVIAL_FRAGMENTS && test.rectangles == reference.rectangles;
            agrees = equal;
            if equal && !excluded {
                None
            } else if excluded {
                Some(Category::ComparisonMethod)
            } else if test.scripted || reference.scripted {
                Some(Category::NeedsScriptHost)
            } else if test.missing_support || reference.missing_support {
                Some(Category::SupportNotVendored)
            } else {
                Some(Category::Layout)
            }
        }
    };
    Outcome {
        name: pair.name.clone(),
        matched: category.is_none(),
        agrees,
        category,
    }
}

/// Every pair, laid out and classified.
pub fn evaluate(suite: &Suite) -> Vec<Outcome> {
    pairs(suite)
        .iter()
        .map(|pair| evaluate_pair(suite, pair))
        .collect()
}

/// The printable report: graded and ungraded figures and the breakdown of what is
/// not matching. `dropped` is `(scripted, other)` from the suite's PROVENANCE.
pub fn summary(suite: &Suite, outcomes: &[Outcome], floor: usize, dropped: (usize, usize)) {
    let total = outcomes.len();
    let passed = outcomes.iter().filter(|o| o.matched).count();
    let count = |c: Category| outcomes.iter().filter(|o| o.category == Some(c)).count();

    println!(
        "{} reftests: {passed}/{total} matched (floor {floor}); ungraded, dropped at \
         vendoring (PROVENANCE): {} scripted + {} other",
        suite.label, dropped.0, dropped.1
    );
    println!(
        "  of the {} not matching: {} need a script host (Phase 10), {} are \
         comparison-method exclusions, {} link a stylesheet that is not vendored, \
         {} did not lay out, {} panicked, {} differ in layout (unimplemented \
         feature or defect)",
        total - passed,
        count(Category::NeedsScriptHost),
        count(Category::ComparisonMethod),
        count(Category::SupportNotVendored),
        count(Category::DidNotLayOut),
        count(Category::Panicked),
        count(Category::Layout),
    );

    // Per top-level directory, as the CSS2 report does per chapter.
    let mut tally: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for o in outcomes {
        let dir = o.name.split_once('/').map_or(".", |(d, _)| d).to_owned();
        let entry = tally.entry(dir).or_insert((0, 0));
        entry.1 += 1;
        if o.matched {
            entry.0 += 1;
        }
    }
    for (dir, (ok, all)) in tally {
        println!("  {dir}: {ok}/{all}");
    }
    if std::env::var("REPORT").is_ok() {
        for o in outcomes.iter().filter(|o| !o.matched) {
            println!("    {:?}: {}", o.category, o.name);
        }
    }
}

/// Dump both laid-out trees of one pair with the source, for `DIAG=<name>`.
pub fn diagnose(suite: &Suite, name: &str) {
    for pair in pairs(suite) {
        if pair.name != name {
            continue;
        }
        for (label, path) in [("TEST", &pair.test), ("REFERENCE", &pair.reference)] {
            println!("===== {label}: {}", path.display());
            println!("{}", std::fs::read_to_string(path).unwrap_or_default());
            let Ok(Some(laid)) = layout_file(suite, path) else {
                println!("(did not lay out)");
                continue;
            };
            println!("  ({} fragments)", laid.fragments);
            for (inline, block, width, height) in laid.rectangles {
                println!(
                    "  {:>8} {:>8} {:>8} {:>8}",
                    inline.0 / 60,
                    block.0 / 60,
                    width.0 / 60,
                    height.0 / 60
                );
            }
        }
        return;
    }
    println!("no pair named {name}");
}

/// Names on `expected_failures` that are stale: they match now, or are not in the
/// corpus.
pub fn stale_exclusions(suite: &Suite, outcomes: &[Outcome]) -> Vec<String> {
    let mut stale = Vec::new();
    for (name, reason) in suite.expected_failures {
        match outcomes.iter().find(|o| o.name == *name) {
            Some(o) if o.agrees || o.category != Some(Category::ComparisonMethod) => {
                stale.push(format!("{name} (listed as: {reason})"));
            }
            Some(_) => {}
            None => stale.push(format!("{name} is on the list but not in the corpus")),
        }
    }
    stale
}
