#![forbid(unsafe_code)]

//! Content process binary.
//!
//! Phase 1's content process does one thing: echo. That is deliberate. The
//! point of this phase is that the process boundary exists and everything
//! afterwards is written across it — not that the thing on the far side is
//! interesting.
//!
//! It holds no handle it was not passed. Its stdin and stdout were handed to
//! it by the broker at spawn and are the entirety of its authority; it opens
//! no file, makes no connection, and never states who it is (invariant 9).

use std::io::{BufReader, BufWriter, stdin, stdout};
use std::process::ExitCode;

use px_ipc::{Channel, IpcError, Request, Response};

fn main() -> ExitCode {
    let channel = Channel::new(BufReader::new(stdin()), BufWriter::new(stdout()));
    match serve(channel) {
        // The broker closed the pipe. Orderly shutdown, not a failure.
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("px-content: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Answer requests until the broker goes away.
fn serve<R, W>(mut channel: Channel<R, W>) -> Result<(), IpcError>
where
    R: std::io::Read,
    W: std::io::Write,
{
    loop {
        let request = match channel.recv::<Request>() {
            Ok(request) => request,
            Err(IpcError::PeerClosed) => return Ok(()),
            Err(other) => return Err(other),
        };

        let response = match request {
            Request::Ping => Response::Pong,
            Request::Echo { payload } => Response::Echo { payload },
            // A content process is not an authority on where frames live; it
            // asks the broker. Receiving this request means the broker is
            // confused, and answering would be inventing an answer.
            Request::FrameHost { .. } => Response::Denied {
                reason: px_ipc::DenyReason::NotYourFrame,
            },
        };

        channel.send(&response)?;
    }
}
