//! `cargo xtask count-lines` -- size of the project, written into README.md
//! between the SIZE markers. Port of `ci/count-lines.sh`.
//!
//! Why a tool and not a number somebody types: a hand-maintained count is
//! wrong the commit after it is written, and this project has already been
//! bitten three times by a figure that was asserted rather than measured. The
//! README section is generated, and it carries the commit it was generated
//! from so that a stale number is visibly stale rather than quietly wrong.
//!
//! ```text
//! cargo xtask count-lines            rewrite the README section
//! cargo xtask count-lines --check    fail if the section is out of date
//! ```
//!
//! `--check` is not wired into a gate: the count changes on nearly every
//! commit, so gating it would fail constantly and teach people to ignore it.
//! It is available for anyone who wants it.
//!
//! Code and comments are counted separately on purpose. This codebase carries
//! an unusual amount of prose -- the working agreement requires the reasoning
//! to be written down where the decision lives -- so a single total would
//! flatter it.

use crate::Gate;
use crate::git;
use std::path::Path;

const BEGIN: &str = "<!-- SIZE:BEGIN -->";
const END: &str = "<!-- SIZE:END -->";

const RUST_COMMENTS: &[&str] = &["//"];
const SHELL_COMMENTS: &[&str] = &["#"];

/// Lines split as Python's `str.splitlines()` does. Not `split('\n')`: that
/// yields a phantom empty element after a file's final newline, inflating the
/// blank count by one per file (caught by checking the total against `wc -l`).
pub fn py_splitlines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        if matches!(
            c,
            '\n' | '\r'
                | '\u{b}'
                | '\u{c}'
                | '\u{1c}'
                | '\u{1d}'
                | '\u{1e}'
                | '\u{85}'
                | '\u{2028}'
                | '\u{2029}'
        ) {
            out.push(text.get(start..i).unwrap_or(""));
            let mut end = i + c.len_utf8();
            if c == '\r' && iter.peek().is_some_and(|(_, n)| *n == '\n') {
                iter.next();
                end += 1;
            }
            start = end;
        }
    }
    if start < text.len() {
        out.push(text.get(start..).unwrap_or(""));
    }
    out
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Split lines into `(code, comment, blank)`.
pub fn measure_text(text: &str, comment_prefixes: &[&str]) -> (usize, usize, usize) {
    let (mut code, mut comment, mut blank) = (0, 0, 0);
    for raw in py_splitlines(text) {
        let line = py_strip(raw);
        if line.is_empty() {
            blank += 1;
        } else if comment_prefixes.iter().any(|p| line.starts_with(p)) {
            comment += 1;
        } else {
            code += 1;
        }
    }
    (code, comment, blank)
}

/// Whether `path` sits beneath a directory sequence. `*` matches any one.
///
/// Filtering is done on path components here rather than by a git pathspec,
/// because the crates that arrive from Phase 4 onward have nested module
/// directories and a `crates/*/src/*.rs` glob would quietly stop counting
/// them -- which is the exact failure this tool exists to prevent.
pub fn under(path: &str, segments: &[&str]) -> bool {
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() <= segments.len() {
        return false;
    }
    segments
        .iter()
        .zip(&parts)
        .all(|(s, p)| *s == "*" || s == p)
}

fn suffix(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or("");
    match name.rfind('.') {
        Some(i) if i > 0 => name.get(i..).unwrap_or(""),
        _ => "",
    }
}

fn first_part(path: &str) -> &str {
    path.split('/').next().unwrap_or("")
}

/// Thousands separators, as Python's `{:,}`.
pub fn commas(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub label: &'static str,
    pub files: usize,
    pub code: usize,
    pub comment: usize,
    pub blank: usize,
}

/// The README section, markers included.
pub fn render_section(rows: &[Row], doc_lines: usize, docs: usize, commit: &str) -> String {
    let total_code: usize = rows.iter().map(|r| r.code).sum();
    let total_comment: usize = rows.iter().map(|r| r.comment).sum();
    let mut lines: Vec<String> = vec![
        BEGIN.into(),
        String::new(),
        "| | Files | Code | Comment | Blank |".into(),
        "|---|---:|---:|---:|---:|".into(),
    ];
    for r in rows {
        lines.push(format!(
            "| {} | {} | {} | {} | {} |",
            r.label,
            r.files,
            commas(r.code),
            commas(r.comment),
            commas(r.blank)
        ));
    }
    lines.push(format!(
        "| **Total** | **{}** | **{}** | **{}** | **{}** |",
        commas(rows.iter().map(|r| r.files).sum()),
        commas(total_code),
        commas(total_comment),
        commas(rows.iter().map(|r| r.blank).sum())
    ));
    lines.push(String::new());
    lines.push(format!(
        "Plus {} lines of specification and decision records across {docs} documents.",
        commas(doc_lines)
    ));
    lines.push(String::new());
    lines.push(format!(
        "Generated by `cargo xtask count-lines`, last run against the tree at \
         `{commit}`. Regenerate rather than edit \u{2014} a number nobody measured is \
         the kind this project keeps getting wrong. `cargo xtask count-lines --check` \
         reports whether this is stale."
    ));
    lines.push(String::new());
    lines.push(END.into());
    lines.join("\n")
}

/// Replace the marked section of `readme`. `None` when the markers are absent
/// (or out of order).
pub fn splice(readme: &str, section: &str) -> Option<String> {
    let (head, rest) = readme.split_once(BEGIN)?;
    let (_, tail) = rest.split_once(END)?;
    Some(format!("{head}{section}{tail}"))
}

fn measure_files(root: &Path, paths: &[&String], prefixes: &[&str]) -> Row2 {
    let (mut code, mut comment, mut blank) = (0, 0, 0);
    for p in paths {
        let Ok(bytes) = std::fs::read(root.join(p)) else {
            continue;
        };
        let (c, m, b) = measure_text(&String::from_utf8_lossy(&bytes), prefixes);
        code += c;
        comment += m;
        blank += b;
    }
    Row2 {
        files: paths.len(),
        code,
        comment,
        blank,
    }
}

struct Row2 {
    files: usize,
    code: usize,
    comment: usize,
    blank: usize,
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let check = args.first().is_some_and(|a| a == "--check");
    let tracked = match git::ls_files(root, &[], &[]) {
        Ok(t) => t,
        Err(e) => {
            g.fail(format!("cannot list tracked files: {e}"));
            return;
        }
    };
    let pick = |pred: &dyn Fn(&str) -> bool| -> Vec<&String> {
        tracked.iter().filter(|p| pred(p)).collect()
    };
    let rust_src = pick(&|p| suffix(p) == ".rs" && under(p, &["crates", "*", "src"]));
    let rust_tests = pick(&|p| suffix(p) == ".rs" && under(p, &["crates", "*", "tests"]));
    let fuzz = pick(&|p| suffix(p) == ".rs" && under(p, &["fuzz"]));
    let scripts = pick(&|p| matches!(suffix(p), ".sh" | ".py") && first_part(p) == "ci");
    let docs = pick(&|p| suffix(p) == ".md" && first_part(p) == "docs");

    let mut rows = Vec::new();
    for (label, paths, prefixes) in [
        ("Rust \u{2014} crate sources", &rust_src, RUST_COMMENTS),
        ("Rust \u{2014} tests", &rust_tests, RUST_COMMENTS),
        ("Rust \u{2014} fuzz targets", &fuzz, RUST_COMMENTS),
        ("CI scripts", &scripts, SHELL_COMMENTS),
    ] {
        let m = measure_files(root, paths, prefixes);
        rows.push(Row {
            label,
            files: m.files,
            code: m.code,
            comment: m.comment,
            blank: m.blank,
        });
    }
    let doc_lines: usize = docs
        .iter()
        .map(|p| {
            std::fs::read(root.join(p))
                .map(|b| py_splitlines(&String::from_utf8_lossy(&b)).len())
                .unwrap_or(0)
        })
        .sum();

    let commit = match git::git(root, &["rev-parse", "--short", "HEAD"]) {
        Ok(b) => String::from_utf8_lossy(&b).trim().to_string(),
        Err(e) => {
            g.fail(format!("cannot read HEAD: {e}"));
            return;
        }
    };
    let section = render_section(&rows, doc_lines, docs.len(), &commit);

    let readme_path = root.join("README.md");
    // Read with universal newlines, as the original did.
    let readme = match std::fs::read(&readme_path) {
        Ok(b) => String::from_utf8_lossy(&b)
            .replace("\r\n", "\n")
            .replace('\r', "\n"),
        Err(e) => {
            g.fail(format!("cannot read README.md: {e}"));
            return;
        }
    };
    let Some(updated) = splice(&readme, &section) else {
        g.fail(format!("README.md has no {BEGIN} / {END} markers"));
        return;
    };

    if check {
        if updated != readme {
            g.fail("README.md size section is out of date; run `cargo xtask count-lines`");
        }
        return;
    }
    if updated != readme {
        match std::fs::write(&readme_path, updated) {
            Ok(()) => g.info(format!("README.md size section updated at {commit}")),
            Err(e) => g.fail(format!("cannot write README.md: {e}")),
        }
    } else {
        g.info(format!(
            "README.md size section already current at {commit}"
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn splitlines_matches_python() {
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("a\nb\n"), ["a", "b"]);
        assert_eq!(py_splitlines("a\n\nb"), ["a", "", "b"]);
        assert_eq!(py_splitlines("a\r\nb\rc"), ["a", "b", "c"]);
        assert_eq!(py_splitlines("\n"), [""]);
        assert_eq!(py_splitlines("a\u{2028}b"), ["a", "b"]);
    }

    #[test]
    fn measure_splits_code_comment_blank() {
        let t = "// c\n\n  // indented\nfn f() {}\n   \nlet x = 1; // trailing\n";
        assert_eq!(measure_text(t, RUST_COMMENTS), (2, 2, 2));
        assert_eq!(
            measure_text("# a\n#!/bin/sh\nx\n", SHELL_COMMENTS),
            (1, 2, 0)
        );
    }

    #[test]
    fn under_and_suffix() {
        assert!(under("crates/px-a/src/x.rs", &["crates", "*", "src"]));
        assert!(under(
            "crates/px-a/src/deep/er/x.rs",
            &["crates", "*", "src"]
        ));
        assert!(!under("crates/px-a/src", &["crates", "*", "src"]));
        assert!(!under("crates/px-a/tests/x.rs", &["crates", "*", "src"]));
        assert_eq!(suffix("a/b.rs"), ".rs");
        assert_eq!(suffix("a/.rs"), "");
        assert_eq!(suffix("a/b.tar.gz"), ".gz");
    }

    #[test]
    fn thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(1234567), "1,234,567");
    }

    #[test]
    fn splice_replaces_only_the_marked_section() {
        let r = format!("top\n{BEGIN}\nold\n{END}\nbottom\n");
        assert_eq!(splice(&r, "NEW").as_deref(), Some("top\nNEW\nbottom\n"));
        assert!(splice("no markers", "x").is_none());
        assert!(splice(&format!("{END}{BEGIN}"), "x").is_none());
    }

    fn tree(fx: &Fixture) {
        fx.write("README.md", &format!("# T\n{BEGIN}\nstale\n{END}\nend\n"))
            .write("crates/px-a/src/lib.rs", "// doc\nfn a() {}\n\n")
            .write("crates/px-a/src/deep/m.rs", "fn b() {}\n")
            .write("crates/px-a/tests/t.rs", "fn t() {}\n")
            .write("fuzz/fuzz_targets/f.rs", "fn f() {}\n")
            .write("ci/x.sh", "# c\necho\n")
            .write("docs/a.md", "l1\nl2\n")
            .write("docs/sub/b.md", "l1\n")
            .commit_all();
    }

    #[test]
    fn write_then_check_round_trip() {
        let fx = Fixture::new("count-lines");
        tree(&fx);
        let mut g = Gate::new("count-lines");
        run(&mut g, fx.path(), &["--check".to_string()]);
        assert_eq!(g.fail_count(), 1, "stale section must fail the check");

        let mut g = Gate::new("count-lines");
        run(&mut g, fx.path(), &[]);
        assert!(g.passed());
        let readme = std::fs::read_to_string(fx.path().join("README.md")).expect("readme");
        assert!(
            readme.contains("| Rust \u{2014} crate sources | 2 | 2 | 1 | 1 |"),
            "{readme}"
        );
        assert!(readme.contains("| Rust \u{2014} tests | 1 | 1 | 0 | 0 |"));
        assert!(readme.contains("| Rust \u{2014} fuzz targets | 1 | 1 | 0 | 0 |"));
        assert!(readme.contains("| CI scripts | 1 | 1 | 1 | 0 |"));
        assert!(readme.contains("| **Total** | **5** | **5** | **2** | **1** |"));
        assert!(
            readme
                .contains("Plus 3 lines of specification and decision records across 2 documents.")
        );
        assert!(readme.starts_with("# T\n") && readme.ends_with("\nend\n"));

        let mut g = Gate::new("count-lines");
        run(&mut g, fx.path(), &["--check".to_string()]);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn missing_markers_fail() {
        let fx = Fixture::new("count-lines-nomarkers");
        fx.write("README.md", "# nothing\n").commit_all();
        let mut g = Gate::new("count-lines");
        run(&mut g, fx.path(), &[]);
        assert_eq!(
            g.failures(),
            ["README.md has no <!-- SIZE:BEGIN --> / <!-- SIZE:END --> markers"]
        );
    }
}
