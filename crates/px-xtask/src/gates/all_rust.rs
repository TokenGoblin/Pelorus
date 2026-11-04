//! `cargo xtask all-rust` -- the 100%-Rust ratchet (ADR 032).
//!
//! Fails if a tracked first-party source file in a non-Rust language exists
//! that is not on `all-rust-allowlist.txt`, and fails if the allowlist names a
//! file that is gone (or that is no longer a non-Rust source). The list may
//! only shrink.
//!
//! The decision lives in the pure `check` so it can be tested against a list
//! of paths; `run` only gathers the inputs (git's tracked files, the allowlist
//! text, a shebang sniffer).

use crate::Gate;
use crate::git;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const ALLOWLIST_PATH: &str = "crates/px-xtask/all-rust-allowlist.txt";

/// Directory prefixes whose contents are vendored third-party data or corpora
/// we do not author (ADR 032).
const EXCLUDED_PREFIXES: &[&str] = &["tests/wpt/", "tests/html5lib/", "fuzz/corpus/"];

/// Single files excluded by rule. `build-python/requirements.txt` is data that
/// pins the one named exception (stylo's Python+Mako build, ADR 023/032); it is
/// not Python source and is not matched by any extension below, but the rule
/// is recorded here so the decision is visible next to the code.
const EXCLUDED_FILES: &[&str] = &["build-python/requirements.txt"];

/// Workflow configuration is YAML, acceptable as config (ADR 032).
const WORKFLOW_DIR: &str = ".github/workflows/";

/// Extensions (lowercase, no dot) of first-party non-Rust source, with a label.
const FOREIGN_EXTENSIONS: &[(&str, &str)] = &[
    ("sh", "shell"),
    ("bash", "shell"),
    ("zsh", "shell"),
    ("ksh", "shell"),
    ("csh", "shell"),
    ("tcsh", "shell"),
    ("fish", "shell"),
    ("py", "Python"),
    ("pyw", "Python"),
    ("pyi", "Python"),
    ("ps1", "PowerShell"),
    ("psm1", "PowerShell"),
    ("psd1", "PowerShell"),
    ("bat", "batch"),
    ("cmd", "batch"),
    ("js", "JavaScript"),
    ("mjs", "JavaScript"),
    ("cjs", "JavaScript"),
    ("jsx", "JavaScript"),
    ("ts", "TypeScript"),
    ("tsx", "TypeScript"),
    ("mts", "TypeScript"),
    ("cts", "TypeScript"),
    ("rb", "Ruby"),
    ("pl", "Perl"),
    ("pm", "Perl"),
    ("php", "PHP"),
    ("lua", "Lua"),
    ("vbs", "VBScript"),
    ("awk", "awk"),
    ("mk", "make"),
    ("c", "C"),
    ("h", "C"),
    ("cc", "C++"),
    ("cpp", "C++"),
    ("cxx", "C++"),
    ("hpp", "C++"),
    ("m", "Objective-C"),
    ("mm", "Objective-C++"),
    ("go", "Go"),
    ("java", "Java"),
    ("kt", "Kotlin"),
    ("swift", "Swift"),
    ("cs", "C#"),
];

/// Extensionless build-tool file names (compared lowercase).
const FOREIGN_NAMES: &[(&str, &str)] = &[
    ("makefile", "make"),
    ("gnumakefile", "make"),
    ("justfile", "just"),
];

pub fn is_excluded(path: &str) -> bool {
    EXCLUDED_PREFIXES.iter().any(|p| path.starts_with(p))
        || EXCLUDED_FILES.contains(&path)
        || (path.starts_with(WORKFLOW_DIR) && (path.ends_with(".yml") || path.ends_with(".yaml")))
}

/// Why `path` counts as first-party non-Rust source, or `None` if it does not.
/// `has_shebang` is asked only for extensionless files.
pub fn classify(path: &str, has_shebang: &dyn Fn(&str) -> bool) -> Option<&'static str> {
    if is_excluded(path) {
        return None;
    }
    let base = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    if let Some((_, lang)) = FOREIGN_NAMES.iter().find(|(n, _)| *n == base) {
        return Some(lang);
    }
    // A leading dot is part of the name (".gitignore"), not an extension.
    match base.rfind('.').filter(|i| *i > 0) {
        Some(i) => {
            let ext = base.get(i + 1..).unwrap_or("");
            FOREIGN_EXTENSIONS
                .iter()
                .find(|(e, _)| *e == ext)
                .map(|(_, lang)| *lang)
        }
        None => has_shebang(path).then_some("script (shebang)"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Problem {
    /// A non-Rust source that is tracked and not on the allowlist.
    Unlisted { path: String, lang: &'static str },
    /// On the allowlist but not tracked any more.
    Stale { path: String },
    /// On the allowlist but excluded by rule: the entry is noise.
    ExcludedListed { path: String },
    /// On the allowlist, tracked, but no longer a non-Rust source.
    NotForeign { path: String },
    /// Listed twice.
    Duplicate { path: String },
    /// Entry with no `# reason`.
    NoReason { line: usize, path: String },
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::Unlisted { path, lang } => write!(
                f,
                "{path} is first-party {lang} source; the project is 100% Rust (ADR 032). \
                 Write it in Rust (cargo xtask / a crate). The allowlist may only shrink."
            ),
            Problem::Stale { path } => write!(
                f,
                "{ALLOWLIST_PATH} lists {path} but it is not a tracked file. \
                 Delete the line in the commit that removes the file."
            ),
            Problem::ExcludedListed { path } => write!(
                f,
                "{ALLOWLIST_PATH} lists {path}, which is excluded by rule; remove the entry"
            ),
            Problem::NotForeign { path } => write!(
                f,
                "{ALLOWLIST_PATH} lists {path}, which is no longer non-Rust source; remove the entry"
            ),
            Problem::Duplicate { path } => {
                write!(f, "{ALLOWLIST_PATH} lists {path} more than once")
            }
            Problem::NoReason { line, path } => write!(
                f,
                "{ALLOWLIST_PATH}:{line}: {path} has no `# reason` comment"
            ),
        }
    }
}

/// One parsed allowlist line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub line: usize,
    pub path: String,
    pub reason: String,
}

/// Parse `path  # reason` lines. Blank lines and lines starting with `#` are
/// ignored. A `#` begins the reason only when preceded by whitespace, so a path
/// may contain `#`.
pub fn parse_allowlist(text: &str) -> Vec<Entry> {
    let mut v = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let split = line
            .char_indices()
            .find(|(j, c)| *c == '#' && line[..*j].ends_with(char::is_whitespace));
        let (path, reason) = match split {
            Some((j, _)) => (line[..j].trim(), line.get(j + 1..).unwrap_or("").trim()),
            None => (line, ""),
        };
        v.push(Entry {
            line: i + 1,
            path: path.to_string(),
            reason: reason.to_string(),
        });
    }
    v
}

#[derive(Debug, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
    /// Tracked non-Rust sources found (listed or not).
    pub foreign: usize,
    /// Allowlist entries that are valid (still a tracked foreign file).
    pub remaining: usize,
}

/// The ratchet. `tracked` is `git ls-files`; `allowlist` is the file's text.
pub fn check(tracked: &[String], allowlist: &str, has_shebang: &dyn Fn(&str) -> bool) -> Report {
    let entries = parse_allowlist(allowlist);
    let tracked_set: BTreeSet<&str> = tracked.iter().map(String::as_str).collect();

    let mut foreign: BTreeMap<&str, &'static str> = BTreeMap::new();
    for p in tracked {
        if let Some(lang) = classify(p, has_shebang) {
            foreign.insert(p.as_str(), lang);
        }
    }

    let mut r = Report {
        foreign: foreign.len(),
        ..Report::default()
    };
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for e in &entries {
        if e.reason.is_empty() {
            r.problems.push(Problem::NoReason {
                line: e.line,
                path: e.path.clone(),
            });
        }
        if !seen.insert(e.path.as_str()) {
            r.problems.push(Problem::Duplicate {
                path: e.path.clone(),
            });
            continue;
        }
        if !tracked_set.contains(e.path.as_str()) {
            r.problems.push(Problem::Stale {
                path: e.path.clone(),
            });
        } else if is_excluded(&e.path) {
            r.problems.push(Problem::ExcludedListed {
                path: e.path.clone(),
            });
        } else if !foreign.contains_key(e.path.as_str()) {
            r.problems.push(Problem::NotForeign {
                path: e.path.clone(),
            });
        } else {
            r.remaining += 1;
        }
    }
    for (path, lang) in &foreign {
        if !seen.contains(path) {
            r.problems.push(Problem::Unlisted {
                path: (*path).to_string(),
                lang,
            });
        }
    }
    r.problems.sort();
    r
}

/// Does the file start with `#!` (and not the Rust inner attribute `#![`)?
fn sniff_shebang(root: &Path, path: &str) -> bool {
    use std::io::Read;
    let Ok(mut f) = std::fs::File::open(root.join(path)) else {
        return false;
    };
    let mut buf = [0u8; 3];
    let n = f.read(&mut buf).unwrap_or(0);
    n >= 2 && buf.starts_with(b"#!") && buf.get(2) != Some(&b'[')
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let tracked = match git::ls_files(root, &[], &[]) {
        Ok(t) if !t.is_empty() => t,
        Ok(_) => {
            g.fail("git ls-files listed no tracked files; nothing to check");
            return;
        }
        Err(e) => {
            g.fail(format!("cannot list tracked files: {e}"));
            return;
        }
    };
    let allowlist = match std::fs::read_to_string(root.join(ALLOWLIST_PATH)) {
        Ok(t) => t,
        Err(e) => {
            g.fail(format!("cannot read {ALLOWLIST_PATH}: {e}"));
            return;
        }
    };
    let report = check(&tracked, &allowlist, &|p| sniff_shebang(root, p));
    g.info(format!(
        "{} tracked files; {} non-Rust source file(s), {} allowlisted for porting",
        tracked.len(),
        report.foreign,
        report.remaining
    ));
    for p in &report.problems {
        g.fail(p.to_string());
    }
    if report.problems.is_empty() {
        if report.remaining == 0 {
            g.ok("no first-party non-Rust source; the allowlist is empty (ADR 032)");
        } else {
            g.ok(format!(
                "no unlisted non-Rust source; {} script(s) left to port, none stale",
                report.remaining
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn no_shebang(_: &str) -> bool {
        false
    }

    fn files(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn problems(tracked: &[&str], allow: &str) -> Vec<Problem> {
        check(&files(tracked), allow, &no_shebang).problems
    }

    #[test]
    fn rust_only_tree_passes_with_empty_allowlist() {
        let r = problems(
            &["Cargo.toml", "crates/a/src/lib.rs", "docs/x.md", "build.rs"],
            "",
        );
        assert!(r.is_empty(), "{r:?}");
    }

    #[test]
    fn every_foreign_extension_is_flagged() {
        for (ext, _) in FOREIGN_EXTENSIONS {
            let p = format!("ci/thing.{ext}");
            assert!(
                classify(&p, &no_shebang).is_some(),
                "{ext} should be flagged"
            );
            let upper = format!("ci/THING.{}", ext.to_ascii_uppercase());
            assert!(classify(&upper, &no_shebang).is_some(), "{upper}");
        }
    }

    #[test]
    fn data_docs_and_assets_are_not_flagged() {
        for p in [
            "Cargo.toml",
            "Cargo.lock",
            "x.json",
            "a.der",
            "tests/net/corpus/000.har",
            "data/x.dat",
            "README.md",
            "logo.svg",
            "page.html",
            "rust-toolchain.toml",
            ".gitignore",
            ".gitattributes",
            "build-python/requirements.txt",
            ".github/workflows/gate.yml",
            "supply-chain/config.toml",
        ] {
            assert_eq!(classify(p, &no_shebang), None, "{p}");
        }
    }

    #[test]
    fn makefiles_by_name() {
        assert!(classify("Makefile", &no_shebang).is_some());
        assert!(classify("sub/dir/makefile", &no_shebang).is_some());
        assert!(classify("GNUmakefile", &no_shebang).is_some());
        assert!(classify("justfile", &no_shebang).is_some());
        assert!(classify("Makefile.md", &no_shebang).is_none());
    }

    #[test]
    fn excluded_paths_are_never_flagged() {
        for p in [
            "tests/wpt/css/a.js",
            "tests/wpt/tools/run.py",
            "tests/html5lib/x/y.py",
            "fuzz/corpus/frame/seed.sh",
            "fuzz/corpus/frame/noext",
        ] {
            assert!(is_excluded(p), "{p}");
            assert_eq!(classify(p, &|_| true), None, "{p}");
        }
        // Prefix means directory, not substring.
        assert!(!is_excluded("tests/wptx/a.js"));
        assert!(!is_excluded("ci/tests/wpt/a.js"));
        assert!(classify("tests/wptx/a.js", &no_shebang).is_some());
        // Only yml/yaml in workflows are exempt: a script hidden there is not.
        assert!(is_excluded(".github/workflows/gate.yml"));
        assert!(!is_excluded(".github/workflows/helper.sh"));
        assert!(classify(".github/workflows/helper.sh", &no_shebang).is_some());
    }

    #[test]
    fn extensionless_shebang_file_is_flagged() {
        let sniff = |p: &str| p == "bin/tool";
        assert_eq!(classify("bin/tool", &sniff), Some("script (shebang)"));
        assert_eq!(classify("bin/other", &sniff), None);
        // A file with an extension is not sniffed.
        assert_eq!(classify("notes.md", &|_| true), None);
    }

    #[test]
    fn unlisted_foreign_file_fails() {
        let r = problems(&["a.rs", "ci/new.sh"], "");
        assert_eq!(
            r,
            vec![Problem::Unlisted {
                path: "ci/new.sh".into(),
                lang: "shell"
            }]
        );
    }

    #[test]
    fn listed_foreign_file_passes() {
        let r = check(
            &files(&["ci/a.sh", "tools/b.py", "src/lib.rs"]),
            "# header\n\nci/a.sh  # port to cargo xtask a\ntools/b.py # port\n",
            &no_shebang,
        );
        assert!(r.problems.is_empty(), "{:?}", r.problems);
        assert_eq!((r.foreign, r.remaining), (2, 2));
    }

    #[test]
    fn stale_entry_fails() {
        let r = problems(&["a.rs"], "ci/gone.sh # was ported\n");
        assert_eq!(
            r,
            vec![Problem::Stale {
                path: "ci/gone.sh".into()
            }]
        );
    }

    #[test]
    fn new_file_and_stale_entry_both_reported() {
        let r = problems(&["ci/new.py"], "ci/old.sh # x\n");
        assert_eq!(r.len(), 2);
        assert!(r.iter().any(|p| matches!(p, Problem::Stale { .. })));
        assert!(r.iter().any(|p| matches!(p, Problem::Unlisted { .. })));
    }

    #[test]
    fn entry_for_file_that_became_rust_fails() {
        let r = problems(&["ci/thing.rs"], "ci/thing.rs # oops\n");
        assert_eq!(
            r,
            vec![Problem::NotForeign {
                path: "ci/thing.rs".into()
            }]
        );
    }

    #[test]
    fn entry_for_excluded_path_fails() {
        let r = problems(&["tests/wpt/a.js"], "tests/wpt/a.js # no\n");
        assert_eq!(
            r,
            vec![Problem::ExcludedListed {
                path: "tests/wpt/a.js".into()
            }]
        );
    }

    #[test]
    fn duplicate_and_missing_reason_fail() {
        let r = problems(&["a.sh"], "a.sh # one\na.sh # two\n");
        assert_eq!(
            r,
            vec![Problem::Duplicate {
                path: "a.sh".into()
            }]
        );
        let r = problems(&["a.sh"], "a.sh\n");
        assert_eq!(
            r,
            vec![Problem::NoReason {
                line: 1,
                path: "a.sh".into()
            }]
        );
        let r = problems(&["a.sh"], "a.sh #   \n");
        assert!(matches!(r.as_slice(), [Problem::NoReason { .. }]));
    }

    #[test]
    fn parse_handles_comments_blank_lines_and_hash_in_path() {
        let e = parse_allowlist("# c\n\n  x/a.sh   # why  \nx/b#c.sh # r\n");
        assert_eq!(e.len(), 2);
        assert_eq!(
            (e[0].line, e[0].path.as_str(), e[0].reason.as_str()),
            (3, "x/a.sh", "why")
        );
        assert_eq!(
            (e[1].path.as_str(), e[1].reason.as_str()),
            ("x/b#c.sh", "r")
        );
    }

    #[test]
    fn allowlist_may_only_shrink_across_a_port() {
        // Before the port: script tracked and listed -> pass.
        let before = files(&["ci/a.sh", "ci/b.sh"]);
        let list = "ci/a.sh # x\nci/b.sh # x\n";
        assert!(check(&before, list, &no_shebang).problems.is_empty());
        // Script deleted, line left behind -> stale.
        let after = files(&["ci/b.sh"]);
        assert!(!check(&after, list, &no_shebang).problems.is_empty());
        // Script deleted, line deleted -> pass.
        assert!(
            check(&after, "ci/b.sh # x\n", &no_shebang)
                .problems
                .is_empty()
        );
    }

    // End to end on a real (temporary) git repository.

    fn run_on(fx: &Fixture) -> Gate {
        let mut g = Gate::new("all-rust");
        run(&mut g, fx.path(), &[]);
        g
    }

    #[test]
    fn fixture_clean_rust_repo_passes() {
        let fx = Fixture::new("allrust-clean");
        fx.write("Cargo.toml", "[workspace]\n")
            .write("crates/a/src/lib.rs", "")
            .write(ALLOWLIST_PATH, "# empty\n")
            .write(".github/workflows/gate.yml", "name: gate\n")
            .write("tests/wpt/a.js", "x")
            .commit_all();
        let g = run_on(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn fixture_new_script_fails_and_untracked_is_ignored() {
        let fx = Fixture::new("allrust-new");
        fx.write("a.rs", "")
            .write(ALLOWLIST_PATH, "# empty\n")
            .commit_all();
        // Untracked scripts are not the ratchet's business until staged.
        fx.write("ci/untracked.sh", "#!/bin/sh\n");
        assert!(run_on(&fx).passed());
        fx.stage_all();
        let g = run_on(&fx);
        assert_eq!(g.fail_count(), 1, "{:?}", g.failures());
        assert!(g.failures()[0].contains("ci/untracked.sh"));
    }

    #[test]
    fn fixture_listed_script_passes_then_stale_after_removal() {
        let fx = Fixture::new("allrust-stale");
        fx.write("ci/a.sh", "#!/bin/sh\n")
            .write(ALLOWLIST_PATH, "ci/a.sh # to port\n")
            .commit_all();
        assert!(run_on(&fx).passed());
        fx.git(&["rm", "-q", "-f", "ci/a.sh"]);
        let g = run_on(&fx);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("not a tracked file"));
    }

    #[test]
    fn fixture_extensionless_shebang_is_caught_but_rust_attr_is_not() {
        let fx = Fixture::new("allrust-sniff");
        fx.write("bin/runme", "#!/usr/bin/env bash\necho hi\n")
            .write("bin/attr", "#![forbid(unsafe_code)]\n")
            .write("bin/plain", "hello\n")
            .write(ALLOWLIST_PATH, "")
            .commit_all();
        let g = run_on(&fx);
        assert_eq!(g.fail_count(), 1, "{:?}", g.failures());
        assert!(g.failures()[0].contains("bin/runme"));
    }

    #[test]
    fn fixture_missing_allowlist_fails_closed() {
        let fx = Fixture::new("allrust-noallow");
        fx.write("a.rs", "").commit_all();
        assert_eq!(run_on(&fx).fail_count(), 1);
    }

    #[test]
    fn fixture_not_a_git_repo_fails_closed() {
        let fx = Fixture::new("allrust-nogit");
        fx.write("a.rs", "");
        // A temp dir under a non-repo parent: git ls-files errors.
        let g = run_on(&fx);
        assert!(!g.passed());
    }
}
