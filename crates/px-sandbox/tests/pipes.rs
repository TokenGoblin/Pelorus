//! The pipe-lifetime properties a sandboxed spawn has to get right.
//!
//! These are integration tests rather than unit tests because they are about
//! what the *operating system* does with handles after `spawn` returns, which
//! is only observable from outside the function that created them.
//!
//! # Why this file exists
//!
//! Phase 1's adversarial session found three ways to wedge the broker forever,
//! and the worst was a content process that orphans its stdout and exits: the
//! pipe never reports EOF, `try_wait` says the child is dead, and the reader
//! thread blocks on a channel whose peer is gone.
//!
//! Phase 2 rewrote the spawn path in `unsafe` FFI, which means it can now
//! *cause* that same wedge by accident — if the parent keeps a copy of the
//! child's end of a pipe, every child looks orphaned forever. Nothing in the
//! type system prevents it: the handles are created in pairs and one of each
//! pair has to be dropped, silently, on a path with several early returns.
//!
//! So the property gets a test rather than a comment.

use std::io::Read;
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::Duration;

/// A process that stays alive until its stdin closes, on either platform.
fn a_spawnable_executable() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\Windows\System32\cmd.exe")
    } else {
        PathBuf::from("/bin/cat")
    }
}

/// How long to wait for EOF before concluding it is never coming.
///
/// Generous on purpose: the failure this guards against is an *infinite* wait,
/// so a slow machine must not look like a broken one.
const EOF_DEADLINE: Duration = Duration::from_secs(10);

#[test]
fn the_parent_keeps_no_copy_of_the_child_end_of_a_pipe() {
    let Ok(mut child) = px_sandbox::spawn(&a_spawnable_executable()) else {
        // Below the floor, refusing is correct. `px-sandbox`'s own suite is
        // where that is asserted under PX_REQUIRE_SANDBOX.
        return;
    };

    let mut stdout = child.take_stdout().expect("the parent's read end");
    drop(child.take_stdin());

    child.kill().expect("the child must be killable");
    child.wait().expect("the child must be reapable");

    // Read on another thread: if the property is broken this blocks forever,
    // and a hung test is a worse failure report than a failed one.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = tx.send(stdout.read_to_end(&mut sink));
    });

    match rx.recv_timeout(EOF_DEADLINE) {
        // EOF, or a broken-pipe error. Both terminate, which is the property.
        Ok(_) => {}
        Err(_) => panic!(
            "the child is dead and the pipe still has not reported EOF; the \
             parent is holding the child's own end, and every content process \
             will look orphaned forever"
        ),
    }
}

#[test]
fn the_two_pipe_ends_the_broker_gets_are_distinct_and_usable() {
    let Ok(mut child) = px_sandbox::spawn(&a_spawnable_executable()) else {
        return;
    };

    let stdin = child.take_stdin().expect("the parent's write end");
    let stdout = child.take_stdout().expect("the parent's read end");

    // Handed over exactly once. A second caller getting a duplicate would mean
    // two owners closing the same handle.
    assert!(child.take_stdin().is_none());
    assert!(child.take_stdout().is_none());

    drop(stdin);
    drop(stdout);
    let _ = child.kill();
    let _ = child.wait();
}
