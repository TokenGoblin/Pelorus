//! `cargo xtask build` -- gate check 1: the workspace builds on both platforms
//! (Phase 0 gate). Port of `ci/gate-build.sh`.
//!
//! `--locked`, always: a build that silently updates Cargo.lock is not the build
//! the reproducibility gate hashed.
//!
//! Three steps, each reported separately and none short-circuiting the others:
//! release build, clippy with warnings denied, `fmt --check`. The pinned Python
//! (ADR 023) reaches the first two through `proc::cargo`.

use crate::Gate;
use crate::proc;
use std::path::Path;

/// The reason the build cannot even start, if there is one. Pure so it can be
/// tested against a fixture tree.
pub fn preflight(root: &Path) -> Option<&'static str> {
    if !root.join("Cargo.toml").is_file() {
        return Some("no workspace Cargo.toml");
    }
    if !root.join("Cargo.lock").is_file() {
        return Some(
            "no committed Cargo.lock \u{2014} --locked cannot be honoured (build-spec \u{a7}5)",
        );
    }
    None
}

/// The cargo invocations and the failure each reports, in order.
pub const STEPS: &[(&[&str], &str)] = &[
    (
        &["build", "--workspace", "--locked", "--release"],
        "release build failed",
    ),
    (
        &[
            "clippy",
            "--workspace",
            "--locked",
            "--all-targets",
            "--",
            "-D",
            "warnings",
        ],
        "clippy failed",
    ),
    (&["fmt", "--all", "--check"], "formatting is not canonical"),
];

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let prog = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    if proc::capture(&prog, &["--version"], Some(root)).is_err() {
        g.fail("cargo is not on PATH");
        return;
    }
    if let Some(why) = preflight(root) {
        g.fail(why);
        return;
    }
    for (args, failure) in STEPS {
        g.info(format!("cargo {}", args.join(" ")));
        match proc::cargo(root, args) {
            Ok(s) if s.success() => {}
            _ => g.fail(*failure),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn a_tree_without_a_manifest_cannot_build() {
        let f = Fixture::new("build-nomanifest");
        assert_eq!(preflight(f.path()), Some("no workspace Cargo.toml"));
    }

    #[test]
    fn locked_needs_a_committed_lockfile() {
        let f = Fixture::new("build-nolock");
        f.write("Cargo.toml", "[workspace]\n");
        assert!(
            preflight(f.path())
                .unwrap()
                .starts_with("no committed Cargo.lock")
        );
        f.write("Cargo.lock", "");
        assert_eq!(preflight(f.path()), None);
    }

    #[test]
    fn the_steps_are_the_scripts_and_all_locked_where_they_resolve() {
        let joined: Vec<String> = STEPS.iter().map(|(a, _)| a.join(" ")).collect();
        assert_eq!(
            joined,
            [
                "build --workspace --locked --release",
                "clippy --workspace --locked --all-targets -- -D warnings",
                "fmt --all --check"
            ]
        );
    }

    #[test]
    fn a_broken_tree_fails_before_any_cargo_runs() {
        let f = Fixture::new("build-broken");
        let mut g = Gate::new("build");
        run(&mut g, f.path(), &[]);
        assert_eq!(g.failures(), ["no workspace Cargo.toml"]);
    }
}
