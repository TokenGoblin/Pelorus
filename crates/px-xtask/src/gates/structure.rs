//! `cargo xtask structure` -- repository structure invariants that no other
//! gate covers. Port of `ci/gate-structure.sh`.
//!
//!   - every crate carries a CLAUDE.md (build-spec section 5, Phase 0 deliverable)
//!   - px-brand does not hold the update URL or signing key (section 14.1)
//!   - scripts CI executes are executable in the index
//!   - no personal identity in git metadata
//!   - no session URLs in commit messages
//!   - every fuzz target's corpus is accounted for in the git index
//!
//! OS-independent. CI runs it on Linux only.

use crate::Gate;
use crate::git;
use std::path::Path;

const BRAND_SRC: &str = "crates/px-brand/src/lib.rs";

/// Fuzz targets with no committed seeds. Named here rather than allowed by
/// silence. The list is a debt, not a policy: an empty corpus means the
/// campaign starts that target from nothing.
const CORPUS_EMPTY: &[&str] = &["http_response", "http_chunked"];

const THIS_FILE: &str = "crates/px-xtask/src/gates/structure.rs";

/// Section 14.1: renaming must never move the update endpoint. The way that
/// breaks is somebody helpfully consolidating "all the constants" into px-brand.
pub fn brand_src_holds_endpoint(content: &str) -> bool {
    let c = content.to_lowercase();
    [
        "update_url",
        "signing_key",
        "public_key",
        "http://",
        "https://",
    ]
    .iter()
    .any(|needle| c.contains(needle))
}

/// A commit identity must use a GitHub noreply address. Two forms: the
/// `<id>+<user>@users.noreply.github.com` address GitHub provides, and
/// `noreply@github.com`, which GitHub itself sets as committer for web-UI
/// commits and which carries no identity.
pub fn is_noreply(email: &str) -> bool {
    email.ends_with("@users.noreply.github.com") || email == "noreply@github.com"
}

/// Fuzz target names from `fuzz/Cargo.toml`: every `name = "..."` line,
/// except the harness package itself.
pub fn fuzz_targets(cargo_toml: &str) -> Vec<String> {
    cargo_toml
        .lines()
        .filter(|l| l.starts_with("name = "))
        .map(|l| {
            let l = l.strip_prefix("name = \"").unwrap_or(l);
            l.strip_suffix('"').unwrap_or(l).to_string()
        })
        .filter(|n| !n.is_empty() && n != "px-fuzz")
        .collect()
}

fn check_crates(g: &mut Gate, root: &Path) {
    let mut crates: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("crates")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("px-") && e.path().is_dir() {
                crates.push(name);
            }
        }
    }
    crates.sort();
    if crates.is_empty() {
        g.fail("no crates found under crates/");
        return;
    }
    for name in &crates {
        let dir = format!("crates/{name}/");
        if root.join(&dir).join("CLAUDE.md").is_file() {
            g.ok(format!("{dir} CLAUDE.md"));
        } else {
            g.fail(format!("{dir} has no CLAUDE.md"));
        }
    }
}

fn check_brand_endpoint(g: &mut Gate, root: &Path) {
    let p = root.join(BRAND_SRC);
    if !p.is_file() {
        return;
    }
    match std::fs::read(&p) {
        Ok(bytes) => {
            if brand_src_holds_endpoint(&String::from_utf8_lossy(&bytes)) {
                g.fail(format!(
                    "{BRAND_SRC} looks like it holds an update URL or key; §14.1 puts"
                ));
                g.fail("  those in px-update, permanently, out of scope for a rename");
            } else {
                g.ok("px-brand holds no update endpoint or key (§14.1)");
            }
        }
        Err(e) => g.fail(format!("cannot read {BRAND_SRC}: {e}")),
    }
}

/// core.filemode is false on a Windows checkout, so `chmod +x` never reaches
/// git and a script committed there arrives on a Linux runner as 0644. Rule: a
/// shebang means executable.
fn check_exec_bits(g: &mut Gate, root: &Path) {
    let entries = match git::ls_files_staged(root, &["ci/*", "tests/*"]) {
        Ok(e) => e,
        Err(e) => {
            g.fail(format!("cannot read the git index: {e}"));
            return;
        }
    };
    for e in entries {
        if !e.path.ends_with(".sh") {
            continue;
        }
        let has_shebang = std::fs::read(root.join(&e.path))
            .map(|b| b.starts_with(b"#!"))
            .unwrap_or(false);
        if !has_shebang {
            continue;
        }
        if e.mode == "100755" {
            g.ok(format!("{} is executable in the index", e.path));
        } else {
            g.fail(format!(
                "{} has a shebang but is mode {} in the index;",
                e.path, e.mode
            ));
            g.fail(format!(
                "  fix with: git update-index --chmod=+x {}",
                e.path
            ));
        }
    }
}

/// This repository is public. Git metadata is the easiest place for a real name
/// or a personal address to end up. The rule is a shape, not a name: a check
/// that grepped for the maintainer's real name would have to contain it.
fn check_identities(g: &mut Gate, root: &Path) {
    let log = match git::git(root, &["log", "--format=%h|%ae|%ce"]) {
        Ok(l) => String::from_utf8_lossy(&l).into_owned(),
        Err(e) => {
            g.fail(format!("cannot read git history: {e}"));
            return;
        }
    };
    let mut bad = false;
    for line in log.lines() {
        let mut it = line.splitn(3, '|');
        let hash = it.next().unwrap_or("");
        let ae = it.next().unwrap_or("");
        let ce = it.next().unwrap_or("");
        if !is_noreply(ae) {
            g.fail(format!(
                "commit {hash} has author email outside the noreply domain"
            ));
            bad = true;
        }
        if !is_noreply(ce) {
            g.fail(format!(
                "commit {hash} has committer email outside the noreply domain"
            ));
            bad = true;
        }
    }
    if !bad {
        g.ok("every commit identity uses a noreply address");
    }
}

/// Session links are account-scoped, useless to anyone reading the public repo,
/// and tie the history to a particular person's activity.
///
/// The shell version piped `git log | grep -q` under `pipefail`: grep exits at
/// the first match, git can die of SIGPIPE, and the pipeline then reads as
/// "no match". This reads the whole log, so a match is always seen.
fn check_session_urls(g: &mut Gate, root: &Path) {
    match git::git(root, &["log", "--format=%B"]) {
        Ok(l) => {
            if String::from_utf8_lossy(&l).contains("claude.ai/code/session") {
                g.fail("a commit message contains a Claude session URL");
            } else {
                g.ok("no session URLs in commit messages");
            }
        }
        Err(e) => g.fail(format!("cannot read git history: {e}")),
    }
}

/// Every fuzz target's committed corpus is checked through the git index, not
/// through the filesystem. An empty directory is invisible to git, so a corpus
/// directory that exists only on one machine is not a corpus: the `dom` job was
/// once red for five commits because of exactly that. The index is the only
/// view of the tree that is the same on every machine.
fn check_fuzz_corpus(g: &mut Gate, root: &Path) {
    let toml = std::fs::read_to_string(root.join("fuzz/Cargo.toml")).unwrap_or_default();
    let mut bad = false;
    for target in fuzz_targets(&toml) {
        let spec = format!("fuzz/corpus/{target}");
        let seeds = match git::ls_files(root, &[], &[&spec]) {
            Ok(v) => v.len(),
            Err(e) => {
                g.fail(format!("cannot list {spec} in the git index: {e}"));
                bad = true;
                continue;
            }
        };
        if CORPUS_EMPTY.contains(&target.as_str()) {
            if seeds == 0 {
                g.ok(format!(
                    "{target} has no committed corpus, which is a known gap"
                ));
            } else {
                g.fail(format!("{target} now has {seeds} committed seed(s);"));
                g.fail(format!("  remove it from CORPUS_EMPTY in {THIS_FILE}"));
                bad = true;
            }
        } else if seeds > 0 {
            g.ok(format!(
                "{target} has {seeds} committed corpus seed(s) in the index"
            ));
        } else {
            g.fail(format!(
                "fuzz target {target} has no corpus file in the git index."
            ));
            g.fail("  A directory that exists only on your machine is not a corpus:");
            g.fail("  git cannot store an empty directory, so CI will not see it.");
            bad = true;
        }
    }
    if !bad {
        g.ok("every fuzz target's corpus is accounted for");
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    check_crates(g, root);
    check_brand_endpoint(g, root);
    check_exec_bits(g, root);
    check_identities(g, root);
    check_session_urls(g, root);
    check_fuzz_corpus(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Fixture, GOOD_EMAIL};

    #[test]
    fn endpoint_patterns_are_case_insensitive() {
        assert!(brand_src_holds_endpoint("const UPDATE_URL: &str = \"\";"));
        assert!(brand_src_holds_endpoint("let signing_key = 1;"));
        assert!(brand_src_holds_endpoint("// see Public_Key"));
        assert!(brand_src_holds_endpoint("x = \"https://example.invalid\""));
        assert!(brand_src_holds_endpoint("x = \"HTTP://example.invalid\""));
        assert!(!brand_src_holds_endpoint(
            "pub const PRODUCT_NAME: &str = \"Zorb\";"
        ));
        assert!(!brand_src_holds_endpoint("http: not a url"));
    }

    #[test]
    fn noreply_shapes() {
        assert!(is_noreply("12345+someone@users.noreply.github.com"));
        assert!(is_noreply("noreply@github.com"));
        assert!(!is_noreply("noreply@anthropic.com"));
        assert!(!is_noreply("me@example.org"));
        assert!(!is_noreply(""));
        assert!(!is_noreply("noreply@github.com.evil.example"));
    }

    #[test]
    fn fuzz_target_extraction() {
        let t = "[package]\nname = \"px-fuzz\"\n\n[[bin]]\nname = \"frame_request\"\npath = \"x\"\n\n[[bin]]\nname = \"http_response\"\n [[bin]]\n name = \"indented_ignored\"\n";
        assert_eq!(fuzz_targets(t), ["frame_request", "http_response"]);
        assert!(fuzz_targets("").is_empty());
    }

    fn healthy(fx: &Fixture) {
        fx.write("crates/px-a/CLAUDE.md", "x")
            .write("crates/px-a/src/lib.rs", "")
            .write("crates/px-brand/CLAUDE.md", "x")
            .write(
                "crates/px-brand/src/lib.rs",
                "pub const PRODUCT_NAME: &str = \"Zorblax\";\n",
            )
            .write(
                "fuzz/Cargo.toml",
                "[package]\nname = \"px-fuzz\"\n[[bin]]\nname = \"seeded\"\n[[bin]]\nname = \"http_response\"\n",
            )
            .write("fuzz/corpus/seeded/s1", "seed")
            .write("ci/ok.sh", "#!/usr/bin/env bash\n");
    }

    fn run_on(fx: &Fixture) -> Gate {
        let mut g = Gate::new("structure");
        run(&mut g, fx.path(), &[]);
        g
    }

    #[test]
    fn healthy_tree_passes() {
        let fx = Fixture::new("structure-ok");
        healthy(&fx);
        fx.git_init()
            .stage_all()
            .git(&["update-index", "--chmod=+x", "ci/ok.sh"])
            .commit_as(GOOD_EMAIL, "init");
        let g = run_on(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn crate_without_claude_md_fails() {
        let fx = Fixture::new("structure-claude");
        healthy(&fx);
        fx.write("crates/px-b/src/lib.rs", "").commit_all();
        let g = run_on(&fx);
        assert!(
            g.failures()
                .iter()
                .any(|f| f == "crates/px-b/ has no CLAUDE.md"),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn no_crates_fails() {
        let fx = Fixture::new("structure-nocrates");
        fx.write("README.md", "x").commit_all();
        let g = run_on(&fx);
        assert!(g.failures().iter().any(|f| f.contains("no crates found")));
    }

    #[test]
    fn brand_crate_holding_an_endpoint_fails_with_two_lines() {
        let fx = Fixture::new("structure-endpoint");
        healthy(&fx);
        fx.write(
            "crates/px-brand/src/lib.rs",
            "pub const UPDATE_URL: &str = \"x\";\n",
        )
        .commit_all();
        let g = run_on(&fx);
        let n = g
            .failures()
            .iter()
            .filter(|f| f.contains("update URL") || f.contains("px-update"))
            .count();
        assert_eq!(n, 2, "{:?}", g.failures());
    }

    #[test]
    fn shebang_script_without_exec_bit_fails() {
        let fx = Fixture::new("structure-chmod");
        healthy(&fx);
        fx.commit_all(); // ci/ok.sh is committed 100644
        let g = run_on(&fx);
        assert!(
            g.failures()
                .iter()
                .any(|f| f.contains("ci/ok.sh has a shebang but is mode 100644")),
            "{:?}",
            g.failures()
        );
        assert!(
            g.failures()
                .iter()
                .any(|f| f.contains("git update-index --chmod=+x ci/ok.sh"))
        );
    }

    #[test]
    fn script_without_shebang_is_ignored() {
        let fx = Fixture::new("structure-noshebang");
        healthy(&fx);
        fx.write("ci/ok.sh", "echo no shebang\n").commit_all();
        let g = run_on(&fx);
        assert!(
            !g.failures().iter().any(|f| f.contains("ci/ok.sh")),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn non_noreply_identity_fails_both_fields() {
        let fx = Fixture::new("structure-ident");
        healthy(&fx);
        fx.git_init()
            .stage_all()
            .commit_as("someone@example.org", "x");
        let g = run_on(&fx);
        let f = g.failures();
        assert!(
            f.iter().any(|m| m.contains("author email outside")),
            "{f:?}"
        );
        assert!(
            f.iter().any(|m| m.contains("committer email outside")),
            "{f:?}"
        );
    }

    #[test]
    fn session_url_in_a_commit_message_fails() {
        let fx = Fixture::new("structure-session");
        healthy(&fx);
        fx.git_init().stage_all().commit_as(
            GOOD_EMAIL,
            "subject\n\nSession: https://claude.ai/code/session_abc",
        );
        let g = run_on(&fx);
        assert!(
            g.failures()
                .iter()
                .any(|f| f.contains("Claude session URL")),
            "{:?}",
            g.failures()
        );
    }

    #[test]
    fn unseeded_fuzz_target_fails_and_known_gap_must_stay_empty() {
        let fx = Fixture::new("structure-corpus");
        healthy(&fx);
        fx.write("fuzz/Cargo.toml", "[package]\nname = \"px-fuzz\"\n[[bin]]\nname = \"unseeded\"\n[[bin]]\nname = \"http_response\"\n")
            .write("fuzz/corpus/http_response/seed", "now seeded")
            .commit_all();
        let g = run_on(&fx);
        let f = g.failures();
        assert!(
            f.iter()
                .any(|m| m.contains("fuzz target unseeded has no corpus file")),
            "{f:?}"
        );
        assert!(
            f.iter()
                .any(|m| m.contains("http_response now has 1 committed seed(s)")),
            "{f:?}"
        );
    }

    #[test]
    fn corpus_dir_only_on_disk_does_not_count() {
        // A file that exists but is not in the index is invisible, as in CI.
        let fx = Fixture::new("structure-untracked-corpus");
        healthy(&fx);
        fx.write(
            "fuzz/Cargo.toml",
            "[package]\nname = \"px-fuzz\"\n[[bin]]\nname = \"ondisk\"\n",
        )
        .commit_all();
        fx.write("fuzz/corpus/ondisk/seed", "untracked");
        let g = run_on(&fx);
        assert!(
            g.failures()
                .iter()
                .any(|m| m.contains("fuzz target ondisk"))
        );
    }
}
