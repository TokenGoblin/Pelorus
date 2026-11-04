//! `cargo xtask compare-hashes <hashes-a> <hashes-b>` -- gate check 2,
//! reproducible builds (invariant 7). Port of `ci/compare-hashes.sh`.
//!
//! Compares the two manifests `cargo xtask build-and-hash` printed from two
//! environments (different path, HOME, CARGO_HOME and, on Linux, user). They must
//! be byte-identical. An empty or missing manifest is a failure of its own: a
//! build that produced no artifacts must not "reproduce" by matching another
//! empty one.
//!
//! The script's verdict word was `reproducible`; the gate is registered under
//! the script's file name, `compare-hashes`.

use crate::Gate;
use std::path::Path;

/// A `hash  name` manifest line split the way `read -r hash name` did.
pub fn split_line(line: &str) -> (&str, &str) {
    match line.split_once(char::is_whitespace) {
        Some((h, rest)) => (h, rest.trim_start()),
        None => (line, ""),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Identical: one `name hash` entry per manifest line.
    Same(Vec<String>),
    /// Different: a line diff to show.
    Differ(Vec<String>),
}

/// Pure comparison of two manifests.
pub fn compare(a: &str, b: &str) -> Verdict {
    if a == b {
        return Verdict::Same(
            a.lines()
                .map(|l| {
                    let (h, n) = split_line(l);
                    format!("{n} {h}")
                })
                .collect(),
        );
    }
    Verdict::Differ(diff_lines(a, b))
}

/// Line diff in the shape of `diff -u` minus hunk headers: ` ` common,
/// `-` only in a, `+` only in b. Longest-common-subsequence; manifests are a
/// handful of lines.
pub fn diff_lines(a: &str, b: &str) -> Vec<String> {
    let a: Vec<&str> = a.lines().collect();
    let b: Vec<&str> = b.lines().collect();
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < a.len() && j < b.len() {
        if a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if t[i + 1][j] >= t[i][j + 1] {
            out.push(format!("-{}", a[i]));
            i += 1;
        } else {
            out.push(format!("+{}", b[j]));
            j += 1;
        }
    }
    out.extend(a[i..].iter().map(|l| format!("-{l}")));
    out.extend(b[j..].iter().map(|l| format!("+{l}")));
    out
}

fn read_manifest(g: &mut Gate, path: &Path, which: &str) -> Option<String> {
    // `[ -s file ]`: exists and is non-empty. Unreadable counts as empty.
    match std::fs::read(path) {
        Ok(b) if !b.is_empty() => Some(String::from_utf8_lossy(&b).into_owned()),
        _ => {
            g.fail(format!(
                "{} is empty \u{2014} the {which} build produced no artifacts",
                path.display()
            ));
            None
        }
    }
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let [a, b] = args else {
        g.fail("usage: compare-hashes <a> <b>");
        return;
    };
    let resolve = |p: &str| {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            root.join(p)
        }
    };
    let (a, b) = (resolve(a), resolve(b));
    let ma = read_manifest(g, &a, "first");
    let mb = read_manifest(g, &b, "second");
    let (Some(ma), Some(mb)) = (ma, mb) else {
        return;
    };
    match compare(&ma, &mb) {
        Verdict::Same(entries) => {
            for e in entries {
                g.ok(e);
            }
        }
        Verdict::Differ(diff) => {
            g.fail("build hashes differ between the two environments");
            for l in diff {
                g.detail(l);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const A: &str = "aaaa  px-broker\nbbbb  px-render\n";

    #[test]
    fn identical_manifests_list_every_artifact() {
        assert_eq!(
            compare(A, A),
            Verdict::Same(vec!["px-broker aaaa".into(), "px-render bbbb".into()])
        );
    }

    #[test]
    fn a_changed_hash_is_a_diff() {
        let b = "aaaa  px-broker\ncccc  px-render\n";
        let Verdict::Differ(d) = compare(A, b) else {
            panic!("expected a difference");
        };
        assert_eq!(
            d,
            [" aaaa  px-broker", "-bbbb  px-render", "+cccc  px-render"]
        );
    }

    #[test]
    fn a_missing_artifact_is_a_diff() {
        let Verdict::Differ(d) = compare(A, "aaaa  px-broker\n") else {
            panic!("expected a difference");
        };
        assert_eq!(d, [" aaaa  px-broker", "-bbbb  px-render"]);
    }

    #[test]
    fn trailing_newline_difference_is_a_difference() {
        // `diff -u` reports a missing final newline as a difference.
        assert!(matches!(compare("a  x\n", "a  x"), Verdict::Differ(_)));
    }

    #[test]
    fn gate_passes_on_equal_files_and_fails_on_a_deliberate_difference() {
        let f = Fixture::new("cmp");
        f.write("a.txt", A).write("b.txt", A);
        let mut g = Gate::new("t");
        run(&mut g, f.path(), &["a.txt".into(), "b.txt".into()]);
        assert!(g.passed());
        f.write("b.txt", "aaaa  px-broker\ncccc  px-render\n");
        run(&mut g, f.path(), &["a.txt".into(), "b.txt".into()]);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("differ"));
    }

    #[test]
    fn empty_and_missing_manifests_each_fail() {
        let f = Fixture::new("cmp-empty");
        f.write("a.txt", "").write("b.txt", A);
        let mut g = Gate::new("t");
        run(&mut g, f.path(), &["a.txt".into(), "b.txt".into()]);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("the first build produced no artifacts"));
        let mut g = Gate::new("t");
        run(&mut g, f.path(), &["a.txt".into(), "nope.txt".into()]);
        assert_eq!(g.fail_count(), 2);
        assert!(g.failures()[1].contains("the second build"));
    }

    #[test]
    fn two_empty_manifests_do_not_reproduce() {
        let f = Fixture::new("cmp-both-empty");
        f.write("a.txt", "").write("b.txt", "");
        let mut g = Gate::new("t");
        run(&mut g, f.path(), &["a.txt".into(), "b.txt".into()]);
        assert_eq!(g.fail_count(), 2);
    }

    #[test]
    fn wrong_argument_count_fails() {
        let mut g = Gate::new("t");
        run(&mut g, Path::new("."), &["only-one".into()]);
        assert_eq!(g.fail_count(), 1);
    }
}
