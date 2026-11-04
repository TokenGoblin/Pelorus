//! `cargo xtask all` -- run the gates a contributor runs before pushing, in the
//! order CI runs them. Port of `ci/gate-all.sh`.
//!
//! "A gate you cannot run on your own machine is a gate you will learn to argue
//! with", so every entry here is the same command CI invokes, run as a child
//! process of *this* binary (never `cargo xtask`, which can pick up another
//! worktree's build when a target directory is shared).
//!
//! Deliberately not here:
//!
//! - **The reproducible-build check.** It builds the tree twice from scratch under
//!   two environments and is driven by the workflow, which varies `HOME`,
//!   `CARGO_HOME` and (on Linux) the user between runs. By hand:
//!   `cargo xtask build-and-hash /tmp/a > /tmp/a.txt`, the same into
//!   `/tmp/some/deeper/path > /tmp/b.txt`, then
//!   `cargo xtask compare-hashes /tmp/a.txt /tmp/b.txt`.
//! - **The per-phase gates for layout, flex-grid, render and style, and the nightly
//!   ones (miri, fuzz-smoke, sanitizers).** The script never listed them. They
//!   are slow, need tools a laptop may not have, or run in their own CI jobs.
//!   Add one here by adding its name to [`CHECKS`].
//!
//! `compat-list` is in the list and fails until the forty-site list is
//! supplied; that is the standing red, not a defect of this command.

use crate::Gate;
use crate::proc;
use std::path::Path;

/// Gate names, in the order the script ran them.
pub const CHECKS: &[&str] = &[
    "build",
    "supply-chain",
    "unsafe-headers",
    "brand-leak",
    "structure",
    "compat-list",
    "ipc",
    "sandbox",
    "network",
    "dom",
];

/// One gate's `PASS`/`FAIL` line, plus the indented `FAIL` lines of its output
/// when it failed (as the script printed them).
pub fn format_entry(name: &str, passed: bool, output: &str) -> Vec<String> {
    let mut lines = vec![format!(
        "{name:<16} {}",
        if passed { "PASS" } else { "FAIL" }
    )];
    if !passed {
        for l in output.lines().filter(|l| l.starts_with("FAIL")) {
            lines.push(format!("{:17}{l}", ""));
        }
    }
    lines
}

/// The closing line: `all checks pass`, or `failed:` and the failed names.
pub fn summary(failed: &[&str]) -> String {
    if failed.is_empty() {
        "all checks pass".to_string()
    } else {
        format!("failed: {}", failed.join(" "))
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            g.fail(format!("cannot locate this binary: {e}"));
            return;
        }
    };
    let exe = exe.to_string_lossy().into_owned();
    let mut failed: Vec<&str> = Vec::new();
    for name in CHECKS {
        let (output, passed) = match proc::capture_merged(&exe, &[name], Some(root)) {
            Ok(r) => r,
            Err(e) => (format!("FAIL {e}"), false),
        };
        for l in format_entry(name, passed, &output) {
            g.info(l);
        }
        if !passed {
            failed.push(name);
        }
    }
    g.info("");
    g.info(summary(&failed));
    for name in &failed {
        g.fail(format!("{name} failed"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_passing_gate_is_one_line() {
        assert_eq!(
            format_entry("build", true, "ok x\nFAIL ignored\n"),
            vec!["build            PASS".to_string()]
        );
    }

    #[test]
    fn a_failing_gate_lists_only_its_fail_lines_indented() {
        let out = "ok a\nFAIL first\n  detail\nFAIL second\n";
        let got = format_entry("dom", false, out);
        assert_eq!(got[0], "dom              FAIL");
        assert_eq!(got[1], "                 FAIL first");
        assert_eq!(got[2], "                 FAIL second");
        assert_eq!(got.len(), 3);
    }

    #[test]
    fn the_summary_names_every_failure_in_order() {
        assert_eq!(summary(&[]), "all checks pass");
        assert_eq!(summary(&["compat-list", "dom"]), "failed: compat-list dom");
    }

    #[test]
    fn every_listed_check_is_a_registered_gate() {
        for name in CHECKS {
            assert!(
                crate::gates::find(name).is_some(),
                "`{name}` is listed in `all` but is not a gate"
            );
        }
    }

    #[test]
    fn the_list_has_no_duplicates() {
        let mut seen = std::collections::HashSet::new();
        for name in CHECKS {
            assert!(seen.insert(*name), "`{name}` is listed twice");
        }
    }
}
