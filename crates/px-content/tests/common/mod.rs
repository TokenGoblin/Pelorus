//! A test peer for the content binary over its real stdin and stdout pipes.
//!
//! The test plays the broker: it answers the three opening requests the content
//! process makes, then talks `ScriptRequest` / `ScriptResponse`. The child is
//! **not** sandboxed here (the sandbox has its own gate); what is exercised is
//! the protocol across a real process boundary.
//!
//! Every wait has a deadline, and the child is killed on drop, so a hung
//! content process fails a test instead of the CI job.

#![allow(dead_code)] // each test file uses a different part

use std::io::{BufReader, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use px_ipc::script::{ScriptRequest, ScriptResponse, ScriptResult, ScriptSource};
use px_ipc::{Channel, IpcError, Request, Response};

pub const PATIENT: Duration = Duration::from_secs(120);

pub struct Peer {
    pub child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Result<ScriptResponse, IpcError>>,
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Peer {
    /// Spawn the content binary and answer its opening exchange. Does **not**
    /// send the seed.
    pub fn spawn_unseeded() -> Peer {
        let mut child = Command::new(env!("CARGO_BIN_EXE_px-content"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn px-content");
        let mut stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        {
            let mut opening = Channel::new(&mut reader, &mut stdin);
            assert_eq!(opening.recv::<Request>().expect("ping"), Request::Ping);
            opening.send(&Response::Pong).expect("pong");
            match opening.recv::<Request>().expect("echo") {
                Request::Echo { payload } => {
                    opening.send(&Response::Echo { payload }).expect("echo");
                }
                other => panic!("expected Echo, got {other:?}"),
            }
            assert!(matches!(
                opening.recv::<Request>().expect("frame host"),
                Request::FrameHost { .. }
            ));
            opening.send(&Response::Denied).expect("denied");
        }
        // The content process sends nothing more until it is spoken to, so the
        // reader's buffer is empty and can be handed to the forwarding thread.

        // One thread owns the read half and blocks freely, so that the test can
        // apply a deadline to a silent peer (as the broker's reader does).
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            let mut inbound = Channel::new(reader, std::io::sink());
            loop {
                let message = inbound.recv::<ScriptResponse>();
                let fatal = message.is_err();
                if tx.send(message).is_err() || fatal {
                    return;
                }
            }
        });
        Peer {
            child,
            stdin: Some(stdin),
            replies,
        }
    }

    /// Spawn and send the `Init` seed.
    pub fn spawn(seed: u64) -> Peer {
        let mut peer = Peer::spawn_unseeded();
        peer.send(&ScriptRequest::Init { seed });
        peer
    }

    pub fn send(&mut self, request: &ScriptRequest) {
        let frame = px_ipc::encode_frame(request).expect("encode");
        self.send_raw(&frame);
    }

    /// Bytes straight onto the wire, bypassing the codec. A broken pipe is not
    /// an error here: the tests that send garbage expect the child to be gone.
    pub fn send_raw(&mut self, bytes: &[u8]) {
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(bytes);
            let _ = stdin.flush();
        }
    }

    pub fn recv(&mut self) -> Result<ScriptResponse, IpcError> {
        match self.replies.recv_timeout(PATIENT) {
            Ok(message) => message,
            Err(RecvTimeoutError::Timeout) => panic!("content process silent for {PATIENT:?}"),
            Err(RecvTimeoutError::Disconnected) => Err(IpcError::PeerClosed),
        }
    }

    /// Run `source` and return its result.
    pub fn run(&mut self, source: &str) -> ScriptResult {
        self.send(&ScriptRequest::RunScript {
            source: ScriptSource::new(source.to_string()).expect("source under the cap"),
        });
        #[allow(unreachable_patterns)] // the testing variants exist only with the feature
        match self.recv().expect("a reply") {
            ScriptResponse::Result(result) => result,
            other => panic!("expected a Result, got {other:?}"),
        }
    }

    /// Wait for the child to exit.
    pub fn wait_exit(&mut self) -> ExitStatus {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().expect("try_wait") {
                return status;
            }
            assert!(
                started.elapsed() < PATIENT,
                "content process still running after {PATIENT:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Close the child's stdin, as the broker does when it is done.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }
}

/// How a script that may never finish came to an end, seen from the broker's
/// side of the pipe.
#[derive(Debug)]
pub enum Ended {
    /// The host stopped script and said so; the process is still up.
    Terminated(px_ipc::script::TerminateReason),
    /// The process died without an answer. The status is the child's.
    Dead(ExitStatus),
    /// Script finished normally (a value) or threw (an error).
    Finished(ScriptResult),
}

impl Peer {
    /// Testing only: shorten the watchdog so a test does not wait the shipping
    /// ten seconds. A build without `testing` has no such knob, and the test
    /// then waits the real limit, which `GENEROUS` allows for.
    #[cfg(feature = "testing")]
    pub fn set_watchdog_ms(&mut self, millis: u32) {
        self.send(&ScriptRequest::WatchdogMillis { millis });
    }

    #[cfg(not(feature = "testing"))]
    pub fn set_watchdog_ms(&mut self, _millis: u32) {}

    /// Send a script without waiting for its answer.
    pub fn send_script(&mut self, source: &str) {
        self.send(&ScriptRequest::RunScript {
            source: ScriptSource::new(source.to_string()).expect("source under the cap"),
        });
    }

    /// The next message within `within`, or `None` if the process went quiet.
    pub fn recv_within(&mut self, within: Duration) -> Option<Result<ScriptResponse, IpcError>> {
        match self.replies.recv_timeout(within) {
            Ok(message) => Some(message),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(IpcError::PeerClosed)),
        }
    }

    /// Run a script that may never finish and say how it ended. A process that
    /// is still up and silent after `bound` is a failed test, not a hang.
    pub fn run_runaway(&mut self, source: &str, bound: Duration) -> Ended {
        self.send_script(source);
        match self.recv_within(bound) {
            Some(Ok(ScriptResponse::Result(ScriptResult::Terminated { reason }))) => {
                Ended::Terminated(reason)
            }
            Some(Ok(ScriptResponse::Result(other))) => Ended::Finished(other),
            Some(Ok(other)) => panic!("unexpected {other:?}"),
            Some(Err(_)) => Ended::Dead(self.wait_exit()),
            None => panic!("neither an answer nor a death within {bound:?} for {source:?}"),
        }
    }
}

/// A bound for "ended within the watchdog plus slack", where the watchdog may
/// be the shipping ten seconds (a build without `testing`) or a test's short
/// one, on a slow, shared runner and an unoptimised engine.
pub const GENEROUS: Duration = Duration::from_secs(90);

/// The short watchdog the process tests ask for under `testing`.
pub const SHORT_WATCHDOG_MS: u32 = 1500;

/// Whether this exit was the watchdog's `abort()`: on Unix a SIGABRT, elsewhere
/// any failure (Windows `abort` is an exit code).
pub fn was_aborted(status: ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(6)
    }
    #[cfg(not(unix))]
    {
        !status.success()
    }
}

/// `ScriptResult::Value` with this text.
pub fn value(text: &str) -> ScriptResult {
    ScriptResult::Value {
        text: px_ipc::script::ResultText::new(text.to_string()).expect("short"),
    }
}
