//! Source-scan helpers shared by the gates: the directory walk and the
//! `grep -rn` shape (`path:line:content`) the shell gates printed.
//!
//! Matching itself stays in each gate as a small pure function with its own
//! tests; this module only walks, numbers lines and drops comment lines.

use std::path::{Path, PathBuf};

/// Shell `[[:space:]]`.
pub fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\u{b}' | '\u{c}' | '\r')
}

/// Regex `\w` / the identifier character class `[A-Za-z0-9_]` (plus Unicode
/// alphanumerics, as grep in a UTF-8 locale treats them).
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A line that is only a `//` comment (leading whitespace allowed). Comments
/// are excluded from the source scans so a rule can be written down in prose
/// without tripping itself.
pub fn is_comment_line(line: &str) -> bool {
    line.trim_start_matches(is_space).starts_with("//")
}

/// True if `text` starts with `word` and the next character (if any) is not a
/// word character: regex `word\b` for a word ending in an identifier
/// character.
pub fn starts_with_word(text: &str, word: &str) -> bool {
    text.strip_prefix(word)
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !is_word(c)))
}

fn walk_inner(dir: &Path, files_only: bool, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let Ok(t) = e.file_type() else { continue };
        if t.is_dir() {
            if !files_only {
                out.push(e.path());
            }
            walk_inner(&e.path(), files_only, out);
        } else if t.is_file() || !files_only {
            out.push(e.path());
        }
    }
}

/// All regular files under `dir`, recursively, without following symlinks (as
/// `grep -r` does for links met during the walk), in path order.
pub fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk_inner(dir, true, &mut v);
    v.sort();
    v
}

/// Every entry under `dir` of any type (what `find dir` lists, minus `dir`
/// itself), in path order.
pub fn walk_entries(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk_inner(dir, false, &mut v);
    v.sort();
    v
}

/// `path` relative to `root`, with `/` separators.
pub fn rel(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// `rel:line:content` for every line of every file under `root/dir` (filtered
/// by `file_ok` on the relative path) that satisfies `hit`, in path order --
/// the shape `grep -rn` printed. Comment lines are included; callers drop them
/// with [`drop_comments`] when the original pipeline did.
pub fn grep_rn(
    root: &Path,
    dir: &str,
    file_ok: fn(&str) -> bool,
    hit: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut found = Vec::new();
    for f in walk_files(&root.join(dir)) {
        let r = rel(root, &f);
        if !file_ok(&r) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        for (n, line) in text.split('\n').enumerate() {
            if hit(line) {
                found.push(format!("{r}:{}:{line}", n + 1));
            }
        }
    }
    found
}

/// `grep -vE '^[^:]*:[0-9]+:[[:space:]]*//'`: drop `path:line:content` records
/// whose content is only a comment.
pub fn drop_comments(records: Vec<String>) -> Vec<String> {
    records
        .into_iter()
        .filter(|r| {
            let Some((path_part, rest)) = r.split_once(':') else {
                return true;
            };
            let _ = path_part; // `[^:]*` is whatever precedes the first colon
            let Some((num, content)) = rest.split_once(':') else {
                return true;
            };
            !(!num.is_empty()
                && num.bytes().all(|b| b.is_ascii_digit())
                && content.trim_start_matches(is_space).starts_with("//"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn comment_lines() {
        assert!(is_comment_line("  // x"));
        assert!(is_comment_line("//! x"));
        assert!(is_comment_line("\t/// x"));
        assert!(!is_comment_line("let x = 1; // trailing"));
        assert!(!is_comment_line(""));
    }

    #[test]
    fn word_boundary_after() {
        assert!(starts_with_word("Node>", "Node"));
        assert!(starts_with_word("Node", "Node"));
        assert!(!starts_with_word("NodeData", "Node"));
        assert!(!starts_with_word("Node_x", "Node"));
        assert!(!starts_with_word("Nod", "Node"));
    }

    #[test]
    fn walk_is_sorted_recursive_and_typed() {
        let fx = Fixture::new("scan-walk");
        fx.write("d/b.rs", "x")
            .write("d/a.rs", "x")
            .write("d/s/c.rs", "x");
        let files: Vec<String> = walk_files(&fx.path().join("d"))
            .iter()
            .map(|p| rel(fx.path(), p))
            .collect();
        assert_eq!(files, ["d/a.rs", "d/b.rs", "d/s/c.rs"]);
        let all: Vec<String> = walk_entries(&fx.path().join("d"))
            .iter()
            .map(|p| rel(fx.path(), p))
            .collect();
        assert_eq!(all, ["d/a.rs", "d/b.rs", "d/s", "d/s/c.rs"]);
        assert!(walk_files(&fx.path().join("missing")).is_empty());
    }

    #[test]
    fn grep_rn_numbers_lines_and_filters_files() {
        let fx = Fixture::new("scan-grep");
        fx.write("src/a.rs", "no\nhit here\n// hit comment\n")
            .write("src/b.txt", "hit\n");
        let hit = |l: &str| l.contains("hit");
        let all = grep_rn(fx.path(), "src", |p| p.ends_with(".rs"), &hit);
        assert_eq!(all, ["src/a.rs:2:hit here", "src/a.rs:3:// hit comment"]);
        assert_eq!(drop_comments(all), ["src/a.rs:2:hit here"]);
    }

    #[test]
    fn drop_comments_keeps_trailing_comment_lines() {
        let v = vec![
            "p.rs:1:let x = 1; // note".to_string(),
            "p.rs:2:   // only".to_string(),
        ];
        assert_eq!(drop_comments(v), ["p.rs:1:let x = 1; // note"]);
    }
}
