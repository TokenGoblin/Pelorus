//! Phase 17: the escape-attempt suite for the content profile on Linux
//! (build-spec section 9: "open file, connect socket, spawn process, read
//! another process, access clipboard, enumerate devices fails every attempt";
//! ADR 043).
//!
//! The real content binary is created through `px_sandbox::spawn`, answers its
//! opening exchange, and is then asked (under `testing`) to attempt each
//! escape from inside itself and report what the kernel answered. The attempts
//! are the same shape as the GPU probe's: an unconfined process fails them for
//! an ordinary reason or succeeds harmlessly (`probe::escape_attempts` says
//! why none can do damage), and a confined one is refused with `EPERM`.
//!
//! Three things are asserted and kept apart, because they are three different
//! kinds of evidence:
//!
//! * what the kernel says about the process (`verify_stage_two`, read by the
//!   parent from `/proc`; never the child's word),
//! * what the child's own attempts returned (behaviour: this is what shows
//!   the filter means something), and
//! * which operations are *permitted* and why, listed rather than hidden.
//!
//! Clipboard: Linux has no clipboard without a display server, and reaching
//! one is a `connect` to the X11 or Wayland UNIX socket. That is exactly what
//! `socket_unix` and `connect_unix_display` attempt, so they are this
//! platform's clipboard attempt. Windows has its own suite when its profile
//! exists (docs/backlog.md).

#![cfg(all(target_os = "linux", feature = "testing"))]

#[cfg(test)]
mod common;

use common::sandboxed::{SandboxedPeer, can_sandbox};
use common::value;
use px_ipc::script::{ScriptRequest, ScriptResponse};
use px_sandbox::Rung;
use px_sandbox::probe::ESCAPES;

const EPERM: i32 = 1;

/// What the probe reported: the Landlock ABI and each attempt by name.
struct Report {
    landlock_abi: u32,
    attempts: Vec<(String, bool, i32)>,
}

impl Report {
    fn get(&self, name: &str) -> (bool, i32) {
        self.attempts
            .iter()
            .find(|(n, _, _)| n == name)
            .map(|(_, ok, errno)| (*ok, *errno))
            .unwrap_or_else(|| panic!("the probe did not report {name}"))
    }
}

#[cfg(test)]
fn probe(peer: &mut SandboxedPeer) -> Report {
    peer.send(&ScriptRequest::EscapeProbe);
    match peer.recv().expect("the probe's report") {
        ScriptResponse::EscapeProbe {
            landlock_abi,
            attempts,
        } => Report {
            landlock_abi,
            attempts: attempts
                .into_vec()
                .into_iter()
                .map(|a| (a.name.as_str().to_owned(), a.succeeded, a.errno))
                .collect(),
        },
        other => panic!("expected the escape report, got {other:?}"),
    }
}

/// Operations the profile permits today, by name, with the reason. Everything
/// else in `ESCAPES` must be refused with `EPERM`. Adding a name here is a
/// decision to widen the profile and belongs in the same commit as the entry in
/// docs/backlog.md that explains it.
///
/// * `stat_file`: metadata calls stay allowed. The allocator and `std` use
///   `fstat`, which shares a syscall with path-based `stat`, and a deny list
///   cannot tell them apart. It leaks whether a path exists and its mode, not
///   its content. Closing it needs the allow list (docs/research/
///   phase-17-syscall-audit.md).
/// * `signal_peer`: refused only where Landlock's signal scope exists (ABI 6,
///   Linux 6.12); see `signal_peer_is_refused_where_landlock_scopes_signals`.
#[cfg(test)]
const PERMITTED: &[&str] = &["stat_file", "signal_peer"];

#[test]
fn sandbox_escape_every_forbidden_attempt_is_refused() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    let report = probe(&mut peer);

    let names: Vec<&str> = report.attempts.iter().map(|(n, _, _)| n.as_str()).collect();
    assert_eq!(
        names, ESCAPES,
        "the probe must report every attempt, in order"
    );

    let mut breaches = Vec::new();
    for name in ESCAPES {
        if PERMITTED.contains(name) {
            continue;
        }
        let (succeeded, errno) = report.get(name);
        if succeeded || errno != EPERM {
            breaches.push(format!("{name}: succeeded={succeeded} errno={errno}"));
        }
    }
    assert!(
        breaches.is_empty(),
        "attempts the content profile did not refuse with EPERM:\n  {}",
        breaches.join("\n  ")
    );
}

#[test]
fn sandbox_escape_the_kernel_confirms_the_profile_the_attempts_ran_under() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    // The rungs the process was created under, as the parent recorded them.
    let applied = peer.child.applied().to_vec();
    assert_eq!(
        applied,
        vec![Rung::NoNewPrivs, Rung::Seccomp, Rung::MemoryCap],
        "the creation-time rung list for a content process"
    );
    // The kernel's account of the process, read from /proc by the parent: both
    // filters installed. This is what makes the attempts above a statement
    // about *this* profile rather than about whatever the child claims.
    let verified = peer
        .child
        .verify_stage_two()
        .expect("the kernel must confirm both stages");
    assert_eq!(verified, vec![Rung::NoNewPrivs, Rung::Seccomp]);
    // And the attempts are made by that very process.
    let report = probe(&mut peer);
    assert_eq!(report.attempts.len(), ESCAPES.len());
    eprintln!(
        "escape suite: created under {applied:?}, kernel-verified {verified:?}, \
         landlock abi {}",
        report.landlock_abi
    );
}

/// Landlock's signal scope (ABI 6) is what stops a content process signalling
/// the broker, which runs as the same user. Without it nothing in the profile
/// does: `kill` is needed for nothing the process does, but `tgkill` is how it
/// aborts itself and cannot be denied by a number-only filter. The test states
/// which side of that line this kernel is on, and fails if the claim and the
/// kernel disagree.
#[test]
fn sandbox_escape_signal_peer_is_refused_where_landlock_scopes_signals() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    let report = probe(&mut peer);
    let (succeeded, errno) = report.get("signal_peer");
    if report.landlock_abi >= 6 {
        assert!(
            !succeeded && errno == EPERM,
            "Landlock ABI {} scopes signals, yet kill(peer, 0) gave succeeded={succeeded} errno={errno}",
            report.landlock_abi
        );
    } else {
        eprintln!(
            "KNOWN GAP: Landlock ABI {} has no signal scope; a content process can signal \
             same-user processes including the broker (docs/backlog.md)",
            report.landlock_abi
        );
        assert!(succeeded, "expected the documented gap on this kernel");
    }
}

/// The permitted list is a record, so it is checked in both directions: what
/// it names really is permitted, which keeps a quiet tightening (or a quiet
/// widening elsewhere) from leaving the documentation false.
#[test]
fn sandbox_escape_the_permitted_operations_are_the_documented_ones() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    let report = probe(&mut peer);
    let (stat_ok, _) = report.get("stat_file");
    assert!(
        stat_ok,
        "stat_file is refused now: remove it from PERMITTED and from the backlog entry"
    );
}

/// Confinement that breaks the product is not a sandbox. After stage two the
/// process still runs script, uses threads (the reader and the watchdog),
/// clocks and timers, and its channel.
#[test]
fn sandbox_escape_the_confined_process_still_does_its_job() {
    if !can_sandbox() {
        return;
    }
    let mut peer = SandboxedPeer::spawn(1);
    assert_eq!(peer.run("1+1"), value("2"));
    assert_eq!(
        peer.run("typeof Date.now() + typeof performance.now()"),
        value("numbernumber")
    );
    assert_eq!(
        peer.run("var n = 0; setTimeout(function () { n = 7 }, 5); 'armed'"),
        value("armed")
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(peer.run("n"), value("7"));
    assert_eq!(peer.run("Math.random() < 1"), value("true"));
    assert!(peer.child.try_wait().expect("try_wait").is_none());
}
