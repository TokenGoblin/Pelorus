//! `cargo xtask vendor-test262` -- vendor a rule-selected test262 subset into
//! `tests/test262/` (Phase 10 unit 5, ADR 040).
//!
//! A developer TOOL, not a pass/fail gate: it fails only when it cannot do its
//! job. Usage:
//!
//! ```text
//! cargo xtask vendor-test262 [--rev <sha>]              # shallow clone into a scratch dir
//! cargo xtask vendor-test262 --from-checkout <dir>      # a local test262 checkout, no network
//! cargo xtask vendor-test262 ... --target-bytes <n>     # default 15_000_000
//! ```
//!
//! The whole of `test/` is 54,078 files and 86.7 MB, and the core-ES part is
//! about 65 MB: far too much beside an 8.9 MB `tests/wpt`. So the subset is
//! chosen by a stated rule (see [`rule_text`], which is also written into
//! `PROVENANCE` so the text cannot drift from the code):
//!
//! 1. Exclusion by directory, fixture, flag, feature, locale or host capability
//!    (each reason is counted).
//! 2. A deterministic sample: files of each leaf directory are taken in sorted
//!    path order and a fixed rational share of them is kept, the share chosen
//!    by search so the total lands at or under the byte target. Every leaf
//!    directory keeps at least its first test.
//! 3. Directories in [`MUST_KEEP`] are never sampled.
//!
//! Everything that decides is a pure function; the clone and the file writes
//! are the only impure parts. The same `--rev` and target reproduce the tree
//! byte for byte (nothing in the output depends on time, hash order or the
//! scratch path).

use crate::Gate;
use crate::proc;
use crate::test262::{IndexEntry, Negative, Phase};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const UPSTREAM: &str = "https://github.com/tc39/test262";
const DEST: &str = "tests/test262";
pub const DEFAULT_TARGET_BYTES: u64 = 15_000_000;
/// Share denominator for the sampling search.
const DEN: u64 = 1000;
/// Hard-sampled directories keep this many times fewer tests than the rest.
const HARD_FACTOR: u64 = 8;

/// Harness files every assembled test may need; always vendored.
pub const ALWAYS_HARNESS: &[&str] = &["assert.js", "sta.js", "doneprintHandle.js"];

/// Directories (relative to `test/`) excluded wholesale, with the reason.
pub const EXCLUDED_DIRS: &[(&str, &str)] = &[
    ("intl402", "Intl is absent in v1 (ADR 040)"),
    ("staging", "staging tests are unstable and engine-specific"),
    (
        "harness",
        "tests of the harness files themselves, not of the language",
    ),
    ("built-ins/Temporal", "Temporal is absent in v1 (ADR 040)"),
    ("built-ins/ShadowRealm", "ShadowRealm proposal, not shipped"),
    (
        "built-ins/SharedArrayBuffer",
        "SharedArrayBuffer is absent in v1 (ADR 040)",
    ),
    (
        "built-ins/Atomics",
        "Atomics works on SharedArrayBuffer, absent in v1",
    ),
    (
        "built-ins/AbstractModuleSource",
        "source-phase-imports proposal",
    ),
];

/// Features dropped by exact name or by prefix (see [`excluded_feature`]).
/// The "Proposed language features" section of the checkout's `features.txt`
/// is added to this at run time, so a new proposal is excluded on arrival.
pub const ABSENT_FEATURE_PREFIXES: &[(&str, &str)] = &[
    ("temporal", "Temporal is absent in v1"),
    ("intl", "Intl is absent in v1"),
    ("webassembly", "WebAssembly is absent in v1"),
    ("sharedarraybuffer", "SharedArrayBuffer is absent in v1"),
    ("atomics", "Atomics needs SharedArrayBuffer and agents"),
    ("shadowrealm", "ShadowRealm proposal"),
];

/// Features that need a module loader, which is out of scope for Phase 10:
/// the host runs classic scripts only. Module support arrives with the DOM.
pub const MODULE_FEATURES: &[&str] = &[
    "dynamic-import",
    "import.meta",
    "top-level-await",
    "json-modules",
    "import-attributes",
    "export-star-as-namespace-from-module",
    "arbitrary-module-namespace-names",
];

/// Features that need a host object or capability we do not provide.
pub const HOST_FEATURES: &[&str] = &["IsHTMLDDA", "host-gc-required"];

/// Directories kept in full, never sampled. `*` ends a path segment prefix.
/// These are the event-loop and promise machinery the Phase 10 gate cares
/// about, plus the array callbacks and loops that stress the interpreter.
pub const MUST_KEEP: &[&str] = &[
    "built-ins/Promise",
    "built-ins/AsyncFunction",
    "built-ins/AsyncGeneratorFunction",
    "built-ins/AsyncGeneratorPrototype",
    "built-ins/AsyncFromSyncIteratorPrototype",
    "built-ins/AsyncIteratorPrototype",
    "built-ins/Array/prototype/map",
    "built-ins/Array/prototype/filter",
    "built-ins/Array/prototype/reduce",
    "built-ins/Array/prototype/sort",
    "language/expressions/async-*",
    "language/expressions/await",
    "language/statements/async-*",
    "language/statements/for*",
    "language/eval-code",
];

/// Directories sampled `HARD_FACTOR` times harder than the rest (huge
/// generated tables whose tests differ only in a data row).
pub const SAMPLE_HARD: &[&str] = &["built-ins/RegExp/property-escapes"];

// ---------------------------------------------------------------------------
// Arguments

#[derive(Debug, PartialEq, Eq)]
pub struct Opts {
    pub rev: String,
    pub checkout: Option<PathBuf>,
    pub target_bytes: u64,
}

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        rev: String::new(),
        checkout: None,
        target_bytes: DEFAULT_TARGET_BYTES,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match flag {
            "--rev" | "--from-checkout" | "--target-bytes" => {
                let v = match inline {
                    Some(v) => v,
                    None => it
                        .next()
                        .cloned()
                        .ok_or_else(|| format!("{flag} needs a value"))?,
                };
                match flag {
                    "--rev" => set_rev(&mut o, v)?,
                    "--from-checkout" => o.checkout = Some(PathBuf::from(v)),
                    _ => {
                        o.target_bytes = v
                            .replace('_', "")
                            .parse()
                            .map_err(|_| format!("--target-bytes: `{v}` is not a number"))?
                    }
                }
            }
            f if f.starts_with("--") => return Err(format!("unknown option `{f}`")),
            _ => set_rev(&mut o, a.clone())?,
        }
    }
    Ok(o)
}

fn set_rev(o: &mut Opts, v: String) -> Result<(), String> {
    if !o.rev.is_empty() {
        return Err(format!("unexpected argument `{v}`"));
    }
    if !(7..=40).contains(&v.len()) || !v.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!("`{v}` is not a commit sha (7 to 40 hex digits)"));
    }
    o.rev = v;
    Ok(())
}

// ---------------------------------------------------------------------------
// Frontmatter: a small parser for the fixed key subset

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Meta {
    pub description: String,
    pub includes: Vec<String>,
    pub flags: Vec<String>,
    pub features: Vec<String>,
    pub locale: Vec<String>,
    pub negative: Option<Negative>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    Scalar(String),
    List(Vec<String>),
    Map(Vec<(String, String)>),
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

fn split_flow(inner: &str) -> Vec<String> {
    inner
        .split(',')
        .map(unquote)
        .filter(|s| !s.is_empty())
        .collect()
}

fn indent_of(l: &str) -> usize {
    l.len() - l.trim_start_matches(' ').len()
}

fn key_value(l: &str) -> Option<(String, String)> {
    let (k, v) = l.split_once(':')?;
    let k = k.trim();
    if k.is_empty() || k.contains(' ') {
        return None;
    }
    Some((k.to_string(), v.trim().to_string()))
}

/// The text between `/*---` and `---*/`, or `None` when there is no block.
fn frontmatter_block(src: &str) -> Option<&str> {
    let s = src.find("/*---")? + 5;
    let e = src[s..].find("---*/")? + s;
    Some(&src[s..e])
}

/// Parse a test's frontmatter. `Ok(None)` when it has none. Unknown keys are
/// ignored; the keys this tool needs are read exactly or the parse fails.
pub fn parse_frontmatter(src: &str) -> Result<Option<Meta>, String> {
    let Some(block) = frontmatter_block(src) else {
        return Ok(None);
    };
    let lines: Vec<&str> = block.lines().map(|l| l.trim_end()).collect();
    let mut vals: BTreeMap<String, Val> = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        i += 1;
        if l.trim().is_empty() || indent_of(l) > 0 {
            continue;
        }
        let Some((k, v)) = key_value(l) else {
            continue;
        };
        if v.starts_with('|') || v.starts_with('>') {
            let mut text = Vec::new();
            while i < lines.len() && (lines[i].trim().is_empty() || indent_of(lines[i]) > 0) {
                text.push(lines[i].trim().to_string());
                i += 1;
            }
            vals.insert(k, Val::Scalar(text.join(" ").trim().to_string()));
        } else if v.is_empty() {
            let mut items = Vec::new();
            let mut pairs = Vec::new();
            while i < lines.len() && (lines[i].trim().is_empty() || indent_of(lines[i]) > 0) {
                let t = lines[i].trim();
                i += 1;
                if let Some(it) = t.strip_prefix('-') {
                    items.push(unquote(it));
                } else if let Some((a, b)) = key_value(t) {
                    pairs.push((a, unquote(&b)));
                }
            }
            if pairs.is_empty() {
                vals.insert(k, Val::List(items));
            } else {
                vals.insert(k, Val::Map(pairs));
            }
        } else if v.starts_with('[') {
            let mut acc = v.clone();
            while !acc.contains(']') && i < lines.len() {
                acc.push(' ');
                acc.push_str(lines[i].trim());
                i += 1;
            }
            let close = acc
                .find(']')
                .ok_or_else(|| format!("unterminated list for key `{k}`"))?;
            vals.insert(k, Val::List(split_flow(&acc[1..close])));
        } else if v.starts_with('{') {
            let close = v
                .find('}')
                .ok_or_else(|| format!("unterminated map for key `{k}`"))?;
            let pairs = split_flow(&v[1..close])
                .iter()
                .filter_map(|p| key_value(p).map(|(a, b)| (a, unquote(&b))))
                .collect();
            vals.insert(k, Val::Map(pairs));
        } else {
            vals.insert(k, Val::Scalar(unquote(&v)));
        }
    }
    let list = |k: &str| -> Vec<String> {
        match vals.get(k) {
            Some(Val::List(l)) => l.clone(),
            Some(Val::Scalar(s)) if !s.is_empty() => vec![s.clone()],
            _ => Vec::new(),
        }
    };
    let negative = match vals.get("negative") {
        Some(Val::Map(m)) => {
            let get = |n: &str| m.iter().find(|(a, _)| a == n).map(|(_, b)| b.as_str());
            let phase = get("phase").ok_or("negative without a phase")?;
            let kind = get("type").ok_or("negative without a type")?;
            Some(Negative {
                phase: Phase::parse(phase).ok_or_else(|| format!("unknown phase `{phase}`"))?,
                kind: kind.to_string(),
            })
        }
        Some(_) => return Err("negative is not a map".to_string()),
        None => None,
    };
    let description = match vals.get("description") {
        Some(Val::Scalar(s)) => s.clone(),
        _ => String::new(),
    };
    Ok(Some(Meta {
        description,
        includes: list("includes"),
        flags: list("flags"),
        features: list("features"),
        locale: list("locale"),
        negative,
    }))
}

/// The feature names in the "Proposed language features" section of
/// `features.txt`: everything before the `## Standard language features`
/// heading, comments and blanks removed.
pub fn proposed_features(features_txt: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for l in features_txt.lines() {
        let t = l.trim();
        if t.starts_with("## Standard language features") {
            break;
        }
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        // `name  # comment`
        let name = t.split('#').next().unwrap_or("").trim();
        if !name.is_empty() {
            out.insert(name.to_string());
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Selection: pure

/// Why a file is not vendored.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Reason {
    NotJs,
    Fixture,
    Dir(&'static str),
    NotUtf8,
    NoFrontmatter,
    BadFrontmatter,
    ModuleFlag,
    Feature(String),
    Locale,
    Agent,
    MissingInclude(String),
    Sampled,
}

impl Reason {
    pub fn key(&self) -> String {
        match self {
            Reason::NotJs => "not-a-js-file".into(),
            Reason::Fixture => "module-fixture".into(),
            Reason::Dir(d) => format!("dir:{d}"),
            Reason::NotUtf8 => "not-utf8".into(),
            Reason::NoFrontmatter => "no-frontmatter".into(),
            Reason::BadFrontmatter => "unparsable-frontmatter".into(),
            Reason::ModuleFlag => "flag:module".into(),
            Reason::Feature(f) => format!("feature:{f}"),
            Reason::Locale => "locale".into(),
            Reason::Agent => "needs-$262.agent".into(),
            Reason::MissingInclude(i) => format!("missing-include:{i}"),
            Reason::Sampled => "sampled-out".into(),
        }
    }
}

/// `path` is relative to `test/`.
pub fn excluded_dir(path: &str) -> Option<&'static str> {
    EXCLUDED_DIRS
        .iter()
        .find(|(d, _)| path == *d || path.strip_prefix(d).is_some_and(|r| r.starts_with('/')))
        .map(|(d, _)| *d)
}

/// Is this feature one we do not ship (or cannot test) in v1?
pub fn excluded_feature(f: &str, proposals: &BTreeSet<String>) -> bool {
    let lower = f.to_ascii_lowercase();
    ABSENT_FEATURE_PREFIXES
        .iter()
        .any(|(p, _)| lower.starts_with(p))
        || proposals.contains(f)
        || MODULE_FEATURES.contains(&f)
        || HOST_FEATURES.contains(&f)
}

/// Decide one file. `text` is `None` when the bytes were not UTF-8.
/// `have_harness` says whether `harness/<name>` exists in the checkout.
pub fn classify(
    path: &str,
    text: Option<&str>,
    proposals: &BTreeSet<String>,
    have_harness: &dyn Fn(&str) -> bool,
) -> Result<Meta, Reason> {
    if !path.ends_with(".js") {
        return Err(Reason::NotJs);
    }
    if let Some(d) = excluded_dir(path) {
        return Err(Reason::Dir(d));
    }
    if path.ends_with("_FIXTURE.js") {
        return Err(Reason::Fixture);
    }
    let text = text.ok_or(Reason::NotUtf8)?;
    let meta = match parse_frontmatter(text) {
        Ok(Some(m)) => m,
        Ok(None) => return Err(Reason::NoFrontmatter),
        Err(_) => return Err(Reason::BadFrontmatter),
    };
    if meta.flags.iter().any(|f| f == "module") {
        return Err(Reason::ModuleFlag);
    }
    let mut feats: Vec<&String> = meta.features.iter().collect();
    feats.sort();
    if let Some(f) = feats.into_iter().find(|f| excluded_feature(f, proposals)) {
        return Err(Reason::Feature(f.clone()));
    }
    if !meta.locale.is_empty() {
        return Err(Reason::Locale);
    }
    if text.contains("$262.agent") || meta.includes.iter().any(|i| i == "atomicsHelper.js") {
        return Err(Reason::Agent);
    }
    if let Some(i) = meta.includes.iter().find(|i| !have_harness(i)) {
        return Err(Reason::MissingInclude(i.clone()));
    }
    Ok(meta)
}

/// Does `path` match one `MUST_KEEP`-style pattern? A pattern is a `/`
/// separated prefix of the path; a segment ending in `*` matches any segment
/// with that prefix.
pub fn matches_pattern(path: &str, pattern: &str) -> bool {
    let mut ps = path.split('/');
    for seg in pattern.split('/') {
        let Some(p) = ps.next() else {
            return false;
        };
        let ok = match seg.strip_suffix('*') {
            Some(prefix) => p.starts_with(prefix),
            None => p == seg,
        };
        if !ok {
            return false;
        }
    }
    // The pattern named a directory: the path must lie inside it.
    ps.next().is_some()
}

pub fn must_keep(path: &str) -> bool {
    MUST_KEEP.iter().any(|p| matches_pattern(path, p))
}

fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(d, _)| d)
}

/// One surviving candidate for sampling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cand {
    pub path: String,
    pub size: u64,
}

fn ceil_div(a: u64, b: u64) -> u64 {
    a.div_ceil(b)
}

/// Keep the `i`-th (0-based) file of a directory at share `num/den`: evenly
/// spread, the first file always kept for `num >= 1`.
pub fn keep_nth(i: u64, num: u64, den: u64) -> bool {
    ceil_div((i + 1) * num, den) > ceil_div(i * num, den)
}

/// The set of kept paths for a share. `cands` must be sorted by path.
pub fn select_at(cands: &[Cand], num: u64) -> Vec<bool> {
    let mut keep = vec![false; cands.len()];
    let mut idx_in_dir = 0u64;
    let mut last_dir: Option<&str> = None;
    for (n, c) in cands.iter().enumerate() {
        let d = parent(&c.path);
        if last_dir != Some(d) {
            idx_in_dir = 0;
            last_dir = Some(d);
        }
        let k = if must_keep(&c.path) {
            true
        } else {
            let hard = SAMPLE_HARD.iter().any(|p| matches_pattern(&c.path, p));
            let den = if hard { DEN * HARD_FACTOR } else { DEN };
            keep_nth(idx_in_dir, num, den)
        };
        keep[n] = k;
        idx_in_dir += 1;
    }
    keep
}

fn bytes_of(cands: &[Cand], keep: &[bool]) -> u64 {
    cands
        .iter()
        .zip(keep)
        .filter(|(_, k)| **k)
        .map(|(c, _)| c.size)
        .sum()
}

#[derive(Debug, PartialEq, Eq)]
pub struct SamplePlan {
    pub keep: Vec<bool>,
    /// The chosen share is `num / DEN` of each directory.
    pub num: u64,
    pub bytes: u64,
    /// Bytes of the never-sampled directories alone.
    pub must_bytes: u64,
    /// True when even the smallest share (one test per directory) is over the
    /// target; the plan is then that smallest share, and says so.
    pub over_target: bool,
}

/// Choose the largest share whose total is at or under `target`.
pub fn plan_sample(cands: &[Cand], target: u64) -> SamplePlan {
    let must_bytes = cands
        .iter()
        .filter(|c| must_keep(&c.path))
        .map(|c| c.size)
        .sum();
    let total = |num: u64| bytes_of(cands, &select_at(cands, num));
    // The byte total is monotone in the share up to rounding at directory
    // edges, so binary search finds a boundary and a downward scan makes the
    // result exact: the largest share in the neighbourhood that fits.
    let (mut lo, mut hi) = (1u64, DEN);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if total(mid) <= target {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let mut num = lo;
    while num > 1 && total(num) > target {
        num -= 1;
    }
    let keep = select_at(cands, num);
    let bytes = bytes_of(cands, &keep);
    SamplePlan {
        keep,
        num,
        bytes,
        must_bytes,
        over_target: bytes > target,
    }
}

/// The rule, in words, generated from the constants it describes.
pub fn rule_text(target: u64) -> String {
    let dirs: Vec<String> = EXCLUDED_DIRS
        .iter()
        .map(|(d, why)| format!("  - test/{d}/ ({why})"))
        .collect();
    let must: Vec<String> = MUST_KEEP.iter().map(|d| format!("  - test/{d}/")).collect();
    let hard: Vec<String> = SAMPLE_HARD
        .iter()
        .map(|d| format!("  - test/{d}/"))
        .collect();
    let abs: Vec<&str> = ABSENT_FEATURE_PREFIXES.iter().map(|(p, _)| *p).collect();
    format!(
        "Selection rule (crates/px-xtask/src/gates/vendor_test262.rs):

1. Only `*.js` files under test/. Excluded by directory:
{dirs}
2. Excluded: `*_FIXTURE.js` (module helper files, not tests), files without a
   parsable frontmatter block, files that are not UTF-8.
3. Excluded by flag: `module`. The Phase 10 host runs classic scripts; a
   module loader arrives with the DOM. Annex B is vendored (ADR 040 turns it on).
4. Excluded by feature (a test is dropped when ANY of its `features:` matches):
   - case-insensitive prefix: {abs}
   - every entry of the \"Proposed language features\" section of the checkout's
     features.txt (stage 3 and below)
   - needs a module loader: {modf}
   - needs a host object or capability we do not provide: {hostf}
5. Excluded: any test with a `locale:` field, any test using `$262.agent` or
   including atomicsHelper.js, any test whose `includes:` names a harness file
   that is not in the checkout.
6. Sampled to at most {target} bytes of test files. Survivors of 1 to 5 are
   grouped by leaf directory and taken in sorted path order; the i-th file of
   a directory is kept when ceil((i+1)*n/D) > ceil(i*n/D), with D = {den} (a
   share of n/D, evenly spread, the first file always kept). One n is chosen
   for the whole corpus: the largest that fits the target. Directories in the
   hard-sampled list use D = {hard_den} instead:
{hard}
7. Never sampled (kept in full):
{must}
8. harness/: assert.js, sta.js, doneprintHandle.js, and every file a kept test
   includes.
",
        dirs = dirs.join("\n"),
        abs = abs.join(", "),
        modf = MODULE_FEATURES.join(", "),
        hostf = HOST_FEATURES.join(", "),
        den = DEN,
        hard_den = DEN * HARD_FACTOR,
        hard = hard.join("\n"),
        must = must.join("\n"),
    )
}

// ---------------------------------------------------------------------------
// The filesystem side

fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, PathBuf)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("read_dir {}: {e}", dir.display()))?;
    let mut ents: Vec<_> = rd.flatten().collect();
    ents.sort_by_key(|e| e.file_name());
    for ent in ents {
        let name = ent.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() {
            name.clone()
        } else {
            format!("{rel}/{name}")
        };
        let ty = ent.file_type().map_err(|e| format!("file_type {r}: {e}"))?;
        if ty.is_dir() {
            walk(&ent.path(), &r, out)?;
        } else if ty.is_file() {
            out.push((r, ent.path()));
        }
    }
    Ok(())
}

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

/// A scratch clone, deleted when dropped.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Shallow-fetch test262 into a fresh directory outside the repository.
fn fetch(rev: &str) -> Result<Scratch, String> {
    let dir = std::env::temp_dir().join(format!("px-test262-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    let guard = Scratch(dir.clone());
    if rev.is_empty() {
        proc::capture(
            "git",
            &["clone", "-q", "--depth", "1", UPSTREAM, "."],
            Some(&dir),
        )?;
    } else {
        proc::capture("git", &["init", "-q"], Some(&dir))?;
        proc::capture("git", &["remote", "add", "origin", UPSTREAM], Some(&dir))?;
        proc::capture(
            "git",
            &["fetch", "-q", "--depth", "1", "origin", rev],
            Some(&dir),
        )?;
        proc::capture("git", &["checkout", "-q", "FETCH_HEAD"], Some(&dir))?;
    }
    Ok(guard)
}

fn resolve_commit(dir: &Path, rev: &str) -> Result<String, String> {
    match proc::capture("git", &["rev-parse", "HEAD"], Some(dir)) {
        Ok(o) => Ok(String::from_utf8_lossy(&o).trim().to_string()),
        Err(_) if !rev.is_empty() => Ok(rev.to_string()),
        Err(_) => Err("cannot tell which commit the checkout is; pass --rev <sha>".to_string()),
    }
}

/// Everything the writer needs, produced by [`select`].
#[derive(Debug)]
pub struct Selection {
    pub entries: Vec<(IndexEntry, u64)>,
    pub harness: BTreeSet<String>,
    pub dropped: BTreeMap<String, usize>,
    pub considered: usize,
    pub plan_num: u64,
    pub must_bytes: u64,
    pub bytes: u64,
    pub over_target: bool,
}

/// Read a checkout and apply the rule. No writes.
pub fn select(checkout: &Path, target: u64) -> Result<Selection, String> {
    let test_dir = checkout.join("test");
    let harness_dir = checkout.join("harness");
    if !test_dir.is_dir() || !harness_dir.is_dir() {
        return Err(format!(
            "{} has no test/ and harness/ directories",
            checkout.display()
        ));
    }
    let features = std::fs::read_to_string(checkout.join("features.txt"))
        .map_err(|e| format!("read features.txt: {e}"))?;
    let proposals = proposed_features(&features);
    if proposals.is_empty() {
        return Err("features.txt has no proposed-features section; refusing to guess".into());
    }
    let mut have: BTreeSet<String> = BTreeSet::new();
    let mut hfiles = Vec::new();
    walk(&harness_dir, "", &mut hfiles)?;
    for (r, _) in hfiles {
        have.insert(r);
    }
    for h in ALWAYS_HARNESS {
        if !have.contains(*h) {
            return Err(format!("harness/{h} is missing from the checkout"));
        }
    }
    let mut files = Vec::new();
    walk(&test_dir, "", &mut files)?;
    let considered = files.len();
    let mut dropped: BTreeMap<String, usize> = BTreeMap::new();
    let mut cands: Vec<Cand> = Vec::new();
    let mut metas: BTreeMap<String, Meta> = BTreeMap::new();
    for (rel, full) in &files {
        let bytes = std::fs::read(full).map_err(|e| format!("read {rel}: {e}"))?;
        let text = std::str::from_utf8(&bytes).ok();
        match classify(rel, text, &proposals, &|h| have.contains(h)) {
            Ok(meta) => {
                cands.push(Cand {
                    path: rel.clone(),
                    size: bytes.len() as u64,
                });
                metas.insert(rel.clone(), meta);
            }
            Err(r) => *dropped.entry(r.key()).or_insert(0) += 1,
        }
    }
    cands.sort_by(|a, b| a.path.cmp(&b.path));
    let plan = plan_sample(&cands, target);
    let mut entries = Vec::new();
    let mut harness: BTreeSet<String> = ALWAYS_HARNESS.iter().map(|s| s.to_string()).collect();
    for (c, k) in cands.iter().zip(&plan.keep) {
        if !k {
            *dropped.entry(Reason::Sampled.key()).or_insert(0) += 1;
            continue;
        }
        let Some(m) = metas.get(&c.path) else {
            continue;
        };
        harness.extend(m.includes.iter().cloned());
        entries.push((
            IndexEntry {
                path: c.path.clone(),
                flags: m.flags.clone(),
                negative: m.negative.clone(),
                includes: m.includes.clone(),
                features: m.features.clone(),
            },
            c.size,
        ));
    }
    Ok(Selection {
        bytes: plan.bytes,
        entries,
        harness,
        dropped,
        considered,
        plan_num: plan.num,
        must_bytes: plan.must_bytes,
        over_target: plan.over_target,
    })
}

pub fn index_text(entries: &[(IndexEntry, u64)]) -> String {
    let mut s = String::from(
        "# test262 subset index (cargo xtask vendor-test262). One test per line, sorted:\n\
         # path<TAB>flags<TAB>negative(phase:Type)<TAB>includes<TAB>features ; `-` is empty.\n\
         # path is relative to tests/test262/test/ ; includes are files in tests/test262/harness/.\n",
    );
    for (e, _) in entries {
        s.push_str(&e.to_line());
        s.push('\n');
    }
    s
}

pub fn provenance_text(commit: &str, sel: &Selection, target: u64, harness_bytes: u64) -> String {
    let mut drops = String::new();
    // Group `feature:*` and `dir:*` keys for readability, but print every key.
    for (k, n) in &sel.dropped {
        drops.push_str(&format!("  {n:>6}  {k}\n"));
    }
    let dropped_total: usize = sel.dropped.values().sum();
    format!(
        "test262 subset, vendored by `cargo xtask vendor-test262`. Do not edit by hand.

Source:   {UPSTREAM}
Commit:   {commit}
Licence:  BSD (Ecma International), see LICENSE beside this file. Each test
          says \"governed by the BSD license found in the LICENSE file\".
Layout:   test/ (tests, upstream relative paths), harness/ (the harness files
          the kept tests include), INDEX (one line per test: flags, negative,
          includes, features, so the runner needs no YAML parser), LICENSE.

Files considered under test/: {considered}
Kept: {kept} tests, {bytes} bytes of tests (target {target}); {harness_n} harness files, {harness_bytes} bytes.
Sampling share n/D: {num}/{den} per directory ({hard_den} for hard-sampled ones).
Bytes in never-sampled directories: {must}.
Reproduce: cargo xtask vendor-test262 --rev {commit} --target-bytes {target}

Dropped, {dropped_total} files, by reason (first matching reason in rule order):
{drops}
{rule}",
        considered = sel.considered,
        kept = sel.entries.len(),
        bytes = sel.bytes,
        harness_n = sel.harness.len(),
        num = sel.plan_num,
        den = DEN,
        hard_den = DEN * HARD_FACTOR,
        must = sel.must_bytes,
        rule = rule_text(target),
    )
}

fn vendor(g: &mut Gate, root: &Path, opts: &Opts) -> Result<(), String> {
    let (_guard, checkout): (Option<Scratch>, PathBuf) = match &opts.checkout {
        Some(d) => (None, d.clone()),
        None => {
            g.info(format!(
                "cloning {UPSTREAM} (shallow) into a scratch directory"
            ));
            let s = fetch(&opts.rev)?;
            let p = s.0.clone();
            (Some(s), p)
        }
    };
    let commit = resolve_commit(&checkout, &opts.rev)?;
    g.info(format!("test262 commit {commit}"));
    let sel = select(&checkout, opts.target_bytes)?;

    let dest = root.join(DEST);
    if dest.exists() {
        std::fs::remove_dir_all(&dest).map_err(|e| format!("remove {}: {e}", dest.display()))?;
    }
    std::fs::create_dir_all(&dest).map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
    for (e, _) in &sel.entries {
        let data = std::fs::read(checkout.join("test").join(&e.path))
            .map_err(|er| format!("read {}: {er}", e.path))?;
        write_file(&dest, &format!("test/{}", e.path), &data)?;
    }
    let mut harness_bytes = 0u64;
    for h in &sel.harness {
        let data = std::fs::read(checkout.join("harness").join(h))
            .map_err(|er| format!("read harness/{h}: {er}"))?;
        harness_bytes += data.len() as u64;
        write_file(&dest, &format!("harness/{h}"), &data)?;
    }
    let lic = std::fs::read(checkout.join("LICENSE")).map_err(|e| format!("read LICENSE: {e}"))?;
    write_file(&dest, "LICENSE", &lic)?;
    write_file(&dest, "INDEX", index_text(&sel.entries).as_bytes())?;
    write_file(
        &dest,
        "PROVENANCE",
        provenance_text(&commit, &sel, opts.target_bytes, harness_bytes).as_bytes(),
    )?;
    g.info(format!(
        "vendored {} of {} files: {} bytes of tests, {} harness files, into {}",
        sel.entries.len(),
        sel.considered,
        sel.bytes,
        sel.harness.len(),
        dest.display()
    ));
    if sel.over_target {
        g.fail(format!(
            "the smallest sample (one test per directory) is {} bytes, over the target {}",
            sel.bytes, opts.target_bytes
        ));
    }
    Ok(())
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            g.fail(format!(
                "{e}\nusage: cargo xtask vendor-test262 [--rev <sha>] [--from-checkout <dir>] [--target-bytes <n>]"
            ));
            return;
        }
    };
    if let Err(e) = vendor(g, root, &opts) {
        g.fail(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn none() -> BTreeSet<String> {
        BTreeSet::new()
    }

    fn cand(p: &str, size: u64) -> Cand {
        Cand {
            path: p.into(),
            size,
        }
    }

    #[test]
    fn args() {
        let o = parse_args(&s(&[
            "--rev",
            "abcdef1",
            "--from-checkout=/x",
            "--target-bytes",
            "1_000",
        ]))
        .unwrap();
        assert_eq!(o.rev, "abcdef1");
        assert_eq!(o.checkout, Some(PathBuf::from("/x")));
        assert_eq!(o.target_bytes, 1000);
        assert_eq!(parse_args(&[]).unwrap().target_bytes, DEFAULT_TARGET_BYTES);
        assert_eq!(parse_args(&s(&["abcdef1"])).unwrap().rev, "abcdef1");
        assert!(parse_args(&s(&["--rev", "main"])).is_err());
        assert!(parse_args(&s(&["--rev", "--evil"])).is_err());
        assert!(parse_args(&s(&["--rev"])).is_err());
        assert!(parse_args(&s(&["--nope"])).is_err());
        assert!(parse_args(&s(&["--target-bytes", "x"])).is_err());
    }

    #[test]
    fn frontmatter_inline_lists_and_negative() {
        let src = "// c\n/*---\ndescription: A thing\nesid: sec-x\nincludes: [compareArray.js, propertyHelper.js]\nflags: [onlyStrict, async]\nfeatures: [Symbol, Promise]\nnegative:\n  phase: parse\n  type: SyntaxError\n---*/\nvar x;";
        let m = parse_frontmatter(src).unwrap().unwrap();
        assert_eq!(m.description, "A thing");
        assert_eq!(m.includes, ["compareArray.js", "propertyHelper.js"]);
        assert_eq!(m.flags, ["onlyStrict", "async"]);
        assert_eq!(m.features, ["Symbol", "Promise"]);
        assert_eq!(
            m.negative,
            Some(Negative {
                phase: Phase::Parse,
                kind: "SyntaxError".into()
            })
        );
    }

    #[test]
    fn frontmatter_block_scalars_and_block_lists() {
        let src = "/*---\ninfo: |\n  Long text: with a colon\n  flags: [module]\ndescription: >\n  folded\n  text\nincludes:\n  - a.js\n  - 'b.js'\nfeatures: [\n  A,\n  B\n]\nlocale: [en-US]\n---*/";
        let m = parse_frontmatter(src).unwrap().unwrap();
        // `flags: [module]` was inside a block scalar, not a key.
        assert!(m.flags.is_empty());
        assert_eq!(m.description, "folded text");
        assert_eq!(m.includes, ["a.js", "b.js"]);
        assert_eq!(m.features, ["A", "B"]);
        assert_eq!(m.locale, ["en-US"]);
    }

    #[test]
    fn frontmatter_flow_map_crlf_and_absent() {
        let src = "/*---\r\nnegative: {phase: runtime, type: ReferenceError}\r\nflags: [raw]\r\n---*/\r\n";
        let m = parse_frontmatter(src).unwrap().unwrap();
        assert_eq!(m.negative.unwrap().kind, "ReferenceError");
        assert_eq!(m.flags, ["raw"]);
        assert_eq!(parse_frontmatter("var x;").unwrap(), None);
        assert!(
            parse_frontmatter("/*---\nnegative:\n  phase: sometime\n  type: E\n---*/").is_err()
        );
        assert!(parse_frontmatter("/*---\nnegative:\n  phase: parse\n---*/").is_err());
        assert!(parse_frontmatter("/*---\nflags: [a\n---*/").is_err());
    }

    #[test]
    fn proposals_are_read_up_to_the_standard_section() {
        let t = "## Proposed language features\n# c\n\nShadowRealm\n## https://x\ndecorators  # note\n## Standard language features\nSymbol\n";
        let p = proposed_features(t);
        assert_eq!(
            p.into_iter().collect::<Vec<_>>(),
            ["ShadowRealm", "decorators"]
        );
    }

    #[test]
    fn features_excluded_by_prefix_proposal_module_and_host() {
        let prop: BTreeSet<String> = ["decorators".to_string()].into();
        for f in [
            "Temporal",
            "Intl.Locale",
            "intl-normative-optional",
            "Atomics.waitAsync",
            "SharedArrayBuffer",
            "WebAssembly",
            "ShadowRealm",
            "decorators",
            "dynamic-import",
            "top-level-await",
            "IsHTMLDDA",
            "host-gc-required",
        ] {
            assert!(excluded_feature(f, &prop), "{f}");
        }
        for f in [
            "Symbol",
            "Promise",
            "async-iteration",
            "Float16Array",
            "Array.fromAsync",
        ] {
            assert!(!excluded_feature(f, &prop), "{f}");
        }
    }

    fn test_src(fm: &str) -> String {
        format!("/*---\n{fm}\n---*/\n")
    }

    fn cls(path: &str, fm: &str) -> Result<Meta, Reason> {
        classify(path, Some(&test_src(fm)), &none(), &|h| {
            matches!(h, "assert.js" | "sta.js" | "compareArray.js")
        })
    }

    #[test]
    fn classify_each_reason() {
        assert!(cls("language/a/b.js", "description: x").is_ok());
        assert_eq!(cls("language/a/b.md", ""), Err(Reason::NotJs));
        assert_eq!(cls("intl402/a.js", ""), Err(Reason::Dir("intl402")));
        assert_eq!(cls("staging/x/a.js", ""), Err(Reason::Dir("staging")));
        assert_eq!(
            cls("built-ins/Temporal/a/b.js", ""),
            Err(Reason::Dir("built-ins/Temporal"))
        );
        // A directory merely sharing a prefix is not excluded.
        assert!(cls("built-ins/TemporalX/b.js", "description: x").is_ok());
        assert_eq!(cls("language/a/x_FIXTURE.js", ""), Err(Reason::Fixture));
        assert_eq!(cls("a/b.js", "flags: [module]"), Err(Reason::ModuleFlag));
        assert_eq!(
            cls("a/b.js", "features: [Symbol, Temporal]"),
            Err(Reason::Feature("Temporal".into()))
        );
        assert_eq!(cls("a/b.js", "locale: [en]"), Err(Reason::Locale));
        assert_eq!(
            cls("a/b.js", "includes: [atomicsHelper.js]"),
            Err(Reason::Agent)
        );
        assert_eq!(
            classify(
                "a/b.js",
                Some("/*---\n---*/\n$262.agent.start('')"),
                &none(),
                &|_| true
            ),
            Err(Reason::Agent)
        );
        assert_eq!(
            cls("a/b.js", "includes: [compareArray.js, nope.js]"),
            Err(Reason::MissingInclude("nope.js".into()))
        );
        assert_eq!(
            classify("a/b.js", Some("var x;"), &none(), &|_| true),
            Err(Reason::NoFrontmatter)
        );
        assert_eq!(
            classify("a/b.js", None, &none(), &|_| true),
            Err(Reason::NotUtf8)
        );
        assert_eq!(
            cls("a/b.js", "negative:\n  phase: nope\n  type: E"),
            Err(Reason::BadFrontmatter)
        );
    }

    #[test]
    fn must_keep_patterns() {
        assert!(must_keep("built-ins/Promise/all/a.js"));
        assert!(must_keep("built-ins/Promise/a.js"));
        assert!(!must_keep("built-ins/PromiseX/a.js"));
        assert!(must_keep("built-ins/Array/prototype/map/a.js"));
        assert!(!must_keep("built-ins/Array/prototype/mapX/a.js"));
        assert!(!must_keep("built-ins/Array/prototype/slice/a.js"));
        assert!(must_keep("language/expressions/async-arrow-function/a.js"));
        assert!(must_keep("language/expressions/async-generator/yield/a.js"));
        assert!(!must_keep("language/expressions/assignment/a.js"));
        assert!(must_keep("language/statements/for/a.js"));
        assert!(must_keep("language/statements/for-await-of/a.js"));
        assert!(must_keep("language/statements/for-of/dstr/a.js"));
        assert!(!must_keep("language/statements/function/a.js"));
        assert!(must_keep("language/eval-code/direct/a.js"));
        // A pattern names a directory, never a file at its own path.
        assert!(!matches_pattern("built-ins/Promise", "built-ins/Promise"));
    }

    #[test]
    fn keep_nth_is_even_and_keeps_the_first() {
        let kept = |n: u64, num: u64| (0..n).filter(|i| keep_nth(*i, num, 10)).count();
        assert_eq!(kept(10, 10), 10);
        assert_eq!(kept(10, 5), 5);
        assert_eq!(kept(10, 1), 1);
        assert_eq!(kept(3, 1), 1);
        assert_eq!(kept(1, 1), 1);
        assert!(keep_nth(0, 1, 1000));
        // every second one at 5/10: indexes 0, 2, 4, ...
        let v: Vec<u64> = (0..6).filter(|i| keep_nth(*i, 5, 10)).collect();
        assert_eq!(v, [0, 2, 4]);
    }

    fn corpus() -> Vec<Cand> {
        let mut v = Vec::new();
        for i in 0..400 {
            v.push(cand(&format!("language/expressions/big/{i:04}.js"), 100));
        }
        for i in 0..40 {
            v.push(cand(&format!("built-ins/Promise/all/{i:03}.js"), 100));
        }
        for i in 0..3 {
            v.push(cand(&format!("built-ins/Tiny/{i}.js"), 100));
        }
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    #[test]
    fn plan_lands_under_the_target_and_keeps_must_have_in_full() {
        let c = corpus();
        let p = plan_sample(&c, 10_000);
        assert!(p.bytes <= 10_000);
        assert!(!p.over_target);
        // Within one directory step of the target.
        assert!(p.bytes >= 9_000, "{}", p.bytes);
        for (c, k) in c.iter().zip(&p.keep) {
            if c.path.starts_with("built-ins/Promise") {
                assert!(*k, "{}", c.path);
            }
        }
        assert_eq!(p.must_bytes, 4_000);
        // Every leaf directory keeps at least one test.
        let mut dirs: BTreeMap<&str, bool> = BTreeMap::new();
        for (c, k) in c.iter().zip(&p.keep) {
            *dirs.entry(parent(&c.path)).or_insert(false) |= *k;
        }
        assert!(dirs.values().all(|v| *v));
    }

    #[test]
    fn plan_is_deterministic_and_monotone_in_the_target() {
        let c = corpus();
        assert_eq!(plan_sample(&c, 12_000), plan_sample(&c, 12_000));
        let small = plan_sample(&c, 8_000);
        let big = plan_sample(&c, 100_000);
        assert!(small.bytes <= big.bytes);
        assert_eq!(big.bytes, 44_300, "a roomy target keeps everything");
    }

    #[test]
    fn plan_reports_when_even_the_floor_is_over_target() {
        let c = corpus();
        let p = plan_sample(&c, 100);
        assert!(p.over_target);
        assert_eq!(p.num, 1);
    }

    #[test]
    fn hard_sampled_directories_keep_fewer() {
        let mut c = Vec::new();
        for i in 0..800 {
            c.push(cand(
                &format!("built-ins/RegExp/property-escapes/generated/{i:04}.js"),
                10,
            ));
            c.push(cand(&format!("built-ins/RegExp/other/{i:04}.js"), 10));
        }
        c.sort_by(|a, b| a.path.cmp(&b.path));
        let keep = select_at(&c, 500);
        let n = |pre: &str| {
            c.iter()
                .zip(&keep)
                .filter(|(c, k)| **k && c.path.contains(pre))
                .count()
        };
        assert_eq!(n("/other/"), 400);
        assert_eq!(n("/property-escapes/"), 50);
    }

    fn mini_checkout(fx: &Fixture) {
        fx.write(
            "features.txt",
            "## Proposed language features\ndecorators\n## Standard language features\nSymbol\n",
        );
        for h in ALWAYS_HARNESS {
            fx.write(&format!("harness/{h}"), "// h\n");
        }
        fx.write("harness/compareArray.js", "// c\n");
        fx.write("LICENSE", "BSD\n");
        for i in 0..20 {
            fx.write(
                &format!("test/language/a/{i:02}.js"),
                &test_src("includes: [compareArray.js]\nflags: [noStrict]"),
            );
        }
        fx.write(
            "test/language/b/neg.js",
            &test_src("negative:\n  phase: parse\n  type: SyntaxError"),
        );
        fx.write("test/language/b/mod.js", &test_src("flags: [module]"));
        fx.write(
            "test/language/b/dec.js",
            &test_src("features: [decorators]"),
        );
        fx.write("test/intl402/x.js", &test_src("description: i"));
        fx.write(
            "test/built-ins/Promise/p.js",
            &test_src("flags: [async]\nincludes: [compareArray.js]"),
        );
    }

    #[test]
    fn select_end_to_end_on_a_fixture_checkout() {
        let fx = Fixture::new("t262-sel");
        mini_checkout(&fx);
        let sel = select(fx.path(), 10_000_000).unwrap();
        assert_eq!(sel.considered, 25);
        let paths: Vec<&str> = sel.entries.iter().map(|(e, _)| e.path.as_str()).collect();
        assert!(paths.contains(&"language/b/neg.js"));
        assert!(paths.contains(&"built-ins/Promise/p.js"));
        assert!(!paths.contains(&"language/b/mod.js"));
        assert_eq!(sel.entries.len(), 22);
        assert_eq!(sel.dropped["flag:module"], 1);
        assert_eq!(sel.dropped["feature:decorators"], 1);
        assert_eq!(sel.dropped["dir:intl402"], 1);
        assert!(sel.harness.contains("compareArray.js"));
        assert!(sel.harness.contains("doneprintHandle.js"));
        // A tight target samples and counts the sampled-out files.
        let tight = select(fx.path(), 1).unwrap();
        assert!(tight.over_target);
        assert!(tight.dropped["sampled-out"] > 0);
        // The index text parses back to the same entries.
        let idx = crate::test262::parse_index(&index_text(&sel.entries)).unwrap();
        assert_eq!(idx.len(), sel.entries.len());
        let neg = idx.iter().find(|e| e.path.ends_with("neg.js")).unwrap();
        assert_eq!(neg.negative.as_ref().unwrap().phase, Phase::Parse);
    }

    #[test]
    fn vendoring_twice_reproduces_the_tree_exactly() {
        let src = Fixture::new("t262-src");
        mini_checkout(&src);
        let out = Fixture::new("t262-out");
        let opts = Opts {
            rev: "abcdef1".into(),
            checkout: Some(src.path().to_path_buf()),
            target_bytes: 10_000_000,
        };
        let snap = |root: &Path| -> Vec<(String, Vec<u8>)> {
            let mut files = Vec::new();
            walk(&root.join(DEST), "", &mut files).unwrap();
            files
                .into_iter()
                .map(|(r, p)| (r, std::fs::read(p).unwrap()))
                .collect()
        };
        let mut g = Gate::new("vendor-test262");
        vendor(&mut g, out.path(), &opts).unwrap();
        assert_eq!(g.fail_count(), 0);
        let first = snap(out.path());
        // Stale files from an earlier run must not survive.
        out.write("tests/test262/test/stale.js", "x");
        vendor(&mut g, out.path(), &opts).unwrap();
        assert_eq!(first, snap(out.path()));
        let prov = std::fs::read_to_string(out.path().join("tests/test262/PROVENANCE")).unwrap();
        assert!(prov.contains("Selection rule"));
        assert!(prov.contains("flag:module"));
        assert!(out.path().join("tests/test262/LICENSE").is_file());
    }
}
