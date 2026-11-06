//! Phase 17, Windows: the gate's `escape-attempts` item on the platform that
//! has no in-process escape suite.
//!
//! `sandbox_escape.rs` is Linux-only: it asks the *content process* to attempt
//! each forbidden operation and report what the kernel said, which needs stage
//! two of the profile (`enter_content_profile`) and the testing probe, and
//! neither exists on Windows (ADR 048). The gate does not skip that item on
//! Windows and does not pass it empty. It names this file instead, prints a
//! note that the evidence is weaker (`WINDOWS_ESCAPE_NOTE` in
//! `px-xtask/src/gates/sandbox.rs`), and holds these tests to the same three
//! conditions as every other suite: they exist, they sit in this file, none is
//! ignored.
//!
//! What is asserted is everything the parent can read back from the operating
//! system about the real content binary under the real shipping profile, which
//! is the only vantage point the content process cannot influence:
//!
//! * the binary starts and stays alive under the profile and does its job (a
//!   profile that kills the product is not a sandbox, and this was the Windows
//!   failure this file was written alongside: `0xC0000142` at load),
//! * the creation-time rung list is exactly the documented one,
//! * the child is in its job, at low integrity, with the job's limits (these
//!   are read back inside `spawn`, so a spawn that returns proves them),
//! * stage two is honestly absent, which is pinned so that adding it has to
//!   touch this file and the gate item together.
//!
//! Not covered, and not claimed: open, connect, spawn-a-process, read another
//! process, clipboard and device enumeration attempts from inside the child.

#![cfg(target_os = "windows")]

#[cfg(test)]
mod common;

use common::sandboxed::{SandboxedPeer, can_sandbox};
use common::value;
use px_sandbox::Rung;

#[test]
fn sandbox_escape_windows_the_real_content_binary_stays_alive_and_does_its_job() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    assert_eq!(peer.run("1+1"), value("2"));
    assert_eq!(
        peer.run("typeof Date.now() + typeof performance.now()"),
        value("numbernumber")
    );
    assert!(
        matches!(peer.child.try_wait(), Ok(None)),
        "the content process must still be running under the profile"
    );
}

#[test]
fn sandbox_escape_windows_the_creation_time_rungs_are_the_documented_ones() {
    if !can_sandbox() {
        return;
    }
    let peer = SandboxedPeer::spawn(1);
    assert_eq!(
        peer.child.applied(),
        [
            Rung::RestrictedToken,
            Rung::JobObject,
            Rung::LowIntegrity,
            Rung::MemoryCap
        ],
        "the Windows creation-time rung list for a content process"
    );
    assert!(
        peer.child.memory_limit().is_some(),
        "the job's commit limit was read back at spawn"
    );
}

/// The absence of stage two is a recorded fact, not an oversight. If this
/// starts failing, Windows has gained a stage two: write the in-process escape
/// attempts for it, point the gate's Windows item at them, and delete the
/// note.
#[test]
fn sandbox_escape_windows_stage_two_is_refused_until_it_exists() {
    if !can_sandbox() {
        return;
    }
    let peer = SandboxedPeer::spawn(1);
    assert!(
        peer.child.verify_stage_two().is_err(),
        "Windows has no stage two; a verification that passes is a claim nobody built"
    );
}
