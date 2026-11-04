#![forbid(unsafe_code)]
//! Repository gates and developer tooling, in Rust (ADR 032).
//!
//! Replaces `ci/lib.sh` and the gate scripts built on it. The contract is the
//! one the scripts had: `ok`/`info`/`fail` lines, a final `<gate>: PASS` or
//! `<gate>: FAIL (n)`, exit 0 or 1. A gate you cannot run on your own machine
//! is a gate you will stop trusting, so `cargo xtask <gate>` is the one way to
//! run each, locally and in CI.
//!
//! Adding a gate: write `src/gates/<name>.rs` with
//! `pub fn run(g: &mut Gate, root: &Path, args: &[String])`, add a line to
//! `gates::GATES`, add tests next to it. See `CLAUDE.md` in this crate.

pub mod clock;
pub mod gate;
pub mod gates;
pub mod git;
pub mod glob;
pub mod json;
pub mod proc;
pub mod pybuild;
pub mod sha256;
pub mod tar;

#[cfg(test)]
pub mod testutil;

use std::path::PathBuf;
use std::process::ExitCode;

pub use gate::Gate;

/// Locate the repository root (what `lib.sh` did with
/// `git rev-parse --show-toplevel; cd`). Gates receive it as a parameter
/// rather than changing the process cwd, so tests can point them elsewhere.
pub fn repo_root() -> Result<PathBuf, String> {
    let out = proc::capture("git", &["rev-parse", "--show-toplevel"], None)?;
    let s = String::from_utf8_lossy(&out).trim().to_string();
    if s.is_empty() {
        return Err("git rev-parse --show-toplevel printed nothing".into());
    }
    Ok(PathBuf::from(s))
}

fn usage() -> String {
    let mut s = String::from("usage: cargo xtask <gate> [args...]\n\ngates:\n");
    for g in gates::GATES {
        s.push_str(&format!("  {:<12} {}\n", g.name, g.about));
    }
    s.push_str("  list         print the gate names\n");
    s
}

/// Run the subcommand named by `args[0]`. Exit codes: 0 pass, 1 gate failed,
/// 2 usage or environment error (fail closed: a gate that could not run is
/// not a pass).
pub fn run(args: &[String]) -> ExitCode {
    let Some(name) = args.first() else {
        eprint!("{}", usage());
        return ExitCode::from(2);
    };
    match name.as_str() {
        "list" => {
            for g in gates::GATES {
                println!("{}", g.name);
            }
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            print!("{}", usage());
            ExitCode::SUCCESS
        }
        _ => {
            let Some(spec) = gates::find(name) else {
                eprintln!("xtask: unknown gate `{name}`\n");
                eprint!("{}", usage());
                return ExitCode::from(2);
            };
            let root = match repo_root() {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("xtask: cannot find the repository root: {e}");
                    return ExitCode::from(2);
                }
            };
            let mut g = Gate::new(spec.name);
            (spec.run)(&mut g, &root, args.get(1..).unwrap_or(&[]));
            g.verdict()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(c: ExitCode) -> String {
        format!("{c:?}")
    }

    #[test]
    fn unknown_gate_is_a_usage_error() {
        assert_eq!(code(run(&["no-such-gate".into()])), code(ExitCode::from(2)));
    }

    #[test]
    fn no_args_is_a_usage_error() {
        assert_eq!(code(run(&[])), code(ExitCode::from(2)));
    }

    #[test]
    fn every_registered_gate_has_a_unique_name() {
        let mut names: Vec<&str> = gates::GATES.iter().map(|g| g.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len());
    }
}
