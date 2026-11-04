//! `cargo xtask compat-list` -- the forty-site compat list (build-spec §8).
//! Port of `ci/gate-compat-list.sh` and `ci/check_compat_sites.py`.
//!
//! Deliberately NOT one of the five Phase 0 gate checks in §9 -- those are
//! build, reproducibility, supply chain, forbid(unsafe_code) and the brand
//! leak. This is a Phase 0 deliverable that only the person who will
//! daily-drive the browser can produce, so it gets its own job. A red job here
//! says "the list is outstanding", not "the phase gate failed", and the
//! distinction is worth a separate line in CI. This gate is red until that
//! list exists; that is its job, not a bug.
//!
//! Asserts the compat site list is complete and usable: forty sites, each with
//! at least one scripted assertion. The list is the real progress meter for the
//! whole project, so an entry with no assertion is worse than a missing entry:
//! it counts towards forty and tests nothing.
//!
//! Also asserts what the file must NOT contain. The repository is public, and
//! which forty sites the maintainer uses daily is personal information; the ids
//! are opaque and the urls live outside this repository entirely.
//!
//! The TOML subset needed to read `tests/compat/sites.toml` is parsed by hand
//! (no third-party crates): tables, array-of-tables, strings, numbers,
//! booleans, arrays and inline tables.

use crate::Gate;
use std::path::Path;

const REQUIRED: usize = 40;
const PATH: &str = "tests/compat/sites.toml";

/// A parsed TOML value, only as detailed as the gate needs.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
    Array(Vec<Value>),
    Table(Vec<(String, Value)>),
}

impl Value {
    /// Python truthiness, which the original check relied on: an empty string,
    /// zero, `false`, an empty array and an empty table are all "absent".
    pub fn truthy(&self) -> bool {
        match self {
            Value::Str(s) => !s.is_empty(),
            Value::Int(i) => *i != 0,
            Value::Float(f) => *f != 0.0,
            Value::Bool(b) => *b,
            Value::Array(a) => !a.is_empty(),
            Value::Table(t) => !t.is_empty(),
        }
    }

    fn display(&self) -> String {
        match self {
            Value::Str(s) => s.clone(),
            Value::Int(i) => i.to_string(),
            Value::Float(f) => f.to_string(),
            Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
            Value::Array(_) => "[...]".to_string(),
            Value::Table(_) => "{...}".to_string(),
        }
    }
}

type Table = Vec<(String, Value)>;

/// What the checks need to know about one `[[site]]` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteFacts {
    /// The id when present and truthy; `None` otherwise.
    pub id: Option<String>,
    pub has_url: bool,
    pub has_assert: bool,
}

fn get<'a>(t: &'a Table, key: &str) -> Option<&'a Value> {
    t.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

pub fn facts(site: &Table) -> SiteFacts {
    let truthy = |k: &str| get(site, k).is_some_and(Value::truthy);
    SiteFacts {
        id: get(site, "id").filter(|v| v.truthy()).map(Value::display),
        has_url: truthy("url"),
        has_assert: truthy("assert"),
    }
}

/// The decision, pure: every failure message, in the order the original
/// reported them (per-site problems, duplicate ids, then the count).
pub fn check_sites(sites: &[SiteFacts]) -> Vec<String> {
    let mut out = Vec::new();
    for (i, site) in sites.iter().enumerate() {
        let where_ = site.id.clone().unwrap_or_else(|| format!("site[{i}]"));
        if site.id.is_none() {
            out.push(format!("{where_}: missing id"));
        }
        // The repository is public (docs/adr/002-public-repository.md). A url
        // here names the site, which is the disclosure the opaque ids exist to
        // avoid.
        if site.has_url {
            out.push(format!(
                "{where_}: carries a url; urls live in the private mapping, \
                 not in a public repository"
            ));
        }
        if !site.has_assert {
            out.push(format!(
                "{where_}: no assertions \u{2014} an entry that tests nothing \
                 still counts towards forty"
            ));
        }
    }
    let ids: Vec<&String> = sites.iter().filter_map(|s| s.id.as_ref()).collect();
    let mut dups: Vec<&String> = ids
        .iter()
        .copied()
        .filter(|i| ids.iter().filter(|j| *j == i).count() > 1)
        .collect();
    dups.sort();
    dups.dedup();
    for dup in dups {
        out.push(format!("duplicate site id: {dup}"));
    }
    if sites.len() != REQUIRED {
        out.push(format!(
            "{PATH} lists {} sites, expected {REQUIRED} (build-spec §8)",
            sites.len()
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// A small TOML reader.
// ---------------------------------------------------------------------------

struct P<'a> {
    s: &'a [char],
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<char> {
        self.s.get(self.i).copied()
    }
    fn starts(&self, pat: &str) -> bool {
        pat.chars()
            .enumerate()
            .all(|(k, c)| self.s.get(self.i + k) == Some(&c))
    }
    fn line(&self) -> usize {
        1 + self.s.iter().take(self.i).filter(|c| **c == '\n').count()
    }
    fn err<T>(&self, msg: &str) -> Result<T, String> {
        Err(format!("{msg} (line {})", self.line()))
    }
    fn slice(&self, from: usize) -> String {
        self.s.iter().skip(from).take(self.i - from).collect()
    }
    fn skip_inline_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.i += 1;
        }
    }
    fn skip_comment(&mut self) {
        if self.peek() == Some('#') {
            while !matches!(self.peek(), None | Some('\n')) {
                self.i += 1;
            }
        }
    }
    /// Whitespace, newlines and comments (between statements and inside arrays).
    fn skip_all(&mut self) {
        loop {
            self.skip_inline_ws();
            match self.peek() {
                Some('#') => self.skip_comment(),
                Some('\n' | '\r') => self.i += 1,
                _ => break,
            }
        }
    }
    /// After a statement: only a comment may follow on the line.
    fn end_of_line(&mut self) -> Result<(), String> {
        self.skip_inline_ws();
        self.skip_comment();
        match self.peek() {
            None | Some('\n') => Ok(()),
            Some('\r') if self.s.get(self.i + 1) == Some(&'\n') => Ok(()),
            _ => self.err("unexpected text after value"),
        }
    }

    fn key_part(&mut self) -> Result<String, String> {
        self.skip_inline_ws();
        match self.peek() {
            Some('"') => self.basic_string(),
            Some('\'') => self.literal_string(),
            _ => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                {
                    self.i += 1;
                }
                if self.i == start {
                    return self.err("expected a key");
                }
                Ok(self.slice(start))
            }
        }
    }

    fn key_path(&mut self) -> Result<Vec<String>, String> {
        let mut path = vec![self.key_part()?];
        loop {
            self.skip_inline_ws();
            if self.peek() == Some('.') {
                self.i += 1;
                path.push(self.key_part()?);
            } else {
                return Ok(path);
            }
        }
    }

    fn basic_string(&mut self) -> Result<String, String> {
        let multi = self.starts("\"\"\"");
        self.i += if multi { 3 } else { 1 };
        if multi && self.peek() == Some('\n') {
            self.i += 1;
        }
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated string");
            };
            if multi && self.starts("\"\"\"") {
                self.i += 3;
                return Ok(out);
            }
            if !multi && c == '"' {
                self.i += 1;
                return Ok(out);
            }
            if !multi && c == '\n' {
                return self.err("newline in a single-line string");
            }
            self.i += 1;
            if c != '\\' {
                out.push(c);
                continue;
            }
            let Some(e) = self.peek() else {
                return self.err("unterminated escape");
            };
            self.i += 1;
            match e {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'b' => out.push('\u{8}'),
                'f' => out.push('\u{c}'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                'u' | 'U' => {
                    let n = if e == 'u' { 4 } else { 8 };
                    let hex: String = self.s.iter().skip(self.i).take(n).collect();
                    self.i += n;
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(ch) => out.push(ch),
                        None => return self.err("bad unicode escape"),
                    }
                }
                '\n' if multi => {
                    while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
                        self.i += 1;
                    }
                }
                _ => return self.err("bad escape"),
            }
        }
    }

    fn literal_string(&mut self) -> Result<String, String> {
        let multi = self.starts("'''");
        self.i += if multi { 3 } else { 1 };
        if multi && self.peek() == Some('\n') {
            self.i += 1;
        }
        let mut out = String::new();
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated string");
            };
            if multi && self.starts("'''") {
                self.i += 3;
                return Ok(out);
            }
            if !multi && c == '\'' {
                self.i += 1;
                return Ok(out);
            }
            if !multi && c == '\n' {
                return self.err("newline in a single-line string");
            }
            out.push(c);
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        self.skip_inline_ws();
        match self.peek() {
            Some('"') => self.basic_string().map(Value::Str),
            Some('\'') => self.literal_string().map(Value::Str),
            Some('[') => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_all();
                    if self.peek() == Some(']') {
                        self.i += 1;
                        return Ok(Value::Array(items));
                    }
                    items.push(self.value()?);
                    self.skip_all();
                    match self.peek() {
                        Some(',') => self.i += 1,
                        Some(']') => {}
                        _ => return self.err("expected `,` or `]` in an array"),
                    }
                }
            }
            Some('{') => {
                self.i += 1;
                let mut table = Table::new();
                self.skip_inline_ws();
                if self.peek() == Some('}') {
                    self.i += 1;
                    return Ok(Value::Table(table));
                }
                loop {
                    let path = self.key_path()?;
                    self.skip_inline_ws();
                    if self.peek() != Some('=') {
                        return self.err("expected `=` in an inline table");
                    }
                    self.i += 1;
                    let v = self.value()?;
                    insert(&mut table, &path, v)
                        .map_err(|e| format!("{e} (line {})", self.line()))?;
                    self.skip_inline_ws();
                    match self.peek() {
                        Some(',') => {
                            self.i += 1;
                            self.skip_inline_ws();
                        }
                        Some('}') => {
                            self.i += 1;
                            return Ok(Value::Table(table));
                        }
                        _ => return self.err("expected `,` or `}` in an inline table"),
                    }
                }
            }
            Some(_) => {
                let start = self.i;
                while self
                    .peek()
                    .is_some_and(|c| !matches!(c, ',' | ']' | '}' | '#' | ' ' | '\t' | '\n' | '\r'))
                {
                    self.i += 1;
                }
                let tok = self.slice(start);
                match scalar(&tok) {
                    Some(v) => Ok(v),
                    None => self.err(&format!("invalid value `{tok}`")),
                }
            }
            None => self.err("expected a value"),
        }
    }
}

fn scalar(tok: &str) -> Option<Value> {
    match tok {
        "true" => return Some(Value::Bool(true)),
        "false" => return Some(Value::Bool(false)),
        "inf" | "+inf" => return Some(Value::Float(f64::INFINITY)),
        "-inf" => return Some(Value::Float(f64::NEG_INFINITY)),
        "nan" | "+nan" | "-nan" => return Some(Value::Float(f64::NAN)),
        _ => {}
    }
    let clean: String = tok.replace('_', "");
    for (prefix, radix) in [("0x", 16), ("0o", 8), ("0b", 2)] {
        if let Some(rest) = clean.strip_prefix(prefix) {
            return i64::from_str_radix(rest, radix).ok().map(Value::Int);
        }
    }
    if let Ok(i) = clean.parse::<i64>() {
        return Some(Value::Int(i));
    }
    if clean.starts_with(|c: char| c.is_ascii_digit() || c == '+' || c == '-') {
        if let Ok(f) = clean.parse::<f64>() {
            return Some(Value::Float(f));
        }
        // Dates and times are legal TOML values and irrelevant here.
        if clean.chars().any(|c| c.is_ascii_digit())
            && clean.chars().all(|c| "0123456789-:TZ.+ ".contains(c))
        {
            return Some(Value::Str(tok.to_string()));
        }
    }
    None
}

/// Insert `v` at `path` inside `table`, creating intermediate tables. A key
/// defined twice is an error, as in TOML.
fn insert(table: &mut Table, path: &[String], v: Value) -> Result<(), String> {
    let Some((first, rest)) = path.split_first() else {
        return Err("empty key".into());
    };
    if rest.is_empty() {
        if get(table, first).is_some() {
            return Err(format!("duplicate key `{first}`"));
        }
        table.push((first.clone(), v));
        return Ok(());
    }
    if get(table, first).is_none() {
        table.push((first.clone(), Value::Table(Vec::new())));
    }
    match table.iter_mut().find(|(k, _)| k == first) {
        Some((_, Value::Table(t))) => insert(t, rest, v),
        _ => Err(format!("key `{first}` is not a table")),
    }
}

/// Where the keys of the current `[header]` go.
enum Target {
    Root,
    /// Inside `[[site]]` number `n`, below the sub-path.
    Site(usize, Vec<String>),
    /// Some other table: parsed for validity, otherwise ignored.
    Ignored,
}

/// Parse a TOML document and return the `[[site]]` entries, as the original
/// `doc.get("site", [])` did. `Err` carries the reason it is not valid TOML.
pub fn parse_sites(text: &str) -> Result<Vec<Table>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut p = P { s: &chars, i: 0 };
    let mut root = Table::new();
    let mut sites: Vec<Table> = Vec::new();
    let mut target = Target::Root;
    loop {
        p.skip_all();
        let Some(c) = p.peek() else { break };
        if c == '[' {
            let array = p.starts("[[");
            let close = if array { "]]" } else { "]" };
            p.i += close.len();
            let path = p.key_path()?;
            p.skip_inline_ws();
            if !p.starts(close) {
                return p.err("unterminated table header");
            }
            p.i += close.len();
            p.end_of_line()?;
            let is_site = path.first().map(String::as_str) == Some("site");
            target = if is_site && array && path.len() == 1 {
                sites.push(Table::new());
                Target::Site(sites.len() - 1, Vec::new())
            } else if is_site && !array && path.len() > 1 && !sites.is_empty() {
                Target::Site(sites.len() - 1, path.iter().skip(1).cloned().collect())
            } else {
                Target::Ignored
            };
            continue;
        }
        let path = p.key_path()?;
        p.skip_inline_ws();
        if p.peek() != Some('=') {
            return p.err("expected `=` after a key");
        }
        p.i += 1;
        let v = p.value()?;
        p.end_of_line()?;
        let line = p.line();
        let res = match &target {
            Target::Root => insert(&mut root, &path, v),
            Target::Site(n, sub) => {
                let full: Vec<String> = sub.iter().chain(path.iter()).cloned().collect();
                match sites.get_mut(*n) {
                    Some(t) => insert(t, &full, v),
                    None => Ok(()),
                }
            }
            Target::Ignored => Ok(()),
        };
        res.map_err(|e| format!("{e} (line {line})"))?;
    }
    // `site = [ { ... } ]` is the same data in the inline spelling.
    if let Some(Value::Array(items)) = get(&root, "site") {
        for item in items {
            if let Value::Table(t) = item {
                sites.push(t.clone());
            }
        }
    }
    Ok(sites)
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let text = match std::fs::read(root.join(PATH)) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                g.fail(format!("{PATH} does not exist"));
            } else {
                g.fail(format!("cannot read {PATH}: {e}"));
            }
            g.fail("compat site list is not ready");
            return;
        }
    };
    let sites = match parse_sites(&text) {
        Ok(s) => s,
        Err(e) => {
            g.fail(format!("{PATH} is not valid TOML: {e}"));
            g.fail("compat site list is not ready");
            return;
        }
    };
    let facts: Vec<SiteFacts> = sites.iter().map(facts).collect();
    let failures = check_sites(&facts);
    if failures.is_empty() {
        g.ok(format!("{} compat sites, all with assertions", facts.len()));
        return;
    }
    for f in failures {
        g.fail(f);
    }
    g.fail("compat site list is not ready");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn site_toml(n: usize) -> String {
        let mut s = String::new();
        for i in 0..n {
            s.push_str(&format!(
                "[[site]]\nid = \"site-{i:02}\"\nassert = [\n    {{ kind = \"status\", equals = 200 }},\n]\n"
            ));
        }
        s
    }

    fn run_on(text: Option<&str>) -> Gate {
        let fx = Fixture::new("compat-list");
        if let Some(t) = text {
            fx.write(PATH, t);
        }
        let mut g = Gate::new("compat-list");
        run(&mut g, fx.path(), &[]);
        g
    }

    #[test]
    fn forty_complete_sites_pass() {
        let g = run_on(Some(&site_toml(40)));
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn empty_list_fails_for_the_count() {
        let g = run_on(Some("# only comments\n"));
        assert_eq!(
            g.failures(),
            [
                "tests/compat/sites.toml lists 0 sites, expected 40 (build-spec §8)",
                "compat site list is not ready"
            ]
        );
    }

    #[test]
    fn missing_file_fails() {
        let g = run_on(None);
        assert_eq!(g.failures()[0], "tests/compat/sites.toml does not exist");
    }

    #[test]
    fn invalid_toml_fails() {
        let g = run_on(Some("[[site]\nid = 1\n"));
        assert!(
            g.failures()[0].contains("is not valid TOML"),
            "{:?}",
            g.failures()
        );
        let g = run_on(Some("[[site]]\nid = \"a\"\nid = \"b\"\n"));
        assert!(
            g.failures()[0].contains("duplicate key"),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn per_site_problems() {
        let mut t = site_toml(37);
        t.push_str("[[site]]\nassert = [1]\n"); // site[37]: no id
        t.push_str("[[site]]\nid = \"site-38\"\nurl = \"x\"\nassert = [1]\n");
        t.push_str("[[site]]\nid = \"site-39\"\nassert = []\n");
        let g = run_on(Some(&t));
        let f = g.failures();
        assert_eq!(f[0], "site[37]: missing id");
        assert!(f[1].starts_with("site-38: carries a url;"));
        assert!(f[2].starts_with("site-39: no assertions"));
        assert_eq!(f.len(), 4, "{f:?}"); // plus the not-ready line
    }

    #[test]
    fn duplicate_ids_reported_once_and_sorted() {
        let site = |i: &str| SiteFacts {
            id: Some(i.to_string()),
            has_url: false,
            has_assert: true,
        };
        let sites: Vec<SiteFacts> = ["b", "a", "b", "a", "b"].into_iter().map(site).collect();
        let f = check_sites(&sites);
        assert_eq!(f[0], "duplicate site id: a");
        assert_eq!(f[1], "duplicate site id: b");
    }

    #[test]
    fn parser_handles_the_documented_shapes() {
        let t = "# c\ntop = 1\n[[site]]\nid = 'lit'  # trailing\nwhy = \"\"\"\nmulti\nline\"\"\"\nlogin = true\nassert = [\n  { kind = \"present\", selector = \"#a\" },\n  { kind = \"no-console-error\" },\n]\n[other]\nx = [1, 2,\n 3]\n[[site]]\nid = \"two\"\nassert = [ { kind = \"pixel\", tolerance = 0 } ]\n";
        let sites = parse_sites(t).expect("valid toml");
        assert_eq!(sites.len(), 2);
        assert_eq!(facts(&sites[0]).id.as_deref(), Some("lit"));
        assert!(facts(&sites[0]).has_assert);
        assert_eq!(facts(&sites[1]).id.as_deref(), Some("two"));
    }

    #[test]
    fn the_committed_file_parses() {
        // The repository's own file: commented-out example, zero entries.
        let text = include_str!("../../../../tests/compat/sites.toml");
        assert!(parse_sites(text).is_ok());
    }
}
