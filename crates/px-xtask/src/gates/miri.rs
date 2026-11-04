//! `cargo xtask miri` -- Miri over px-dom (build-spec §4.5, ADR 022). Port of
//! `ci/gate-miri.sh`.
//!
//! §4.5: "Miri on px-dom, px-ipc, px-store unit tests (pure-Rust paths only --
//! Miri cannot run the FFI ones, and that limitation is documented rather than
//! papered over)." This covers one of the three; px-ipc and px-store are in
//! docs/backlog.md.
//!
//! px-dom is `#![forbid(unsafe_code)]`, so Miri finds nothing in its own code.
//! What it exercises is the closure ADR 017 brought in -- parking_lot 539
//! unsafe tokens, tendril 129, smallvec 75, string_cache 19 -- which the
//! unsafe audit counts and nothing else runs.
//!
//! ADR 022 lists which suites are included and why the rest are not: Miri
//! interprets at roughly a thousandfold slowdown, so anything sized for a fuzz
//! harness or a conformance corpus is out of reach.
//!
//! Needs the pinned nightly with the `miri` component, so it cannot run in a
//! plain stable environment; it says so and fails.

use crate::Gate;
use crate::proc;
use std::path::Path;

/// The three `parse` tests skipped build 100,000- and 1,000,000-node
/// documents. Under Miri that is not slow, it is unfinishable.
pub const SKIP: &[&str] = &[
    "bounds_a_nesting_bomb",
    "large_but_shallow",
    "keeps_deep_but_legal",
];

pub const SUITES: &[&str] = &["parse", "ranges", "handles", "snapshots", "order"];

/// `grep -oE 'nightly-[0-9]{4}-[0-9]{2}-[0-9]{2}' | head -1`: the first
/// pinned-nightly name anywhere in the file.
pub fn parse_nightly(text: &str) -> Option<String> {
    let b = text.as_bytes();
    let digits = |s: &[u8]| s.iter().all(u8::is_ascii_digit);
    let mut i = 0;
    while let Some(off) = text.get(i..)?.find("nightly-") {
        let start = i + off;
        let d = start + "nightly-".len();
        // YYYY-MM-DD
        if let Some(date) = b.get(d..d + 10)
            && digits(&date[0..4])
            && date[4] == b'-'
            && digits(&date[5..7])
            && date[7] == b'-'
            && digits(&date[8..10])
        {
            return Some(text[start..d + 10].to_string());
        }
        i = start + 1;
    }
    None
}

/// The suites ADR 022 lists that have no test file under `crates/px-dom/tests`.
pub fn missing_suites(root: &Path) -> Vec<&'static str> {
    SUITES
        .iter()
        .copied()
        .filter(|s| !root.join(format!("crates/px-dom/tests/{s}.rs")).is_file())
        .collect()
}

/// Arguments after `cargo` for one suite.
pub fn miri_args(nightly: &str, suite: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        &format!("+{nightly}"),
        "miri",
        "test",
        "-p",
        "px-dom",
        "--locked",
        "--test",
        suite,
        "--",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    for s in SKIP {
        a.push("--skip".into());
        a.push((*s).into());
    }
    a
}

/// `cargo +<toolchain> ...` only works through the rustup proxy found on
/// `PATH`; `$CARGO` (which `cargo xtask` sets) is the stable toolchain's real
/// binary and does not understand `+toolchain`.
fn run_rustup_cargo(root: &Path, args: &[String]) -> bool {
    std::process::Command::new("cargo")
        .args(args)
        .current_dir(root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    // The nightly ADR 006 pins, by exact name, invoked explicitly -- the same
    // construction ADR 011 used for the sanitizers. The root toolchain file is
    // not touched, so `fuzz-smoke`'s assertion that nothing outside fuzz/
    // resolves to nightly keeps holding.
    let nightly = std::fs::read_to_string(root.join("fuzz/rust-toolchain.toml"))
        .ok()
        .and_then(|t| parse_nightly(&t));
    let Some(nightly) = nightly else {
        g.fail("cannot read the pinned nightly from fuzz/rust-toolchain.toml");
        return;
    };
    g.info(format!(
        "using {nightly} (pinned by ADR 006, invoked per ADR 011)"
    ));

    if proc::capture(
        "rustup",
        &["run", &nightly, "cargo", "miri", "--version"],
        None,
    )
    .is_err()
    {
        g.fail(format!("miri is not installed for {nightly}"));
        g.fail(format!("  rustup component add miri --toolchain {nightly}"));
        return;
    }

    // Presence first, and separately, so a missing suite is reported as a
    // missing suite. Running the loop first meant a deleted file failed with
    // "miri found undefined behaviour in snapshots", which is untrue and sends
    // somebody looking in the wrong place -- found by deleting one and reading
    // what the gate said.
    //
    // It is also the non-vacuity guard. A Miri job whose suite list has shrunk
    // is the "green job that inspects nothing" the Phase 0 backlog entry
    // warned about, arrived at by attrition rather than by decision.
    let missing = missing_suites(root);
    for s in &missing {
        g.fail(format!(
            "Miri suite '{s}' does not exist; ADR 022 lists five"
        ));
    }
    if missing.is_empty() {
        g.ok("all five Miri suites are present");
    }

    for suite in SUITES {
        if missing.contains(suite) {
            continue;
        }
        g.info(format!("miri: {suite}"));
        let args = miri_args(&nightly, suite);
        match run_rustup_cargo(root, &args) {
            true => g.ok(format!("miri {suite}")),
            // Could be undefined behaviour, could be an ordinary assertion:
            // Miri runs the tests as well as interpreting them. The gate says
            // what it knows rather than guessing which.
            false => g.fail(format!(
                "miri run failed for {suite} (undefined behaviour, or a failing test)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn nightly_is_the_first_dated_match() {
        let t = "[toolchain]\n# was nightly-2024-01-01 once\nchannel = \"nightly-2025-06-30\"\n";
        assert_eq!(parse_nightly(t).as_deref(), Some("nightly-2024-01-01"));
        assert_eq!(
            parse_nightly("channel = \"nightly-2025-06-30\"").as_deref(),
            Some("nightly-2025-06-30")
        );
        // Not a date: skipped, a later one is found.
        assert_eq!(
            parse_nightly("nightly-latest nightly-2025-1-30 nightly-2026-01-02x").as_deref(),
            Some("nightly-2026-01-02")
        );
        assert_eq!(parse_nightly("channel = \"nightly\""), None);
        assert_eq!(parse_nightly("nightly-2025-06-3"), None);
        assert_eq!(parse_nightly(""), None);
    }

    #[test]
    fn committed_pin_parses() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/rust-toolchain.toml");
        if let Ok(t) = std::fs::read_to_string(p) {
            assert!(parse_nightly(&t).is_some());
        }
    }

    #[test]
    fn reports_exactly_the_missing_suites() {
        let f = Fixture::new("miri");
        assert_eq!(missing_suites(f.path()), SUITES);
        for s in SUITES {
            f.write(&format!("crates/px-dom/tests/{s}.rs"), "");
        }
        assert!(missing_suites(f.path()).is_empty());
        f.remove("crates/px-dom/tests/snapshots.rs");
        assert_eq!(missing_suites(f.path()), ["snapshots"]);
    }

    #[test]
    fn command_line() {
        assert_eq!(
            miri_args("nightly-2025-06-30", "order").join(" "),
            "+nightly-2025-06-30 miri test -p px-dom --locked --test order -- \
             --skip bounds_a_nesting_bomb --skip large_but_shallow --skip keeps_deep_but_legal"
        );
    }

    #[test]
    fn unreadable_pin_fails_closed() {
        let f = Fixture::new("miri-nopin");
        let mut g = Gate::new("miri");
        run(&mut g, f.path(), &[]);
        assert_eq!(
            g.failures(),
            ["cannot read the pinned nightly from fuzz/rust-toolchain.toml"]
        );
    }
}
