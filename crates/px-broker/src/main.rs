#![forbid(unsafe_code)]

//! The browser binary.
//!
//! Named `px-browser` rather than the product name: no compiled artifact
//! carries the brand, and `packaging/` supplies the user-facing name (§2.1).
//!
//! Phase 1 has no chrome and no engine, so this spawns one content process,
//! proves the boundary is real, and exits. `px-ui` takes over at Phase 18.

use std::path::PathBuf;
use std::process::ExitCode;

use px_broker::{Broker, ContentProcess};
use px_ipc::{Request, Response};

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

    let request = Request::Echo {
        payload: b"phase 1".to_vec(),
    };
    match content.round_trip(&request) {
        Ok(Response::Echo { payload }) if payload == b"phase 1" => {
            println!(
                "content process on channel {} answered",
                content.channel_id().as_u64()
            );
            ExitCode::SUCCESS
        }
        Ok(other) => {
            eprintln!("px-browser: unexpected response: {other:?}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("px-browser: {error}");
            ExitCode::FAILURE
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
