//! Gate item 2: with the sandbox forced unavailable, the browser refuses to
//! launch content processes and says why (build-spec §9, §14.5).
//!
//! This lives in `px-content` for the same reason `crash_restart.rs` does:
//! `CARGO_BIN_EXE_px-content` is only set for this package's own tests, and
//! the question is about a real content process rather than a mock one.
//!
//! # What "refuses" has to mean
//!
//! Not "starts and then stops". Not "starts with less". Invariant 8 says an
//! operation whose security control cannot be applied does not proceed, so the
//! assertion is that **no process exists** — the refusal happens before
//! anything is spawned, and the error names the mechanism that was missing so
//! that a user has something to act on rather than a reason to reach for
//! `--no-sandbox`.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use px_broker::{Broker, ContentProcess};

/// Serialises every test that reads or writes the sandbox override.
///
/// The override is a process-wide environment variable and the harness runs
/// tests concurrently, so without this the forced-unavailable test leaks its
/// setting into whichever test happens to be probing capabilities at the time.
/// That is not hypothetical: it is what this file did on first run, and the
/// positive control below is what caught it.
static ENVIRONMENT: Mutex<()> = Mutex::new(());

/// Take the environment lock, ignoring poisoning.
///
/// A panicking test has already failed; refusing to run the others because of
/// it turns one failure into several and hides which was first.
fn environment_lock() -> MutexGuard<'static, ()> {
    ENVIRONMENT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The real content process binary.
fn content_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_px-content"))
}

/// Every mechanism the refusal is allowed to name, in the platform's own
/// spelling. §14.5's requirement is that the message names what is missing;
/// this is the vocabulary it may use.
const MECHANISMS: &[&str] = &[
    "no_new_privs",
    "seccomp-bpf",
    "Landlock",
    "user namespaces",
    "restricted token",
    "job object",
];

/// Whether the test-only override is compiled in.
///
/// §14.4 keeps it behind a feature so it cannot reach a release artifact, so
/// the end-to-end half of this file only has something to force when the
/// feature is on. The gate runs the suite with `--features testing`; a plain
/// `cargo test` runs the rest.
const OVERRIDE_COMPILED_IN: bool = cfg!(feature = "testing");

#[test]
fn sandbox_refuses_below_the_floor_and_names_the_mechanism() {
    // Constructed directly rather than forced through the environment, so this
    // half holds with or without the testing feature — and so it tests the
    // decision rather than the override.
    let nothing = px_sandbox::Capabilities::default();
    let refusal = px_sandbox::admit(&nothing).expect_err("an empty machine clears no floor");

    let message = refusal.to_string();
    assert!(
        MECHANISMS.iter().any(|name| message.contains(name)),
        "the refusal must name a mechanism; got: {message}"
    );
    assert!(
        !refusal.missing().is_empty(),
        "a refusal must carry the rungs that were required and absent"
    );
}

#[test]
fn sandbox_refuses_without_leaving_a_content_process_running() {
    if !OVERRIDE_COMPILED_IN {
        // Nothing to force: the override does not exist in this build. The
        // positive control below still runs, and the gate runs this file with
        // the feature on.
        return;
    }

    // Held for the whole test: the variable is process-wide, and every other
    // test in this binary that probes capabilities takes the same lock.
    let _environment = environment_lock();

    // SAFETY: `set_var` is unsafe in edition 2024 because a concurrent reader
    // in another thread is a data race. The lock above excludes every other
    // reader in this test binary, and the only reader inside this section is
    // `px_sandbox::detect` on this thread, reached through the spawn below.
    unsafe {
        std::env::set_var("PX_TEST_FORCE_SANDBOX_UNAVAILABLE", "1");
    }

    let mut broker = Broker::new();
    let outcome = ContentProcess::spawn(&mut broker, &content_executable());

    // SAFETY: as above, and before the lock is released — leaving the variable
    // set would make every later test see a machine with no sandbox.
    unsafe {
        std::env::remove_var("PX_TEST_FORCE_SANDBOX_UNAVAILABLE");
    }

    let error = match outcome {
        Ok(_process) => panic!(
            "a content process was launched with the sandbox unavailable; \
             invariant 8 says the operation must not proceed"
        ),
        Err(error) => error.to_string(),
    };

    assert!(
        MECHANISMS.iter().any(|name| error.contains(name)),
        "the refusal must name what is missing; got: {error}"
    );

    // The refusal must not have cost a channel. A broker that hands out a
    // channel for a process it then refuses to start is leaking authority for
    // a peer that will never exist.
    assert_eq!(
        broker.channel_count(),
        0,
        "a refused launch must not register a channel"
    );
}

#[test]
fn sandbox_refuses_only_when_it_should_and_otherwise_launches() {
    // The positive control, and the reason it matters: a refusal test passes
    // trivially against a browser that refuses everything. Phase 1's lesson
    // was a product binary that failed on a clean run behind a green gate, so
    // the same file asserts both directions.
    let _environment = environment_lock();

    if !px_sandbox::detect().clears_floor() {
        // This machine genuinely cannot clear the floor. Refusing is correct,
        // and asserting a launch would make this a test about the machine.
        return;
    }

    let mut broker = Broker::new();
    let mut process = ContentProcess::spawn(&mut broker, &content_executable())
        .expect("a machine above the floor must be able to launch a content process");

    assert!(
        process.is_alive(),
        "a content process that launched under a policy must be running"
    );
    assert_eq!(
        broker.channel_count(),
        1,
        "a launched content process gets exactly one channel"
    );

    let _ = process.kill();
}
