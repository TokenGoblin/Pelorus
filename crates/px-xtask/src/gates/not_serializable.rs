//! `cargo xtask not-serializable <TypeName> <file>...` -- assert a type is not
//! serialisable. Port of `ci/check_not_serializable.py`, which was only ever
//! called by `cargo xtask ipc`; it is a gate of its own here so that script
//! (and its eventual port) calls one command.
//!
//! Used for `ChannelId`, which is the broker's answer to "who sent this". If
//! it could be serialised it could travel inside a message, and then it would
//! be a claim rather than an observation (invariant 9).
//!
//! # This is the weaker of two checks, on purpose
//!
//! `cargo xtask ipc` also asserts that `px-broker` has no `serde` dependency at
//! all. That is the check that actually holds the property: without a
//! dependency, a derive of the serialisation trait cannot name the trait under
//! any spelling or alias, so the guarantee is structural rather than a matter
//! of inspection. This gate is defence in depth, and it is the only thing that
//! catches a hand-written `impl`.
//!
//! # Why it is deliberately crude
//!
//! Two smarter versions failed. First, `grep -B3 'struct ChannelId' file |
//! grep -q 'Serialize'` reports success when the first grep matches nothing at
//! all, so renaming or moving the type turned it into a check that passed
//! having inspected nothing. Requiring `derive(` and the trait name on the same
//! line then missed a multi-line derive -- which is what rustfmt emits once the
//! list is long. A backwards walk balancing brackets missed this, because
//! brackets inside a string literal count as delimiters and drove the walk to
//! stop early:
//!
//! ```text
//! #[derive(Serialize)]
//! #[doc = concat!(
//!     "((("
//! )]
//! pub struct ChannelId(u64);
//! ```
//!
//! So this version does no parsing. It looks for a derive mentioning the trait
//! anywhere in the window above the declaration, and over-approximates: an
//! unrelated serde-derived type within `WINDOW` lines would fail the gate. That
//! is fail-closed, it cannot arise in a crate with no serde dependency, and it
//! has no clever failure mode for formatting to defeat.
//!
//! Exit: pass if the type is found and is not serialisable, fail otherwise
//! (including when it is not found: this check must not pass by failing to
//! find its subject).

use super::count_lines::py_splitlines;
use crate::Gate;
use std::path::Path;

const TRAITS: [&str; 2] = ["Serialize", "Deserialize"];

/// Generous. A derive block, however formatted, plus doc comments, is well
/// under this -- and being too generous only makes the check stricter.
const WINDOW: usize = 30;

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `\b` at index `i` of `t`.
fn boundary(t: &[char], i: usize) -> bool {
    let before = i > 0 && t.get(i - 1).is_some_and(|c| is_word(*c));
    let after = t.get(i).is_some_and(|c| is_word(*c));
    before != after
}

fn at(t: &[char], i: usize, pat: &str) -> bool {
    pat.chars()
        .enumerate()
        .all(|(k, c)| t.get(i + k) == Some(&c))
}

/// `^\s*(?:pub(?:\([^)]*\))?\s+)?(?:struct|enum)\s+NAME\b`
pub fn is_declaration(line: &str, name: &str) -> bool {
    let t: Vec<char> = line.chars().collect();
    let skip_ws = |mut i: usize| {
        while t.get(i).is_some_and(|c| c.is_whitespace()) {
            i += 1;
        }
        i
    };
    let start = skip_ws(0);
    let mut starts = vec![start];
    if at(&t, start, "pub") {
        let mut j = start + 3;
        if t.get(j) == Some(&'(') {
            match t.iter().skip(j + 1).position(|c| *c == ')') {
                Some(p) => j += p + 2,
                None => return false,
            }
        }
        if t.get(j).is_some_and(|c| c.is_whitespace()) {
            starts.push(skip_ws(j));
        }
    }
    starts.iter().any(|&s| {
        ["struct", "enum"].iter().any(|kw| {
            if !at(&t, s, kw) {
                return false;
            }
            let j = s + kw.len();
            if !t.get(j).is_some_and(|c| c.is_whitespace()) {
                return false;
            }
            let k = skip_ws(j);
            at(&t, k, name) && boundary(&t, k + name.chars().count())
        })
    })
}

/// First `\b(Serialize|Deserialize)\b` in a line.
pub fn derived_trait(line: &str) -> Option<&'static str> {
    let t: Vec<char> = line.chars().collect();
    (0..t.len()).find_map(|i| {
        TRAITS
            .iter()
            .copied()
            .find(|tr| at(&t, i, tr) && boundary(&t, i) && boundary(&t, i + tr.chars().count()))
    })
}

/// Hand-written impls: the regex
/// `impl\b[^;{]*\b(Serialize|Deserialize)\b[^;{]*\bfor\s+NAME\b` over the whole
/// text, leftmost non-overlapping. Returns `(line number of the match start,
/// trait)`.
pub fn manual_impls(text: &str, name: &str) -> Vec<(usize, &'static str)> {
    let t: Vec<char> = text.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if !(at(&t, i, "impl") && !t.get(i + 4).is_some_and(|c| is_word(*c))) {
            i += 1;
            continue;
        }
        let run_end = t
            .iter()
            .skip(i + 4)
            .position(|c| *c == ';' || *c == '{')
            .map_or(t.len(), |p| i + 4 + p);
        // `for\s+NAME\b` occurrences inside the run: (start, end).
        let mut fors = Vec::new();
        for fs in (i + 4)..run_end {
            if !(at(&t, fs, "for") && boundary(&t, fs)) {
                continue;
            }
            let mut j = fs + 3;
            let ws_start = j;
            while t.get(j).is_some_and(|c| c.is_whitespace()) {
                j += 1;
            }
            if j == ws_start || !at(&t, j, name) {
                continue;
            }
            let end = j + n.len();
            if boundary(&t, end) {
                fors.push((fs, end));
            }
        }
        // Greedy: the last trait word that still has a `for NAME` after it,
        // then the last such `for NAME`.
        let mut chosen: Option<(&'static str, usize)> = None;
        for ts in (i + 4)..run_end {
            for tr in TRAITS {
                let te = ts + tr.chars().count();
                if !(at(&t, ts, tr) && boundary(&t, ts) && boundary(&t, te)) {
                    continue;
                }
                if let Some(end) = fors
                    .iter()
                    .filter(|(fs, _)| *fs >= te)
                    .map(|(_, e)| *e)
                    .max()
                {
                    chosen = Some((tr, end));
                }
            }
        }
        match chosen {
            Some((tr, end)) => {
                let line = 1 + t.iter().take(i).filter(|c| **c == '\n').count();
                out.push((line, tr));
                i = end;
            }
            None => i += 1,
        }
    }
    out
}

/// One finding: the message `FAIL`s with.
pub type Finding = String;

/// Check one file's text. Returns `(declared_or_impl_found, findings)`.
pub fn check_text(path: &str, text: &str, name: &str) -> (bool, Vec<Finding>) {
    let mut found = false;
    let mut findings = Vec::new();
    let lines = py_splitlines(text);
    for (i, line) in lines.iter().enumerate() {
        if !is_declaration(line, name) {
            continue;
        }
        found = true;
        let start = i.saturating_sub(WINDOW);
        let window = lines.get(start..i).unwrap_or(&[]);
        // Only look when the window mentions a derive at all, so a bare
        // `use serde::Serialize` import elsewhere in the file does not fail
        // every type declared after it.
        if !window.iter().any(|w| w.contains("derive")) {
            continue;
        }
        for (offset, w) in window.iter().enumerate() {
            // Comments are skipped, and not as a convenience: the type's own
            // doc comment explains that it must never be serialisable, so a
            // scan that reads comments fails on the very sentence documenting
            // the property it is checking.
            let stripped = w.trim();
            if stripped.starts_with("//") {
                continue;
            }
            if let Some(hit) = derived_trait(w) {
                findings.push(format!(
                    "{path}:{} a derive above {name} (declared at line {}) mentions \
                     {hit}:\n        {stripped}",
                    start + offset + 1,
                    i + 1
                ));
            }
        }
    }
    for (line, tr) in manual_impls(text, name) {
        found = true;
        findings.push(format!("{path}:{line} {name} has a hand-written {tr} impl"));
    }
    (found, findings)
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let [name, paths @ ..] = args else {
        g.fail("usage: cargo xtask not-serializable <TypeName> <file> [file...]");
        return;
    };
    if paths.is_empty() {
        g.fail("usage: cargo xtask not-serializable <TypeName> <file> [file...]");
        return;
    }
    let mut found = false;
    for path in paths {
        let text = match std::fs::read(root.join(path))
            .map_err(|e| e.to_string())
            .and_then(|b| String::from_utf8(b).map_err(|e| e.to_string()))
        {
            Ok(t) => t,
            Err(e) => {
                g.fail(format!("cannot read {path}: {e}"));
                return;
            }
        };
        let (f, findings) = check_text(path, &text, name);
        found |= f;
        for finding in findings {
            g.fail(finding);
        }
    }
    if !found {
        g.fail(format!(
            "{name} was not declared in any of: {}\n      this check must not pass by \
             failing to find its subject",
            paths.join(" ")
        ));
    }
    if g.passed() {
        g.ok(format!("{name} is not serialisable"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn run_on(src: &str) -> Gate {
        let fx = Fixture::new("not-ser");
        fx.write("a.rs", src);
        let mut g = Gate::new("not-serializable");
        run(&mut g, fx.path(), &["ChannelId".into(), "a.rs".into()]);
        g
    }

    #[test]
    fn plain_type_passes() {
        let g = run_on("#[derive(Clone, Copy)]\npub struct ChannelId(u64);\n");
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn single_and_multi_line_derives_fail() {
        let g = run_on("#[derive(Clone, Serialize)]\npub struct ChannelId(u64);\n");
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].starts_with(
            "a.rs:1 a derive above ChannelId (declared at line 2) mentions Serialize:"
        ));
        // rustfmt's multi-line form.
        let g = run_on(
            "#[derive(\n    Clone,\n    Deserialize,\n)]\npub(crate) struct ChannelId(u64);\n",
        );
        assert!(
            g.failures()[0].contains("a.rs:3 a derive above"),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn the_string_literal_bracket_trap_is_caught() {
        let g = run_on(
            "#[derive(Serialize)]\n#[doc = concat!(\n    \"(((\"\n)]\npub struct ChannelId(u64);\n",
        );
        assert_eq!(g.fail_count(), 1);
    }

    #[test]
    fn comments_and_imports_do_not_fail() {
        let g = run_on(
            "/// Never Serialize this.\n// derive Serialize no\n#[derive(Clone)]\nstruct ChannelId;\n",
        );
        assert!(g.passed(), "{:?}", g.failures());
        // No derive in the window at all: an import elsewhere is irrelevant.
        let g = run_on("use serde::Serialize;\nstruct ChannelId;\n");
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn hand_written_impl_fails() {
        let g = run_on(
            "struct ChannelId(u64);\nimpl serde::Serialize for ChannelId {\n    fn serialize() {}\n}\n",
        );
        assert_eq!(
            g.failures(),
            ["a.rs:2 ChannelId has a hand-written Serialize impl"]
        );
        let g = run_on("struct ChannelId;\nimpl<'de> Deserialize<'de> for ChannelId {}\n");
        assert_eq!(
            g.failures(),
            ["a.rs:2 ChannelId has a hand-written Deserialize impl"]
        );
        // Another type, or another trait, is fine.
        let g =
            run_on("struct ChannelId;\nimpl Serialize for Other {}\nimpl Clone for ChannelId {}\n");
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn subject_not_found_fails_closed() {
        let g = run_on("struct Renamed;\n");
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].starts_with("ChannelId was not declared in any of: a.rs"));
    }

    #[test]
    fn usage_and_unreadable_file_fail() {
        let fx = Fixture::new("not-ser-usage");
        let mut g = Gate::new("not-serializable");
        run(&mut g, fx.path(), &["ChannelId".into()]);
        assert_eq!(g.fail_count(), 1);
        let mut g = Gate::new("not-serializable");
        run(
            &mut g,
            fx.path(),
            &["ChannelId".into(), "missing.rs".into()],
        );
        assert!(g.failures()[0].starts_with("cannot read missing.rs:"));
    }

    #[test]
    fn declaration_shapes() {
        assert!(is_declaration("struct ChannelId;", "ChannelId"));
        assert!(is_declaration("  pub struct ChannelId(u64);", "ChannelId"));
        assert!(is_declaration("pub(crate) enum ChannelId {", "ChannelId"));
        assert!(is_declaration(
            "pub(in crate::a) struct ChannelId;",
            "ChannelId"
        ));
        assert!(!is_declaration("struct ChannelIdx;", "ChannelId"));
        assert!(!is_declaration("// struct ChannelId;", "ChannelId"));
        assert!(!is_declaration("type ChannelId = u64;", "ChannelId"));
        assert!(!is_declaration("pub(crate)struct ChannelId;", "ChannelId"));
    }
}
