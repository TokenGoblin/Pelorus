//! `cargo xtask script` -- the Phase 10 gate (build-spec section 9, ADR 040,
//! `docs/phase-10-plan.md`).
//!
//! ```text
//! test262 subset threshold                       -> unit 5 (landed: script_test262_)
//! event ordering tests                           -> units 2 and 6
//! no unbounded task-queue growth, stress page    -> units 2, 4 and 6
//! timer resolution asserted                      -> unit 3
//! script runs only in the sandboxed content proc -> unit 1 (and 4 for survival)
//! ```
//!
//! Written failing in unit 0, before any engine existed. Every item is a named
//! test suite held to the shared three conditions (see `suites`): the filter
//! matches a test in `cargo test -- --list`, the home file carries the
//! `#[test]`s, and none is `#[ignore]`d. Items whose owning unit has not landed
//! are in [`REMAINING`], and are reported by unit rather than omitted: a gate
//! that silently drops what is not built yet passes for the wrong reason.
//!
//! This gate does not run the test262 runner or the stress
//! fixtures of units 5 and 6; it names them so the phase's open work is visible
//! in its own output.

use super::suites::{self, Suite};
use crate::Gate;
use crate::proc;
use std::path::Path;

/// Gate items whose tests exist once units 0 to 3 have landed.
pub const SUITES: &[Suite] = &[
    // Unit 1: the host, over a real pipe to the real content binary.
    (
        "script-eval-over-a-real-process",
        "script_eval_",
        "crates/px-content/tests/script_process.rs",
    ),
    (
        "script-wire-hostile",
        "script_hostile_",
        "crates/px-ipc/tests/hostile_script.rs",
    ),
    (
        "script-only-in-content",
        "script_confinement_",
        "crates/px-content/tests/script_confinement.rs",
    ),
    (
        "deliberately-absent-globals",
        "script_absent_",
        "crates/px-script/tests/globals.rs",
    ),
    // Unit 2: the loop, the queues, the bounds.
    (
        "content-loop-and-counters",
        "script_loop_",
        "crates/px-content/tests/script_loop.rs",
    ),
    (
        "event-ordering",
        "script_order_",
        "crates/px-script/tests/ordering.rs",
    ),
    (
        "queue-logic-pure",
        "script_queue_pure_",
        "crates/px-script/src/queue.rs",
    ),
    (
        "no-unbounded-queue-growth",
        "script_stress_",
        "crates/px-script/tests/stress.rs",
    ),
    // Unit 4: resource limits: layer 1 (the engine), the heartbeat and the
    // watchdog decision, then the runaway shapes and isolation through the real
    // content binary, and the honest memory tests.
    (
        "engine-limits-are-terminal",
        "script_limits_engine_",
        "crates/px-script/tests/limits.rs",
    ),
    (
        "watchdog-heartbeat-signal",
        "script_limits_heartbeat_",
        "crates/px-script/src/watch.rs",
    ),
    (
        "watchdog-decision-is-pure-and-tested",
        "script_limits_watchdog_",
        "crates/px-content/src/watchdog.rs",
    ),
    (
        "runaway-scripts-end-terminated-or-dead-broker-alive",
        "script_limits_runaway_",
        "crates/px-content/tests/script_limits.rs",
    ),
    (
        "two-tabs-one-runaway-the-other-answers",
        "script_limits_isolation_",
        "crates/px-content/tests/script_limits.rs",
    ),
    (
        "memory-bomb-gap-documented-not-hidden",
        "script_limits_memory_",
        "crates/px-content/tests/script_limits.rs",
    ),
    // Unit 5: the test262 runner bound to the engine (the pure rules are shared
    // with `cargo xtask test262`; the floor and the ratchet are enforced there).
    (
        "test262-subset-threshold-and-expected-failures",
        "script_test262_",
        "crates/px-script/tests/test262.rs",
    ),
    // Unit 3: the coarse clock.
    (
        "timer-resolution-properties",
        "script_clock_prop_",
        "crates/px-script/src/clock.rs",
    ),
    (
        "timer-resolution-in-the-content-process",
        "script_clock_process_",
        "crates/px-content/tests/script_clock.rs",
    ),
];

/// What is still to be built, and the unit that owns it: `(item, unit, filter,
/// home file)`. The same three conditions apply the day the file appears.
pub const REMAINING: &[(&str, &str, &str, &str)] = &[(
    "ordering-and-stress-fixture-table",
    "unit 6",
    "script_fixture_",
    "tests/script/fixtures.rs",
)];

/// Files that must exist for the phase to be closed, and the unit that writes
/// them.
pub const REQUIRED_FILES: &[(&str, &str)] = &[
    ("docs/phase-10-gate-report.md", "unit 7"),
    (
        "tests/test262/LICENSE",
        "unit 5 (vendored corpus, BSD licence)",
    ),
];

/// The line that reports an item whose unit has not landed, so the output says
/// which unit owes it rather than only that it is missing.
pub fn remaining_message(item: &str, unit: &str, why: &str) -> String {
    format!("remaining ({unit}): '{item}': {why}")
}

/// Judge one remaining suite, naming its owner when it fails.
pub fn judge_remaining(
    item: &str,
    unit: &str,
    filter: &str,
    home: &str,
    listed: &str,
    home_src: Option<&str>,
    ignored: &str,
) -> Result<String, String> {
    match suites::judge(filter, listed, home_src, ignored) {
        Ok((matched, in_home)) => Ok(format!(
            "{item}: {matched} test(s), {in_home} in {home}, none ignored"
        )),
        Err(p) => Err(remaining_message(
            item,
            unit,
            &suites::problem_message(item, filter, home, &p),
        )),
    }
}

/// Entries of `required` that `exists` does not find.
pub fn missing_files<'a>(
    required: &'a [(&'a str, &'a str)],
    exists: impl Fn(&str) -> bool,
) -> Vec<(&'a str, &'a str)> {
    required
        .iter()
        .copied()
        .filter(|(p, _)| !exists(p))
        .collect()
}

/// One item, one owner: names that appear in both tables.
pub fn duplicated_items<'a>(
    suites: &'a [Suite],
    remaining: &'a [(&'a str, &'a str, &'a str, &'a str)],
) -> Vec<&'a str> {
    remaining
        .iter()
        .map(|r| r.0)
        .filter(|name| suites.iter().any(|s| s.0 == *name))
        .collect()
}

fn check_remaining(g: &mut Gate, root: &Path) {
    let ignored = suites::list_tests(root, &["--ignored"]);
    for (item, unit, filter, home) in REMAINING {
        let listed = suites::list_tests(root, &[filter]);
        let src = std::fs::read(root.join(home))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        match judge_remaining(item, unit, filter, home, &listed, src.as_deref(), &ignored) {
            Ok(line) => g.ok(line),
            Err(line) => g.fail(line),
        }
    }
    for (path, unit) in missing_files(REQUIRED_FILES, |p| root.join(p).is_file()) {
        g.fail(remaining_message(path, unit, "file does not exist"));
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }
    let dup = duplicated_items(SUITES, REMAINING);
    if !dup.is_empty() {
        g.fail(format!(
            "items in both SUITES and REMAINING: {}",
            dup.join(" ")
        ));
    }
    suites::check_suites(g, root, SUITES);
    check_remaining(g, root);

    // The crates that carry the phase. `testing` turns on the high-water-mark
    // counters and the raw clock log (build-spec 14.4); a release build has
    // neither, which `cargo xtask sandbox` scans for.
    g.info("cargo test -p px-script -p px-content -p px-ipc -p px-broker --features testing");
    match proc::cargo(
        root,
        &[
            "test",
            "--locked",
            "-p",
            "px-script",
            "-p",
            "px-content",
            "-p",
            "px-ipc",
            "-p",
            "px-broker",
            "--features",
            "testing",
        ],
    ) {
        Ok(s) if s.success() => {}
        _ => g.fail("tests failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LISTING: &str = "a::script_x_one: test\n";

    #[test]
    fn a_remaining_item_names_its_unit() {
        let r = judge_remaining("i", "unit 4", "script_x_", "t.rs", "", None, "");
        assert_eq!(
            r.unwrap_err(),
            "remaining (unit 4): 'i': gate item 'i' has no tests matching 'script_x_'"
        );
    }

    #[test]
    fn a_remaining_item_passes_once_its_tests_exist() {
        let src = Some("#[test]\nfn script_x_one() {}\n");
        let r = judge_remaining("i", "unit 4", "script_x_", "t.rs", LISTING, src, "");
        assert_eq!(r.unwrap(), "i: 1 test(s), 1 in t.rs, none ignored");
    }

    #[test]
    fn an_ignored_test_does_not_satisfy_a_remaining_item() {
        let src = Some("#[test]\nfn script_x_one() {}\n");
        let r = judge_remaining("i", "unit 5", "script_x_", "t.rs", LISTING, src, LISTING);
        assert!(r.unwrap_err().contains("#[ignore]d"));
    }

    #[test]
    fn missing_files_are_listed_with_their_unit() {
        let m = missing_files(&[("a", "unit 7"), ("b", "unit 5")], |p| p == "a");
        assert_eq!(m, [("b", "unit 5")]);
        assert!(missing_files(&[("a", "u")], |_| true).is_empty());
    }

    #[test]
    fn units_six_and_seven_are_named_not_omitted() {
        let units: Vec<&str> = REMAINING.iter().map(|r| r.1).collect();
        assert!(
            units.contains(&"unit 6"),
            "no remaining item owned by unit 6"
        );
        assert!(REQUIRED_FILES.iter().any(|f| f.1 == "unit 7"));
    }

    #[test]
    fn no_item_is_in_both_tables() {
        assert!(duplicated_items(SUITES, REMAINING).is_empty());
        let dup = duplicated_items(&[("a", "f", "h")], &[("a", "unit 4", "script_f", "h")]);
        assert_eq!(dup, ["a"]);
    }

    #[test]
    fn filters_are_script_prefixed_and_distinct() {
        let mut filters: Vec<&str> = SUITES.iter().map(|s| s.1).collect();
        filters.extend(REMAINING.iter().map(|r| r.2));
        for (i, a) in filters.iter().enumerate() {
            assert!(a.starts_with("script_") && a.len() > 8, "weak filter {a}");
            for b in filters.iter().skip(i + 1) {
                assert_ne!(a, b, "duplicate filter");
            }
        }
    }
}
