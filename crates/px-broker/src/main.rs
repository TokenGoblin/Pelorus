#![forbid(unsafe_code)]

//! The browser binary.
//!
//! Named `px-browser` rather than the product name: no compiled artifact
//! carries the brand, and `packaging/` supplies the user-facing name (§2.1).
//!
//! Phase 1 has no chrome and no engine. This spawns a content process, serves
//! the exchange it makes, restarts it once to demonstrate that crash recovery
//! works in the product rather than only in tests, and exits. `px-ui` takes
//! over at Phase 18.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use px_broker::{Broker, ContentProcess, ServeError, serve_once};

/// How long a content process may stay silent before it is reaped.
///
/// A stalled peer is cheaper for an attacker than a crashing one, and without
/// a deadline it is also more effective: the broker is the process that may
/// not die, and nothing else in the design stops it waiting forever.
const DEADLINE: Duration = Duration::from_secs(5);

/// Requests `px-content` makes before it goes quiet.
///
/// The broker decides when the conversation is over; it does not wait for a
/// peer to say so. That is not a detail — the earlier version served until the
/// peer closed, and `px-content` parks after its exchange by design, so both
/// sides waited on each other and the deadline resolved the deadlock by
/// reaping a perfectly healthy child and exiting FAILURE on a clean run.
const EXCHANGES: u32 = 3;

fn main() -> ExitCode {
    let Some(executable) = content_executable() else {
        eprintln!("px-browser: cannot locate the px-content executable");
        return ExitCode::FAILURE;
    };

    let mut broker = Broker::new();
    let mut content = match ContentProcess::spawn(&mut broker, &executable) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("px-browser: cannot spawn a content process: {error}");
            return ExitCode::FAILURE;
        }
    };

    if let Err(code) = run_exchange(&mut broker, &mut content) {
        return code;
    }

    // Crash recovery, in the product. Until now `restart` was called only from
    // tests — structurally the same "dead code" the process boundary itself
    // suffered from, recreated by the rewrite that fixed it.
    if let Err(error) = content.restart(&mut broker) {
        eprintln!("px-browser: cannot restart the content process: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(code) = run_exchange(&mut broker, &mut content) {
        return code;
    }

    let denials = broker.audit_log().count();
    println!(
        "served {} exchanges across {} content processes, {denials} denied",
        EXCHANGES * 2,
        content.restarts() + 1
    );

    // The broker ends the conversation. Dropping the process closes the pipes
    // and kills the child.
    ExitCode::SUCCESS
}

fn run_exchange(broker: &mut Broker, content: &mut ContentProcess) -> Result<(), ExitCode> {
    for exchange in 0..EXCHANGES {
        match serve_once(broker, content, DEADLINE) {
            Ok(_) => {}
            Err(ServeError::Closed) => {
                eprintln!("px-browser: content process closed after {exchange} exchanges");
                return Err(ExitCode::FAILURE);
            }
            Err(reason) => {
                // Every one of these is the peer's choice or a fault on this
                // side, and neither is something to keep serving through.
                eprintln!("px-browser: reaping content process: {reason:?}");
                let _ = content.kill();
                return Err(ExitCode::FAILURE);
            }
        }
    }
    Ok(())
}

/// Find the content process binary next to this one.
///
/// Phase 20 replaces this with an installed layout. Deliberately not a search
/// path: the broker must launch the content process it shipped with, not
/// whichever one is first on `PATH`.
fn content_executable() -> Option<PathBuf> {
    let mut path = std::env::current_exe().ok()?;
    path.pop();
    path.push(if cfg!(windows) {
        "px-content.exe"
    } else {
        "px-content"
    });
    path.exists().then_some(path)
}
