#![forbid(unsafe_code)]

//! Content process binary.
//!
//! Phase 1's content process does one thing: exercise the boundary from the
//! unprivileged side. That is deliberate. The point of this phase is that the
//! process boundary exists and everything afterwards is written across it —
//! not that the thing on the far side is interesting.
//!
//! It is the **client**. It asks; the broker decides. That direction is not
//! cosmetic: the capability table is keyed by the channel a request arrives
//! on, so a design where the broker asks and the content process answers has
//! no place to apply it.
//!
//! After that exchange it is the script host (Phase 10, ADR 040): the only
//! process in the product that links the engine.
//!
//! It holds no handle it was not passed. Its stdin and stdout were handed to
//! it by the broker at spawn and are the entirety of its authority; it opens
//! no file, makes no connection, and never states who it is (invariant 9).

use std::io::{BufReader, BufWriter, stdin, stdout};
use std::process::ExitCode;

use px_ipc::{Channel, FrameId, IpcError, Request, Response};

mod script;
mod watchdog;

fn main() -> ExitCode {
    // Stage two of the sandbox profile (ADR 043, reusing ADR 035): before the
    // channel is read, before any thread exists, the process takes away its own
    // ability to open, create, connect, execute or enumerate. Threads made
    // later inherit it. The broker does not trust this call having happened: it
    // asks the kernel (`SandboxedChild::verify_stage_two`). If it cannot be
    // applied the process exits rather than serve, and does so before it has
    // read a byte a page could have influenced.
    //
    // Linux only: the Windows content process keeps its creation-time profile
    // and has no stage two yet (`enter_content_profile` refuses there).
    #[cfg(target_os = "linux")]
    if let Err(error) = px_sandbox::enter_content_profile() {
        eprintln!("px-content: {error}");
        return ExitCode::FAILURE;
    }

    let channel = Channel::new(BufReader::new(stdin()), BufWriter::new(stdout()));
    match run(channel) {
        // The broker closed the pipe. Orderly shutdown, not a failure.
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("px-content: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Exercise the boundary, then wait for the broker to go away.
fn run<R, W>(mut channel: Channel<R, W>) -> Result<(), IpcError>
where
    R: std::io::Read + Send + 'static,
    W: std::io::Write,
{
    channel.send(&Request::Ping)?;
    expect(&channel.recv::<Response>()?, &Response::Pong)?;

    let payload = b"across a process boundary".to_vec();
    channel.send(&Request::Echo {
        payload: payload.clone(),
    })?;
    expect(&channel.recv::<Response>()?, &Response::Echo { payload })?;

    // Ask about a frame this process was never given. The broker must refuse,
    // and the refusal must say nothing about whether it exists.
    channel.send(&Request::FrameHost {
        frame: FrameId::new(0, 0),
    })?;
    expect(&channel.recv::<Response>()?, &Response::Denied)?;

    // Then serve script until the broker drops the pipe. A content process that
    // exits immediately cannot be killed by a test, and a process that goes
    // quiet is exactly the shape the broker's deadline exists for: before the
    // broker sends its seed, this waits silently.
    let (reader, writer) = channel.into_parts();
    script::serve(reader, writer)
}

fn expect(actual: &Response, wanted: &Response) -> Result<(), IpcError> {
    if actual == wanted {
        Ok(())
    } else {
        eprintln!("px-content: expected {wanted:?}, got {actual:?}");
        Err(IpcError::Malformed)
    }
}
