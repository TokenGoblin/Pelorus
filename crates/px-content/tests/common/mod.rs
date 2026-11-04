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

/// `ScriptResult::Value` with this text.
pub fn value(text: &str) -> ScriptResult {
    ScriptResult::Value {
        text: px_ipc::script::ResultText::new(text.to_string()).expect("short"),
    }
}
