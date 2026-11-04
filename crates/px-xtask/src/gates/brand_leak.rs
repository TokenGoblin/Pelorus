//! `cargo xtask brand-leak` -- gate check 5, brand leak (build-spec §2.2,
//! Phase 0 gate). Port of `tests/brand/gate-brand-leak.sh`.
//!
//! Fails if the product name appears anywhere outside `tests/brand/allowlist.txt`.
//!
//! This file contains no occurrence of the product name, by construction: the
//! gate reads the name out of px-brand at runtime. That means it cannot
//! accidentally exempt itself from its own check, and a rename does not touch
//! it. The tests below use a synthetic name for the same reason.
//!
//! The gate proves itself with a deliberate violation: see
//! `deliberate_violation_is_detected` (and the Phase 0 transcript in
//! `tests/brand/evidence/`).

use crate::Gate;
use crate::git;
use crate::glob;
use std::path::Path;

const BRAND_SRC: &str = "crates/px-brand/src/lib.rs";
const ALLOWLIST: &str = "tests/brand/allowlist.txt";

/// Read `pub const PRODUCT_NAME: &str = "...";` from px-brand's source. Takes
/// the first such line, as the shell version's `sed | head -n1` did.
pub fn read_product_name(src: &str) -> Option<String> {
    for line in src.lines() {
        let Some(rest) = line.strip_prefix("pub const PRODUCT_NAME: &str = \"") else {
            continue;
        };
        let Some((name, after)) = rest.split_once('"') else {
            continue;
        };
        if after.starts_with(';') {
            return Some(name.to_string());
        }
    }
    None
}

/// Allowlist patterns: blank lines and `#` comments skipped. A pattern ending
/// in `/` is a directory prefix; any other is a glob over the whole
/// repository-relative path (`*` crosses `/`).
pub fn parse_allowlist(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

pub fn allowed(patterns: &[String], path: &str) -> bool {
    patterns.iter().any(|pat| {
        if pat.ends_with('/') {
            path.starts_with(pat.as_str())
        } else {
            glob::matches(pat, path)
        }
    })
}

/// Up to `limit` `(line_number, line)` pairs containing `name`, ignoring
/// ASCII/Unicode case: a lowercase or uppercase spelling is the same leak.
/// A file with a NUL byte is binary and is skipped (`grep -I`).
pub fn find_leaks(bytes: &[u8], name: &str, limit: usize) -> Vec<(usize, String)> {
    if bytes.contains(&0) {
        return Vec::new();
    }
    let needle = name.to_lowercase();
    String::from_utf8_lossy(bytes)
        .lines()
        .enumerate()
        .filter(|(_, l)| l.to_lowercase().contains(&needle))
        .take(limit)
        .map(|(i, l)| (i + 1, l.to_string()))
        .collect()
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let src = match std::fs::read(root.join(BRAND_SRC)) {
        Ok(b) => String::from_utf8_lossy(&b).into_owned(),
        Err(_) => {
            g.fail(format!(
                "{BRAND_SRC} does not exist — there is no source of truth to read the name from"
            ));
            return;
        }
    };
    let name = match read_product_name(&src) {
        Some(n) if !n.is_empty() => n,
        _ => {
            g.fail(format!("could not read PRODUCT_NAME from {BRAND_SRC}"));
            return;
        }
    };
    g.info(format!(
        "read PRODUCT_NAME from {BRAND_SRC} ({} chars); scanning tracked files",
        name.chars().count()
    ));

    // A missing allowlist allows nothing: everything is scanned.
    let patterns = std::fs::read_to_string(root.join(ALLOWLIST))
        .map(|t| parse_allowlist(&t))
        .unwrap_or_default();

    let tracked = match git::ls_files(root, &[], &[]) {
        Ok(t) => t,
        Err(e) => {
            g.fail(format!("cannot list tracked files: {e}"));
            return;
        }
    };

    let mut scanned = 0usize;
    for path in &tracked {
        if allowed(&patterns, path) {
            continue;
        }
        scanned += 1;
        // Unreadable (e.g. deleted in the worktree but still in the index)
        // matches nothing, as `grep ... 2>/dev/null` did.
        let Ok(bytes) = std::fs::read(root.join(path)) else {
            continue;
        };
        let hits = find_leaks(&bytes, &name, 5);
        if !hits.is_empty() {
            g.fail(format!("product name appears in {path}"));
            for (n, line) in hits {
                g.detail(format!("{n}:{line}"));
            }
        }
    }
    g.info(format!("scanned {scanned} tracked files"));

    // The scan is over `git ls-files`, so an untracked file is invisible to it.
    // In CI everything is tracked and this never fires. Locally it is the
    // difference between a clean gate and a gate that has not looked at your
    // work yet.
    if let Ok(u) = git::ls_files(root, &["--others", "--exclude-standard"], &[])
        && !u.is_empty()
    {
        g.info(format!(
            "note: {} untracked file(s) were NOT scanned; stage them first",
            u.len()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    // Synthetic; the real name never appears in this crate.
    const NAME: &str = "Zorblax";

    #[test]
    fn reads_the_name_like_the_sed_did() {
        let src = "//! doc\npub const PRODUCT_NAME: &str = \"Zorblax\"; // trailing\n";
        assert_eq!(read_product_name(src).as_deref(), Some("Zorblax"));
        assert_eq!(read_product_name("pub const OTHER: &str = \"x\";"), None);
        // Needs the closing `";`
        assert_eq!(
            read_product_name("pub const PRODUCT_NAME: &str = \"x\""),
            None
        );
        // First matching line wins.
        let two = "pub const PRODUCT_NAME: &str = \"A\";\npub const PRODUCT_NAME: &str = \"B\";";
        assert_eq!(read_product_name(two).as_deref(), Some("A"));
        // Indented lines do not match (anchored at ^).
        assert_eq!(
            read_product_name("  pub const PRODUCT_NAME: &str = \"A\";"),
            None
        );
    }

    #[test]
    fn allowlist_directory_prefix_and_glob() {
        let p = parse_allowlist(
            "# c\n\ncrates/px-brand/\ndocs/\nREADME.md\nCargo.toml\ncrates/*/CLAUDE.md\n",
        );
        assert!(allowed(&p, "crates/px-brand/src/lib.rs"));
        assert!(allowed(&p, "docs/adr/001.md"));
        assert!(allowed(&p, "README.md"));
        assert!(allowed(&p, "Cargo.toml"));
        assert!(allowed(&p, "crates/px-a/CLAUDE.md"));
        assert!(!allowed(&p, "crates/px-a/src/lib.rs"));
        assert!(!allowed(&p, "crates/px-a/Cargo.toml"));
        assert!(!allowed(&p, "docsx/a.md"), "prefix means directory");
        assert!(!allowed(&p, "tests/brand/evidence/x.txt"));
        assert!(!allowed(&[], "anything"));
    }

    #[test]
    fn leak_search_is_case_insensitive_binary_safe_and_capped() {
        assert_eq!(
            find_leaks(b"a\nxx ZORBLAX yy\nc", NAME, 5),
            [(2, "xx ZORBLAX yy".into())]
        );
        assert_eq!(find_leaks(b"zorblax", NAME, 5).len(), 1);
        assert!(find_leaks(b"\0zorblax", NAME, 5).is_empty());
        let many = "zorblax\n".repeat(9);
        assert_eq!(find_leaks(many.as_bytes(), NAME, 5).len(), 5);
        assert!(find_leaks(b"clean", NAME, 5).is_empty());
    }

    fn base(fx: &Fixture) {
        fx.write(
            BRAND_SRC,
            &format!("pub const PRODUCT_NAME: &str = \"{NAME}\";\n"),
        )
        .write(ALLOWLIST, "crates/px-brand/\ndocs/\nREADME.md\n")
        .write("README.md", &format!("{NAME} is the product\n"))
        .write("docs/spec.md", &format!("about {NAME}\n"))
        .write("crates/px-layout/src/lib.rs", "pub fn f() {}\n");
    }

    fn run_on(fx: &Fixture) -> Gate {
        let mut g = Gate::new("brand-leak");
        run(&mut g, fx.path(), &[]);
        g
    }

    #[test]
    fn clean_tree_passes() {
        let fx = Fixture::new("brand-clean");
        base(&fx);
        fx.commit_all();
        let g = run_on(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    /// The gate's proof of itself (Phase 0): a deliberate violation, staged in
    /// a crate outside px-brand, must be rejected. The same shape as
    /// `tests/brand/evidence/violation-detected.txt`.
    #[test]
    fn deliberate_violation_is_detected() {
        let fx = Fixture::new("brand-violation");
        base(&fx);
        fx.write(
            "crates/px-layout/src/lib.rs",
            &format!("pub fn f() {{}}\npub const WINDOW_TITLE_SUFFIX: &str = \" - {NAME}\";\n"),
        )
        .commit_all();
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["product name appears in crates/px-layout/src/lib.rs"]
        );
    }

    #[test]
    fn lowercase_and_uppercase_spellings_leak_too() {
        let fx = Fixture::new("brand-case");
        base(&fx);
        fx.write("crates/px-a/src/lib.rs", "// ZORBLAX\n")
            .write("crates/px-b/src/lib.rs", "// zorblax\n")
            .commit_all();
        assert_eq!(run_on(&fx).fail_count(), 2);
    }

    #[test]
    fn unstaged_leak_is_not_scanned_but_staged_leak_is() {
        let fx = Fixture::new("brand-staged");
        base(&fx);
        fx.commit_all();
        fx.write("crates/px-a/src/lib.rs", "// Zorblax\n");
        assert!(
            run_on(&fx).passed(),
            "untracked files are invisible to the scan"
        );
        fx.stage_all();
        assert_eq!(run_on(&fx).fail_count(), 1);
    }

    #[test]
    fn missing_brand_source_fails_closed() {
        let fx = Fixture::new("brand-nosrc");
        fx.write("README.md", "x").commit_all();
        let g = run_on(&fx);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("does not exist"));
    }

    #[test]
    fn unreadable_name_fails_closed() {
        let fx = Fixture::new("brand-noname");
        fx.write(BRAND_SRC, "pub const PRODUCT_NAME: &str = \"\";\n")
            .commit_all();
        let g = run_on(&fx);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("could not read PRODUCT_NAME"));
    }

    #[test]
    fn missing_allowlist_scans_everything() {
        let fx = Fixture::new("brand-noallow");
        base(&fx);
        fx.remove(ALLOWLIST).commit_all();
        // README, docs/spec.md and px-brand's own source are no longer exempt.
        assert_eq!(run_on(&fx).fail_count(), 3);
    }

    #[test]
    fn binary_files_are_skipped() {
        let fx = Fixture::new("brand-binary");
        base(&fx);
        fx.write_bytes("data/blob.bin", b"\0\0zorblax\0")
            .commit_all();
        assert!(run_on(&fx).passed());
    }
}
