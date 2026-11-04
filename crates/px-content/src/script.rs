//! The script half of the content process: the receive loop for
//! [`ScriptRequest`], the event loop that runs timers between requests, and the
//! mapping from the host's outcomes to wire data.
//!
//! The process learns nothing about itself from the wire and says nothing about
//! itself on it. What arrives is a seed and script text; what leaves is a
//! result string, an error string, or the fact that script was stopped. All of
//! it is bounded by the types in `px_ipc::script`.
//!
//! # One thread owns the engine
//!
//! A reader thread blocks on the pipe and forwards decoded requests over a
//! small bounded queue. The main thread owns the engine and alternates: handle
//! every request already waiting, run one loop step, and when there is nothing
//! to do wait for the next request, or for the next timer to come due. A page
//! whose timer re-arms itself therefore never starves the channel: a request is
//! seen after at most one task.
//!
//! # Protocol breaks end the process
//!
//! A `RunScript` before the `Init`, a second `Init`, a frame the codec refuses:
//! each ends the process. Fail closed: a content process that cannot tell what
//! state its peer believes it is in is not one to keep running script in.

use std::io::{Read, Write};
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError, sync_channel};
use std::time::Duration;

use px_ipc::script::{
    ErrorText, ResultText, ScriptRequest, ScriptResponse, ScriptResult, TerminateReason as Wire,
};
use px_ipc::{Channel, IpcError};
use px_script::{Host, HostOptions, Outcome, Step, TerminateReason};

/// Requests the reader thread may hold ahead of the engine. A full queue blocks
/// the reader, which stops reading the pipe: backpressure, not growth.
const INBOX_DEPTH: usize = 8;

type Inbox = Receiver<Result<ScriptRequest, IpcError>>;

/// Serve [`ScriptRequest`]s until the broker closes the channel.
pub fn serve<R, W>(reader: R, writer: W) -> Result<(), IpcError>
where
    R: Read + Send + 'static,
    W: Write,
{
    let (tx, inbox) = sync_channel(INBOX_DEPTH);
    std::thread::spawn(move || {
        let mut inbound = Channel::new(reader, std::io::sink());
        loop {
            let message = inbound.recv::<ScriptRequest>();
            let finished = message.is_err();
            if tx.send(message).is_err() || finished {
                return;
            }
        }
    });
    let mut session = Session {
        out: Channel::new(std::io::empty(), writer),
        host: None,
        told_of_termination: false,
    };
    session.run(&inbox)
}

struct Session<W: Write> {
    out: Channel<std::io::Empty, W>,
    /// Absent until the seed arrives: there is nothing a `RunScript` could run
    /// in before that.
    host: Option<Host>,
    /// Whether the broker has been told script is stopped.
    told_of_termination: bool,
}

/// What the loop decided to wait for.
enum Wait {
    Forever,
    For(Duration),
}

impl<W: Write> Session<W> {
    fn run(&mut self, inbox: &Inbox) -> Result<(), IpcError> {
        loop {
            // Requests first, so a busy page cannot starve the channel.
            match inbox.try_recv() {
                Ok(message) => {
                    if self.handle(message)? == Flow::Stop {
                        return Ok(());
                    }
                    continue;
                }
                Err(TryRecvError::Disconnected) => return Ok(()),
                Err(TryRecvError::Empty) => {}
            }
            let wait = match self.host.as_mut().map(Host::step) {
                Some(Step::Ran) => continue,
                Some(Step::Waiting(delay)) => Wait::For(delay),
                Some(Step::Terminated(reason)) => {
                    self.tell_of_termination(reason)?;
                    Wait::Forever
                }
                Some(Step::Idle) | None => Wait::Forever,
            };
            let message = match wait {
                Wait::Forever => match inbox.recv() {
                    Ok(message) => message,
                    Err(_) => return Ok(()),
                },
                Wait::For(delay) => match inbox.recv_timeout(delay) {
                    Ok(message) => message,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => return Ok(()),
                },
            };
            if self.handle(message)? == Flow::Stop {
                return Ok(());
            }
        }
    }

    fn handle(&mut self, message: Result<ScriptRequest, IpcError>) -> Result<Flow, IpcError> {
        match message {
            Ok(ScriptRequest::Init { seed }) => {
                // A second Init would let a confused peer re-seed the clock
                // mid-run.
                if self.host.is_some() {
                    return Err(IpcError::Malformed);
                }
                // The seed came down the channel from the broker; this process
                // has no entropy source of its own and wants none. The wall
                // epoch is read once, here: `Date.now()` is that plus coarse
                // elapsed time, never a second look at the wall clock.
                let options = HostOptions {
                    wall_epoch_ms: wall_clock_ms(),
                    ..HostOptions::new(seed)
                };
                let host = Host::with_options(options)
                    .map_err(|e| IpcError::Io(std::io::Error::other(e)))?;
                self.host = Some(host);
                Ok(Flow::Go)
            }
            Ok(ScriptRequest::RunScript { source }) => {
                let Some(host) = self.host.as_mut() else {
                    return Err(IpcError::Malformed);
                };
                let outcome = host.eval(source.as_str());
                if matches!(outcome, Outcome::Terminated(_)) {
                    self.told_of_termination = true;
                }
                self.out.send(&ScriptResponse::Result(to_wire(outcome)))?;
                Ok(Flow::Go)
            }
            #[cfg(feature = "testing")]
            Ok(ScriptRequest::Counters) => {
                let Some(host) = self.host.as_ref() else {
                    return Err(IpcError::Malformed);
                };
                self.out.send(&ScriptResponse::Counters(counters(host)))?;
                Ok(Flow::Go)
            }
            #[cfg(feature = "testing")]
            Ok(ScriptRequest::ClockLog) => {
                let Some(host) = self.host.as_ref() else {
                    return Err(IpcError::Malformed);
                };
                let nanos = px_ipc::gpu::Bounded::truncated(host.clock_log());
                self.out.send(&ScriptResponse::ClockLog { nanos })?;
                Ok(Flow::Go)
            }
            // A request this build does not serve: the testing-only variants reach
            // a content process built WITHOUT its own `testing` feature whenever
            // another crate in the same workspace build enables `px-ipc/testing`.
            // Fail closed: it is a protocol violation, and it ends the channel.
            Ok(_) => Err(IpcError::Malformed),
            Err(IpcError::PeerClosed) => Ok(Flow::Stop),
            Err(other) => Err(other),
        }
    }

    /// A timer task, not a request, stopped script: say so once, unasked.
    fn tell_of_termination(&mut self, reason: TerminateReason) -> Result<(), IpcError> {
        if self.told_of_termination {
            return Ok(());
        }
        self.told_of_termination = true;
        self.out
            .send(&ScriptResponse::Result(ScriptResult::Terminated {
                reason: wire_reason(reason),
            }))
    }
}

#[derive(PartialEq, Eq)]
enum Flow {
    Go,
    Stop,
}

/// Unix milliseconds now, once, for `Date.now()`'s epoch. A clock set before
/// 1970 reads as zero rather than failing.
fn wall_clock_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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
        Outcome::Terminated(reason) => ScriptResult::Terminated {
            reason: wire_reason(reason),
        },
    }
}

/// No wildcard: a new reason in the host must be mapped here or this does not
/// compile.
fn wire_reason(reason: TerminateReason) -> Wire {
    match reason {
        TerminateReason::TaskQueueOverflow => Wire::TaskQueueOverflow,
        TerminateReason::MicrotaskQueueOverflow => Wire::MicrotaskQueueOverflow,
        TerminateReason::TimerOverflow => Wire::TimerOverflow,
        TerminateReason::MicrotaskBudget => Wire::MicrotaskBudget,
    }
}

#[cfg(feature = "testing")]
fn counters(host: &Host) -> px_ipc::script::CounterReport {
    let marks = host.high_water();
    let caps = host.caps();
    let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    px_ipc::script::CounterReport {
        marker: px_ipc::script::BoundedStr::truncated(px_script::COUNTERS_MARKER.to_string()),
        tasks: (n(marks.tasks), n(caps.tasks)),
        microtasks: (n(marks.microtasks), n(caps.microtasks)),
        timers: (n(marks.timers), n(caps.timers)),
        checkpoint: (n(marks.checkpoint), n(caps.checkpoint_budget)),
    }
}
