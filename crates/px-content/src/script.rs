//! The script half of the content process: the receive loop for
//! [`ScriptRequest`], and the mapping from the host's outcomes to wire data.
//!
//! The process learns nothing about itself from the wire and says nothing about
//! itself on it. What arrives is a seed and script text; what leaves is a
//! result string, an error string, or the fact that script was stopped. All of
//! it is bounded by the types in `px_ipc::script`.
//!
//! A peer that breaks the protocol (a `RunScript` before the `Init`, a second
//! `Init`, a frame the codec refuses) ends the process. Fail closed: a content
//! process that cannot tell what state its peer believes it is in is not one
//! to keep running script in.

use std::io::{Read, Write};

use px_ipc::script::{ErrorText, ResultText, ScriptRequest, ScriptResponse, ScriptResult};
use px_ipc::{Channel, IpcError};
use px_script::{Host, Outcome};

/// Serve [`ScriptRequest`]s until the broker closes the channel.
pub fn serve<R, W>(mut channel: Channel<R, W>) -> Result<(), IpcError>
where
    R: Read,
    W: Write,
{
    // The seed is the first message and only the first (ADR 040). Until it has
    // arrived there is no host, so there is nothing a RunScript could run in.
    let _seed = match channel.recv::<ScriptRequest>() {
        Ok(ScriptRequest::Init { seed }) => seed,
        Ok(_) => return Err(IpcError::Malformed),
        Err(IpcError::PeerClosed) => return Ok(()),
        Err(other) => return Err(other),
    };
    let mut host = Host::new();

    loop {
        match channel.recv::<ScriptRequest>() {
            Ok(ScriptRequest::RunScript { source }) => {
                let result = to_wire(host.eval(source.as_str()));
                channel.send(&ScriptResponse::Result(result))?;
            }
            // The testing vocabulary is served from unit 2 (counters) and
            // unit 3 (clock log); until then it is a protocol error.
            #[cfg(feature = "testing")]
            Ok(ScriptRequest::Counters | ScriptRequest::ClockLog) => {
                return Err(IpcError::Malformed);
            }
            // A second Init would let a confused peer re-seed the clock mid-run.
            Ok(ScriptRequest::Init { .. }) => return Err(IpcError::Malformed),
            Err(IpcError::PeerClosed) => return Ok(()),
            Err(other) => return Err(other),
        }
    }
}

/// An outcome as wire data. Over-long text is cut, never refused: a script that
/// returns a megabyte string still gets an answer.
fn to_wire(outcome: Outcome) -> ScriptResult {
    match outcome {
        Outcome::Value(text) => ScriptResult::Value {
            text: ResultText::truncated(text),
        },
        Outcome::Error(message) => ScriptResult::Error {
            message: ErrorText::truncated(message),
        },
    }
}
