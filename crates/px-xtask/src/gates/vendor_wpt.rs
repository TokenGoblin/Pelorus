//! `cargo xtask vendor-wpt` -- vendor a WPT subset into `tests/wpt/`.
//! Port of `ci/vendor-css2-reftests.sh` (ADR 029, ADR 031, ADR 032).
//!
//! This is a developer TOOL, not a pass/fail gate: it reports through the same
//! `Gate` plumbing, and fails only when it cannot do its job (no `gh`, no
//! network, a checkout that is not a checkout).
//!
//! Usage:
//!
//! ```text
//! cargo xtask vendor-wpt [<sha>]                        # CSS2 subset, WPT master by default
//! cargo xtask vendor-wpt [<sha>] --module css-flexbox   # reftests by <link rel=match>
//! cargo xtask vendor-wpt [<sha>] --module css-grid
//! cargo xtask vendor-wpt ... --from-checkout <dir>      # read a local WPT checkout, not gh
//! ```
//!
//! Two selection modes, because the corpora differ:
//!
//! * CSS2 (default): `.html` files in a fixed list of directories, a test paired
//!   with the `-ref.html` of the same name; crashtests to `tests/wpt/css2-crashtests`
//!   (`$CRASH_DEST` overrides). Behaviour is the script's, line for line.
//! * `--module`: Phase 7 showed that pairing on the file name silently drops
//!   nearly half of a modern module (references are shared and live in
//!   subdirectories), so the reference is read from the test's
//!   `<link rel="match" href>`. See `select_module` for the rule.
//!
//! Network access is `gh api` / `git` as children (`proc::capture`); the JSON
//! the script extracted with `--jq` is still extracted with `--jq`, as TSV, so
//! there is no JSON parser here. `--from-checkout` needs neither.
//!
//! Everything that decides what is vendored is a pure function over a file
//! listing and file contents, so it is tested without a network.

use crate::Gate;
use crate::proc;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const REPO: &str = "web-platform-tests/wpt";
const CSS2_DEST: &str = "tests/wpt/css2";
const CSS2_CRASH_DEST: &str = "tests/wpt/css2-crashtests";

/// The directories that map onto Phase 6's scope: "box tree, BFCs, inline,
/// floats, positioned" (section 9). Deliberately not the whole of CSS2:
/// generated-content needs ::before/::after box generation, pagination is print
/// layout, lists need list markers, tables are their own layout algorithm.
const CSS2_DIRS: &[&str] = &[
    "normal-flow",
    "floats",
    "floats-clear",
    "box-display",
    "visuren",
    "positioning",
    "linebox",
];

// ---------------------------------------------------------------------------
// Arguments

#[derive(Debug, PartialEq, Eq)]
pub struct Opts {
    pub rev: String,
    pub module: Option<String>,
    pub checkout: Option<PathBuf>,
}

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut rev: Option<String> = None;
    let mut module = None;
    let mut checkout = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match flag {
            "--module" | "--from-checkout" => {
                let v = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))?,
                };
                if flag == "--module" {
                    if module_spec(&v).is_none() {
                        return Err(format!(
                            "unknown module `{v}` (expected css-flexbox, css-grid or css-tables)"
                        ));
                    }
                    module = Some(v);
                } else {
                    checkout = Some(PathBuf::from(v));
                }
            }
            f if f.starts_with("--") => return Err(format!("unknown option `{f}`")),
            _ => {
                if rev.is_some() {
                    return Err(format!("unexpected argument `{a}`"));
                }
                rev = Some(a.clone());
            }
        }
    }
    Ok(Opts {
        rev: rev.unwrap_or_default(),
        module,
        checkout,
    })
}

// ---------------------------------------------------------------------------
// Sources: a file listing plus a way to read one file

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to the subtree root (`css/CSS2` or `css/<module>`).
    pub path: String,
    /// Blob sha for `gh`; the relative path for a checkout.
    pub id: String,
}

trait Source {
    fn list(&self) -> Result<Vec<Entry>, String>;
    fn read(&self, e: &Entry) -> Result<Vec<u8>, String>;
}

struct Checkout {
    dir: PathBuf,
}

impl Source for Checkout {
    fn list(&self) -> Result<Vec<Entry>, String> {
        let mut out = Vec::new();
        walk(&self.dir, "", &mut out)?;
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }
    fn read(&self, e: &Entry) -> Result<Vec<u8>, String> {
        std::fs::read(self.dir.join(&e.id)).map_err(|er| format!("read {}: {er}", e.id))
    }
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<Entry>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let ty = ent.file_type().map_err(|e| format!("file_type {r}: {e}"))?;
        if ty.is_dir() {
            walk(&ent.path(), &r, out)?;
        } else if ty.is_file() {
            out.push(Entry {
                path: r.clone(),
                id: r,
            });
        }
    }
    Ok(())
}

struct Gh {
    resolved: String,
    tree: String,
}

fn gh(args: &[&str]) -> Result<String, String> {
    let out = proc::capture("gh", args, None)?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Resolve `rev` to a commit and walk `subtree` (e.g. `["css", "CSS2"]`) down to
/// its tree sha, one cheap call per level, then the caller reads the whole
/// subtree in one recursive call rather than one API call per file.
fn gh_open(rev: &str, subtree: &[&str]) -> Result<Gh, String> {
    let resolved = gh(&[
        "api",
        &format!("repos/{REPO}/commits/{rev}"),
        "--jq",
        ".sha",
    ])?;
    if !is_hex(&resolved) {
        return Err(format!("`{rev}` did not resolve to a commit sha"));
    }
    let mut tree = resolved.clone();
    for part in subtree {
        let jq = format!(".tree[] | select(.path==\"{part}\") | .sha");
        tree = gh(&[
            "api",
            &format!("repos/{REPO}/git/trees/{tree}"),
            "--jq",
            &jq,
        ])?;
        if !is_hex(&tree) {
            return Err(format!("no `{part}` in the tree of {resolved}"));
        }
    }
    Ok(Gh { resolved, tree })
}

impl Source for Gh {
    fn list(&self) -> Result<Vec<Entry>, String> {
        let out = gh(&[
            "api",
            &format!("repos/{REPO}/git/trees/{}?recursive=1", self.tree),
            "--jq",
            ".tree[] | select(.type==\"blob\") | [.path, .sha] | @tsv",
        ])?;
        Ok(parse_tsv_listing(&out))
    }
    fn read(&self, e: &Entry) -> Result<Vec<u8>, String> {
        if !is_hex(&e.id) {
            return Err(format!("bad blob id for {}", e.path));
        }
        // The blob API rather than raw.githubusercontent, so the content is
        // addressed by the sha the tree listing gave: the file cannot change
        // between enumerating and fetching it.
        let b64 = gh(&[
            "api",
            &format!("repos/{REPO}/git/blobs/{}", e.id),
            "--jq",
            ".content",
        ])?;
        base64_decode(&b64)
    }
}

pub fn parse_tsv_listing(s: &str) -> Vec<Entry> {
    s.lines()
        .filter_map(|l| l.split_once('\t'))
        .map(|(p, i)| Entry {
            path: p.to_string(),
            id: i.trim().to_string(),
        })
        .collect()
}

/// Standard-alphabet base64, ignoring whitespace (GitHub wraps at 60 columns).
pub fn base64_decode(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' | b'\t' => continue,
            _ => return Err(format!("invalid base64 byte {b:#x}")),
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// CSS2 mode: pure selection

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Css2Plan {
    pub crashtests: Vec<Entry>,
    /// (test, reference)
    pub pairs: Vec<(Entry, Entry)>,
}

/// The script's selection: `.html` files under `dirs` (in `dirs` order, then
/// listing order); a `-ref.html` is fetched with its test; a path under a
/// `crashtests/` directory or named `*-crash.html` is a crashtest (it has no
/// reference); any other test needs a `<name>-ref.html` or it is not a reftest.
pub fn plan_css2(listing: &[Entry], dirs: &[&str]) -> Css2Plan {
    let mut wanted: Vec<&Entry> = Vec::new();
    for d in dirs {
        let prefix = format!("{d}/");
        wanted.extend(
            listing
                .iter()
                .filter(|e| e.path.starts_with(&prefix) && e.path.ends_with(".html")),
        );
    }
    let mut plan = Css2Plan::default();
    for e in &wanted {
        if e.path.ends_with("-ref.html") {
            continue;
        }
        if e.path.contains("/crashtests/") || e.path.ends_with("-crash.html") {
            plan.crashtests.push((*e).clone());
            continue;
        }
        let want = format!("{}-ref.html", e.path.trim_end_matches(".html"));
        if let Some(r) = wanted.iter().find(|r| r.path == want) {
            plan.pairs.push(((*e).clone(), (*r).clone()));
        }
    }
    plan
}

// ---------------------------------------------------------------------------
// A small HTML tag scanner (no third-party tokenizer)

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    /// Lowercased element name.
    pub name: String,
    /// Lowercased attribute names with their values.
    pub attrs: Vec<(String, String)>,
}

impl Tag {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

/// Start tags of an HTML document, in order. Comments, doctype, end tags and the
/// bodies of raw-text elements (`script`, `style`, `textarea`, `title`) are
/// skipped, so markup inside them is not mistaken for elements. Enough of the
/// tokenizer for selection; not a parser.
pub fn start_tags(html: &str) -> Vec<Tag> {
    let b = html.as_bytes();
    let n = b.len();
    let mut tags = Vec::new();
    let mut i = 0;
    while i < n {
        if b[i] != b'<' {
            i += 1;
            continue;
        }
        if b[i..].starts_with(b"<!--") {
            i = find(b, i + 4, b"-->").map_or(n, |p| p + 3);
            continue;
        }
        match b.get(i + 1) {
            Some(b'!') | Some(b'?') | Some(b'/') => {
                i = find(b, i, b">").map_or(n, |p| p + 1);
                continue;
            }
            Some(c) if c.is_ascii_alphabetic() => {}
            _ => {
                i += 1;
                continue;
            }
        }
        let mut j = i + 1;
        while j < n && !is_ws(b[j]) && b[j] != b'/' && b[j] != b'>' {
            j += 1;
        }
        let name = lower(&b[i + 1..j]);
        let mut attrs = Vec::new();
        loop {
            while j < n && (is_ws(b[j]) || b[j] == b'/') {
                j += 1;
            }
            if j >= n {
                break;
            }
            if b[j] == b'>' {
                j += 1;
                break;
            }
            let s = j;
            // A leading `=` is part of the name, per the HTML tokenizer.
            j += 1;
            while j < n && !is_ws(b[j]) && b[j] != b'=' && b[j] != b'>' && b[j] != b'/' {
                j += 1;
            }
            let an = lower(&b[s..j]);
            while j < n && is_ws(b[j]) {
                j += 1;
            }
            let mut val = String::new();
            if j < n && b[j] == b'=' {
                j += 1;
                while j < n && is_ws(b[j]) {
                    j += 1;
                }
                if j < n && (b[j] == b'"' || b[j] == b'\'') {
                    let q = b[j];
                    let vs = j + 1;
                    let ve = b[vs..].iter().position(|&c| c == q).map_or(n, |p| vs + p);
                    val = String::from_utf8_lossy(&b[vs..ve]).into_owned();
                    j = (ve + 1).min(n);
                } else {
                    let vs = j;
                    while j < n && !is_ws(b[j]) && b[j] != b'>' {
                        j += 1;
                    }
                    val = String::from_utf8_lossy(&b[vs..j]).into_owned();
                }
            }
            attrs.push((an, val));
        }
        let raw = matches!(name.as_str(), "script" | "style" | "textarea" | "title");
        tags.push(Tag {
            name: name.clone(),
            attrs,
        });
        i = j;
        if raw {
            let close = format!("</{name}");
            i = find_ci(b, i, close.as_bytes()).unwrap_or(n);
        }
    }
    tags
}

fn lower(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_lowercase()
}

fn find(h: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= h.len() {
        return None;
    }
    h[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

fn find_ci(h: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= h.len() {
        return None;
    }
    h[from..]
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| p + from)
}

/// What selection needs to know about one document.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The document has a `<link>` whose rel contains `match` or `mismatch`.
    pub is_reftest: bool,
    pub has_mismatch: bool,
    /// `href` of every `<link rel=match>`.
    pub match_hrefs: Vec<String>,
    /// A `<script>` element or any attribute whose name starts with `on`
    /// (the same test the reftest harness's `needs_script` applies).
    pub scripted: bool,
    /// Files the document needs to render: `<link rel=stylesheet href>`,
    /// `<img|source|video|audio|iframe|embed src>`, `<object data>`.
    pub support: Vec<String>,
}

pub fn scan(html: &str) -> Scan {
    let mut s = Scan::default();
    for t in start_tags(html) {
        if t.name == "script" || t.attrs.iter().any(|(n, _)| n.starts_with("on")) {
            s.scripted = true;
        }
        match t.name.as_str() {
            "link" => {
                let rel = t.attr("rel").unwrap_or("").to_ascii_lowercase();
                let toks: Vec<&str> = rel.split_ascii_whitespace().collect();
                let href = t.attr("href");
                if toks.contains(&"match") {
                    s.is_reftest = true;
                    if let Some(h) = href {
                        s.match_hrefs.push(h.trim().to_string());
                    }
                }
                if toks.contains(&"mismatch") {
                    s.is_reftest = true;
                    s.has_mismatch = true;
                }
                if toks.contains(&"stylesheet")
                    && let Some(h) = href
                {
                    s.support.push(h.trim().to_string());
                }
            }
            "img" | "source" | "video" | "audio" | "iframe" | "embed" => {
                if let Some(v) = t.attr("src") {
                    s.support.push(v.trim().to_string());
                }
            }
            "object" => {
                if let Some(v) = t.attr("data") {
                    s.support.push(v.trim().to_string());
                }
            }
            _ => {}
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Module mode: href resolution and selection

pub struct ModuleSpec {
    pub name: &'static str,
    /// Directory names excluded anywhere under the module.
    pub excluded: &'static [&'static str],
}

pub fn module_spec(name: &str) -> Option<ModuleSpec> {
    match name {
        // crashtests are vendored separately; ADR 031 item 2 excludes grid-lanes.
        "css-flexbox" => Some(ModuleSpec {
            name: "css-flexbox",
            excluded: &["crashtests"],
        }),
        "css-grid" => Some(ModuleSpec {
            name: "css-grid",
            excluded: &["crashtests", "grid-lanes"],
        }),
        "css-tables" => Some(ModuleSpec {
            name: "css-tables",
            excluded: &["crashtests"],
        }),
        _ => None,
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// Path relative to the module root.
    Inside(String),
    /// Anywhere else; the path is WPT-root-relative (`css/support/x.png`).
    Outside(String),
}

/// Resolve `href` as written in the module file `from` (module-relative).
/// `None` for anything that is not a path in the WPT tree (a URL with a scheme,
/// `//host`, a bare fragment, empty).
pub fn resolve(module: &ModuleSpec, from: &str, href: &str) -> Option<Resolved> {
    let href = href.split(['#', '?']).next().unwrap_or("").trim();
    if href.is_empty() || href.starts_with("//") {
        return None;
    }
    if let Some(colon) = href.find(':') {
        let scheme = &href[..colon];
        if !scheme.contains('/')
            && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'-' | b'.'))
        {
            return None;
        }
    }
    let mut stack: Vec<&str> = Vec::new();
    let rel: &str = if let Some(abs) = href.strip_prefix('/') {
        abs
    } else {
        stack.push("css");
        stack.push(module.name);
        if let Some((dir, _)) = from.rsplit_once('/') {
            stack.extend(dir.split('/'));
        }
        href
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                stack.pop();
            }
            s => stack.push(s),
        }
    }
    let full = stack.join("/");
    let prefix = format!("css/{}/", module.name);
    Some(match full.strip_prefix(&prefix) {
        Some(inner) if !is_excluded(module, inner) => Resolved::Inside(inner.to_string()),
        _ => Resolved::Outside(full),
    })
}

pub fn is_excluded(module: &ModuleSpec, path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|p| module.excluded.contains(p))
}

fn ext(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, e)| e)
}

fn is_html(path: &str) -> bool {
    matches!(ext(path), "html" | "htm")
}

#[derive(Debug, Default)]
pub struct Selection {
    pub candidates: usize,
    /// (test, reference), module-relative, in path order.
    pub pairs: Vec<(String, String)>,
    pub refs: BTreeSet<String>,
    /// Support files that are neither a kept test nor a kept reference.
    pub support: BTreeSet<String>,
    pub dropped_scripted: usize,
    /// Reason -> count, for everything else dropped.
    pub dropped_other: BTreeMap<String, usize>,
    /// Needed by a vendored file but not in the tree: WPT-root path -> users.
    pub gaps: BTreeMap<String, BTreeSet<String>>,
    /// `.html` crashtests (file name contains `crash`), module-relative.
    pub crashtests: Vec<String>,
}

impl Selection {
    pub fn tests(&self) -> BTreeSet<&str> {
        self.pairs.iter().map(|(t, _)| t.as_str()).collect()
    }
    /// Every file to vendor, tests, references and support, sorted.
    pub fn files(&self) -> BTreeSet<String> {
        let mut s: BTreeSet<String> = self.support.clone();
        for (t, r) in &self.pairs {
            s.insert(t.clone());
            s.insert(r.clone());
        }
        s
    }
}

fn drop_other(sel: &mut Selection, reason: String) {
    *sel.dropped_other.entry(reason).or_insert(0) += 1;
}

/// The Phase 7 rule, written down by hand in the two PROVENANCE files and now
/// executable. `files` is the module's module-relative file list; `read`
/// returns a file's text.
///
///  1. Walk the module excluding the module's excluded directories.
///  2. A candidate is a document with a `<link>` whose rel contains `match` or
///     `mismatch`.
///  3. Drop it if it is not `.html`/`.htm`, if it is `rel=mismatch`, or if it is
///     scripted (a `<script>` or an `on*` attribute), checked in that order.
///  4. Resolve its single `rel=match` href against its own directory.
///  5. Drop the pair if the reference is not `.html`/`.htm`, is outside the
///     module (or in an excluded directory), is missing, or is scripted.
///  6. Vendor the test, its reference (once, however many tests share it) and
///     the support files either names, when they exist inside the module.
pub fn select_module(
    module: &ModuleSpec,
    files: &BTreeSet<String>,
    read: &mut dyn FnMut(&str) -> Result<String, String>,
) -> Result<Selection, String> {
    let mut sel = Selection::default();
    let mut needs: Vec<(String, Vec<String>)> = Vec::new(); // kept file -> its support hrefs
    for path in files {
        // Crashtests are taken from the whole module, excluded directories
        // included: they are vendored separately and need no reference.
        if is_html(path) && path.rsplit('/').next().is_some_and(|n| n.contains("crash")) {
            sel.crashtests.push(path.clone());
        }
        if is_excluded(module, path) {
            continue;
        }
        if !matches!(ext(path), "html" | "htm" | "xht" | "xhtml" | "svg") {
            continue;
        }
        let sc = scan(&read(path)?);
        if !sc.is_reftest {
            continue;
        }
        sel.candidates += 1;
        if !is_html(path) {
            drop_other(&mut sel, format!("test is .{}", ext(path)));
            continue;
        }
        if sc.has_mismatch {
            drop_other(&mut sel, "rel=mismatch".into());
            continue;
        }
        if sc.scripted {
            sel.dropped_scripted += 1;
            continue;
        }
        if sc.match_hrefs.len() != 1 {
            drop_other(
                &mut sel,
                format!("test has {} rel=match links", sc.match_hrefs.len()),
            );
            continue;
        }
        let rpath = match resolve(module, path, &sc.match_hrefs[0]) {
            None => {
                drop_other(&mut sel, "reference href is not a path".into());
                continue;
            }
            Some(Resolved::Outside(p)) | Some(Resolved::Inside(p)) if !is_html(&p) => {
                drop_other(&mut sel, format!("reference is .{}", ext(&p)));
                continue;
            }
            Some(Resolved::Outside(_)) => {
                drop_other(&mut sel, "reference outside this module".into());
                continue;
            }
            Some(Resolved::Inside(p)) => p,
        };
        if !files.contains(&rpath) {
            drop_other(&mut sel, "reference missing".into());
            continue;
        }
        let rsc = scan(&read(&rpath)?);
        if rsc.scripted {
            drop_other(&mut sel, "reference is scripted".into());
            continue;
        }
        sel.pairs.push((path.clone(), rpath.clone()));
        sel.refs.insert(rpath.clone());
        needs.push((path.clone(), sc.support));
        needs.push((rpath, rsc.support));
    }
    for (user, hrefs) in needs {
        for h in hrefs {
            match resolve(module, &user, &h) {
                None => {}
                Some(Resolved::Inside(p)) if files.contains(&p) => {
                    sel.support.insert(p);
                }
                Some(Resolved::Inside(p)) => {
                    sel.gaps
                        .entry(format!("css/{}/{p}", module.name))
                        .or_default()
                        .insert(user.clone());
                }
                Some(Resolved::Outside(p)) => {
                    sel.gaps.entry(p).or_default().insert(user.clone());
                }
            }
        }
    }
    let kept: BTreeSet<String> = sel
        .pairs
        .iter()
        .flat_map(|(t, r)| [t.clone(), r.clone()])
        .collect();
    sel.support.retain(|p| !kept.contains(p));
    Ok(sel)
}

// ---------------------------------------------------------------------------
// PROVENANCE text

pub fn css2_provenance(resolved: &str, pairs: usize, files: usize) -> String {
    let dirs = CSS2_DIRS.join(" ");
    format!(
        "WPT CSS2 reftest subset, vendored by `cargo xtask vendor-wpt`.

repository  https://github.com/{REPO}
commit      {resolved}
directories {dirs}
selection   .html files only, and only those with a matching -ref.html
pairs       {pairs}
files       {files}

Why only .html: css/CSS2 holds 10,501 .xht files against 816 .html. px-dom has
no XML path, and whether html5ever's HTML parser produces the tree an XML parser
would was an empirical question about a parser this project did not write.

Answered at the end of Phase 6 by sampling thirty .xht files: it does not, and not
for the reason you would guess. There are no self-closing non-void elements in the
sample at all. What there is, in half of them, is a bare CDATA section immediately
inside <style> -- and an HTML parser hands that to the CSS parser as stylesheet
text, which discards the entire stylesheet rather than one rule. A test and a
reference with no styles are both the user-agent skeleton, and two skeletons agree,
so these files would raise the conformance number without measuring anything.
ADR 029 has the measurement.

This paragraph lives in crates/px-xtask/src/gates/vendor_wpt.rs, because PROVENANCE
is generated and the first version of the note was written into the file and lost
the next time the tool ran.

Why these directories: they are section 9 Phase 6's scope -- box tree, BFCs,
inline, floats, positioned. generated-content, pagination, lists and tables each
need a subsystem this phase does not build.

Refresh by re-running the tool. The diff is the record of what moved.
"
    )
}

pub fn css2_crash_provenance(resolved: &str, crashtests: usize) -> String {
    let dirs = CSS2_DIRS.join(" ");
    format!(
        "WPT CSS2 crashtests, vendored by `cargo xtask vendor-wpt`.

repository  https://github.com/{REPO}
commit      {resolved}
directories {dirs}
selection   files under a crashtests/ directory, or named *-crash.html
files       {crashtests}

A crashtest carries no reference and is not compared against one. It passes if
the engine does not crash on it -- and for this project that means it does not
panic, does not overflow its stack and does not fail to terminate, because
px-layout contains no unsafe code and has no other way to crash.

These were skipped for the whole of Phase 6 because the pairing rule matches a
test with a `-ref.html` of the same name and a crashtest has none. They are the
inputs that made Chrome and Firefox crash, so as an adversarial corpus for
exactly the features these directories cover they are better chosen than
anything written by hand.
"
    )
}

pub fn module_provenance(module: &ModuleSpec, resolved: &str, sel: &Selection) -> String {
    let name = module.name;
    let kept = sel.pairs.len();
    let total_files = sel.files().len();
    let mut dropped = String::new();
    for (reason, n) in &sel.dropped_other {
        dropped.push_str(&format!("      {reason:<66}{n:>5}\n"));
    }
    let dropped_other: usize = sel.dropped_other.values().sum();
    let mut gaps = String::new();
    for (p, users) in &sel.gaps {
        gaps.push_str(&format!("  {p}  ({} files)\n", users.len()));
    }
    if gaps.is_empty() {
        gaps.push_str("  none\n");
    }
    let excluded = module.excluded.join(", ");
    format!(
        "WPT {name} reftest subset (ADR 031), vendored by `cargo xtask vendor-wpt --module {name}`.

repository  https://github.com/{REPO}
commit      {resolved}  (css/{name})
paths       preserved relative to css/{name}/

counts
  candidates (files with a <link rel=match|mismatch>; {excluded} excluded)  {cand}
  kept as test+reference pairs                                       {kept}
  distinct reference files among them (shared ones vendored once)    {refs}
  support files (stylesheets, images) a kept file needs              {support}
  files vendored (tests + references + support, excluding this file) {total_files}

  dropped-scripted (<script> or an on* attribute in the test)        {scripted}
  dropped-other                                                      {dropped_other}
{dropped}
Selection rule (executable: crates/px-xtask/src/gates/vendor_wpt.rs, select_module)
  1. Walk css/{name}/ excluding directories named {excluded}.
  2. A file is a candidate if it has a <link> whose rel contains \"match\" or
     \"mismatch\".
  3. Drop the candidate if it is not .html/.htm, if it has rel=mismatch, or if
     it contains a <script> element or any attribute whose name starts with
     \"on\", checked in that order.
  4. Resolve the single rel=match href against the test's own directory
     (absolute /css/{name}/... hrefs resolve inside the module).
  5. Drop the pair if the reference is not .html/.htm, is outside css/{name}/,
     is missing, or itself contains script or on* attributes.
  6. Vendor the test, its reference (once if shared), and any file named by
     <link rel=stylesheet href>, <img src>, <source|video|audio|iframe|embed
     src> or <object data> in either, if that file exists inside css/{name}/.

Known gaps that remain in the denominator (nothing was removed to hide them):
files a vendored test or reference names that are not in the module, so are NOT
vendored. The harness reports these as \"support file not vendored\".
{gaps}
Refresh by re-running the tool against a new commit; the diff is the record.
",
        cand = sel.candidates,
        refs = sel.refs.len(),
        support = sel.support.len(),
        scripted = sel.dropped_scripted,
    )
}

pub fn module_crash_provenance(module: &ModuleSpec, resolved: &str, n: usize) -> String {
    let name = module.name;
    format!(
        "WPT {name} crashtests, vendored by `cargo xtask vendor-wpt --module {name}`.

repository  https://github.com/{REPO}
commit      {resolved}
directory   css/{name} (-> tests/wpt/{name}-crashtests)
selection   every .html under that directory whose file name contains \"crash\",
            at any depth, with the relative path preserved
files       {n}

A crashtest has no reference and no assertions: it passes if layout does not
panic, overflow the stack or fail to terminate. Some carry a <script>, which
cannot run until Phase 10; they are still laid out as written, as the CSS2
crashtests are.
"
    )
}

// ---------------------------------------------------------------------------
// Writing

fn safe_rel(p: &str) -> Result<(), String> {
    if p.is_empty() || p.starts_with('/') || p.split('/').any(|s| s == ".." || s.is_empty()) {
        Err(format!("refusing unsafe path `{p}`"))
    } else {
        Ok(())
    }
}

fn write_file(dest: &Path, rel: &str, data: &[u8]) -> Result<(), String> {
    safe_rel(rel)?;
    let p = dest.join(rel);
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| format!("mkdir {}: {e}", d.display()))?;
    }
    std::fs::write(&p, data).map_err(|e| format!("write {}: {e}", p.display()))
}

/// Remove files under `dest` that are not in `keep` (and `PROVENANCE`), then
/// empty directories, so a re-run reproduces the tree rather than adding to it.
/// Returns how many files were removed.
fn prune(dest: &Path, keep: &BTreeSet<String>) -> Result<usize, String> {
    let mut existing = Vec::new();
    if dest.is_dir() {
        walk(dest, "", &mut existing)?;
    }
    let mut n = 0;
    for e in existing {
        if e.path != "PROVENANCE" && !keep.contains(&e.path) {
            std::fs::remove_file(dest.join(&e.path))
                .map_err(|er| format!("remove {}: {er}", e.path))?;
            n += 1;
        }
    }
    remove_empty_dirs(dest);
    Ok(n)
}

fn remove_empty_dirs(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() && remove_empty_dirs(&p) {
            continue;
        }
        empty = false;
    }
    if empty {
        let _ = std::fs::remove_dir(dir);
    }
    empty
}

// ---------------------------------------------------------------------------
// Drivers

fn open(g: &mut Gate, opts: &Opts, subtree: &[&str]) -> Option<(Box<dyn Source>, String)> {
    if let Some(dir) = &opts.checkout {
        let sub = subtree.iter().fold(dir.clone(), |p, s| p.join(s));
        if !sub.is_dir() {
            g.fail(format!("{} is not a directory", sub.display()));
            return None;
        }
        let resolved = if !opts.rev.is_empty() {
            opts.rev.clone()
        } else {
            match proc::capture("git", &["rev-parse", "HEAD"], Some(dir)) {
                Ok(o) => String::from_utf8_lossy(&o).trim().to_string(),
                Err(_) => {
                    g.fail("cannot tell which commit the checkout is; pass <sha> explicitly");
                    return None;
                }
            }
        };
        g.info(format!("reading {} (commit {resolved})", sub.display()));
        return Some((Box::new(Checkout { dir: sub }), resolved));
    }
    let rev = if opts.rev.is_empty() {
        "master"
    } else {
        &opts.rev
    };
    if proc::capture("gh", &["--version"], None).is_err() {
        g.fail("gh is required to enumerate and fetch the subset (or pass --from-checkout <dir>)");
        return None;
    }
    match gh_open(rev, subtree) {
        Ok(src) => {
            g.info(format!("WPT {rev} resolves to {}", src.resolved));
            let resolved = src.resolved.clone();
            Some((Box::new(src), resolved))
        }
        Err(e) => {
            g.fail(e);
            None
        }
    }
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            g.fail(format!("{e}\nusage: cargo xtask vendor-wpt [<sha>] [--module css-flexbox|css-grid] [--from-checkout <dir>]"));
            return;
        }
    };
    let r = match &opts.module {
        Some(m) => vendor_module(g, root, &opts, m),
        None => vendor_css2(g, root, &opts),
    };
    if let Err(e) = r {
        g.fail(e);
    }
}

fn vendor_css2(g: &mut Gate, root: &Path, opts: &Opts) -> Result<(), String> {
    let Some((src, resolved)) = open(g, opts, &["css", "CSS2"]) else {
        return Ok(());
    };
    let listing = src.list()?;
    let plan = plan_css2(&listing, CSS2_DIRS);
    let dest = root.join(CSS2_DEST);
    let crash_dest =
        root.join(std::env::var("CRASH_DEST").unwrap_or_else(|_| CSS2_CRASH_DEST.into()));
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&crash_dest).map_err(|e| e.to_string())?;

    for e in &plan.crashtests {
        write_file(&crash_dest, &e.path, &src.read(e)?)?;
    }
    let mut fetched = 0;
    for (t, r) in &plan.pairs {
        for e in [t, r] {
            write_file(&dest, &e.path, &src.read(e)?)?;
            fetched += 1;
        }
    }
    let pairs = plan.pairs.len();
    let crashtests = plan.crashtests.len();
    write_file(
        &crash_dest,
        "PROVENANCE",
        css2_crash_provenance(&resolved, crashtests).as_bytes(),
    )?;
    write_file(
        &dest,
        "PROVENANCE",
        css2_provenance(&resolved, pairs, fetched).as_bytes(),
    )?;
    g.info(format!(
        "vendored {pairs} pairs ({fetched} files) into {}",
        dest.display()
    ));
    g.info(format!(
        "vendored {crashtests} crashtests into {}",
        crash_dest.display()
    ));
    Ok(())
}

fn vendor_module(g: &mut Gate, root: &Path, opts: &Opts, name: &str) -> Result<(), String> {
    let spec = module_spec(name).ok_or_else(|| format!("unknown module {name}"))?;
    let Some((src, resolved)) = open(g, opts, &["css", name]) else {
        return Ok(());
    };
    let listing = src.list()?;
    let by_path: BTreeMap<&str, &Entry> = listing.iter().map(|e| (e.path.as_str(), e)).collect();
    let files: BTreeSet<String> = listing.iter().map(|e| e.path.clone()).collect();
    let mut read = |p: &str| -> Result<String, String> {
        let e = by_path
            .get(p)
            .ok_or_else(|| format!("not in listing: {p}"))?;
        Ok(String::from_utf8_lossy(&src.read(e)?).into_owned())
    };
    let sel = select_module(&spec, &files, &mut read)?;

    let dest = root.join("tests/wpt").join(name);
    let crash_dest = root.join("tests/wpt").join(format!("{name}-crashtests"));
    let keep = sel.files();
    let crash_keep: BTreeSet<String> = sel.crashtests.iter().cloned().collect();
    let pruned = prune(&dest, &keep)? + prune(&crash_dest, &crash_keep)?;
    for p in &keep {
        write_file(&dest, p, &src.read(by_path[p.as_str()])?)?;
    }
    for p in &sel.crashtests {
        write_file(&crash_dest, p, &src.read(by_path[p.as_str()])?)?;
    }
    write_file(
        &dest,
        "PROVENANCE",
        module_provenance(&spec, &resolved, &sel).as_bytes(),
    )?;
    write_file(
        &crash_dest,
        "PROVENANCE",
        module_crash_provenance(&spec, &resolved, sel.crashtests.len()).as_bytes(),
    )?;
    g.info(format!(
        "{name}: {} candidates, {} pairs, {} references, {} support, {} files into {}",
        sel.candidates,
        sel.pairs.len(),
        sel.refs.len(),
        sel.support.len(),
        keep.len(),
        dest.display()
    ));
    g.info(format!(
        "{name}: dropped {} scripted, {} other; {} crashtests into {}",
        sel.dropped_scripted,
        sel.dropped_other.values().sum::<usize>(),
        sel.crashtests.len(),
        crash_dest.display()
    ));
    if pruned > 0 {
        g.info(format!("removed {pruned} stale files no longer selected"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn e(p: &str) -> Entry {
        Entry {
            path: p.into(),
            id: format!("id-{p}"),
        }
    }

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn args() {
        let o = parse_args(&s(&["abc", "--module", "css-grid", "--from-checkout=/x"])).unwrap();
        assert_eq!(o.rev, "abc");
        assert_eq!(o.module.as_deref(), Some("css-grid"));
        assert_eq!(o.checkout, Some(PathBuf::from("/x")));
        assert_eq!(parse_args(&[]).unwrap().rev, "");
        assert!(parse_args(&s(&["--module", "css-text"])).is_err());
        assert!(parse_args(&s(&["--module"])).is_err());
        assert!(parse_args(&s(&["a", "b"])).is_err());
        assert!(parse_args(&s(&["--nope"])).is_err());
    }

    #[test]
    fn base64_and_tsv() {
        assert_eq!(base64_decode("aGVs\nbG8=\n").unwrap(), b"hello");
        assert_eq!(base64_decode("aGk=").unwrap(), b"hi");
        assert!(base64_decode("a$b").is_err());
        let l = parse_tsv_listing("a/b.html\tdead\nc.html\tbeef\n");
        assert_eq!(
            l,
            vec![
                Entry {
                    path: "a/b.html".into(),
                    id: "dead".into()
                },
                Entry {
                    path: "c.html".into(),
                    id: "beef".into()
                },
            ]
        );
    }

    #[test]
    fn css2_pairs_crashtests_and_dirs() {
        let l: Vec<Entry> = [
            "floats/a.html",
            "floats/a-ref.html",
            "floats/b.html", // no reference
            "floats/c.xht",  // not html
            "floats/c-ref.xht",
            "floats/d-crash.html", // crashtest
            "floats/crashtests/e.html",
            "floats/crashtests/e-ref.html", // reference names are skipped even there
            "tables/t.html",                // not a selected directory
            "tables/t-ref.html",
            "linebox/x.html",
            "linebox/x-ref.html",
        ]
        .iter()
        .map(|p| e(p))
        .collect();
        let p = plan_css2(&l, CSS2_DIRS);
        let pairs: Vec<(&str, &str)> = p
            .pairs
            .iter()
            .map(|(t, r)| (t.path.as_str(), r.path.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("floats/a.html", "floats/a-ref.html"),
                ("linebox/x.html", "linebox/x-ref.html")
            ]
        );
        let c: Vec<&str> = p.crashtests.iter().map(|x| x.path.as_str()).collect();
        assert_eq!(c, ["floats/d-crash.html", "floats/crashtests/e.html"]);
    }

    #[test]
    fn tags_skip_comments_and_raw_text() {
        let html = "<!doctype html><!-- <script> --><style>a<b>{}</style>\
            <p ONCLICK=x class='a b'><script>var s='<img src=z>';</script><img src=\"p.png\"/>";
        let t = start_tags(html);
        let names: Vec<&str> = t.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["style", "p", "script", "img"]);
        assert_eq!(t[1].attr("onclick"), Some("x"));
        assert_eq!(t[1].attr("class"), Some("a b"));
        assert_eq!(t[3].attr("src"), Some("p.png"));
    }

    #[test]
    fn scan_reads_links_and_support() {
        let sc = scan(
            "<link rel=\"help\" href=h><link rel='MATCH' href='ref/a.html'>\
             <link rel=stylesheet href=s.css><img src=i.png><object data=o.svg>\
             <iframe src=f.html></iframe>",
        );
        assert!(sc.is_reftest && !sc.has_mismatch && !sc.scripted);
        assert_eq!(sc.match_hrefs, ["ref/a.html"]);
        assert_eq!(sc.support, ["s.css", "i.png", "o.svg", "f.html"]);
        assert!(scan("<link rel=mismatch href=x.html>").has_mismatch);
        assert!(scan("<div onload=1>").scripted);
        assert!(scan("<script src=x></script>").scripted);
        assert!(!scan("<!-- <script> --><p>text <b>").scripted);
        assert!(!scan("<p class=x>").is_reftest);
    }

    #[test]
    fn resolves_relative_absolute_and_escaping() {
        let m = module_spec("css-grid").unwrap();
        let r = |from, h| resolve(&m, from, h);
        assert_eq!(
            r("a/b/t.html", "r.html"),
            Some(Resolved::Inside("a/b/r.html".into()))
        );
        assert_eq!(
            r("a/b/t.html", "../r.html?x#y"),
            Some(Resolved::Inside("a/r.html".into()))
        );
        assert_eq!(
            r("t.html", "./sub/r.html"),
            Some(Resolved::Inside("sub/r.html".into()))
        );
        assert_eq!(
            r("a/t.html", "/css/css-grid/x/r.html"),
            Some(Resolved::Inside("x/r.html".into()))
        );
        assert_eq!(
            r("t.html", "/css/reference/g.html"),
            Some(Resolved::Outside("css/reference/g.html".into()))
        );
        assert_eq!(
            r("t.html", "../support/g.png"),
            Some(Resolved::Outside("css/support/g.png".into()))
        );
        assert_eq!(
            r("t.html", "/css/css-flexbox/r.html"),
            Some(Resolved::Outside("css/css-flexbox/r.html".into()))
        );
        assert_eq!(
            r("t.html", "grid-lanes/r.html"),
            Some(Resolved::Outside("css/css-grid/grid-lanes/r.html".into()))
        );
        assert_eq!(r("t.html", "http://x/y"), None);
        assert_eq!(r("t.html", "data:image/png;base64,AA"), None);
        assert_eq!(r("t.html", "//cdn/x"), None);
        assert_eq!(r("t.html", "#frag"), None);
        assert_eq!(r("t.html", ""), None);
    }

    fn tree(files: &[(&str, &str)]) -> (BTreeSet<String>, BTreeMap<String, String>) {
        (
            files.iter().map(|(p, _)| p.to_string()).collect(),
            files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        )
    }

    fn m(r: &str) -> String {
        format!("<link rel=match href=\"{r}\">")
    }

    #[test]
    fn module_selection_rule() {
        let m_ = |r: &str| m(r);
        let (files, map) = tree(&[
            // kept, reference in a subdirectory, shared with a second test
            (
                "a.html",
                &format!(
                    "{}<link rel=stylesheet href=support/s.css>",
                    m_("ref/r.html")
                ),
            ),
            ("b.html", &m_("ref/r.html")),
            (
                "ref/r.html",
                "<img src=\"../support/i.png\"><img src=/fonts/ahem.png>",
            ),
            ("support/s.css", "x"),
            ("support/i.png", "x"),
            // shared reference given by absolute href, from a subdirectory
            ("d/t.html", &m_("/css/css-flexbox/ref/r.html")),
            // dropped
            (
                "scr.html",
                &format!("{}<script>1</script>", m_("ref/r.html")),
            ),
            (
                "onc.html",
                &format!("{}<body onload=f()>", m_("ref/r.html")),
            ),
            ("mm.html", "<link rel=mismatch href=ref/r.html>"),
            ("x.xhtml", &m_("ref/r.html")),
            ("x.xht", &m_("ref/r.html")),
            ("xr.html", &m_("ref/x-ref.xht")),
            ("out.html", &m_("/css/reference/green.html")),
            ("miss.html", &m_("nothing.html")),
            (
                "two.html",
                &format!("{}{}", m_("ref/r.html"), m_("ref/r.html")),
            ),
            ("ref/x-ref.xht", "<p>"),
            ("ref/s-ref.html", "<script></script>"),
            ("sref.html", &m_("ref/s-ref.html")),
            // not candidates, and excluded
            ("plain.html", "<p>no link"),
            ("crashtests/c.html", &m_("../ref/r.html")),
            ("grid-lanes/g.html", &m_("../ref/r.html")),
            ("a-crash.html", "<p>"),
            ("sub/b-crash.html", "<p>"),
            ("crashtests/z.html", "<p>"),
        ]);
        let spec = ModuleSpec {
            name: "css-flexbox",
            excluded: &["crashtests", "grid-lanes"],
        };
        let mut read = |p: &str| Ok(map[p].clone());
        let sel = select_module(&spec, &files, &mut read).unwrap();
        let pairs: Vec<(&str, &str)> = sel
            .pairs
            .iter()
            .map(|(a, b)| (a.as_str(), b.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("a.html", "ref/r.html"),
                ("b.html", "ref/r.html"),
                ("d/t.html", "ref/r.html"),
            ]
        );
        assert_eq!(sel.refs.len(), 1);
        assert_eq!(sel.candidates, 3 + 9 + 1); // kept + drops + sref
        assert_eq!(sel.dropped_scripted, 2);
        let d = &sel.dropped_other;
        assert_eq!(d["rel=mismatch"], 1);
        assert_eq!(d["test is .xhtml"], 1);
        assert_eq!(d["test is .xht"], 1);
        assert_eq!(d["reference is .xht"], 1);
        assert_eq!(d["reference outside this module"], 1);
        assert_eq!(d["reference missing"], 1);
        assert_eq!(d["test has 2 rel=match links"], 1);
        assert_eq!(d["reference is scripted"], 1);
        assert_eq!(
            sel.support.iter().map(String::as_str).collect::<Vec<_>>(),
            ["support/i.png", "support/s.css"]
        );
        assert_eq!(sel.files().len(), 6);
        assert_eq!(sel.gaps["fonts/ahem.png"].len(), 1);
        assert_eq!(sel.crashtests, ["a-crash.html", "sub/b-crash.html"]);
    }

    #[test]
    fn support_that_is_also_a_test_is_counted_once() {
        let (files, map) = tree(&[
            ("t.html", &format!("{}<iframe src=r.html>", m("r.html"))),
            ("r.html", "<p>"),
        ]);
        let spec = module_spec("css-flexbox").unwrap();
        let sel = select_module(&spec, &files, &mut |p| Ok(map[p].clone())).unwrap();
        assert!(sel.support.is_empty());
        assert_eq!(sel.files().len(), 2);
    }

    #[test]
    fn end_to_end_from_a_checkout_and_rerun_prunes() {
        let w = Fixture::new("wpt-src");
        w.write("css/css-flexbox/t.html", &m("ref/r.html"));
        w.write("css/css-flexbox/ref/r.html", "<p>ref");
        w.write("css/css-flexbox/x-crash.html", "<p>");
        let out = Fixture::new("wpt-out");
        out.write("tests/wpt/css-flexbox/stale.html", "old");
        let mut g = Gate::new("vendor-wpt");
        let args = s(&[
            "deadbeef",
            "--module",
            "css-flexbox",
            "--from-checkout",
            w.path().to_str().unwrap(),
        ]);
        run(&mut g, out.path(), &args);
        assert!(g.passed(), "{:?}", g.failures());
        let d = out.path().join("tests/wpt/css-flexbox");
        assert_eq!(
            std::fs::read_to_string(d.join("ref/r.html")).unwrap(),
            "<p>ref"
        );
        assert!(!d.join("stale.html").exists());
        let prov = std::fs::read_to_string(d.join("PROVENANCE")).unwrap();
        assert!(prov.contains("commit      deadbeef"));
        assert!(prov.contains("kept as test+reference pairs"));
        assert!(
            out.path()
                .join("tests/wpt/css-flexbox-crashtests/x-crash.html")
                .exists()
        );
    }

    #[test]
    fn css2_end_to_end_from_a_checkout() {
        let w = Fixture::new("wpt-css2-src");
        w.write("css/CSS2/floats/a.html", "A");
        w.write("css/CSS2/floats/a-ref.html", "R");
        w.write("css/CSS2/floats/z-crash.html", "C");
        let out = Fixture::new("wpt-css2-out");
        let mut g = Gate::new("vendor-wpt");
        run(
            &mut g,
            out.path(),
            &s(&["abc123", "--from-checkout", w.path().to_str().unwrap()]),
        );
        assert!(g.passed(), "{:?}", g.failures());
        assert!(out.path().join("tests/wpt/css2/floats/a-ref.html").exists());
        assert!(
            out.path()
                .join("tests/wpt/css2-crashtests/floats/z-crash.html")
                .exists()
        );
        let prov = std::fs::read_to_string(out.path().join("tests/wpt/css2/PROVENANCE")).unwrap();
        assert!(prov.contains("pairs       1\nfiles       2\n"));
    }

    #[test]
    fn unsafe_paths_are_refused() {
        assert!(safe_rel("a/../b").is_err());
        assert!(safe_rel("/a").is_err());
        assert!(safe_rel("a/b.html").is_ok());
    }
}
