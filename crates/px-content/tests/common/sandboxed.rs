//! A test peer for the content binary created through `px_sandbox::spawn_with`,
//! so it runs under the same creation-time profile the broker gives it: the
//! seccomp filter, `no_new_privs` and the address-space cap, and the cap
//! confirmed from the kernel.
//!
//! `Peer` (the parent module) spawns the binary with `std::process::Command`
//! and is the right tool for protocol tests. This one is for the tests whose
//! subject is the sandbox. It speaks the same protocol over the same pipes.

#![allow(dead_code)] // each test file uses a different part

use std::io::BufReader;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use px_ipc::script::{ScriptRequest, ScriptResponse, ScriptResult, ScriptSource};
use px_ipc::{Channel, IpcError, Request, Response};
use px_sandbox::{Policy, SandboxedChild};

use super::PATIENT;

/// How a sandboxed process ended, as the parent saw it.
#[derive(Debug)]
pub struct Death {
    /// `wait()`'s code: the exit code, or -1 when a signal ended the process
    /// (SIGABRT from an allocation failure or the watchdog, SIGKILL, ...).
    pub code: i32,
    /// The largest `VmPeak` (kibibytes of address space) any poll saw while the
    /// process lived, or 0 if it was never read. The kernel reports the peak,
    /// not the current size, so a poll that lands late still sees the high
    /// water mark; the last poll before death is what matters.
    pub peak_kib: u64,
    /// Time from the script being sent until the process was seen dead.
    pub elapsed: Duration,
}

/// Whether this machine can sandbox at all, and a loud failure when it cannot
/// but was promised it could: a silent skip is indistinguishable from a pass,
/// so CI sets `PX_REQUIRE_SANDBOX` and a machine below the floor fails instead.
pub fn can_sandbox() -> bool {
    if px_sandbox::detect().clears_floor() {
        return true;
    }
    assert!(
        std::env::var_os("PX_REQUIRE_SANDBOX").is_none(),
        "PX_REQUIRE_SANDBOX is set but this machine does not clear the sandbox floor"
    );
    false
}

pub struct SandboxedPeer {
    pub child: SandboxedChild,
    stdin: Option<std::fs::File>,
    replies: Receiver<Result<ScriptResponse, IpcError>>,
}

impl Drop for SandboxedPeer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl SandboxedPeer {
    /// Spawn the content binary under the shipping content policy, answer its
    /// opening exchange, and send the seed.
    pub fn spawn(seed: u64) -> SandboxedPeer {
        SandboxedPeer::spawn_with(seed, &Policy::content())
    }

    pub fn spawn_with(seed: u64, policy: &Policy) -> SandboxedPeer {
        let mut peer = SandboxedPeer::spawn_unseeded(policy);
        peer.send(&ScriptRequest::Init { seed });
        peer
    }

    pub fn spawn_unseeded(policy: &Policy) -> SandboxedPeer {
        let mut child = px_sandbox::spawn_with(
            std::path::Path::new(env!("CARGO_BIN_EXE_px-content")),
            policy,
        )
        .expect("spawn px-content under the sandbox");
        let mut stdin = child.take_stdin().expect("stdin");
        let stdout = child.take_stdout().expect("stdout");
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
        SandboxedPeer {
            child,
            stdin: Some(stdin),
            replies,
        }
    }

    pub fn send(&mut self, request: &ScriptRequest) {
        use std::io::Write;
        let frame = px_ipc::encode_frame(request).expect("encode");
        if let Some(stdin) = self.stdin.as_mut() {
            let _ = stdin.write_all(&frame);
            let _ = stdin.flush();
        }
    }

    /// Testing only: lengthen or shorten the watchdog. The memory tests
    /// lengthen it so that a death by the cap cannot be mistaken for the
    /// watchdog's.
    #[cfg(feature = "testing")]
    pub fn set_watchdog_ms(&mut self, millis: u32) {
        self.send(&ScriptRequest::WatchdogMillis { millis });
    }

    pub fn send_script(&mut self, source: &str) {
        self.send(&ScriptRequest::RunScript {
            source: ScriptSource::new(source.to_string()).expect("source under the cap"),
        });
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
        self.send_script(source);
        match self.recv().expect("a reply") {
            ScriptResponse::Result(result) => result,
            other => panic!("expected a Result, got {other:?}"),
        }
    }

    /// The next message within `within`, or `None` if the process went quiet.
    pub fn recv_within(&mut self, within: Duration) -> Option<Result<ScriptResponse, IpcError>> {
        match self.replies.recv_timeout(within) {
            Ok(message) => Some(message),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => Some(Err(IpcError::PeerClosed)),
        }
    }

    /// `VmPeak` of this process in KiB, from the kernel, if it can be read.
    pub fn vm_peak_kib(&self) -> Option<u64> {
        let status = std::fs::read_to_string(format!("/proc/{}/status", self.child.id())).ok()?;
        status
            .lines()
            .find_map(|line| line.strip_prefix("VmPeak:"))?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    /// Send a script that is expected to kill the process, and wait for it to
    /// die, polling the kernel's address-space high-water mark as it goes.
    /// Panics if the process answers (it should not) or outlives `bound`.
    pub fn run_until_death(&mut self, source: &str, bound: Duration) -> Death {
        let started = Instant::now();
        self.send_script(source);
        let mut peak_kib = 0;
        loop {
            if let Some(peak) = self.vm_peak_kib() {
                peak_kib = peak_kib.max(peak);
            }
            match self.child.try_wait() {
                Ok(Some(code)) => {
                    return Death {
                        code,
                        peak_kib,
                        elapsed: started.elapsed(),
                    };
                }
                Ok(None) => {}
                Err(error) => panic!("try_wait: {error}"),
            }
            assert!(
                started.elapsed() < bound,
                "process still alive after {bound:?} running {source:?}"
            );
            // A reply means the script ended without killing the process.
            if let Some(Ok(reply)) = self.recv_within(Duration::from_millis(5)) {
                panic!("expected death, got {reply:?} for {source:?}");
            }
        }
    }
}
