#![forbid(unsafe_code)]

//! The browser binary.
//!
//! Named `px-browser` rather than the product name: no compiled artifact
//! carries the brand, and `packaging/` supplies the user-facing name (§2.1).
//!
//! Phase 1 has no chrome and no engine, so this spawns one content process,
//! serves it under a deadline until it goes away, and exits. `px-ui` takes
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

    let channel = content.channel_id();
    let mut served = 0u32;
    loop {
        match serve_once(&mut broker, &mut content, DEADLINE) {
            Ok(_) => served = served.saturating_add(1),
            Err(ServeError::Closed) => {
                println!(
                    "content process on channel {} served {served} requests",
                    channel.as_u64()
                );
                return ExitCode::SUCCESS;
            }
            Err(reason) => {
                // Every one of these is the peer's choice, not an accident:
                // it stalled, desynchronised the stream, or stopped reading.
                eprintln!("px-browser: reaping content process: {reason:?}");
                let _ = content.kill();
                broker.close_channel(channel);
                return ExitCode::FAILURE;
            }
        }
    }
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
