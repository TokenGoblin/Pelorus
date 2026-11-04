//! `cargo xtask style` -- the Phase 5 gate (build-spec section 9). Port of
//! `ci/gate-style.sh`.
//!
//! ```text
//! computed-style fixtures match across a defined property set  -> here
//! WPT css/css-cascade subset passes                            -> here
//! a documented decision on whether stylo survived contact      -> here
//! ```
//!
//! Written failing, before px-css had a line of stylo in it. The phase is done
//! when this passes in CI on both operating systems, and not when the build is
//! green and not when it passes on the machine that wrote it. Phase 4 ended
//! with four red CI jobs behind a local pass, so that second clause is not
//! rhetoric.
//!
//! # Why this gate is shaped differently from the four before it
//!
//! Section 9 calls Phase 5 the highest-risk phase, and the risk is not that the
//! code will be wrong. It is that the phase will *appear* to succeed. Three
//! specific ways, each of which this gate is built to refuse:
//!
//! 1. **The property set shrinks to fit.** "Computed-style fixtures match
//!    across a defined property set" is the only gate item in the whole plan
//!    whose difficulty is set by a number this project chooses for itself.
//!    Define the set as six properties and it passes in an afternoon. So the
//!    set is a committed manifest with a pinned floor, and every property in it
//!    must be exercised by a fixture -- a property listed and never tested is
//!    worse than one that was never listed, because it reads as coverage.
//!
//! 2. **The WPT subset shrinks to fit.** Same failure, different corpus. The
//!    subset is committed, its size is pinned, and the gate prints the
//!    unadjusted number next to the graded one the way ADR 019 made Phase 4 do.
//!
//! 3. **The decision item gets answered by not answering it.** "A documented
//!    decision on whether stylo survived contact or is being replaced" is
//!    satisfiable by an ADR that says "stylo is working so far". That is a
//!    status report, not a decision. The gate requires the ADR to be in a
//!    decided state and to answer ADR 021's tripwire, which is a yes-or-no
//!    question about something that either happened or did not.
//!
//! The tripwire is the reason for item 4, which section 9 does not ask for. ADR
//! 021 deferred the borrowed style-view type on the explicit claim that every
//! layout decision it depended on had already been banked, and named its own
//! falsification: Phase 5 having to change the arena's slot layout, `NodeId`'s
//! representation, or the opaque packing. That is a mechanical question, so this
//! gate asks it mechanically instead of trusting somebody to volunteer it.
//!
//! # What differs from the script
//!
//! The script parsed `properties.toml` with an embedded Python `tomllib` block.
//! Python is no longer involved: [`parse_manifest`] reads the (small) subset of
//! TOML the manifest uses and rejects anything else, which fails closed. As in
//! the script, a malformed property set is reported as the Python block's own
//! uncounted `FAIL` lines followed by exactly one counted failure.

use super::suites::{self, Suite};
use crate::Gate;
use crate::git;
use crate::scan::{self, is_space};
use std::path::Path;

/// Items 1 and 2 as named test suites (see `suites` for the three conditions;
/// an `#[ignore]` is how a gate item becomes decorative without anything going
/// red).
///
/// `property-sweep` is not a section 9 gate item. It is here because without it
/// the property-set floor below is satisfiable by a manifest full of properties
/// nothing exercises -- which this gate's own header calls worse than not
/// listing them, because it reads as coverage. It earned its place on the first
/// run: eight of the sixty-four properties did not cascade. Four were a fixture
/// error (CSS computes border-*-width to zero while border-*-style is none) and
/// four were stylo prefs -- grid and writing-mode are off by default in the
/// servo build, and a declaration for a pref'd-off property parses, cascades
/// nothing, and reports nothing.
pub const SUITES: &[Suite] = &[
    (
        "computed-style",
        "computed_",
        "crates/px-css/tests/computed.rs",
    ),
    ("css-cascade", "cascade_", "crates/px-css/tests/cascade.rs"),
    (
        "property-sweep",
        "properties_",
        "crates/px-css/tests/properties.rs",
    ),
];

/// "A defined property set" is the phrase section 9 uses, and defining it is
/// this project's job. Two things make a definition real rather than nominal:
/// it lives in one committed file that the tests read (so the tests and the
/// gate cannot disagree about what was promised), and it cannot shrink.
///
/// `PROPERTY_FLOOR` is the ADR 019 pattern. Phase 4 set a precedent worth
/// keeping: an exclusion or a threshold is allowed to exist, and is not allowed
/// to move quietly. If the set legitimately needs to lose a property, the floor
/// changes in the same commit and the diff says so.
pub const PROPERTY_MANIFEST: &str = "crates/px-css/tests/properties.toml";
pub const PROPERTY_FLOOR: i64 = 40;

pub const SURVIVAL_ADR: &str = "docs/adr/025-stylo-survived-contact.md";

/// ADR 021's tripwire files, pinned as blob hashes (`git rev-parse HEAD:path`)
/// rather than by reading the working tree. Phase 4 shipped a check that read
/// the filesystem and passed against a directory that existed on one machine;
/// the index is the only view of the tree that is the same everywhere.
/// `PENDING` skips a file.
pub const TRIPWIRE_FILES: &[(&str, &str)] = &[
    (
        "crates/px-dom/tests/layout.rs",
        "837ca5624caf13ed1af3028596898c6c7c4b7cb7",
    ),
    (
        "crates/px-dom/tests/opaque.rs",
        "5cd4f7fee8103b390dc1738769ae880322b3ee6b",
    ),
];

const CSS_SRC: &str = "crates/px-css/src";

// ---------------------------------------------------------------------------
// The property manifest: a hand-written reader for the TOML subset it uses.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Str(String),
    Arr(Vec<Value>),
    Other,
}

type Table = (Vec<String>, Vec<(String, Value)>);

/// `[a.b]` tables in file order, each with its `key = value` pairs.
#[derive(Debug, Default)]
pub struct Doc {
    tables: Vec<Table>,
}

impl Doc {
    fn table(&self, path: &[&str]) -> Option<&Vec<(String, Value)>> {
        self.tables
            .iter()
            .find(|(p, _)| p.iter().map(String::as_str).eq(path.iter().copied()))
            .map(|(_, kv)| kv)
    }

    fn get(&self, table: &[&str], key: &str) -> Option<&Value> {
        self.table(table)?
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v)
    }
}

struct Cursor<'a> {
    s: &'a str,
}

impl<'a> Cursor<'a> {
    /// Skip whitespace, newlines and `#` comments.
    fn skip_trivia(&mut self) {
        loop {
            self.s = self.s.trim_start_matches(is_space);
            if self.s.starts_with('#') {
                self.s = self.s.find('\n').map_or("", |i| &self.s[i..]);
            } else {
                return;
            }
        }
    }

    /// Skip spaces and tabs only (not newlines).
    fn skip_blanks(&mut self) {
        self.s = self.s.trim_start_matches([' ', '\t']);
    }

    fn string(&mut self) -> Result<String, String> {
        let quote = self.s.chars().next().ok_or("unterminated string")?;
        let body = &self.s[1..];
        let mut out = String::new();
        let mut it = body.char_indices();
        while let Some((i, c)) = it.next() {
            if c == quote {
                self.s = &body[i + 1..];
                return Ok(out);
            }
            if c == '\n' {
                break;
            }
            if c == '\\' && quote == '"' {
                match it.next() {
                    Some((_, 'n')) => out.push('\n'),
                    Some((_, 't')) => out.push('\t'),
                    Some((_, '"')) => out.push('"'),
                    Some((_, '\\')) => out.push('\\'),
                    Some((_, e)) => return Err(format!("unsupported escape \\{e}")),
                    None => break,
                }
            } else {
                out.push(c);
            }
        }
        Err("unterminated string".into())
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.s.chars().next() {
            Some('"') | Some('\'') => Ok(Value::Str(self.string()?)),
            Some('[') => {
                self.s = &self.s[1..];
                let mut items = Vec::new();
                loop {
                    self.skip_trivia();
                    if let Some(r) = self.s.strip_prefix(']') {
                        self.s = r;
                        return Ok(Value::Arr(items));
                    }
                    items.push(self.value()?);
                    self.skip_trivia();
                    if let Some(r) = self.s.strip_prefix(',') {
                        self.s = r;
                    } else if !self.s.starts_with(']') {
                        return Err("expected `,` or `]` in an array".into());
                    }
                }
            }
            Some('{') => Err("inline tables are not supported".into()),
            Some(_) => {
                let end = self
                    .s
                    .find(|c: char| c == '#' || c == ',' || c == ']' || is_space(c))
                    .unwrap_or(self.s.len());
                let tok = &self.s[..end];
                self.s = &self.s[end..];
                if tok.is_empty() {
                    return Err("expected a value".into());
                }
                Ok(match tok.replace('_', "").parse::<i64>() {
                    Ok(n) => Value::Int(n),
                    Err(_) => Value::Other,
                })
            }
            None => Err("expected a value".into()),
        }
    }

    fn key(&mut self) -> Result<String, String> {
        if self.s.starts_with('"') || self.s.starts_with('\'') {
            return self.string();
        }
        let end = self
            .s
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .unwrap_or(self.s.len());
        if end == 0 {
            return Err(format!(
                "unexpected `{}`",
                self.s.chars().next().unwrap_or(' ')
            ));
        }
        let k = self.s[..end].to_string();
        self.s = &self.s[end..];
        Ok(k)
    }
}

/// Read the TOML subset the manifest uses: `[a.b]` table headers, `key =
/// value` with strings, integers and (multi-line) arrays, `#` comments.
/// Anything else (arrays of tables, inline tables, multi-line strings) is an
/// error, so a manifest that grows past this reader fails the gate rather than
/// being read wrongly.
pub fn parse_manifest(text: &str) -> Result<Doc, String> {
    let mut doc = Doc::default();
    let mut c = Cursor { s: text };
    loop {
        c.skip_trivia();
        if c.s.is_empty() {
            return Ok(doc);
        }
        if let Some(r) = c.s.strip_prefix('[') {
            if r.starts_with('[') {
                return Err("arrays of tables are not supported".into());
            }
            let Some(end) = r.find(']') else {
                return Err("unterminated table header".into());
            };
            let path: Vec<String> = r[..end]
                .split('.')
                .map(|p| p.trim().trim_matches('"').to_string())
                .collect();
            if path.iter().any(String::is_empty) {
                return Err("empty table name".into());
            }
            if doc.tables.iter().any(|(p, _)| *p == path) {
                return Err(format!("table [{}] defined twice", path.join(".")));
            }
            c.s = &r[end + 1..];
            doc.tables.push((path, Vec::new()));
            continue;
        }
        let key = c.key()?;
        c.skip_blanks();
        c.s =
            c.s.strip_prefix('=')
                .ok_or_else(|| format!("expected `=` after key `{key}`"))?;
        c.skip_blanks();
        let v = c.value()?;
        let Some((_, kv)) = doc.tables.last_mut() else {
            return Err("key outside any table".into());
        };
        if kv.iter().any(|(k, _)| *k == key) {
            return Err(format!("key `{key}` defined twice"));
        }
        kv.push((key.clone(), v));
        // The rest of the line must be blank or a comment.
        c.skip_blanks();
        if !(c.s.is_empty()
            || c.s.starts_with('\n')
            || c.s.starts_with('#')
            || c.s.starts_with('\r'))
        {
            return Err(format!("unexpected text after the value of `{key}`"));
        }
    }
}

/// The result of grading a manifest: `Ok` carries the two `ok` lines, `Err`
/// the uncounted `FAIL ...` lines the script's Python block printed.
///
/// Parsed, not grepped. The first version of this check counted every quoted
/// line in the file and reported 80 properties when there are 64 -- it was also
/// counting the sixteen cascade-mechanics cases, which are not properties. A
/// count that is 25% high is worse than no count: it would have let the real
/// set fall to 48 while still clearing a floor of 40.
pub fn judge_manifest(text: &str, path: &str, floor: i64) -> Result<[String; 2], Vec<String>> {
    let doc =
        parse_manifest(text).map_err(|e| vec![format!("FAIL {path} cannot be parsed: {e}")])?;

    let groups: Vec<(&str, &Vec<(String, Value)>)> = doc
        .tables
        .iter()
        .filter(|(p, _)| p.len() == 2 && p[0] == "groups")
        .map(|(p, kv)| (p[1].as_str(), kv))
        .collect();
    if groups.is_empty() {
        return Err(vec![format!("FAIL {path} declares no property groups")]);
    }

    let mut errs = Vec::new();
    let mut seen: Vec<(String, usize)> = Vec::new();
    let mut per = Vec::new();
    for (name, kv) in &groups {
        let find = |k: &str| kv.iter().find(|(key, _)| key == k).map(|(_, v)| v);
        let props: Vec<&String> = match find("properties") {
            Some(Value::Arr(items)) => items
                .iter()
                .filter_map(|v| if let Value::Str(s) = v { Some(s) } else { None })
                .collect(),
            _ => Vec::new(),
        };
        if props.is_empty() {
            errs.push(format!("FAIL group '{name}' lists no properties"));
        }
        // The set's whole justification is that every property is consumed by a
        // later phase. A group with no consumer is a property checklist.
        let read_by = matches!(find("read_by"), Some(Value::Str(s)) if !s.is_empty())
            || matches!(find("read_by"), Some(Value::Int(n)) if *n != 0)
            || matches!(find("read_by"), Some(Value::Arr(a)) if !a.is_empty());
        if !read_by {
            errs.push(format!(
                "FAIL group '{name}' does not say which phase reads it"
            ));
        }
        per.push(format!("{name}:{}", props.len()));
        for p in props {
            match seen.iter_mut().find(|(s, _)| s == p) {
                Some((_, n)) => *n += 1,
                None => seen.push((p.clone(), 1)),
            }
        }
    }

    let mut dupes: Vec<&str> = seen
        .iter()
        .filter(|(_, n)| *n > 1)
        .map(|(p, _)| p.as_str())
        .collect();
    dupes.sort_unstable();
    if !dupes.is_empty() {
        errs.push(format!(
            "FAIL properties listed more than once: {}",
            dupes.join(", ")
        ));
    }

    let total = seen.len();
    let declared = match doc.get(&["meta"], "floor") {
        Some(Value::Int(n)) => Some(*n),
        _ => None,
    };
    if declared != Some(floor) {
        // Duplicated on purpose. If the manifest could name its own floor the
        // gate would be reading the number from the file it is policing.
        let shown = declared.map_or("None".to_string(), |n| n.to_string());
        errs.push(format!(
            "FAIL {path} says floor={shown}, the gate says {floor}"
        ));
    }
    if (total as i64) < floor {
        errs.push(format!(
            "FAIL the property set lists {total} properties, floor is {floor}"
        ));
    }
    if !errs.is_empty() {
        return Err(errs);
    }

    let cases = match doc.get(&["mechanics"], "cases") {
        Some(Value::Arr(a)) => a.len(),
        _ => 0,
    };
    Ok([
        format!(
            "the property set lists {total} distinct properties (floor {floor}) across {} groups -- {}",
            groups.len(),
            per.join(", ")
        ),
        format!("and {cases} cascade-mechanics cases, which are not counted as properties"),
    ])
}

fn check_property_set(g: &mut Gate, root: &Path) {
    let Ok(bytes) = std::fs::read(root.join(PROPERTY_MANIFEST)) else {
        g.fail(format!(
            "the computed-style property set is not defined at {PROPERTY_MANIFEST}"
        ));
        g.fail("  §9's phrase is 'a defined property set'; an undefined one cannot be");
        g.fail("  matched against, and a set defined inside the test that reads it can");
        g.fail("  be narrowed in the same edit that makes it pass");
        return;
    };
    let text = String::from_utf8_lossy(&bytes);
    match judge_manifest(&text, PROPERTY_MANIFEST, PROPERTY_FLOOR) {
        Ok(lines) => {
            for l in lines {
                g.ok(l);
            }
        }
        Err(lines) => {
            // The Python block's own stderr, not counted as failures.
            for l in lines {
                eprintln!("{l}");
            }
            g.fail("the property set is not well formed");
        }
    }
}

// ---------------------------------------------------------------------------
// Item 3: the decision, in an ADR, actually decided.
// ---------------------------------------------------------------------------

/// `^[-*[:space:]]*(\*\*)?Status(\*\*)?:?[[:space:]]*(\*\*)?[[:space:]]*
/// (Accepted|Rejected|Superseded)`, case-insensitive, on some line.
///
/// The pattern has to match how this project actually writes an ADR header,
/// which is `- **Status:** accepted` -- a list item with bold markup. The first
/// version anchored on `Status:` at the start of a line and rejected a
/// correctly-written ADR 025, which is the worst kind of gate bug: it blocks
/// the right answer and the fix looks like weakening the check.
pub fn status_is_decided(adr: &str) -> bool {
    adr.lines().any(|line| {
        let r = line.trim_start_matches(|c: char| c == '-' || c == '*' || is_space(c));
        let lower = r.to_lowercase();
        let Some(r) = lower.strip_prefix("status") else {
            return false;
        };
        let r = r.strip_prefix("**").unwrap_or(r);
        let r = r.strip_prefix(':').unwrap_or(r);
        let r = r.trim_start_matches(is_space);
        let r = r.strip_prefix("**").unwrap_or(r);
        let r = r.trim_start_matches(is_space);
        ["accepted", "rejected", "superseded"]
            .iter()
            .any(|w| r.starts_with(w))
    })
}

/// `survived contact|being replaced|is replaced`, case-insensitive: section
/// 9's words, so the answer is findable without interpretation.
pub fn answers_in_section_nine_terms(adr: &str) -> bool {
    let l = adr.to_lowercase();
    ["survived contact", "being replaced", "is replaced"]
        .iter()
        .any(|w| l.contains(w))
}

/// `ADR 021|021-style-view-deferred`: the ADR reports the tripwire outcome. ADR
/// 021 named its own falsification and asked for it to be reported rather than
/// absorbed. An ADR that does not mention it has not looked.
pub fn mentions_tripwire(adr: &str) -> bool {
    adr.contains("ADR 021") || adr.contains("021-style-view-deferred")
}

/// `tripwire (fired|has fired)|ADR 021 was wrong`, case-insensitive: the ADR
/// records that the tripwire fired.
pub fn records_tripwire_fired(adr: &str) -> bool {
    let l = adr.to_lowercase();
    ["tripwire fired", "tripwire has fired", "adr 021 was wrong"]
        .iter()
        .any(|w| l.contains(w))
}

fn check_decision_adr(g: &mut Gate, root: &Path) {
    let Ok(bytes) = std::fs::read(root.join(SURVIVAL_ADR)) else {
        g.fail(format!("{SURVIVAL_ADR} does not exist"));
        g.fail("  §9 Phase 5's third gate item is a documented decision, not an outcome");
        return;
    };
    let adr = String::from_utf8_lossy(&bytes);

    if status_is_decided(&adr) {
        g.ok("the stylo decision ADR is in a decided state");
    } else {
        g.fail(format!(
            "{SURVIVAL_ADR} is not in a decided state (Status must be Accepted,"
        ));
        g.fail("  Rejected or Superseded). 'Proposed' means the gate item -- which is");
        g.fail("  the decision itself -- has not been met");
    }

    if answers_in_section_nine_terms(&adr) {
        g.ok("the stylo decision ADR answers §9's question in §9's terms");
    } else {
        g.fail(format!(
            "{SURVIVAL_ADR} does not state whether stylo survived contact or is"
        ));
        g.fail("  being replaced. Those are §9's words and the answer has to be");
        g.fail("  findable without interpretation");
    }

    if mentions_tripwire(&adr) {
        g.ok("the stylo decision ADR reports ADR 021's tripwire");
    } else {
        g.fail(format!(
            "{SURVIVAL_ADR} does not report ADR 021's tripwire outcome."
        ));
        g.fail("  ADR 021 deferred the borrowed view type on the claim that the");
        g.fail("  layout decisions were already banked, and asked Phase 5 to say");
        g.fail("  plainly whether that was true. Silence reads as 'no problem' and");
        g.fail("  is the one answer the ADR asked not to be given by default");
    }
}

// ---------------------------------------------------------------------------
// Item 4, which section 9 does not ask for: ADR 021's tripwire, asked
// mechanically.
// ---------------------------------------------------------------------------

/// ADR 021's Verification section: "Wrong if Phase 5 has to change the arena's
/// slot layout, `NodeId`'s representation, or the opaque packing in order to
/// write `StyleNode`."
///
/// All three of those are asserted by px-dom's own tests -- `tests/layout.rs`
/// for the handle representation and the niche, `tests/opaque.rs` for the
/// packing. If Phase 5 needs a different layout, those tests are what stands in
/// the way, and the tempting edit is the one that adjusts the assertion to match
/// the new code. That edit is exactly the event ADR 021 wants reported.
///
/// So the tripwire is: those two files are compared against their content at
/// the Phase 4 merge. Changing them is allowed -- ADR 021 does not forbid being
/// wrong, it forbids being quiet about it -- but the survival ADR has to say so.
/// Returns whether the tripwire fired.
fn check_tripwire(g: &mut Gate, root: &Path, files: &[(&str, &str)]) {
    let mut fired = false;
    for (path, pinned) in files {
        if *pinned == "PENDING" {
            continue;
        }
        let actual = git::git(root, &["rev-parse", &format!("HEAD:{path}")])
            .map(|b| {
                String::from_utf8_lossy(&b)
                    .trim_end_matches('\n')
                    .to_string()
            })
            .unwrap_or_else(|_| "missing".into());
        if actual == *pinned {
            g.ok(format!(
                "tripwire intact: {path} is unchanged since Phase 4"
            ));
        } else {
            g.info(format!(
                "tripwire FIRED: {path} changed since Phase 4 ({actual})"
            ));
            fired = true;
        }
    }
    if !fired {
        return;
    }
    // Not a failure by itself. The failure is changing them silently.
    let recorded = std::fs::read(root.join(SURVIVAL_ADR))
        .is_ok_and(|b| records_tripwire_fired(&String::from_utf8_lossy(&b)));
    if recorded {
        g.ok(format!("the tripwire fired and {SURVIVAL_ADR} records it"));
    } else {
        g.fail(format!(
            "px-dom's layout assertions changed and {SURVIVAL_ADR} does not say so."
        ));
        g.fail("  This is ADR 021's falsification condition. Record that the tripwire");
        g.fail("  fired and what it means -- specifically that the claim the layout");
        g.fail("  decisions had already been banked was the thing that was wrong.");
    }
}

// ---------------------------------------------------------------------------
// px-css's unsafe rule, which is narrower than the one it will replace.
// ---------------------------------------------------------------------------

/// `(^|[^a-zA-Z_])unsafe[[:space:]]*\{`: an unsafe block.
pub fn is_unsafe_block(line: &str) -> bool {
    unsafe_followed_by(line, |r| r.trim_start_matches(is_space).starts_with('{'))
}

/// `(^|[^a-zA-Z_])unsafe[[:space:]]+impl`: an unsafe impl.
pub fn is_unsafe_impl(line: &str) -> bool {
    unsafe_followed_by(line, |r| {
        let t = r.trim_start_matches(is_space);
        t.len() < r.len() && t.starts_with("impl")
    })
}

fn unsafe_followed_by(line: &str, rest_ok: fn(&str) -> bool) -> bool {
    line.match_indices("unsafe").any(|(i, m)| {
        let before = line[..i].chars().next_back();
        before.is_none_or(|c| !(c.is_ascii_alphabetic() || c == '_'))
            && rest_ok(&line[i + m.len()..])
    })
}

/// stylo's `TElement` declares six `unsafe fn` methods, and implementing an
/// unsafe method is what `#![forbid(unsafe_code)]` rejects -- verified in
/// `docs/research/stylo-requirements.md` §3.6 by compiling a probe, not
/// inferred. So `/CLAUDE.md`'s first hard rule cannot hold for px-css as
/// written, and the amendment is proposed in ADR 024 rather than taken here.
///
/// This check is written so that it holds either way. The property that
/// matters is not `forbid`; it is that px-css contains no unsafe *block* and no
/// unsafe *impl* -- the body of an `unsafe fn` needs neither. That is true
/// today, and it is the rule ADR 024 asks to be held to afterwards. Checking it
/// now means the guarantee does not lapse during the phase that removes
/// `forbid`, which is the window where it would. Skipped if the directory is
/// absent, as the script did. Comment lines are not counted.
fn check_unsafe(g: &mut Gate, root: &Path) {
    if !root.join(CSS_SRC).is_dir() {
        return;
    }
    let count = |hit: fn(&str) -> bool| {
        scan::drop_comments(scan::grep_rn(root, CSS_SRC, |_| true, &hit)).len()
    };
    let blocks = count(is_unsafe_block);
    let impls = count(is_unsafe_impl);
    if blocks == 0 {
        g.ok("px-css contains no unsafe block");
    } else {
        g.fail(format!(
            "px-css contains {blocks} unsafe block(s); the rule is zero."
        ));
        g.fail("  An unsafe fn required by a stylo trait signature needs no unsafe");
        g.fail("  body. If one is genuinely needed, the operation belongs in");
        g.fail("  px-sandbox, which is the audited unsafe core (ADR 008).");
    }
    if impls == 0 {
        g.ok("px-css contains no unsafe impl");
    } else {
        g.fail(format!(
            "px-css contains {impls} unsafe impl(s); the rule is zero."
        ));
        g.fail("  Send/Sync over a borrowed view are derivable rather than asserted");
        g.fail("  (stylo-requirements.md §3.2). An unsafe impl here is a claim the");
        g.fail("  compiler was going to check for you.");
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }
    suites::check_suites(g, root, SUITES);
    suites::run_workspace_tests(g, root);
    check_property_set(g, root);
    check_decision_adr(g, root);
    check_tripwire(g, root, TRIPWIRE_FILES);
    check_unsafe(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn manifest(floor: i64, extra: &str) -> String {
        format!(
            "[meta]\nfloor = {floor}\n\n[groups.box]\nread_by = \"phase 6\"\nproperties = [\n  \"a\", # c\n  \"b\",\n]\n\n[groups.text]\nread_by = \"phase 7\"\nproperties = [\"c\"]\n{extra}\n[mechanics]\ncases = [\n  \"x: ]#[ tricky\",\n  'y',\n]\n"
        )
    }

    #[test]
    fn manifest_ok_counts_distinct_properties_and_cases() {
        let ok = judge_manifest(&manifest(3, ""), "m.toml", 3).unwrap();
        assert_eq!(
            ok[0],
            "the property set lists 3 distinct properties (floor 3) across 2 groups -- box:2, text:1"
        );
        assert_eq!(
            ok[1],
            "and 2 cascade-mechanics cases, which are not counted as properties"
        );
    }

    #[test]
    fn manifest_below_floor_and_floor_mismatch() {
        let e = judge_manifest(&manifest(3, ""), "m.toml", 4).unwrap_err();
        assert_eq!(
            e,
            [
                "FAIL m.toml says floor=3, the gate says 4",
                "FAIL the property set lists 3 properties, floor is 4"
            ]
        );
        let e = judge_manifest(&manifest(40, ""), "m.toml", 3).unwrap_err();
        assert_eq!(e, ["FAIL m.toml says floor=40, the gate says 3"]);
    }

    #[test]
    fn manifest_duplicates_empty_groups_and_missing_consumer() {
        let extra = "\n[groups.dup]\nread_by = \"p\"\nproperties = [\"a\", \"c\"]\n\n[groups.empty]\nproperties = []\n";
        let e = judge_manifest(&manifest(3, extra), "m.toml", 3).unwrap_err();
        assert_eq!(
            e,
            [
                "FAIL group 'empty' lists no properties",
                "FAIL group 'empty' does not say which phase reads it",
                "FAIL properties listed more than once: a, c"
            ]
        );
    }

    #[test]
    fn manifest_without_groups_or_floor_or_valid_syntax() {
        let e = judge_manifest("[meta]\nfloor = 1\n", "m.toml", 1).unwrap_err();
        assert_eq!(e, ["FAIL m.toml declares no property groups"]);
        let nofloor = "[groups.a]\nread_by = \"p\"\nproperties = [\"x\"]\n";
        let e = judge_manifest(nofloor, "m.toml", 1).unwrap_err();
        assert_eq!(e, ["FAIL m.toml says floor=None, the gate says 1"]);
        assert_eq!(judge_manifest("[[a]]\n", "m", 1).unwrap_err().len(), 1);
        assert_eq!(judge_manifest("x = {a=1}\n", "m", 1).unwrap_err().len(), 1);
        assert_eq!(
            judge_manifest("[a]\nk = \"u\n", "m", 1).unwrap_err().len(),
            1
        );
    }

    #[test]
    fn real_shaped_manifest_parses_comments_and_trailing_text() {
        let t = "# top\n[meta] # note\nfloor = 40 # c\n[groups.g]\nread_by = \"x\"\nproperties = [\"p\"]\n";
        assert!(parse_manifest(t).is_ok());
        assert!(parse_manifest("[a]\nk = 1 junk\n").is_err());
        assert!(parse_manifest("k = 1\n").is_err());
    }

    #[test]
    fn status_forms() {
        assert!(status_is_decided("- **Status:** accepted\n"));
        assert!(status_is_decided("Status: Accepted"));
        assert!(status_is_decided("  * **Status**: Rejected (reason)"));
        assert!(status_is_decided("## x\nstatus **superseded** by 30\n"));
        assert!(!status_is_decided("- **Status:** proposed\n"));
        assert!(!status_is_decided("The Status of this is accepted\n"));
        assert!(!status_is_decided("- **Statuses:** accepted\n"));
        assert!(!status_is_decided(""));
    }

    #[test]
    fn adr_phrases() {
        assert!(answers_in_section_nine_terms("Stylo SURVIVED CONTACT."));
        assert!(answers_in_section_nine_terms("it is replaced by x"));
        assert!(!answers_in_section_nine_terms("status quo"));
        assert!(mentions_tripwire("see ADR 021"));
        assert!(mentions_tripwire("021-style-view-deferred.md"));
        assert!(!mentions_tripwire("adr 021"));
        assert!(records_tripwire_fired("The Tripwire Has Fired."));
        assert!(records_tripwire_fired("ADR 021 was wrong"));
        assert!(!records_tripwire_fired("adr 021 was right"));
    }

    #[test]
    fn unsafe_scans() {
        assert!(is_unsafe_block("    unsafe { x() }"));
        assert!(is_unsafe_block("let a = unsafe{ x };"));
        assert!(is_unsafe_block("unsafe\t{"));
        assert!(is_unsafe_block("x2unsafe {")); // a digit is not in [a-zA-Z_]
        assert!(!is_unsafe_block("my_unsafe { }"));
        assert!(!is_unsafe_block("unsafe fn f() {"));
        assert!(!is_unsafe_block("forbid(unsafe_code)"));
        assert!(is_unsafe_impl("unsafe impl Send for X {}"));
        assert!(is_unsafe_impl("    pub unsafe impl<T> Sync for X<T> {}"));
        assert!(!is_unsafe_impl("unsafeimpl"));
        assert!(!is_unsafe_impl("unsafe fn impl_x()"));
    }

    fn gate_of(f: fn(&mut Gate, &Path), fx: &Fixture) -> Gate {
        let mut g = Gate::new("style");
        f(&mut g, fx.path());
        g
    }

    #[test]
    fn unsafe_gate_skips_missing_dir_ignores_comments_and_counts_lines() {
        let fx = Fixture::new("style-unsafe");
        assert!(gate_of(check_unsafe, &fx).passed());
        fx.write(
            "crates/px-css/src/a.rs",
            "// unsafe { not code }\nunsafe fn f() {}\n",
        );
        assert!(gate_of(check_unsafe, &fx).passed());
        fx.write(
            "crates/px-css/src/b.rs",
            "fn g() { unsafe { h() } }\nunsafe impl Send for T {}\n",
        );
        let g = gate_of(check_unsafe, &fx);
        assert_eq!(g.fail_count(), 8);
        assert_eq!(
            g.failures()[0],
            "px-css contains 1 unsafe block(s); the rule is zero."
        );
    }

    #[test]
    fn decision_adr_missing_proposed_and_good() {
        let fx = Fixture::new("style-adr");
        assert_eq!(gate_of(check_decision_adr, &fx).fail_count(), 2);
        fx.write(SURVIVAL_ADR, "- **Status:** proposed\nnothing\n");
        assert_eq!(gate_of(check_decision_adr, &fx).fail_count(), 3 + 3 + 5);
        fx.write(
            SURVIVAL_ADR,
            "- **Status:** accepted\nStylo survived contact. ADR 021 held.\n",
        );
        assert!(gate_of(check_decision_adr, &fx).passed());
    }

    #[test]
    fn tripwire_intact_fired_silent_and_recorded() {
        let fx = Fixture::new("style-trip");
        fx.write("a.txt", "one\n");
        fx.commit_all();
        let sha = String::from_utf8(git::git(fx.path(), &["rev-parse", "HEAD:a.txt"]).unwrap())
            .unwrap()
            .trim()
            .to_string();
        let sha: &'static str = Box::leak(sha.into_boxed_str());

        let mut g = Gate::new("style");
        check_tripwire(&mut g, fx.path(), &[("a.txt", sha), ("p.txt", "PENDING")]);
        assert!(g.passed());

        // Fired and the ADR is silent: fails.
        let mut g = Gate::new("style");
        check_tripwire(&mut g, fx.path(), &[("a.txt", "0000")]);
        assert_eq!(g.fail_count(), 4);
        // A missing file also fires.
        let mut g = Gate::new("style");
        check_tripwire(&mut g, fx.path(), &[("nope.txt", sha)]);
        assert_eq!(g.fail_count(), 4);

        // Fired and recorded: passes.
        fx.write(SURVIVAL_ADR, "the tripwire fired.\n");
        let mut g = Gate::new("style");
        check_tripwire(&mut g, fx.path(), &[("a.txt", "0000")]);
        assert!(g.passed());
    }

    #[test]
    fn property_set_gate_reports_one_counted_failure_when_malformed() {
        let fx = Fixture::new("style-props");
        assert_eq!(gate_of(check_property_set, &fx).fail_count(), 4);
        fx.write(PROPERTY_MANIFEST, "[meta]\nfloor = 40\n");
        let g = gate_of(check_property_set, &fx);
        assert_eq!(g.failures(), ["the property set is not well formed"]);
    }
}
