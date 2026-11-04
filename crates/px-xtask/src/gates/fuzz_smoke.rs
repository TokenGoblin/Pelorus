//! `cargo xtask fuzz-smoke` -- fuzz smoke, on every push (ADR 006). Port of
//! `ci/gate-fuzz-smoke.sh`.
//!
//! 60 seconds per target against the committed corpus. This is not the 24h
//! campaign Phase 1's gate requires -- that runs on a schedule and reports; see
//! `.github/workflows/fuzz-campaign.yml`. What this catches is a regression of
//! a crash the campaign already found, in the time a reviewer will actually
//! wait.
//!
//! Every crash the campaign finds gets its input committed to the corpus,
//! which is what makes 60 seconds meaningful rather than decorative.
//!
//! Environment (unchanged from the script):
//!
//! * `FUZZ_SECONDS`  seconds per target, default 60
//! * `FUZZ_TARGET`   restrict the run to one target (the campaign uses it to
//!   shard across jobs, because a GitHub-hosted job is capped at six hours and
//!   a 24-hour campaign therefore cannot be one job)
//! * `FUZZ_MAX_LEN`  override the per-target `-max_len`
//!
//! `cargo-fuzz` and the pinned nightly are needed; `cargo fuzz` runs as a child
//! of the *rustup proxy* `cargo` on `PATH`, with `CARGO` and `RUSTUP_TOOLCHAIN`
//! removed from its environment. Both are set by `cargo xtask` for its own
//! (stable) toolchain, and either would defeat the directory-scoped nightly
//! this gate exists to assert.

use super::supply_chain::find_in_path;
use crate::Gate;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `FUZZ_SECONDS` default.
const DEFAULT_SECONDS: &str = "60";

/// The largest input a target should be given, per target.
///
/// `-max_len` is not optional. libFuzzer's default is 4096 bytes, which puts
/// the entire large-payload path -- the only code in recv that allocates, and
/// the MAX_MESSAGE_BYTES boundary this gate names -- out of reach. A 24-hour
/// campaign ran without it and reported clean while never testing what it was
/// meant to.
///
/// And one value does not fit every target, which the Phase 3 campaign
/// showed. 1,100,000 straddles px-ipc's MAX_MESSAGE_BYTES of 1,048,576 exactly
/// as intended, and comes nowhere near px-net's MAX_BODY_BYTES of 32 MiB or
/// MAX_CHUNK_BYTES of 8 MiB -- so the HTTP targets reported clean for inputs
/// up to 1.1 MB while their own size boundaries went unfuzzed.
///
/// Each value straddles the limit its target actually has. The HTTP figure is
/// deliberately just past MAX_CHUNK_BYTES rather than past MAX_BODY_BYTES: a
/// 32 MiB ceiling would spend the whole budget generating enormous inputs
/// instead of exploring structure, and the chunk bound is the one a declared
/// length reaches directly rather than by accumulation.
///
/// The DOM targets go the other way, and for a different reason. Their
/// harnesses assert the whole tree's invariants after *every* operation, and
/// one byte is roughly one operation, so cost grows with the square of the
/// input. Measured, release: 1 KB is 0.4 ms, 4 KB is 2.9 ms, 16 KB is 20 ms,
/// 64 KB is 170 ms. At the 1,100,000 default a single execution would take
/// minutes and the campaign would explore almost nothing.
///
/// 4 KB is about two thousand operations -- enough to build real trees, churn
/// slots, and reach the forced generation-exhaustion path -- at roughly 350
/// executions per second. The check-everything-every-step design is what makes
/// these targets precise, and this is what it costs.
///
/// The parser goes the other way again: its input is HTML, and the committed
/// corpus holds a 1.1 MB seed -- 100,000 levels of nesting opened and closed,
/// which §4.4 asks for by name. A smaller `-max_len` would truncate it and
/// quietly drop the coverage the seed exists for.
///
/// Affordable because `parse` abandons a nesting bomb after eight refusals:
/// measured on release, the 1.1 MB nesting seed costs 75 ms and a 1 MB shallow
/// document 104 ms, against 6 ms at 64 KB. Large inputs are the cheap ones
/// here, which is the opposite of the two arena targets above.
pub fn max_len_for(target: &str) -> &'static str {
    match target {
        "http_response" | "http_chunked" => "8500000",
        "dom_stale_handle" | "dom_mutation" => "4096",
        "dom_parse" => "1200000",
        _ => "1100000",
    }
}

/// The toolchain name in `rustup show active-toolchain` output: first field of
/// the last line (`tail -1 | cut -d' ' -f1`). `rustup show active-toolchain`
/// prints installation progress and warnings on the first call for a toolchain
/// that is not yet downloaded, so take the last line rather than the first.
pub fn active_toolchain(stdout: &str) -> String {
    stdout
        .lines()
        .next_back()
        .and_then(|l| l.split(' ').next())
        .unwrap_or("")
        .to_string()
}

/// What the ADR 006 fence assertion concludes. `Fail` carries the message.
#[derive(Debug, PartialEq, Eq)]
pub enum Fence {
    Ok(&'static str),
    Fail(&'static str),
}

/// ADR 006: the nightly is fenced to fuzz/ by a directory-scoped
/// rust-toolchain.toml. Assert the fence rather than trusting it -- the whole
/// point of pinning stable is that nothing outside this directory drifts.
pub fn fence(root_tc: &str, fuzz_tc: &str) -> Vec<Fence> {
    let mut v = Vec::new();
    if root_tc.starts_with("nightly") {
        v.push(Fence::Fail(
            "the repository root resolves to a nightly toolchain; ADR 006 fences nightly to fuzz/",
        ));
    }
    if fuzz_tc.starts_with("nightly") {
        v.push(Fence::Ok("nightly is scoped to fuzz/"));
    } else {
        v.push(Fence::Fail(
            "fuzz/ does not resolve to a nightly toolchain; cargo-fuzz needs one",
        ));
    }
    v
}

/// Apply `FUZZ_TARGET` to the `cargo fuzz list` output.
///
/// Matched with an explicit comparison over whitespace-split names. The
/// obvious `case " $targets " in *" $t "*` does NOT work: `cargo fuzz list` is
/// newline-separated, so no name is ever surrounded by spaces and every shard
/// fails with "not a known target". That shipped, and the first real campaign
/// died in twelve seconds instead of running for four hours.
pub fn select_targets(listed: &str, only: Option<&str>) -> Result<Vec<String>, String> {
    let all: Vec<String> = listed.split_whitespace().map(str::to_string).collect();
    match only {
        Some(want) if !want.is_empty() => {
            if all.iter().any(|t| t == want) {
                Ok(vec![want.to_string()])
            } else {
                Err(format!(
                    "FUZZ_TARGET={want} is not a known target; have: {}",
                    all.join(" ")
                ))
            }
        }
        _ => Ok(all),
    }
}

/// libFuzzer arguments after `--`.
pub fn fuzz_args(target: &str, seconds: &str, max_len: &str) -> Vec<String> {
    // -rss_limit_mb bounds the fuzzer itself: an OOM in the harness is not a
    // finding about the code under test.
    vec![
        "fuzz".into(),
        "run".into(),
        target.into(),
        "--".into(),
        format!("-max_total_time={seconds}"),
        format!("-max_len={max_len}"),
        "-rss_limit_mb=2048".into(),
        "-print_final_stats=1".into(),
    ]
}

/// A reproducer file under `fuzz/artifacts/<target>`, if libFuzzer wrote one
/// (`find -type f | head -1`; sorted here so the answer is stable).
pub fn find_reproducer(artifacts: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.is_file() {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(artifacts, &mut v);
    v.sort();
    v.into_iter().next()
}

fn clean(prog: &str, cwd: &Path) -> Command {
    let mut c = Command::new(prog);
    c.current_dir(cwd)
        .env_remove("CARGO")
        .env_remove("RUSTUP_TOOLCHAIN");
    c
}

/// `rustup show active-toolchain` in `cwd`; empty on any failure (`|| true`).
fn toolchain_in(cwd: &Path) -> String {
    clean("rustup", cwd)
        .args(["show", "active-toolchain"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| active_toolchain(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    let seconds = env_nonempty("FUZZ_SECONDS").unwrap_or_else(|| DEFAULT_SECONDS.to_string());

    let fuzz = root.join("fuzz");
    if !fuzz.is_dir() {
        g.fail("fuzz/ does not exist");
        return;
    }

    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if find_in_path("cargo-fuzz", &dirs).is_none() {
        g.fail("cargo-fuzz is not installed (cargo install --locked cargo-fuzz)");
        return;
    }

    let root_tc = toolchain_in(root);
    let fuzz_tc = toolchain_in(&fuzz);
    g.info(format!("toolchain at repository root: {root_tc}"));
    g.info(format!("toolchain inside fuzz/:      {fuzz_tc}"));
    for f in fence(&root_tc, &fuzz_tc) {
        match f {
            Fence::Ok(m) => g.ok(m),
            Fence::Fail(m) => g.fail(m),
        }
    }

    let listed = clean("cargo", &fuzz)
        .args(["fuzz", "list"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map(|o| {
            if o.status.success() {
                String::from_utf8_lossy(&o.stdout).into_owned()
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
    let targets = match select_targets(&listed, env_nonempty("FUZZ_TARGET").as_deref()) {
        Ok(t) => t,
        Err(e) => {
            g.fail(e);
            return;
        }
    };
    if targets.is_empty() {
        g.fail("no fuzz targets defined");
        return;
    }

    for target in &targets {
        let max_len = env_nonempty("FUZZ_MAX_LEN").unwrap_or_else(|| max_len_for(target).into());
        g.info(format!(
            "fuzzing {target} for {seconds}s (-max_len={max_len})"
        ));
        let ok = clean("cargo", &fuzz)
            .args(fuzz_args(target, &seconds, &max_len))
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if ok {
            g.ok(target);
            continue;
        }
        // A non-zero exit is not the same as a crash, and saying so matters:
        // libFuzzer writes a reproducer when it crashes the target and writes
        // nothing when it could not start. Reporting the second as the first
        // sends somebody hunting for a file that does not exist.
        //
        // Both happen. On a Windows machine without the MSVC ASAN runtime every
        // target exits 0xc0000135 (STATUS_DLL_NOT_FOUND) before executing a
        // single input, and the script reported nine crashes and nine
        // reproducer paths, all of them empty directories.
        match find_reproducer(&fuzz.join("artifacts").join(target)) {
            Some(p) => {
                // The script printed the path relative to the repository root.
                let shown = p.strip_prefix(root).unwrap_or(&p).display().to_string();
                g.fail(format!(
                    "{target} found a crash; the reproducer is {}",
                    shown.replace('\\', "/")
                ))
            }
            None => {
                // One `fail`, then `info` for the explanation: `fail`
                // increments the count the verdict reports, and four lines
                // about one problem would read as four problems.
                g.fail(format!(
                    "{target} did not run to completion and wrote no reproducer"
                ));
                g.info("so this is a harness or toolchain failure, not a finding.");
                g.info("On Windows without the MSVC ASAN runtime every target exits");
                g.info("0xc0000135 here; ADR 011 covers the sanitizer toolchain.");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn max_len_table() {
        for t in ["http_response", "http_chunked"] {
            assert_eq!(max_len_for(t), "8500000");
        }
        for t in ["dom_stale_handle", "dom_mutation"] {
            assert_eq!(max_len_for(t), "4096");
        }
        assert_eq!(max_len_for("dom_parse"), "1200000");
        assert_eq!(max_len_for("ipc_decode"), "1100000");
        assert_eq!(max_len_for(""), "1100000");
    }

    #[test]
    fn toolchain_is_first_field_of_last_line() {
        assert_eq!(
            active_toolchain(
                "info: syncing channel updates\nnightly-2026-09-01-x86_64-unknown-linux-gnu (overridden by '/r/fuzz/rust-toolchain.toml')\n"
            ),
            "nightly-2026-09-01-x86_64-unknown-linux-gnu"
        );
        assert_eq!(active_toolchain("stable-x86_64 (default)"), "stable-x86_64");
        assert_eq!(active_toolchain(""), "");
    }

    #[test]
    fn fence_cases() {
        let ok = "nightly is scoped to fuzz/";
        assert_eq!(fence("stable-x", "nightly-2026-x"), [Fence::Ok(ok)]);
        let leak = fence("nightly-2026", "nightly-2026");
        assert!(matches!(leak[0], Fence::Fail(m) if m.contains("repository root")));
        assert_eq!(leak[1], Fence::Ok(ok));
        let none = fence("stable", "stable");
        assert_eq!(none.len(), 1);
        assert!(matches!(none[0], Fence::Fail(m) if m.contains("fuzz/ does not resolve")));
        // Unreadable (empty) toolchains fail closed.
        assert!(matches!(fence("", "")[0], Fence::Fail(_)));
    }

    #[test]
    fn target_selection_handles_newline_separated_lists() {
        let listed = "dom_mutation\ndom_parse\nhttp_chunked\n";
        // The bug the script's comment records: names are separated by
        // newlines, never spaces.
        assert_eq!(
            select_targets(listed, Some("dom_parse")).unwrap(),
            ["dom_parse"]
        );
        assert_eq!(select_targets(listed, None).unwrap().len(), 3);
        assert_eq!(select_targets(listed, Some("")).unwrap().len(), 3);
        // Exact names only, not prefixes or substrings.
        assert_eq!(
            select_targets(listed, Some("dom")).unwrap_err(),
            "FUZZ_TARGET=dom is not a known target; have: dom_mutation dom_parse http_chunked"
        );
        assert!(select_targets("", None).unwrap().is_empty());
    }

    #[test]
    fn libfuzzer_command_line() {
        assert_eq!(
            fuzz_args("dom_parse", "60", "1200000").join(" "),
            "fuzz run dom_parse -- -max_total_time=60 -max_len=1200000 -rss_limit_mb=2048 -print_final_stats=1"
        );
    }

    #[test]
    fn reproducer_lookup() {
        let f = Fixture::new("fuzz");
        let dir = f.path().join("fuzz/artifacts/t");
        assert!(find_reproducer(&dir).is_none());
        f.write("fuzz/artifacts/t/crash-b", "");
        f.write("fuzz/artifacts/t/sub/crash-a", "");
        std::fs::create_dir_all(f.path().join("fuzz/artifacts/t/emptydir")).unwrap();
        assert_eq!(find_reproducer(&dir).unwrap(), dir.join("crash-b"));
        // A directory alone is not a reproducer (the Windows failure mode).
        let g = Fixture::new("fuzz-empty");
        std::fs::create_dir_all(g.path().join("a/b")).unwrap();
        assert!(find_reproducer(&g.path().join("a")).is_none());
    }

    #[test]
    fn missing_fuzz_dir_fails_closed() {
        let f = Fixture::new("fuzz-none");
        let mut g = Gate::new("fuzz-smoke");
        run(&mut g, f.path(), &[]);
        assert_eq!(g.failures(), ["fuzz/ does not exist"]);
    }
}
