//! The broker's side of the script protocol (Phase 10, ADR 040, ADR 009).
//!
//! A [`ScriptSession`] is one sandboxed content process that runs script on the
//! broker's behalf. The broker links **no engine**: it sends [`ScriptRequest`]s
//! and reads [`ScriptResponse`]s, which are plain bounded data from
//! `px-ipc`. A test enforces that no `boa_*` crate is in this crate's graph.
//!
//! # What the content process is told, and what it is believed about
//!
//! It is told one thing about itself: a seed ([`ScriptRequest::Init`]), drawn
//! here from [`fresh_seed`]. No identity, origin or tab id goes down the wire,
//! and none is read back: which process a response came from is a fact about
//! the pipe it arrived on (invariant 9). The opening exchange the content
//! process initiates (`Ping`, `Echo`, `FrameHost`) is dispatched through the
//! broker's capability table under this session's channel like any other
//! request, so a hostile process asking about frames it was not given learns
//! nothing here either.
//!
//! # Every failure closes the session
//!
//! The content process runs arbitrary script and may be compromised by it, so
//! everything it can do other than answer correctly is treated the same way:
//! a malformed or oversized frame, a response of the wrong variant, a second
//! answer to one question, a response flood, silence past the deadline, a
//! closed pipe. The session is marked dead, the child is killed, its channel is
//! closed (its frames and capabilities go with it, nobody else's are touched)
//! and the call returns [`ScriptOutcome::ProcessDied`] or
//! [`ScriptOutcome::Unresponsive`]. The broker keeps running. A replacement is
//! made with [`ScriptSession::restart`], under the same bound as
//! [`crate::ContentProcess`].
//!
//! # Bounds
//!
//! Memory: both queues hold [`crate::QUEUE_DEPTH`] frames, each at most
//! `MAX_MESSAGE_BYTES`, and every string a response carries is a
//! `BoundedStr` capped far below that. A peer that floods is stopped by the
//! reader thread blocking on a full queue and is then found out by the next
//! call (see [`ScriptSession::run_script`]). Time: every wait has a deadline.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use px_ipc::script::{ScriptRequest, ScriptResponse, ScriptResult, ScriptSource, TerminateReason};
use px_ipc::{Channel, IpcError, Request};

use crate::{Broker, ChannelId, MAX_RESTARTS, QUEUE_DEPTH};

/// Requests the content process makes of the broker before it serves script:
/// `Ping`, `Echo`, `FrameHost`. After exactly this many, everything it sends is
/// a [`ScriptResponse`].
const OPENING_REQUESTS: usize = 3;

/// How long the opening exchange may take, in total. Generous: it covers the
/// process starting under the sandbox profile on a loaded machine.
pub const OPENING_DEADLINE: Duration = Duration::from_secs(30);

/// What one [`ScriptSession::run_script`] came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptOutcome {
    /// The completion value, as text (at most `MAX_SCRIPT_RESULT_BYTES`).
    Value(String),
    /// An uncaught script error, as data (at most `MAX_SCRIPT_ERROR_BYTES`).
    Error(String),
    /// The host stopped script in this process for good. The session is dead.
    Terminated(TerminateReason),
    /// The process is gone, or did something that ends a session: it closed
    /// its pipe, crashed, aborted (the watchdog), or broke the protocol. The
    /// session is dead.
    ProcessDied,
    /// The deadline passed with no answer. The process was killed and the
    /// session is dead.
    Unresponsive,
    /// The source exceeds `MAX_SCRIPT_SOURCE_BYTES`. Nothing was sent and the
    /// session is unchanged.
    Rejected,
}

/// Why a session is dead, for logs and tests. Never sent anywhere.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DeathCause {
    /// The peer closed its end or the process exited.
    PeerClosed,
    /// A frame that did not decode, a response of the wrong variant, or more
    /// answers than questions.
    Protocol,
    /// No answer within the deadline.
    Unresponsive,
    /// The peer stopped reading what the broker sent.
    NotDraining,
    /// The host reported script stopped for good.
    ScriptTerminated,
    /// The broker ended it.
    Killed,
}

/// What the reader thread hands over.
enum Inbound {
    /// One of the opening requests.
    Request(Request),
    /// A script response.
    Script(ScriptResponse),
    /// The stream ended or failed; the reader has exited.
    Failed(IpcError),
}

/// The thing to reap when a session dies: a real child, or nothing (tests that
/// stand in for the process with in-memory pipes).
enum Reaper {
    Child(px_sandbox::SandboxedChild),
    #[cfg(test)]
    None,
}

impl Reaper {
    fn kill(&mut self) {
        match self {
            Self::Child(child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            #[cfg(test)]
            Self::None => {}
        }
    }

    fn id(&self) -> Option<u32> {
        match self {
            Self::Child(child) => Some(child.id()),
            #[cfg(test)]
            Self::None => None,
        }
    }

    fn applied(&self) -> Vec<px_sandbox::Rung> {
        match self {
            Self::Child(child) => child.applied().to_vec(),
            #[cfg(test)]
            Self::None => Vec::new(),
        }
    }
}

/// A sandboxed content process running script for the broker.
pub struct ScriptSession {
    reaper: Reaper,
    id: ChannelId,
    executable: PathBuf,
    inbound: Option<Receiver<Inbound>>,
    outbound: Option<SyncSender<Vec<u8>>>,
    dead: Option<DeathCause>,
    restarts: u32,
}

impl ScriptSession {
    /// Spawn a content process **under the sandbox profile**, serve its opening
    /// exchange and seed it.
    ///
    /// Fails closed exactly as [`crate::ContentProcess::spawn`] does: if the
    /// machine does not clear the sandbox floor, no process exists. If the
    /// opening exchange fails, the process is killed and its channel closed
    /// before the error returns.
    pub fn spawn(broker: &mut Broker, executable: &Path) -> std::io::Result<Self> {
        let mut child = px_sandbox::spawn(executable)
            .map_err(|refusal| std::io::Error::other(refusal.to_string()))?;
        let stdout = child.take_stdout().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "child has no stdout")
        })?;
        let stdin = child.take_stdin().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "child has no stdin")
        })?;
        Self::attach(
            broker,
            Reaper::Child(child),
            executable,
            stdout,
            stdin,
            OPENING_DEADLINE,
        )
    }

    /// Wire up the transport and run the opening exchange.
    fn attach<R, W>(
        broker: &mut Broker,
        reaper: Reaper,
        executable: &Path,
        reader: R,
        writer: W,
        opening: Duration,
    ) -> std::io::Result<Self>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let mut reaper = reaper;
        let Some(id) = broker.open_channel() else {
            reaper.kill();
            return Err(std::io::Error::other(
                "channel identifiers exhausted; refusing to reuse one",
            ));
        };

        let (inbound_tx, inbound) = sync_channel::<Inbound>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut channel = Channel::new(BufReader::new(reader), std::io::sink());
            let mut seen = 0usize;
            loop {
                let message = if seen < OPENING_REQUESTS {
                    seen += 1;
                    channel.recv::<Request>().map(Inbound::Request)
                } else {
                    channel.recv::<ScriptResponse>().map(Inbound::Script)
                };
                let (item, fatal) = match message {
                    Ok(item) => (item, false),
                    Err(error) => (Inbound::Failed(error), true),
                };
                if inbound_tx.send(item).is_err() || fatal {
                    return;
                }
            }
        });

        let (outbound, frames) = sync_channel::<Vec<u8>>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut writer = BufWriter::new(writer);
            while let Ok(frame) = frames.recv() {
                if px_ipc::write_frame(&mut writer, &frame).is_err() {
                    return;
                }
            }
        });

        let mut session = Self {
            reaper,
            id,
            executable: executable.to_path_buf(),
            inbound: Some(inbound),
            outbound: Some(outbound),
            dead: None,
            restarts: 0,
        };
        if let Err(cause) = session.open(broker, opening) {
            session.die(broker, cause);
            return Err(std::io::Error::other(format!(
                "the content process failed its opening exchange: {cause:?}"
            )));
        }
        Ok(session)
    }

    /// Serve the three opening requests through the capability table, then send
    /// the seed.
    fn open(&mut self, broker: &mut Broker, total: Duration) -> Result<(), DeathCause> {
        let started = Instant::now();
        for _ in 0..OPENING_REQUESTS {
            let left = total.saturating_sub(started.elapsed());
            let request = match self.receive(left)? {
                Inbound::Request(request) => request,
                _ => return Err(DeathCause::Protocol),
            };
            let decision = broker.dispatch_guarded(self.id, request);
            if !broker.is_open(self.id) {
                return Err(DeathCause::Protocol);
            }
            let frame =
                px_ipc::encode_frame(&decision.response).map_err(|_| DeathCause::Protocol)?;
            self.send_frame(frame)?;
        }
        let init = ScriptRequest::Init { seed: fresh_seed() };
        let frame = px_ipc::encode_frame(&init).map_err(|_| DeathCause::Protocol)?;
        self.send_frame(frame)
    }

    /// This session's channel.
    pub fn channel_id(&self) -> ChannelId {
        self.id
    }

    /// Why the session is dead, or `None` while it is live.
    pub fn death(&self) -> Option<DeathCause> {
        self.dead
    }

    /// Whether the session can still run script.
    pub fn is_live(&self) -> bool {
        self.dead.is_none()
    }

    /// The operating-system process id, for diagnostics and for tests that read
    /// the kernel's own account of the process's confinement. Never sent to
    /// anyone.
    pub fn process_id(&self) -> Option<u32> {
        self.reaper.id()
    }

    /// The sandbox rungs the process was created under.
    pub fn applied_rungs(&self) -> Vec<px_sandbox::Rung> {
        self.reaper.applied()
    }

    /// How many times this session has been replaced.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }

    /// Run `source` in the content process and wait at most `deadline` for the
    /// answer.
    ///
    /// There is no correlation id on the wire, so the protocol is strictly one
    /// question, one answer. Before sending, anything already queued is
    /// examined: an unsolicited `Terminated` is honoured (script is over), and
    /// anything else is more answers than questions, which ends the session. A
    /// peer that floods therefore cannot stay in sync with a broker that
    /// counts.
    pub fn run_script(
        &mut self,
        broker: &mut Broker,
        source: &str,
        deadline: Duration,
    ) -> ScriptOutcome {
        if self.dead.is_some() {
            return ScriptOutcome::ProcessDied;
        }
        let Ok(source) = ScriptSource::new(source.to_owned()) else {
            return ScriptOutcome::Rejected;
        };
        if let Some(outcome) = self.drain_unsolicited(broker) {
            return outcome;
        }
        let request = ScriptRequest::RunScript { source };
        let sent = px_ipc::encode_frame(&request)
            .map_err(|_| DeathCause::Protocol)
            .and_then(|frame| self.send_frame(frame));
        if let Err(cause) = sent {
            return self.fail(broker, cause);
        }
        let message = match self.receive(deadline) {
            Ok(message) => message,
            Err(cause) => return self.fail(broker, cause),
        };
        let result = match message {
            Inbound::Script(ScriptResponse::Result(result)) => result,
            // A request after the opening, or a response variant that is not
            // an answer to RunScript (including the testing-only ones).
            _ => return self.fail(broker, DeathCause::Protocol),
        };
        match result {
            ScriptResult::Value { text } => {
                if self.has_extra_answer() {
                    return self.fail(broker, DeathCause::Protocol);
                }
                ScriptOutcome::Value(text.into_string())
            }
            ScriptResult::Error { message } => {
                if self.has_extra_answer() {
                    return self.fail(broker, DeathCause::Protocol);
                }
                ScriptOutcome::Error(message.into_string())
            }
            ScriptResult::Terminated { reason } => {
                self.die(broker, DeathCause::ScriptTerminated);
                ScriptOutcome::Terminated(reason)
            }
        }
    }

    /// Send a request other than `Init` and `RunScript` (which have their own
    /// paths). Exists for the testing-only vocabulary; the shipping vocabulary
    /// has nothing else to send.
    pub fn send_extra(
        &mut self,
        broker: &mut Broker,
        request: &ScriptRequest,
    ) -> Result<(), DeathCause> {
        if self.dead.is_some() {
            return Err(DeathCause::Killed);
        }
        if matches!(
            request,
            ScriptRequest::Init { .. } | ScriptRequest::RunScript { .. }
        ) {
            return Err(DeathCause::Protocol);
        }
        let sent = px_ipc::encode_frame(request)
            .map_err(|_| DeathCause::Protocol)
            .and_then(|frame| self.send_frame(frame));
        if let Err(cause) = sent {
            self.die(broker, cause);
        }
        sent
    }

    /// Kill the process and close the session. Idempotent.
    pub fn kill(&mut self, broker: &mut Broker) {
        self.die(broker, DeathCause::Killed);
    }

    /// Replace the process with a fresh one under a new channel, within
    /// [`MAX_RESTARTS`]. The old session is closed only after the new one has
    /// started, so a failed spawn leaves the old session as it was.
    pub fn restart(&mut self, broker: &mut Broker) -> std::io::Result<()> {
        if self.restarts >= MAX_RESTARTS {
            return Err(std::io::Error::other(
                "content process restarted too many times; refusing to continue",
            ));
        }
        let executable = self.executable.clone();
        let attempts = self.restarts.saturating_add(1);
        let mut replacement = Self::spawn(broker, &executable)?;
        replacement.restarts = attempts;
        self.die(broker, DeathCause::Killed);
        *self = replacement;
        Ok(())
    }

    // ---- internals -------------------------------------------------------

    /// Wait up to `timeout` for one message.
    fn receive(&mut self, timeout: Duration) -> Result<Inbound, DeathCause> {
        let Some(inbound) = self.inbound.as_ref() else {
            return Err(DeathCause::PeerClosed);
        };
        match inbound.recv_timeout(timeout) {
            Ok(Inbound::Failed(IpcError::PeerClosed)) => Err(DeathCause::PeerClosed),
            Ok(Inbound::Failed(_)) => Err(DeathCause::Protocol),
            Ok(message) => Ok(message),
            Err(RecvTimeoutError::Timeout) => Err(DeathCause::Unresponsive),
            Err(RecvTimeoutError::Disconnected) => Err(DeathCause::PeerClosed),
        }
    }

    /// Queue an already-encoded frame without blocking.
    fn send_frame(&mut self, frame: Vec<u8>) -> Result<(), DeathCause> {
        let Some(outbound) = self.outbound.as_ref() else {
            return Err(DeathCause::PeerClosed);
        };
        match outbound.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(DeathCause::NotDraining),
            Err(TrySendError::Disconnected(_)) => Err(DeathCause::PeerClosed),
        }
    }

    /// Whether something that can only be a second answer is already waiting.
    /// A queued `Terminated` is not one: it is a legitimate notice and the next
    /// call consumes it.
    fn has_extra_answer(&self) -> bool {
        let Some(inbound) = self.inbound.as_ref() else {
            return false;
        };
        match inbound.try_recv() {
            Ok(Inbound::Script(ScriptResponse::Result(ScriptResult::Terminated { .. }))) => false,
            Ok(Inbound::Failed(IpcError::PeerClosed)) => false,
            Ok(_) => true,
            Err(_) => false,
        }
    }

    /// Examine anything queued before asking a new question.
    fn drain_unsolicited(&mut self, broker: &mut Broker) -> Option<ScriptOutcome> {
        let queued = self.inbound.as_ref()?.try_recv().ok()?;
        match queued {
            Inbound::Script(ScriptResponse::Result(ScriptResult::Terminated { reason })) => {
                self.die(broker, DeathCause::ScriptTerminated);
                Some(ScriptOutcome::Terminated(reason))
            }
            Inbound::Failed(IpcError::PeerClosed) => {
                Some(self.fail(broker, DeathCause::PeerClosed))
            }
            _ => Some(self.fail(broker, DeathCause::Protocol)),
        }
    }

    fn fail(&mut self, broker: &mut Broker, cause: DeathCause) -> ScriptOutcome {
        self.die(broker, cause);
        match cause {
            DeathCause::Unresponsive => ScriptOutcome::Unresponsive,
            _ => ScriptOutcome::ProcessDied,
        }
    }

    /// Mark dead, kill the child, release the workers and close the channel.
    fn die(&mut self, broker: &mut Broker, cause: DeathCause) {
        if self.dead.is_none() {
            self.dead = Some(cause);
        }
        self.reaper.kill();
        self.inbound = None;
        self.outbound = None;
        broker.close_channel(self.id);
    }
}

impl Drop for ScriptSession {
    fn drop(&mut self) {
        self.reaper.kill();
    }
}

/// A seed for a content process's clock jitter and `Math.random`.
///
/// **Best-effort entropy, not a CSPRNG.** `std` has no randomness API without
/// `unsafe` or a dependency. The nearest honest source is
/// [`RandomState`], whose keys the standard library draws from the operating
/// system once per thread and then increments per instance; this mixes in the
/// wall clock, the pid and a process-wide counter so two seeds never coincide.
/// What the seed protects against is script predicting its own coarse-clock
/// jitter and `Math.random` across tabs; a seed that is guessable is a weaker
/// timer defence, not a break of confinement. If a CSPRNG source is ever
/// admitted (ADR needed), it replaces this function and nothing else.
pub fn fresh_seed() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut hasher = RandomState::new().build_hasher();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    hasher.write_u128(nanos);
    hasher.write_u32(std::process::id());
    hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_ipc::Response;
    use px_ipc::script::{ErrorText, ResultText};
    use std::sync::mpsc::{Receiver, Sender, channel};

    const SHORT: Duration = Duration::from_millis(300);
    const PATIENT: Duration = Duration::from_secs(20);

    /// The read half the session sees: bytes the fake peer wrote.
    struct PipeReader {
        rx: Receiver<Vec<u8>>,
        buf: Vec<u8>,
        at: usize,
    }

    impl PipeReader {
        fn new(rx: Receiver<Vec<u8>>) -> Self {
            Self {
                rx,
                buf: Vec::new(),
                at: 0,
            }
        }
    }

    impl Read for PipeReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            while self.at >= self.buf.len() {
                match self.rx.recv() {
                    Ok(chunk) => {
                        self.buf = chunk;
                        self.at = 0;
                    }
                    Err(_) => return Ok(0),
                }
            }
            let n = out.len().min(self.buf.len() - self.at);
            out[..n].copy_from_slice(&self.buf[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }

    /// The write half the session uses: bytes the fake peer reads.
    struct PipeWriter(Sender<Vec<u8>>);

    impl Write for PipeWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0
                .send(bytes.to_vec())
                .map_err(|_| std::io::ErrorKind::BrokenPipe)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// What a fake content process gets: typed access to its end, and raw
    /// bytes for the hostile cases.
    struct Fake {
        to_broker: Sender<Vec<u8>>,
        from_broker: Channel<PipeReader, std::io::Sink>,
    }

    impl Fake {
        fn raw(&self, bytes: &[u8]) {
            let _ = self.to_broker.send(bytes.to_vec());
        }
        fn frame<T: px_ipc::Message>(&self, message: &T) {
            self.raw(&px_ipc::encode_frame(message).expect("encode"));
        }
        /// Play the opening exchange as the real content process does.
        fn opening(&mut self) {
            for request in [
                Request::Ping,
                Request::Echo {
                    payload: b"x".to_vec(),
                },
                Request::FrameHost {
                    frame: px_ipc::FrameId::new(0, 0),
                },
            ] {
                self.frame(&request);
                let _ = self.from_broker.recv::<Response>();
            }
        }
        fn expect_init(&mut self) {
            assert!(matches!(
                self.from_broker.recv::<ScriptRequest>().expect("init"),
                ScriptRequest::Init { .. }
            ));
        }
        fn expect_run(&mut self) {
            assert!(matches!(
                self.from_broker.recv::<ScriptRequest>().expect("run"),
                ScriptRequest::RunScript { .. }
            ));
        }
        fn answer(&self, text: &str) {
            self.frame(&ScriptResponse::Result(ScriptResult::Value {
                text: ResultText::new(text.into()).expect("short"),
            }));
        }
    }

    /// A session against a fake peer running `behave` on its own thread.
    fn session_with<F>(broker: &mut Broker, behave: F) -> ScriptSession
    where
        F: FnOnce(Fake) + Send + 'static,
    {
        let (to_broker, from_fake) = channel::<Vec<u8>>();
        let (to_fake, from_broker) = channel::<Vec<u8>>();
        let fake = Fake {
            to_broker,
            from_broker: Channel::new(PipeReader::new(from_broker), std::io::sink()),
        };
        std::thread::spawn(move || behave(fake));
        ScriptSession::attach(
            broker,
            Reaper::None,
            Path::new("fake"),
            PipeReader::new(from_fake),
            PipeWriter(to_fake),
            PATIENT,
        )
        .expect("opening exchange")
    }

    fn assert_dead(session: &ScriptSession, broker: &Broker, cause: DeathCause, other: ChannelId) {
        assert_eq!(session.death(), Some(cause));
        assert!(!broker.is_open(session.channel_id()), "channel closed");
        assert!(broker.is_open(other), "another channel is untouched");
    }

    #[test]
    fn script_broker_hostile_a_well_behaved_peer_gets_a_value() {
        let mut broker = Broker::new();
        let mut session = session_with(&mut broker, |mut fake| {
            fake.opening();
            fake.expect_init();
            fake.expect_run();
            fake.answer("2");
            fake.expect_run();
            fake.frame(&ScriptResponse::Result(ScriptResult::Error {
                message: ErrorText::new("boom".into()).expect("short"),
            }));
        });
        assert_eq!(
            session.run_script(&mut broker, "1+1", PATIENT),
            ScriptOutcome::Value("2".into())
        );
        assert_eq!(
            session.run_script(&mut broker, "x", PATIENT),
            ScriptOutcome::Error("boom".into())
        );
        assert!(session.is_live());
    }

    #[test]
    fn script_broker_hostile_the_seed_is_sent_first_and_carries_no_identity() {
        let mut broker = Broker::new();
        let (tx, rx) = channel();
        let _session = session_with(&mut broker, move |mut fake| {
            fake.opening();
            let _ = tx.send(fake.from_broker.recv::<ScriptRequest>());
        });
        match rx.recv_timeout(PATIENT).expect("sent") {
            Ok(ScriptRequest::Init { .. }) => {}
            other => panic!("the first message must be Init, got {other:?}"),
        }
    }

    #[test]
    fn script_broker_hostile_seeds_differ() {
        let a = fresh_seed();
        let b = fresh_seed();
        let c = fresh_seed();
        assert!(a != b && b != c && a != c);
    }

    /// Run one hostile behaviour and check the whole fail-closed contract.
    fn hostile<F>(behave: F, expect: ScriptOutcome, cause: DeathCause, deadline: Duration)
    where
        F: FnOnce(Fake) + Send + 'static,
    {
        let mut broker = Broker::new();
        let bystander = broker.open_channel().expect("channel");
        let frame = broker.create_frame(bystander).expect("frame");
        let mut session = session_with(&mut broker, move |mut fake| {
            fake.opening();
            fake.expect_init();
            fake.expect_run();
            behave(fake);
        });
        assert_eq!(session.run_script(&mut broker, "1+1", deadline), expect);
        assert_dead(&session, &broker, cause, bystander);
        // The frame tree is untouched, and the dead session is not served.
        assert!(broker.holds_frame(bystander, frame));
        assert_eq!(
            session.run_script(&mut broker, "1+1", deadline),
            ScriptOutcome::ProcessDied
        );
    }

    #[test]
    fn script_broker_hostile_garbage_bytes_close_the_session() {
        hostile(
            |fake| fake.raw(&[0xFF; 64]),
            ScriptOutcome::ProcessDied,
            DeathCause::Protocol,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_an_oversized_frame_is_refused_without_reading_it() {
        // A length prefix far over the ceiling and no body at all.
        hostile(
            |fake| {
                let huge = u32::try_from(px_ipc::MAX_MESSAGE_BYTES * 8).expect("fits");
                fake.raw(&huge.to_le_bytes());
            },
            ScriptOutcome::ProcessDied,
            DeathCause::Protocol,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_a_string_over_its_field_cap_is_refused() {
        // A Value claiming one byte over MAX_SCRIPT_RESULT_BYTES, bytes present,
        // inside a frame the codec itself accepts.
        hostile(
            |fake| {
                let n = px_ipc::script::MAX_SCRIPT_RESULT_BYTES + 1;
                // Result (variant 0), Value (variant 0), varint length, bytes.
                let mut body = vec![0u8, 0u8];
                let mut length = n;
                loop {
                    let byte = (length & 0x7f) as u8;
                    length >>= 7;
                    if length == 0 {
                        body.push(byte);
                        break;
                    }
                    body.push(byte | 0x80);
                }
                body.extend(std::iter::repeat_n(b'a', n));
                let mut frame = u32::try_from(body.len() + 1)
                    .expect("fits")
                    .to_le_bytes()
                    .to_vec();
                frame.push(<ScriptResponse as px_ipc::Message>::TAG);
                frame.extend(body);
                fake.raw(&frame);
            },
            ScriptOutcome::ProcessDied,
            DeathCause::Protocol,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_a_reflected_request_is_refused() {
        hostile(
            |fake| {
                fake.frame(&ScriptRequest::Init { seed: 1 });
            },
            ScriptOutcome::ProcessDied,
            DeathCause::Protocol,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_a_wrong_direction_message_is_refused() {
        // A Request where a ScriptResponse belongs.
        hostile(
            |fake| fake.frame(&Request::Ping),
            ScriptOutcome::ProcessDied,
            DeathCause::Protocol,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_a_peer_that_closes_its_pipe_is_dead() {
        hostile(
            drop,
            ScriptOutcome::ProcessDied,
            DeathCause::PeerClosed,
            PATIENT,
        );
    }

    #[test]
    fn script_broker_hostile_a_silent_peer_hits_the_deadline() {
        let started = Instant::now();
        hostile(
            |fake| {
                // Stay alive and say nothing until well after the deadline.
                std::thread::sleep(Duration::from_secs(3));
                drop(fake);
            },
            ScriptOutcome::Unresponsive,
            DeathCause::Unresponsive,
            SHORT,
        );
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn script_broker_hostile_half_a_frame_then_silence_hits_the_deadline() {
        hostile(
            |fake| {
                fake.raw(&[200, 0, 0, 0, 0xB2, 0]);
                std::thread::sleep(Duration::from_secs(3));
                drop(fake);
            },
            ScriptOutcome::Unresponsive,
            DeathCause::Unresponsive,
            SHORT,
        );
    }

    #[test]
    fn script_broker_hostile_two_answers_to_one_question_end_the_session() {
        // The second answer is queued behind the first; the broker notices on
        // the call that consumed the first or, at the latest, the next.
        let mut broker = Broker::new();
        let mut session = session_with(&mut broker, |mut fake| {
            fake.opening();
            fake.expect_init();
            fake.expect_run();
            fake.answer("1");
            fake.answer("2");
            std::thread::sleep(Duration::from_secs(5));
            drop(fake);
        });
        std::thread::sleep(Duration::from_millis(200));
        let first = session.run_script(&mut broker, "1", PATIENT);
        assert!(matches!(
            first,
            ScriptOutcome::ProcessDied | ScriptOutcome::Value(_)
        ));
        let second = session.run_script(&mut broker, "2", PATIENT);
        assert_eq!(second, ScriptOutcome::ProcessDied);
        assert_eq!(session.death(), Some(DeathCause::Protocol));
    }

    #[test]
    fn script_broker_hostile_a_response_flood_is_bounded_and_closes_the_session() {
        let mut broker = Broker::new();
        let mut session = session_with(&mut broker, move |mut fake| {
            fake.opening();
            fake.expect_init();
            // Answers nobody asked for, as fast as the pipe takes them.
            let frame = px_ipc::encode_frame(&ScriptResponse::Result(ScriptResult::Value {
                text: ResultText::new("a".repeat(8000)).expect("short"),
            }))
            .expect("encode");
            for _ in 0..20_000 {
                fake.raw(&frame);
            }
        });
        std::thread::sleep(Duration::from_millis(300));
        let started = Instant::now();
        let outcome = session.run_script(&mut broker, "1+1", PATIENT);
        assert_eq!(outcome, ScriptOutcome::ProcessDied);
        assert_eq!(session.death(), Some(DeathCause::Protocol));
        assert!(started.elapsed() < Duration::from_secs(5));
        // The reader holds at most QUEUE_DEPTH decoded messages, so memory is
        // bounded by that, not by how many frames the peer wrote.
        assert!(!broker.is_open(session.channel_id()));
    }

    #[test]
    fn script_broker_hostile_an_unsolicited_terminated_is_honoured_and_final() {
        let mut broker = Broker::new();
        let mut session = session_with(&mut broker, |mut fake| {
            fake.opening();
            fake.expect_init();
            fake.frame(&ScriptResponse::Result(ScriptResult::Terminated {
                reason: TerminateReason::TimerOverflow,
            }));
            std::thread::sleep(Duration::from_secs(2));
            drop(fake);
        });
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            session.run_script(&mut broker, "1", PATIENT),
            ScriptOutcome::Terminated(TerminateReason::TimerOverflow)
        );
        assert_eq!(session.death(), Some(DeathCause::ScriptTerminated));
    }

    #[test]
    fn script_broker_hostile_an_oversized_source_is_rejected_and_nothing_is_sent() {
        let mut broker = Broker::new();
        let mut session = session_with(&mut broker, |mut fake| {
            fake.opening();
            fake.expect_init();
            fake.expect_run();
            fake.answer("ok");
        });
        let too_big = "1".repeat(px_ipc::script::MAX_SCRIPT_SOURCE_BYTES + 1);
        assert_eq!(
            session.run_script(&mut broker, &too_big, PATIENT),
            ScriptOutcome::Rejected
        );
        assert!(session.is_live(), "a refused source costs nothing");
        assert_eq!(
            session.run_script(&mut broker, "1", PATIENT),
            ScriptOutcome::Value("ok".into())
        );
    }

    #[test]
    fn script_broker_hostile_a_peer_that_never_opens_fails_the_spawn() {
        let (to_broker, from_fake) = channel::<Vec<u8>>();
        let (to_fake, _from_broker) = channel::<Vec<u8>>();
        let mut broker = Broker::new();
        let started = Instant::now();
        let result = ScriptSession::attach(
            &mut broker,
            Reaper::None,
            Path::new("fake"),
            PipeReader::new(from_fake),
            PipeWriter(to_fake),
            SHORT,
        );
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_eq!(broker.channel_count(), 0, "a failed open leaks no channel");
        drop(to_broker);
    }

    #[test]
    fn script_broker_hostile_the_opening_goes_through_the_capability_table() {
        // A peer asking about a frame it was not given is denied and audited
        // under its channel, exactly as for any other content process.
        let mut broker = Broker::new();
        let session = session_with(&mut broker, |mut fake| {
            fake.opening();
            fake.expect_init();
        });
        let denied: Vec<_> = broker.audit_log().collect();
        assert_eq!(denied.len(), 1);
        assert_eq!(denied[0].0, session.channel_id());
    }
}
