//! `cargo xtask render` -- the Phase 8 render gate (ADR 033 (a)-(c)).
//!
//! **Text paints as placeholder blocks until Phase 9**, so this gate says nothing
//! about text rendering, and every baseline containing text is regenerated, in one
//! reviewed `--bless` commit, when Phase 9 lands.
//!
//! The gate renders every page in `tests/render/pages.toml` through the product
//! path (parse, style, layout, display list, software raster, PNG), and compares
//! the result to `tests/render/baselines/<id>.png` **exactly**: the baseline is
//! decoded and every pixel compared, tolerance zero. It fails on
//!
//!   - fewer than [`PAGE_FLOOR`] pages (the corpus cannot shrink),
//!   - a page without a baseline, and a baseline without a page,
//!   - a page that fails to render, or renders blank (one flat colour),
//!   - a page whose render differs from its baseline (first differing pixel and
//!     the count are reported; the actual PNG is written to `target/render-actual/`),
//!   - a render that is not byte-identical when done twice,
//!   - for a WPT-derived page, a test bitmap that differs from its reference.
//!
//! `--bless` rewrites the baselines and prints the files it changed. It refuses to
//! run when `CI` is set. `--select` is the corpus selection tool: it applies the
//! rule in [`select_every_kth`] to the vendored WPT suites and prints the result.
//!
//! The renderer is `crates/px-paint/examples/render_pages.rs`, run through cargo:
//! this crate is std-only and does not link the style engine. The PNG decoder is
//! `px-paint`'s own file, included by path, so the gate decodes with the same code
//! that wrote the baselines.

use crate::Gate;
use crate::proc;
use std::collections::BTreeMap;
use std::path::Path;

/// The PNG codec, included from `px-paint` rather than duplicated.
#[path = "../../../px-paint/src/png.rs"]
#[allow(dead_code)]
pub mod px_png;

/// The corpus may not shrink below this many pages (ADR 033 (a)).
pub const PAGE_FLOOR: usize = 25;

/// A rendered page with fewer ink pixels than this (pixels differing from its most
/// common colour) is not a test. Used by the WPT selection rule: 20 x 20 pixels.
pub const MIN_INK_PIXELS: usize = 400;

/// `(suite directory under tests/wpt, quota)`: ADR 033 (a), 6 + 3 + 3.
pub const SUITES: &[(&str, usize)] = &[("css2", 6), ("css-flexbox", 3), ("css-grid", 3)];

const MANIFEST: &str = "tests/render/pages.toml";
const BASELINES: &str = "tests/render/baselines";

// ------------------------------------------------------------------ manifest

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub id: String,
    pub source: String,
    /// WPT-derived pages: the reference the test must render identically to.
    pub reference: Option<String>,
}

/// Parse `pages.toml`: `[[page]]` tables with `id`, `source` and optional
/// `reference`, all string values. Anything else is an error.
pub fn parse_manifest(text: &str) -> Result<Vec<Page>, String> {
    let mut pages: Vec<Page> = Vec::new();
    let mut cur: Option<BTreeMap<String, String>> = None;
    fn finish(
        cur: &mut Option<BTreeMap<String, String>>,
        out: &mut Vec<Page>,
    ) -> Result<(), String> {
        if let Some(mut t) = cur.take() {
            let id = t.remove("id").ok_or("a [[page]] has no id")?;
            let source = t
                .remove("source")
                .ok_or_else(|| format!("page {id} has no source"))?;
            let reference = t.remove("reference");
            if let Some(k) = t.keys().next() {
                return Err(format!("page {id}: unknown key {k}"));
            }
            out.push(Page {
                id,
                source,
                reference,
            });
        }
        Ok(())
    }
    for (n, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[page]]" {
            finish(&mut cur, &mut pages)?;
            cur = Some(BTreeMap::new());
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = \"value\"", n + 1))?;
        let v = v.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| format!("line {}: value must be a quoted string", n + 1))?;
        let t = cur
            .as_mut()
            .ok_or_else(|| format!("line {}: key outside a [[page]]", n + 1))?;
        if t.insert(k.trim().to_string(), v.to_string()).is_some() {
            return Err(format!("line {}: duplicate key {}", n + 1, k.trim()));
        }
    }
    finish(&mut cur, &mut pages)?;
    Ok(pages)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The decisions that need only the manifest and the list of baseline file stems.
/// `bless` waives "missing baseline" and "baseline without page" (bless creates
/// and removes them).
pub fn check_manifest(pages: &[Page], baselines: &[String], bless: bool) -> Vec<String> {
    let mut f = Vec::new();
    if pages.len() < PAGE_FLOOR {
        f.push(format!(
            "the corpus has {} pages, the pinned floor is {PAGE_FLOOR}",
            pages.len()
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for p in pages {
        if !valid_id(&p.id) {
            f.push(format!(
                "page id {:?} must be lowercase letters, digits and '-'",
                p.id
            ));
        }
        if !seen.insert(p.id.as_str()) {
            f.push(format!("page id {} is listed twice", p.id));
        }
        if !bless && !baselines.contains(&p.id) {
            f.push(format!(
                "page {} has no baseline (tests/render/baselines/{}.png); run `cargo xtask render --bless` and review it",
                p.id, p.id
            ));
        }
    }
    if !bless {
        for b in baselines {
            if !pages.iter().any(|p| p.id == *b) {
                f.push(format!("baseline {b}.png has no page in {MANIFEST}"));
            }
        }
    }
    f
}

/// `--bless` is refused when the `CI` environment variable is set and non-empty.
pub fn bless_refusal(ci: Option<&str>) -> Option<&'static str> {
    match ci {
        Some(v) if !v.is_empty() => Some(
            "`--bless` refuses to run when CI is set: baselines change only by a deliberate local run, reviewed as a PNG diff",
        ),
        _ => None,
    }
}

// ------------------------------------------------------------------- results

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rendered {
    Ok {
        id: String,
        width: u32,
        height: u32,
        ink: usize,
    },
    Fail {
        id: String,
        reason: String,
    },
}

/// One line of the renderer's stdout (`OK id w h ink` / `FAIL id reason`).
pub fn parse_render_line(line: &str) -> Option<Rendered> {
    let mut it = line.splitn(3, ' ');
    match it.next()? {
        "OK" => {
            let id = it.next()?.to_string();
            let mut nums = it.next()?.split(' ');
            let width = nums.next()?.parse().ok()?;
            let height = nums.next()?.parse().ok()?;
            let ink = nums.next()?.parse().ok()?;
            Some(Rendered::Ok {
                id,
                width,
                height,
                ink,
            })
        }
        "FAIL" => Some(Rendered::Fail {
            id: it.next()?.to_string(),
            reason: it.next().unwrap_or("").to_string(),
        }),
        _ => None,
    }
}

/// Render failures and blank pages. A page absent from `results` failed.
pub fn judge_renders(pages: &[Page], results: &[Rendered]) -> Vec<String> {
    let mut f = Vec::new();
    for p in pages {
        let ids = std::iter::once(p.id.clone()).chain(p.reference.as_ref().map(|_| ref_id(&p.id)));
        for id in ids {
            match results.iter().find(|r| match r {
                Rendered::Ok { id: i, .. } | Rendered::Fail { id: i, .. } => *i == id,
            }) {
                None => f.push(format!("{id}: the renderer produced no result")),
                Some(Rendered::Fail { reason, .. }) => {
                    f.push(format!("{id}: render failed: {reason}"))
                }
                Some(Rendered::Ok { ink: 0, .. }) => f.push(format!(
                    "{id}: renders blank (one flat colour); a blank page tests nothing"
                )),
                Some(Rendered::Ok { .. }) => {}
            }
        }
    }
    f
}

pub fn ref_id(id: &str) -> String {
    format!("{id}.ref")
}

// ------------------------------------------------------------------- compare

#[derive(Debug, PartialEq, Eq)]
pub enum Diff {
    Dimensions {
        baseline: (u32, u32),
        actual: (u32, u32),
    },
    Pixels {
        first: (u32, u32),
        baseline: [u8; 4],
        actual: [u8; 4],
        count: usize,
    },
}

/// Exact comparison of two decoded images; `None` when identical.
pub fn compare_images(baseline: &px_png::Image, actual: &px_png::Image) -> Option<Diff> {
    if (baseline.width, baseline.height) != (actual.width, actual.height) {
        return Some(Diff::Dimensions {
            baseline: (baseline.width, baseline.height),
            actual: (actual.width, actual.height),
        });
    }
    let mut first = None;
    let mut count = 0usize;
    let w = baseline.width.max(1) as usize;
    for (i, (b, a)) in baseline
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(actual.rgba.as_chunks::<4>().0.iter())
        .enumerate()
    {
        if b != a {
            count += 1;
            if first.is_none() {
                first = Some((
                    (i % w) as u32,
                    (i / w) as u32,
                    [b[0], b[1], b[2], b[3]],
                    [a[0], a[1], a[2], a[3]],
                ));
            }
        }
    }
    first.map(|(x, y, baseline, actual)| Diff::Pixels {
        first: (x, y),
        baseline,
        actual,
        count,
    })
}

pub fn describe_diff(id: &str, d: &Diff) -> String {
    match d {
        Diff::Dimensions { baseline, actual } => format!(
            "{id}: dimensions differ: baseline {}x{}, actual {}x{}",
            baseline.0, baseline.1, actual.0, actual.1
        ),
        Diff::Pixels {
            first,
            baseline,
            actual,
            count,
        } => format!(
            "{id}: {count} pixel(s) differ; first at ({}, {}): baseline {baseline:?}, actual {actual:?}",
            first.0, first.1
        ),
    }
}

// ----------------------------------------------------------------- selection

/// The corpus selection rule (ADR 033 (a)). `eligible` is the pairs that qualify
/// (test and reference both render, rasterize to identical bytes, and carry at
/// least [`MIN_INK_PIXELS`] of ink), **in sorted path order**. With `n` eligible
/// and quota `q`, `k = n / q` (at least 1) and the chosen pairs are the ones at
/// positions `0, k, 2k, ...` until `q` are taken. `None` when `n < q`. Nothing
/// depends on how a page looks.
pub fn select_every_kth(eligible: &[String], quota: usize) -> Option<(usize, Vec<String>)> {
    if quota == 0 || eligible.len() < quota {
        return None;
    }
    let k = (eligible.len() / quota).max(1);
    let chosen = eligible.iter().step_by(k).take(quota).cloned().collect();
    Some((k, chosen))
}

/// Whether a pair qualifies for selection, from the renderer's results and the two
/// PNG byte strings.
pub fn pair_eligible(test: &Rendered, reference: &Rendered, same_bytes: bool) -> bool {
    match (test, reference) {
        (Rendered::Ok { ink, .. }, Rendered::Ok { .. }) => same_bytes && *ink >= MIN_INK_PIXELS,
        _ => false,
    }
}

/// The `href` of the single `<link rel="match">` in a document, if there is exactly
/// one and the document has no `rel="mismatch"`.
pub fn match_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let mut hrefs = Vec::new();
    let mut from = 0;
    while let Some(i) = lower[from..].find("<link") {
        let start = from + i;
        let end = lower[start..].find('>').map(|e| start + e)?;
        let tag = &html[start..end];
        let tag_l = &lower[start..end];
        let rel = attr_value(tag, tag_l, "rel")
            .unwrap_or_default()
            .to_ascii_lowercase();
        if rel.split_ascii_whitespace().any(|w| w == "mismatch") {
            return None;
        }
        if rel.split_ascii_whitespace().any(|w| w == "match") {
            hrefs.push(attr_value(tag, tag_l, "href")?);
        }
        from = end;
    }
    if hrefs.len() == 1 { hrefs.pop() } else { None }
}

fn attr_value(tag: &str, tag_l: &str, name: &str) -> Option<String> {
    let mut search = 0;
    while let Some(i) = tag_l[search..].find(name) {
        let at = search + i;
        search = at + name.len();
        let before_ok = at > 0
            && tag_l
                .as_bytes()
                .get(at - 1)
                .is_some_and(|b| b.is_ascii_whitespace());
        let rest = tag.get(at + name.len()..)?.trim_start();
        if !before_ok {
            continue;
        }
        let rest = rest.strip_prefix('=')?.trim_start();
        let (q, body) = match rest.chars().next()? {
            c @ ('"' | '\'') => (Some(c), &rest[1..]),
            _ => (None, rest),
        };
        let end = match q {
            Some(c) => body.find(c)?,
            None => body
                .find(|c: char| c.is_ascii_whitespace() || c == '/')
                .unwrap_or(body.len()),
        };
        return Some(body[..end].to_string());
    }
    None
}

/// Resolve a reference href against its test's path (relative, repo-root-relative
/// `/`-separated strings); `/css/<module>/x` maps to `tests/wpt/<module>/x`.
pub fn resolve_href(test: &str, href: &str) -> Option<String> {
    let href = href.split(['#', '?']).next().unwrap_or("");
    if href.is_empty() || href.contains("://") || href.starts_with("//") {
        return None;
    }
    let mut parts: Vec<&str> = if let Some(rest) = href.strip_prefix('/') {
        let rest = rest.strip_prefix("css/")?;
        let mut v = vec!["tests", "wpt"];
        v.extend(rest.split('/'));
        return Some(normalise(v));
    } else {
        let mut v: Vec<&str> = test.split('/').collect();
        v.pop();
        v
    };
    parts.extend(href.split('/'));
    Some(normalise(parts))
}

fn normalise(parts: Vec<&str>) -> String {
    let mut out: Vec<&str> = Vec::new();
    for p in parts {
        match p {
            "" | "." => {}
            ".." => {
                out.pop();
            }
            p => out.push(p),
        }
    }
    out.join("/")
}

// ----------------------------------------------------------------------- run

fn list_html(dir: &Path, rel: &str, out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(Result::ok) {
        let name = e.file_name().to_string_lossy().into_owned();
        let rel = format!("{rel}/{name}");
        let p = e.path();
        if p.is_dir() {
            list_html(&p, &rel, out);
        } else if name.ends_with(".html") || name.ends_with(".htm") {
            out.push(rel);
        }
    }
}

/// Run the renderer over `specs` (`id=source`), writing PNGs into `out`.
fn run_renderer(
    root: &Path,
    out: &Path,
    twice: bool,
    specs: &[String],
) -> Result<Vec<Rendered>, String> {
    let mut results = Vec::new();
    for chunk in specs.chunks(60) {
        let mut c = proc::cargo_command(root);
        c.args([
            "run",
            "-q",
            "-p",
            "px-paint",
            "--example",
            "render_pages",
            "--",
        ]);
        if twice {
            c.arg("--twice");
        }
        c.arg(out).arg(root).args(chunk);
        let o = c.output().map_err(|e| format!("cannot run cargo: {e}"))?;
        if !o.status.success() {
            return Err(format!(
                "the renderer exited with {}: {}",
                o.status,
                String::from_utf8_lossy(&o.stderr).trim()
            ));
        }
        results.extend(
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(parse_render_line),
        );
    }
    Ok(results)
}

fn read_manifest(root: &Path) -> Result<Vec<Page>, String> {
    let text = std::fs::read_to_string(root.join(MANIFEST))
        .map_err(|e| format!("cannot read {MANIFEST}: {e}"))?;
    parse_manifest(&text)
}

fn baseline_stems(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join(BASELINES))
        .map(|rd| {
            rd.filter_map(Result::ok)
                .filter_map(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .strip_suffix(".png")
                        .map(String::from)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let bless = args.iter().any(|a| a == "--bless");
    let select = args.iter().any(|a| a == "--select");
    if let Some(bad) = args
        .iter()
        .find(|a| !matches!(a.as_str(), "--bless" | "--select"))
    {
        g.fail(format!(
            "unknown argument {bad} (usage: render [--bless | --select])"
        ));
        return;
    }
    g.info("Text paints as placeholder blocks until Phase 9; every baseline containing text is regenerated then (ADR 033 (c)).");
    if select {
        run_select(g, root);
        return;
    }
    if bless {
        let ci = std::env::var("CI").ok();
        if let Some(why) = bless_refusal(ci.as_deref()) {
            g.fail(why);
            return;
        }
    }

    let pages = match read_manifest(root) {
        Ok(p) => p,
        Err(e) => {
            g.fail(e);
            return;
        }
    };
    let stems = baseline_stems(root);
    for f in check_manifest(&pages, &stems, bless) {
        g.fail(f);
    }
    for p in &pages {
        if !root.join(&p.source).is_file() {
            g.fail(format!("page {}: source {} does not exist", p.id, p.source));
        }
        if let Some(r) = &p.reference
            && !root.join(r).is_file()
        {
            g.fail(format!("page {}: reference {r} does not exist", p.id));
        }
    }
    if !g.passed() {
        return;
    }

    let out = proc::target_dir(root).join("render-actual");
    let _ = std::fs::remove_dir_all(&out);
    let mut specs = Vec::new();
    for p in &pages {
        specs.push(format!("{}={}", p.id, p.source));
        if let Some(r) = &p.reference {
            specs.push(format!("{}={r}", ref_id(&p.id)));
        }
    }
    let results = match run_renderer(root, &out, true, &specs) {
        Ok(r) => r,
        Err(e) => {
            g.fail(e);
            return;
        }
    };
    for f in judge_renders(&pages, &results) {
        g.fail(f);
    }
    for p in &pages {
        if p.reference.is_some() {
            let t = std::fs::read(out.join(format!("{}.png", p.id)));
            let r = std::fs::read(out.join(format!("{}.png", ref_id(&p.id))));
            if let (Ok(t), Ok(r)) = (t, r)
                && t != r
            {
                g.fail(format!(
                    "{}: the test and its reference rendered differently",
                    p.id
                ));
            }
        }
    }
    if !g.passed() {
        return;
    }

    if bless {
        bless_baselines(g, root, &pages, &out);
        return;
    }
    let mut matched = 0;
    for p in &pages {
        let actual = std::fs::read(out.join(format!("{}.png", p.id)));
        let base = std::fs::read(root.join(BASELINES).join(format!("{}.png", p.id)));
        let (Ok(actual), Ok(base)) = (actual, base) else {
            g.fail(format!("{}: cannot read the actual or baseline PNG", p.id));
            continue;
        };
        match (px_png::decode(&base), px_png::decode(&actual)) {
            (Ok(b), Ok(a)) => match compare_images(&b, &a) {
                None => matched += 1,
                Some(d) => g.fail(describe_diff(&p.id, &d)),
            },
            (Err(e), _) => g.fail(format!("{}: baseline does not decode: {e}", p.id)),
            (_, Err(e)) => g.fail(format!("{}: actual does not decode: {e}", p.id)),
        }
    }
    g.info(format!(
        "{matched}/{} pages match their baselines exactly; actual PNGs are in {}",
        pages.len(),
        out.display()
    ));
    if g.passed() {
        g.ok(format!(
            "{} pages rendered twice byte-identically and match their baselines",
            pages.len()
        ));
    }
}

fn bless_baselines(g: &mut Gate, root: &Path, pages: &[Page], out: &Path) {
    let dir = root.join(BASELINES);
    if let Err(e) = std::fs::create_dir_all(&dir) {
        g.fail(format!("cannot create {BASELINES}: {e}"));
        return;
    }
    let mut changed = Vec::new();
    for p in pages {
        let Ok(actual) = std::fs::read(out.join(format!("{}.png", p.id))) else {
            g.fail(format!("{}: no rendered PNG to bless", p.id));
            continue;
        };
        let dest = dir.join(format!("{}.png", p.id));
        if std::fs::read(&dest).ok().as_deref() != Some(actual.as_slice()) {
            match std::fs::write(&dest, &actual) {
                Ok(()) => changed.push(format!("{BASELINES}/{}.png", p.id)),
                Err(e) => g.fail(format!("cannot write {}: {e}", dest.display())),
            }
        }
    }
    for stem in baseline_stems(root) {
        if !pages.iter().any(|p| p.id == stem) {
            let rel = format!("{BASELINES}/{stem}.png");
            if std::fs::remove_file(root.join(&rel)).is_ok() {
                changed.push(format!("{rel} (removed)"));
            }
        }
    }
    if changed.is_empty() {
        g.ok("bless: no baseline changed");
    } else {
        g.info(format!("bless changed {} file(s):", changed.len()));
        for c in &changed {
            g.info(format!("  {c}"));
        }
        g.ok("bless: review the PNG diff and give the reason in the commit message");
    }
}

fn run_select(g: &mut Gate, root: &Path) {
    let out = proc::target_dir(root).join("render-select");
    let _ = std::fs::remove_dir_all(&out);
    for (suite, quota) in SUITES {
        let base = format!("tests/wpt/{suite}");
        let mut files = Vec::new();
        list_html(&root.join(&base), &base, &mut files);
        files.sort();
        // (test, reference) in sorted order of the test path.
        let mut pairs: Vec<(String, String)> = Vec::new();
        for f in &files {
            let Ok(src) = std::fs::read_to_string(root.join(f)) else {
                continue;
            };
            if let Some(r) = match_href(&src).and_then(|h| resolve_href(f, &h))
                && root.join(&r).is_file()
            {
                pairs.push((f.clone(), r));
            }
        }
        let mut specs = Vec::new();
        for (i, (t, r)) in pairs.iter().enumerate() {
            specs.push(format!("t{i}={t}"));
            specs.push(format!("r{i}={r}"));
        }
        let results = match run_renderer(root, &out, false, &specs) {
            Ok(r) => r,
            Err(e) => {
                g.fail(e);
                return;
            }
        };
        let find = |id: &str| {
            results.iter().find(|r| match r {
                Rendered::Ok { id: i, .. } | Rendered::Fail { id: i, .. } => i == id,
            })
        };
        let mut eligible = Vec::new();
        for (i, (t, r)) in pairs.iter().enumerate() {
            let (Some(tr), Some(rr)) = (find(&format!("t{i}")), find(&format!("r{i}"))) else {
                continue;
            };
            let same = match (
                std::fs::read(out.join(format!("t{i}.png"))),
                std::fs::read(out.join(format!("r{i}.png"))),
            ) {
                (Ok(a), Ok(b)) => a == b,
                _ => false,
            };
            if pair_eligible(tr, rr, same) {
                eligible.push((t.clone(), r.clone()));
            }
        }
        let names: Vec<String> = eligible.iter().map(|(t, _)| t.clone()).collect();
        match select_every_kth(&names, *quota) {
            None => g.fail(format!(
                "{suite}: {} eligible pairs, fewer than the quota {quota}",
                names.len()
            )),
            Some((k, chosen)) => {
                g.info(format!(
                    "{suite}: {} pairs, {} eligible, k = {k}, quota {quota}",
                    pairs.len(),
                    names.len()
                ));
                for c in chosen {
                    if let Some((t, r)) = eligible.iter().find(|(t, _)| *t == c) {
                        g.info(format!("  {t} -> {r}"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn pg(id: &str) -> Page {
        Page {
            id: id.into(),
            source: format!("tests/render/pages/{id}.html"),
            reference: None,
        }
    }

    fn many(n: usize) -> (Vec<Page>, Vec<String>) {
        let pages: Vec<Page> = (0..n).map(|i| pg(&format!("p{i}"))).collect();
        let stems = pages.iter().map(|p| p.id.clone()).collect();
        (pages, stems)
    }

    #[test]
    fn manifest_parses_pages_and_rejects_junk() {
        let t = "# c\n[[page]]\nid = \"a\"\nsource = \"x.html\"\n\n[[page]]\nid = \"b\"\nsource = \"y.html\"\nreference = \"z.html\"\n";
        let p = parse_manifest(t).unwrap();
        assert_eq!(p.len(), 2);
        assert_eq!(p[1].reference.as_deref(), Some("z.html"));
        assert!(parse_manifest("id = \"a\"").is_err());
        assert!(parse_manifest("[[page]]\nid = a").is_err());
        assert!(parse_manifest("[[page]]\nid = \"a\"").is_err());
        assert!(parse_manifest("[[page]]\nid = \"a\"\nsource = \"s\"\nfoo = \"b\"").is_err());
        assert!(parse_manifest("[[page]]\nid = \"a\"\nid = \"b\"\nsource = \"s\"").is_err());
    }

    #[test]
    fn floor_is_pinned_at_25() {
        let (p, s) = many(25);
        assert!(check_manifest(&p, &s, false).is_empty());
        let (p, s) = many(24);
        let f = check_manifest(&p, &s, false);
        assert_eq!(f, ["the corpus has 24 pages, the pinned floor is 25"]);
    }

    #[test]
    fn missing_baseline_and_orphan_baseline_fail_but_bless_waives_them() {
        let (p, mut s) = many(25);
        s.remove(3);
        s.push("ghost".into());
        let f = check_manifest(&p, &s, false);
        assert_eq!(f.len(), 2, "{f:?}");
        assert!(f[0].contains("page p3 has no baseline"));
        assert!(f[1].contains("baseline ghost.png has no page"));
        assert!(check_manifest(&p, &s, true).is_empty());
    }

    #[test]
    fn duplicate_and_bad_ids_fail() {
        let (mut p, s) = many(25);
        p[1].id = "p0".into();
        p[2].id = "Bad Id".into();
        let f = check_manifest(&p, &s, true);
        assert!(f.iter().any(|m| m.contains("listed twice")));
        assert!(f.iter().any(|m| m.contains("must be lowercase")));
    }

    #[test]
    fn bless_refuses_when_ci_is_set() {
        assert!(bless_refusal(Some("true")).is_some());
        assert!(bless_refusal(Some("1")).is_some());
        assert!(bless_refusal(Some("")).is_none());
        assert!(bless_refusal(None).is_none());
    }

    #[test]
    fn bless_run_is_refused_under_ci_without_touching_anything() {
        // SAFETY-free: the refusal is the pure function above; this checks that
        // `run` reaches it before reading the (absent) manifest.
        let fx = Fixture::new("render-bless-ci");
        // Environment is process-global, so only assert through the pure path.
        assert!(bless_refusal(Some("true")).unwrap().contains("CI"));
        assert!(!fx.path().join(BASELINES).exists());
    }

    #[test]
    fn render_lines_parse() {
        assert_eq!(
            parse_render_line("OK a 800 600 1234"),
            Some(Rendered::Ok {
                id: "a".into(),
                width: 800,
                height: 600,
                ink: 1234
            })
        );
        assert_eq!(
            parse_render_line("FAIL a layout failed"),
            Some(Rendered::Fail {
                id: "a".into(),
                reason: "layout failed".into()
            })
        );
        assert_eq!(parse_render_line("warning: x"), None);
        assert_eq!(parse_render_line("OK a 8 x 9"), None);
    }

    #[test]
    fn blank_failed_and_missing_renders_fail() {
        let pages = vec![pg("a"), pg("b"), pg("c"), pg("d")];
        let ok = |id: &str, ink| Rendered::Ok {
            id: id.into(),
            width: 1,
            height: 1,
            ink,
        };
        let results = vec![
            ok("a", 10),
            ok("b", 0),
            Rendered::Fail {
                id: "c".into(),
                reason: "boom".into(),
            },
        ];
        let f = judge_renders(&pages, &results);
        assert_eq!(f.len(), 3, "{f:?}");
        assert!(f[0].starts_with("b: renders blank"));
        assert!(f[1].starts_with("c: render failed: boom"));
        assert!(f[2].starts_with("d: the renderer produced no result"));
    }

    #[test]
    fn wpt_page_requires_its_reference_to_render_too() {
        let mut p = pg("w");
        p.reference = Some("r.html".into());
        let f = judge_renders(
            &[p],
            &[Rendered::Ok {
                id: "w".into(),
                width: 1,
                height: 1,
                ink: 5,
            }],
        );
        assert_eq!(f, ["w.ref: the renderer produced no result"]);
    }

    fn img(w: u32, h: u32, rgba: Vec<u8>) -> px_png::Image {
        px_png::Image {
            width: w,
            height: h,
            rgba,
        }
    }

    #[test]
    fn identical_images_compare_equal_and_one_pixel_is_found() {
        let a = img(2, 2, vec![0; 16]);
        assert_eq!(compare_images(&a, &a), None);
        let mut b = a.clone();
        b.rgba[4 * 3] = 9; // pixel (1, 1)
        b.rgba[0] = 1; // pixel (0, 0)
        let d = compare_images(&a, &b).unwrap();
        assert_eq!(
            d,
            Diff::Pixels {
                first: (0, 0),
                baseline: [0; 4],
                actual: [1, 0, 0, 0],
                count: 2
            }
        );
        assert!(describe_diff("x", &d).contains("2 pixel(s) differ; first at (0, 0)"));
    }

    #[test]
    fn different_dimensions_fail_even_with_equal_bytes_count() {
        let a = img(2, 1, vec![0; 8]);
        let b = img(1, 2, vec![0; 8]);
        assert!(matches!(
            compare_images(&a, &b),
            Some(Diff::Dimensions { .. })
        ));
    }

    #[test]
    fn png_round_trip_through_the_included_codec() {
        let rgba: Vec<u8> = (0..4 * 4 * 4).map(|i| (i * 7) as u8).collect();
        let png = px_png::encode(4, 4, &rgba).unwrap();
        let back = px_png::decode(&png).unwrap();
        assert_eq!(compare_images(&img(4, 4, rgba), &back), None);
    }

    #[test]
    fn selection_takes_every_kth_in_order() {
        let e: Vec<String> = (0..10).map(|i| format!("p{i}")).collect();
        // n = 10, q = 3: k = 3, positions 0, 3, 6.
        assert_eq!(
            select_every_kth(&e, 3),
            Some((3, vec!["p0".into(), "p3".into(), "p6".into()]))
        );
        // n = 3, q = 3: k = 1, everything.
        assert_eq!(
            select_every_kth(&e[..3], 3).map(|(k, v)| (k, v.len())),
            Some((1, 3))
        );
        // Fewer than the quota is not silently padded.
        assert_eq!(select_every_kth(&e[..2], 3), None);
        assert_eq!(select_every_kth(&e, 0), None);
        // k = n / q: n = 11, q = 6 -> k = 1, the first six.
        let e11: Vec<String> = (0..11).map(|i| format!("p{i}")).collect();
        let (k, v) = select_every_kth(&e11, 6).unwrap();
        assert_eq!((k, v.last().map(String::as_str)), (1, Some("p5")));
    }

    #[test]
    fn eligibility_needs_identical_bytes_and_real_ink() {
        let ok = |ink| Rendered::Ok {
            id: "x".into(),
            width: 1,
            height: 1,
            ink,
        };
        assert!(pair_eligible(
            &ok(MIN_INK_PIXELS),
            &ok(MIN_INK_PIXELS),
            true
        ));
        assert!(!pair_eligible(&ok(MIN_INK_PIXELS - 1), &ok(1), true));
        assert!(!pair_eligible(
            &ok(MIN_INK_PIXELS),
            &ok(MIN_INK_PIXELS),
            false
        ));
        let fail = Rendered::Fail {
            id: "x".into(),
            reason: String::new(),
        };
        assert!(!pair_eligible(&fail, &ok(MIN_INK_PIXELS), true));
    }

    #[test]
    fn match_href_reads_one_match_link() {
        let h = "<title>x</title><link rel=\"help\" href=\"h\"><link rel=\"match\" href=\"a-ref.html\">";
        assert_eq!(match_href(h).as_deref(), Some("a-ref.html"));
        assert_eq!(
            match_href("<link href='b.html' rel='match'>").as_deref(),
            Some("b.html")
        );
        assert_eq!(
            match_href("<link rel=match href=c.html>").as_deref(),
            Some("c.html")
        );
        assert_eq!(match_href("<link rel=mismatch href=c.html>"), None);
        assert_eq!(
            match_href("<link rel=match href=a><link rel=match href=b>"),
            None
        );
        assert_eq!(match_href("<p>none</p>"), None);
    }

    #[test]
    fn hrefs_resolve_like_the_reftest_harness() {
        assert_eq!(
            resolve_href("tests/wpt/css2/floats/a.html", "a-ref.html").as_deref(),
            Some("tests/wpt/css2/floats/a-ref.html")
        );
        assert_eq!(
            resolve_href("tests/wpt/css-grid/abspos/a.html", "../ref/b.html").as_deref(),
            Some("tests/wpt/css-grid/ref/b.html")
        );
        assert_eq!(
            resolve_href("tests/wpt/css-grid/a.html", "/css/css-grid/ref/c.html").as_deref(),
            Some("tests/wpt/css-grid/ref/c.html")
        );
        assert_eq!(resolve_href("a.html", "http://x/y"), None);
    }

    #[test]
    fn baseline_stems_lists_png_files_sorted() {
        let fx = Fixture::new("render-stems");
        fx.write_bytes("tests/render/baselines/b.png", b"x")
            .write_bytes("tests/render/baselines/a.png", b"x")
            .write("tests/render/baselines/README", "x");
        assert_eq!(baseline_stems(fx.path()), ["a", "b"]);
    }
}
