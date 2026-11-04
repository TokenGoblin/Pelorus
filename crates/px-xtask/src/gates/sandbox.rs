//! `cargo xtask sandbox` -- the Phase 2 gate (build-spec section 9). Port of
//! `ci/gate-sandbox.sh`.
//!
//! ```text
//! content process launches under a policy on both OSes   -> here
//! with the sandbox forced unavailable, the browser        -> here
//!   REFUSES to launch content processes and says why
//! Linux fallback ladder documented and exercised (§14.5)  -> here
//! ```
//!
//! The second is the one built first: it is the check that fails closed, and
//! the one a real user on a restricted kernel actually meets.
//!
//! Needs a machine that can sandbox (seccomp and `no_new_privs` on Linux, a
//! restricted token and job object on Windows): with `PX_REQUIRE_SANDBOX=1` the
//! policy tests fail rather than skip when it cannot, which is the point.

use super::suites::{self, Suite};
use crate::Gate;
use crate::proc;
use std::path::Path;
use std::process::Command;

/// Gate items 1 and 3 as named tests, with the same three conditions the
/// Phase 1 gate learned to apply (see `suites`).
pub const SUITES: &[Suite] = &[
    (
        "policy-applied",
        "sandbox_policy",
        "crates/px-sandbox/src/lib.rs",
    ),
    (
        "refuses-below-floor",
        "sandbox_refuses",
        "crates/px-content/tests/sandbox.rs",
    ),
    ("ladder", "sandbox_ladder", "crates/px-sandbox/src/lib.rs"),
];

/// A machine below the floor makes the policy tests skip, and a silent skip is
/// indistinguishable from a pass -- the failure that has cost this project more
/// than any other. Under `PX_REQUIRE_SANDBOX` those skips become failures, so a
/// CI runner that cannot sandbox reports that instead of a green gate. (The
/// script exported it for everything after the suites; so does this.)
pub const REQUIRE_ENV: (&str, &str) = ("PX_REQUIRE_SANDBOX", "1");

/// The override that exists only under `--features testing`. See the
/// release-artifact scan.
pub const OVERRIDE_ENV: &str = "PX_TEST_FORCE_SANDBOX_UNAVAILABLE";

/// The mechanisms a refusal must be able to name (section 14.5 requires the
/// message to say what is missing). Matched case-insensitively.
pub const MECHANISMS: &[&str] = &["seccomp", "no_new_privs", "restricted token", "job object"];

/// ADR 007's ladder has to be written down, not just implemented. A rung nobody
/// documented is a rung nobody can tell you lost.
pub const LADDER_DOC: &str = "docs/adr/007-linux-sandbox-ladder.md";
pub const RUNGS: &[&str] = &["no_new_privs", "seccomp", "Landlock", "user namespace"];

/// `grep -qiE 'seccomp|no_new_privs|restricted token|job object'`.
pub fn names_mechanism(out: &str) -> bool {
    let l = out.to_lowercase();
    MECHANISMS.iter().any(|m| l.contains(m))
}

/// `grep -qi 'px-content'`.
pub fn mentions_content_process(out: &str) -> bool {
    out.to_lowercase().contains("px-content")
}

/// Rungs of the ladder (in order) that the document does not mention,
/// case-insensitively.
pub fn missing_rungs<'a>(doc: &str, rungs: &[&'a str]) -> Vec<&'a str> {
    let l = doc.to_lowercase();
    rungs
        .iter()
        .copied()
        .filter(|r| !l.contains(&r.to_lowercase()))
        .collect()
}

/// `grep -a -q needle` over a binary: a plain byte search.
pub fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// What the browser did when run with the sandbox forced unavailable.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// Exited zero: it started anyway (invariant 8 says it must refuse).
    Started,
    /// Refused, and named the missing mechanism.
    Named,
    /// Refused but did not say why.
    Silent,
}

pub fn judge_refusal(succeeded: bool, out: &str) -> Refusal {
    if succeeded {
        Refusal::Started
    } else if names_mechanism(out) {
        Refusal::Named
    } else {
        Refusal::Silent
    }
}

/// Run `cargo args` in `root` with inherited stdio and `PX_REQUIRE_SANDBOX=1`.
fn cargo_required(root: &Path, args: &[&str]) -> bool {
    proc::cargo_command(root)
        .args(args)
        .env(REQUIRE_ENV.0, REQUIRE_ENV.1)
        .status()
        .is_ok_and(|s| s.success())
}

/// Run the browser with the override set, stdout and stderr merged
/// (`out="$(... 2>&1)"`), returning whether it exited zero and its output with
/// trailing newlines stripped. A browser that cannot be started counts as a
/// non-zero exit whose output is the error, as the shell reported it.
fn run_refusal(browser: &Path, root: &Path) -> (bool, String) {
    let mut c = Command::new(browser);
    c.current_dir(root)
        .env(REQUIRE_ENV.0, REQUIRE_ENV.1)
        .env(OVERRIDE_ENV, "1");
    match proc::capture_merged_cmd(c) {
        Ok((out, ok)) => (ok, out.trim_end_matches('\n').to_string()),
        Err(e) => (false, e),
    }
}

/// Gate item 2, end to end: the browser must REFUSE, not degrade.
///
/// Phase 1 learned that a library test is not the product. px-browser exited
/// FAILURE on a clean run for a week's worth of green checks because nothing
/// executed it. So this runs the binary. `PX_TEST_FORCE_SANDBOX_UNAVAILABLE`
/// only exists under `--features testing`.
fn check_refusal(g: &mut Gate, root: &Path) {
    g.info("px-browser refuses to launch when the sandbox is below the floor");
    if !cargo_required(
        root,
        &[
            "build",
            "--workspace",
            "--locked",
            "--features",
            "testing",
            "--quiet",
        ],
    ) {
        g.fail("testing-feature build failed; cannot run the refusal check");
        return;
    }
    let browser = proc::built_binary(root, "debug", "px-browser");
    if !browser.is_file() {
        g.fail("px-browser was not built; the refusal check cannot run");
        return;
    }
    let (succeeded, out) = run_refusal(&browser, root);
    match judge_refusal(succeeded, &out) {
        Refusal::Started => {
            g.fail("px-browser started with the sandbox unavailable; invariant 8");
            g.fail("  says it must refuse");
        }
        Refusal::Named => g.ok("px-browser refused and named the missing mechanism"),
        Refusal::Silent => {
            g.fail("px-browser refused but did not say why; §14.5 requires the");
            g.fail(format!("  message to name what is missing. Got: {out}"));
        }
    }
    // Refusing must mean no content process exists, not one that started and
    // was killed afterwards.
    if mentions_content_process(&out) {
        g.info("note: output mentions px-content; check no process was spawned");
    }
}

/// Section 14.4: the forced-unavailable override is a test-only capability, and
/// a test-only capability in a release build is a critical vulnerability. A
/// flag being off is not evidence; absence from the artifact is.
///
/// This scan was filed in `docs/backlog.md` as a Phase 8 and 12 prerequisite.
/// Phase 2 pulls it forward, because Phase 2 is the phase that introduces the
/// first test-only capability.
fn check_release_artifacts(g: &mut Gate, root: &Path) {
    g.info("release artifacts carry no test-only capability");
    if !cargo_required(
        root,
        &["build", "--workspace", "--locked", "--release", "--quiet"],
    ) {
        g.fail("release build failed; cannot scan artifacts");
        return;
    }
    let mut leaked = false;
    for stem in ["px-browser", "px-content"] {
        let bin = proc::built_binary(root, "release", stem);
        let Ok(bytes) = std::fs::read(&bin) else {
            g.fail(format!("{stem} was not built for release"));
            continue;
        };
        if contains_bytes(&bytes, OVERRIDE_ENV.as_bytes()) {
            g.fail(format!("{stem} contains the sandbox override symbol"));
            leaked = true;
        }
    }
    // As in the script, printed whenever nothing leaked, even if a binary was
    // missing (that case has its own failure above).
    if !leaked {
        g.ok("no test-only sandbox override in release binaries");
    }
}

fn check_ladder_doc(g: &mut Gate, root: &Path) {
    let Ok(bytes) = std::fs::read(root.join(LADDER_DOC)) else {
        g.fail(format!(
            "{LADDER_DOC} does not exist; §14.5 requires the ladder documented"
        ));
        return;
    };
    let missing = missing_rungs(&String::from_utf8_lossy(&bytes), RUNGS);
    if missing.is_empty() {
        g.ok("the ladder is documented rung by rung");
    } else {
        g.fail(format!(
            "{LADDER_DOC} does not document: {}",
            missing.join(" ")
        ));
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }
    suites::check_suites(g, root, SUITES);

    g.info("cargo test --workspace --locked --features testing (PX_REQUIRE_SANDBOX=1)");
    if !cargo_required(
        root,
        &["test", "--workspace", "--locked", "--features", "testing"],
    ) {
        g.fail("tests failed");
    }

    check_refusal(g, root);
    check_release_artifacts(g, root);
    check_ladder_doc(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn gate_of(f: fn(&mut Gate, &Path), fx: &Fixture) -> Gate {
        let mut g = Gate::new("sandbox");
        f(&mut g, fx.path());
        g
    }

    #[test]
    fn refusal_judging() {
        assert_eq!(judge_refusal(true, "seccomp missing"), Refusal::Started);
        assert_eq!(
            judge_refusal(false, "refusing: no_new_privs unavailable"),
            Refusal::Named
        );
        assert_eq!(
            judge_refusal(false, "Restricted Token failed"),
            Refusal::Named
        );
        assert_eq!(judge_refusal(false, "a JOB OBJECT error"), Refusal::Named);
        assert_eq!(judge_refusal(false, "something broke"), Refusal::Silent);
        assert_eq!(judge_refusal(false, ""), Refusal::Silent);
    }

    #[test]
    fn content_process_note() {
        assert!(mentions_content_process("spawning PX-Content"));
        assert!(!mentions_content_process("px content"));
    }

    #[test]
    fn rungs_are_matched_case_insensitively_and_listed_in_order() {
        let doc = "We use NO_NEW_PRIVS then Seccomp.";
        assert_eq!(missing_rungs(doc, RUNGS), ["Landlock", "user namespace"]);
        assert!(missing_rungs("landlock USER NAMESPACE no_new_privs seccomp", RUNGS).is_empty());
        // The phrase must be on one line: a newline is not a space.
        assert_eq!(
            missing_rungs("user\nnamespace", &["user namespace"]),
            ["user namespace"]
        );
    }

    #[test]
    fn byte_search() {
        assert!(contains_bytes(b"\0\0PX_TEST_FORCE\0", b"PX_TEST_FORCE"));
        assert!(!contains_bytes(b"PX_TEST_FORC", b"PX_TEST_FORCE"));
        assert!(!contains_bytes(b"abc", b""));
        assert!(!contains_bytes(b"", b"a"));
    }

    #[test]
    fn ladder_doc_gate() {
        let fx = Fixture::new("sb-ladder");
        assert_eq!(gate_of(check_ladder_doc, &fx).fail_count(), 1);
        fx.write(LADDER_DOC, "no_new_privs and seccomp");
        let g = gate_of(check_ladder_doc, &fx);
        assert_eq!(
            g.failures(),
            ["docs/adr/007-linux-sandbox-ladder.md does not document: Landlock user namespace"]
        );
        fx.write(LADDER_DOC, "no_new_privs seccomp landlock user namespace");
        assert!(gate_of(check_ladder_doc, &fx).passed());
    }
}
