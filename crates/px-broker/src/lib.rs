#![forbid(unsafe_code)]

//! Parent process, capability broker, policy, frame tree.
//!
//! # The one rule this crate exists to enforce
//!
//! [`Broker::dispatch`] takes the channel as a separate argument from the
//! message:
//!
//! ```ignore
//! fn dispatch(&mut self, channel: ChannelId, request: Request) -> Decision
//! ```
//!
//! That signature is invariant 9. The caller supplies the channel from the
//! connection it read the bytes off; the request cannot supply it, because
//! [`Request`] has no such field. There is no code path where a message's
//! contents influence who the broker thinks sent it, so the confused-deputy
//! class has nowhere to live.
//!
//! [`ChannelId`] is deliberately not serialisable. It cannot be put on the
//! wire even by accident.
//!
//! # The broker never blocks on a content process
//!
//! An adversarial review found three ways a hostile child wedged the parent
//! forever, all of them cheaper for an attacker than crashing:
//!
//! - never read its stdin, so the broker's `write_all` blocks on a full pipe;
//! - write half a frame and stop, so the broker's `read_exact` never returns;
//! - spawn a grandchild holding the same stdout, then exit — so `try_wait`
//!   reports the child **dead** while the read blocks forever.
//!
//! None of them is a panic, so `catch_unwind` does nothing about any of them.
//! The answer is that all blocking IO happens on worker threads owned by
//! [`ContentProcess`], and every broker-side wait has a deadline. See
//! [`ContentProcess::next_request`].

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::time::Duration;

use px_ipc::{Channel, FrameHost, FrameId, IpcError, Request, Response};

/// Messages queued in either direction before a peer is considered
/// uncooperative.
///
/// Bounded on purpose. An unbounded queue in front of a peer that never reads
/// is a memory leak whose size the attacker chooses; a full queue is evidence
/// the peer has stopped cooperating, which is actionable.
const QUEUE_DEPTH: usize = 32;

/// Which connection a message arrived on.
///
/// Broker-internal and not `Serialize`: authority comes from the channel, so a
/// channel identifier that could travel in a message would be an identity
/// claim waiting to be believed.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ChannelId(u64);

impl ChannelId {
    /// For logs and test assertions only.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// Why the broker refused, for the audit log.
///
/// Broker-side only. This deliberately does **not** appear in [`Response`]:
/// distinguishing "no such frame" from "not yours" on the wire let a hostile
/// content process enumerate the live frames of every other site.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// The channel does not hold the frame it named.
    NotYourFrame,
    /// The frame does not exist, or its slot has been reused since.
    NoSuchFrame,
    /// The channel is not open.
    UnknownChannel,
    /// The broker failed while handling the request and refused rather than
    /// guessing.
    Internal,
}

/// What the broker decided, and what the audit log should record about it.
///
/// Two fields because the peer and the log are owed different things: the peer
/// gets the least information that lets it proceed, the log gets all of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    /// What goes on the wire.
    pub response: Response,
    /// Why, if this was a refusal. Never sent.
    pub audit: Option<DenyReason>,
}

impl Decision {
    fn allow(response: Response) -> Self {
        Self {
            response,
            audit: None,
        }
    }

    fn deny(reason: DenyReason) -> Self {
        Self {
            response: Response::Denied,
            audit: Some(reason),
        }
    }
}

/// One frame's slot in the tree.
#[derive(Debug)]
struct FrameSlot {
    /// Bumped every time the slot is reused. A stale [`FrameId`] carries the
    /// old value and stops resolving.
    generation: u32,
    /// `None` once the frame is gone and before the slot is reused.
    owner: Option<ChannelId>,
    /// Retired slots are never handed out again. §14.3: on generation
    /// overflow, retire rather than wrap — wrapping makes a stale handle valid
    /// again, which is the bug the generation exists to prevent.
    retired: bool,
}

/// The frame tree, and the authority over it.
///
/// Flat in Phase 1. Parent/child structure arrives with real navigation; what
/// this phase needs is that a `FrameId` resolves through the broker and may
/// name a frame in another process, so that Phase 14 is a change of answer
/// rather than a change of shape.
#[derive(Debug, Default)]
pub struct FrameTree {
    slots: Vec<FrameSlot>,
}

impl FrameTree {
    /// Create a frame owned by `owner`, or `None` if no handle can be minted.
    ///
    /// Returns `Option` for the same reason every accessor here does. The
    /// failure needs more than `u32::MAX` live slots and is remote — but the
    /// alternative was `u32::try_from(index).unwrap_or(u32::MAX)`, which
    /// silently mints the *same* `FrameId` for two different slots. In a
    /// function whose entire purpose is that handles are unambiguous, that is
    /// capability confusion rather than a rounding error.
    pub fn create(&mut self, owner: ChannelId) -> Option<FrameId> {
        // Reuse a free, non-retired slot before growing.
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if slot.owner.is_none() && !slot.retired {
                match slot.generation.checked_add(1) {
                    Some(next) => {
                        let index = u32::try_from(index).ok()?;
                        slot.generation = next;
                        slot.owner = Some(owner);
                        return Some(FrameId::new(index, slot.generation));
                    }
                    None => {
                        // §14.3: retire permanently rather than wrap.
                        slot.retired = true;
                    }
                }
            }
        }
        // Compute the index before pushing, so a tree too large to address is
        // refused rather than grown into a state that cannot be described.
        let index = u32::try_from(self.slots.len()).ok()?;
        self.slots.push(FrameSlot {
            generation: 0,
            owner: Some(owner),
            retired: false,
        });
        Some(FrameId::new(index, 0))
    }

    /// Destroy a frame, if `owner` is the channel that holds it.
    ///
    /// The owner argument is not decoration. `create` takes one and an earlier
    /// `destroy` did not, so the moment a `Request::DestroyFrame` arrives the
    /// obvious one-line implementation would let any channel free any other
    /// channel's frame — and it would read as correct. Closing the asymmetry
    /// costs nothing now and removes the trap.
    pub fn destroy(&mut self, owner: ChannelId, frame: FrameId) -> bool {
        match self.slot_mut(frame) {
            Some(slot) if slot.owner == Some(owner) => {
                slot.owner = None;
                true
            }
            _ => false,
        }
    }

    /// Owner of a live frame, or `None` if the handle is stale, forged, or the
    /// frame is gone.
    ///
    /// Returns `Option`, like every accessor over a generational arena. There
    /// is no infallible variant, not even a private one.
    pub fn owner(&self, frame: FrameId) -> Option<ChannelId> {
        let index = usize::try_from(frame.index()).ok()?;
        let slot = self.slots.get(index)?;
        if slot.generation != frame.generation() {
            return None;
        }
        slot.owner
    }

    /// Drop every frame owned by a channel. Called when a process dies: a
    /// restarted process begins with zero authority.
    pub fn release_all(&mut self, channel: ChannelId) {
        for slot in &mut self.slots {
            if slot.owner == Some(channel) {
                slot.owner = None;
            }
        }
    }

    fn slot_mut(&mut self, frame: FrameId) -> Option<&mut FrameSlot> {
        let index = usize::try_from(frame.index()).ok()?;
        let slot = self.slots.get_mut(index)?;
        if slot.generation != frame.generation() {
            return None;
        }
        Some(slot)
    }
}

/// What a channel is permitted to do.
///
/// Keyed by channel in [`Broker::capabilities`], never carried in a message.
#[derive(Debug, Default)]
struct ChannelCaps {
    frames: HashSet<FrameId>,
}

/// The capability broker.
#[derive(Debug, Default)]
pub struct Broker {
    next_channel: u64,
    capabilities: HashMap<ChannelId, ChannelCaps>,
    frames: FrameTree,
    /// Makes `dispatch` panic, so that the `catch_unwind` in
    /// [`Broker::dispatch_guarded`] can be tested against a real unwind rather
    /// than a simulated one.
    ///
    /// `cfg(test)` rather than a feature flag: §14.4's rule is that test-only
    /// capabilities must not be able to reach a release artifact, and a field
    /// that does not exist outside `cargo test` cannot.
    #[cfg(test)]
    panic_on_dispatch: bool,
}

impl Broker {
    /// A broker with no channels.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new connection and return its identifier.
    ///
    /// Identifiers are never reused within a run, so a message that somehow
    /// outlives its channel cannot land on a successor. `checked_add` rather
    /// than `saturating_add`: saturating would hand the same id to two live
    /// processes and let each hold the other's authority — the exact confusion
    /// `FrameTree::create` refuses one screen up.
    pub fn open_channel(&mut self) -> Option<ChannelId> {
        let id = ChannelId(self.next_channel);
        self.next_channel = self.next_channel.checked_add(1)?;
        self.capabilities.insert(id, ChannelCaps::default());
        Some(id)
    }

    /// Tear down a channel and everything it held.
    ///
    /// Called on process death as well as on orderly shutdown, because from
    /// the broker's side those are the same event: the peer is gone and its
    /// authority goes with it.
    pub fn close_channel(&mut self, channel: ChannelId) {
        self.capabilities.remove(&channel);
        self.frames.release_all(channel);
    }

    /// Create a frame owned by a channel and grant that channel access to it.
    ///
    /// The channel is checked *before* the tree is touched. Creating first and
    /// validating second leaks a slot on every failure: the slot is left owned
    /// by a channel that will never be closed again, `FrameTree::create` skips
    /// owned slots, and `ChannelId`s are never reused — so nothing can ever
    /// reclaim it. A crash loop recreating frames against a cached channel id
    /// would leak one slot per iteration.
    pub fn create_frame(&mut self, channel: ChannelId) -> Option<FrameId> {
        if !self.capabilities.contains_key(&channel) {
            return None;
        }
        let frame = self.frames.create(channel)?;
        self.capabilities.get_mut(&channel)?.frames.insert(frame);
        Some(frame)
    }

    /// Whether a channel holds a frame. Fails closed: an unknown channel, a
    /// stale handle and a frame owned by somebody else are all `false`.
    pub fn holds_frame(&self, channel: ChannelId, frame: FrameId) -> bool {
        self.frames.owner(frame) == Some(channel)
            && self
                .capabilities
                .get(&channel)
                .is_some_and(|caps| caps.frames.contains(&frame))
    }

    /// How many channels are open. For tests and logging.
    pub fn channel_count(&self) -> usize {
        self.capabilities.len()
    }

    /// Handle one request, surviving a panic in the handling of it.
    ///
    /// §4.3: the broker unwinds and wraps IPC dispatch in `catch_unwind`,
    /// because it is the one process that may not die. A content process can
    /// be restarted; a dead broker takes every tab with it.
    ///
    /// After a caught panic the channel is closed rather than answered as if
    /// nothing happened: broker state may be inconsistent, which is what a
    /// panic mid-mutation means, and a panic while deciding a capability
    /// question is the least justified moment to keep serving that channel.
    ///
    /// `AssertUnwindSafe` is required because `&mut self` is not `UnwindSafe`,
    /// and that is not a formality being waived: it is the compiler pointing
    /// at the inconsistent-state problem. Closing the channel is the answer.
    ///
    /// Note what this does **not** do: it cannot stop the peer. `Broker` holds
    /// no link back to a [`ContentProcess`], so the hostile child keeps
    /// running with its pipes. Reaping it is the supervisor's job — see
    /// [`serve_once`].
    pub fn dispatch_guarded(&mut self, channel: ChannelId, request: Request) -> Decision {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch(channel, request)
        }));
        match caught {
            Ok(decision) => decision,
            Err(_) => {
                self.close_channel(channel);
                Decision::deny(DenyReason::Internal)
            }
        }
    }

    /// Handle one request from one channel.
    ///
    /// The channel argument comes from the transport, never from `request`.
    /// See the crate documentation. Callers on the IPC path should use
    /// [`Broker::dispatch_guarded`].
    pub fn dispatch(&mut self, channel: ChannelId, request: Request) -> Decision {
        #[cfg(test)]
        assert!(
            !self.panic_on_dispatch,
            "deliberate panic for the unwind test"
        );

        // Fail closed, before looking at the request at all. Without this,
        // "closing the channel" only revoked frames: Ping still answered and
        // Echo still echoed, so a caller holding the id of a killed, restarted
        // or panicking process kept being served by a broker that believed it
        // had cut that process off.
        if !self.capabilities.contains_key(&channel) {
            return Decision::deny(DenyReason::UnknownChannel);
        }

        match request {
            Request::Ping => Decision::allow(Response::Pong),
            Request::Echo { payload } => Decision::allow(Response::Echo { payload }),
            Request::FrameHost { frame } => match self.frames.owner(frame) {
                None => Decision::deny(DenyReason::NoSuchFrame),
                Some(owner) if owner == channel => {
                    if self.holds_frame(channel, frame) {
                        Decision::allow(Response::FrameHost {
                            host: FrameHost::Local,
                        })
                    } else {
                        Decision::deny(DenyReason::NotYourFrame)
                    }
                }
                // Owned by another process. The peer is told only "Denied",
                // with no way to tell this apart from a frame that never
                // existed — otherwise it can enumerate everyone else's frames.
                Some(_) => Decision::deny(DenyReason::NotYourFrame),
            },
        }
    }
}

/// Why a supervised exchange returned without a request.
#[derive(Debug, PartialEq, Eq)]
pub enum ServeError {
    /// The peer closed its end. Routine.
    Closed,
    /// The deadline passed with nothing to read. The peer is stalled, and the
    /// caller should reap it — stalling is strictly cheaper for an attacker
    /// than crashing, and this is the only signal of it.
    TimedOut,
    /// The peer sent something that could not be decoded, or the stream is
    /// misaligned. The channel is finished either way.
    Protocol,
    /// The peer has stopped draining its responses.
    NotDraining,
}

/// A content process, and the threads that talk to it.
///
/// The channel is the process's inherited stdin and stdout (ADR 005). The
/// broker holds both ends' handles, which is what makes "authority comes from
/// the channel" true in the strongest sense: it is not trusting a claim, it is
/// holding the pipe.
///
/// Reading and writing happen on worker threads. That is not concurrency for
/// its own sake — it is the only way the parent can apply a deadline to a peer
/// that may simply stop, which `std`'s pipes offer no way to do directly.
pub struct ContentProcess {
    child: Child,
    id: ChannelId,
    executable: PathBuf,
    /// `None` once the workers have been abandoned; see [`Self::abandon`].
    inbound: Option<Receiver<Result<Request, IpcError>>>,
    outbound: Option<SyncSender<Response>>,
}

impl ContentProcess {
    /// Spawn a content process and register its channel with the broker.
    pub fn spawn(broker: &mut Broker, executable: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(executable)
            // The content process gets nothing it was not handed (invariant 1).
            // Without env_clear it inherits the broker's entire environment,
            // which on a developer or CI machine routinely carries tokens and
            // paths. One line, and it does not have to wait for px-sandbox.
            .env_clear()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // stderr is inherited on purpose: a content process's diagnostics
            // should reach the terminal without the broker relaying them.
            .stderr(Stdio::inherit())
            .spawn()?;

        let stdout = child.stdout.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "child has no stdout")
        })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "child has no stdin")
        })?;

        let id = broker.open_channel().ok_or_else(|| {
            std::io::Error::other("channel identifiers exhausted; refusing to reuse one")
        })?;

        // Reader: owns the read half, blocks freely, and is the only thing in
        // this process that ever waits on that child's output.
        let (request_tx, inbound) = sync_channel::<Result<Request, IpcError>>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut channel = Channel::new(BufReader::new(stdout), std::io::sink());
            loop {
                let message = channel.recv::<Request>();
                let fatal = message.is_err();
                if request_tx.send(message).is_err() || fatal {
                    return;
                }
            }
        });

        // Writer: owns the write half. Bounded queue, so a peer that stops
        // reading cannot make the broker buffer without limit.
        let (outbound, response_rx) = sync_channel::<Response>(QUEUE_DEPTH);
        std::thread::spawn(move || {
            let mut channel = Channel::new(std::io::empty(), BufWriter::new(stdin));
            while let Ok(response) = response_rx.recv() {
                if channel.send(&response).is_err() {
                    return;
                }
            }
        });

        Ok(Self {
            child,
            id,
            executable: executable.to_path_buf(),
            inbound: Some(inbound),
            outbound: Some(outbound),
        })
    }

    /// This process's channel.
    pub fn channel_id(&self) -> ChannelId {
        self.id
    }

    /// Wait up to `timeout` for the next request from this process.
    ///
    /// This is the call that makes a hostile child survivable. It waits on a
    /// queue, not on a pipe, so none of the three wedges reaches the broker: a
    /// child that never writes, one that writes half a frame and stops, and
    /// one that orphans its stdout to a grandchild and exits all produce
    /// [`ServeError::TimedOut`]. The last is the case where `try_wait` reports
    /// the process dead while the pipe stays open forever.
    pub fn next_request(&mut self, timeout: Duration) -> Result<Request, ServeError> {
        let Some(inbound) = self.inbound.as_ref() else {
            return Err(ServeError::Closed);
        };
        match inbound.recv_timeout(timeout) {
            Ok(Ok(request)) => Ok(request),
            Ok(Err(IpcError::PeerClosed)) => Err(ServeError::Closed),
            Ok(Err(_)) => Err(ServeError::Protocol),
            Err(RecvTimeoutError::Timeout) => Err(ServeError::TimedOut),
            Err(RecvTimeoutError::Disconnected) => Err(ServeError::Closed),
        }
    }

    /// Queue a response. Never blocks.
    ///
    /// A full queue means the peer has stopped draining, which is a decision
    /// the peer made and the broker should act on rather than absorb.
    pub fn reply(&mut self, response: Response) -> Result<(), ServeError> {
        let Some(outbound) = self.outbound.as_ref() else {
            return Err(ServeError::Closed);
        };
        match outbound.try_send(response) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(ServeError::NotDraining),
            Err(TrySendError::Disconnected(_)) => Err(ServeError::Closed),
        }
    }

    /// Whether the child process is still running.
    ///
    /// Necessary and not sufficient. A child can be dead by this measure while
    /// a grandchild still holds its stdout: liveness of the *process* is not
    /// liveness of the *channel*, and only [`Self::next_request`] can tell you
    /// about the second.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the child and stop talking to it.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        let _ = self.child.wait();
        self.abandon();
        Ok(())
    }

    /// Drop the queues, releasing the worker threads that can be released.
    ///
    /// Honest about what this does not do. If the child orphaned its stdout to
    /// a grandchild, killing the child does not close the pipe, and the reader
    /// thread stays blocked. Dropping the receiver means that thread exits the
    /// moment its read returns — which may be never. That leaks one thread and
    /// one pipe handle per hostile process, bounded by the number of processes
    /// that misbehave, and Phase 17's job objects and cgroups kill the whole
    /// process tree, which releases it. The broker itself is not blocked,
    /// which is the property that matters.
    fn abandon(&mut self) {
        self.inbound = None;
        self.outbound = None;
    }

    /// Replace a dead process with a fresh one.
    ///
    /// The replacement gets a **new** [`ChannelId`] and starts with nothing —
    /// the same rule §7.3 states for `px-mcp`, for the same reason: a process
    /// that can crash its way back to its old authority can crash its way into
    /// somebody else's.
    ///
    /// Spawning happens first, deliberately. Closing the old channel before a
    /// spawn that then fails leaves `self` holding a `ChannelId` the broker no
    /// longer knows about and a dead `Child`, while the caller sees only an
    /// error — a half-restarted object that looks alive to everything except
    /// the broker.
    pub fn restart(&mut self, broker: &mut Broker) -> std::io::Result<()> {
        let executable = self.executable.clone();
        let replacement = Self::spawn(broker, &executable)?;

        let previous = self.id;
        let _ = self.kill();
        *self = replacement;
        broker.close_channel(previous);
        Ok(())
    }
}

impl Drop for ContentProcess {
    fn drop(&mut self) {
        // Kills the direct child only. A grandchild it spawned survives and
        // may still hold this pipe; there is no process-tree teardown until
        // Phase 17's job objects and cgroups. Stated rather than implied,
        // because the previous version of this comment claimed the stronger
        // property.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run one supervised request/response exchange.
///
/// Phase 1 in one function: bytes arrive on a channel, the broker decides
/// using *that channel* as the caller's identity, and the reply goes back.
/// `dispatch` is reached from here, so invariant 9's machinery sits on the
/// path a hostile process actually drives rather than being a property of code
/// that only tests call.
pub fn serve_once(
    broker: &mut Broker,
    process: &mut ContentProcess,
    timeout: Duration,
) -> Result<Decision, ServeError> {
    let channel = process.channel_id();
    let request = process.next_request(timeout)?;
    let decision = broker.dispatch_guarded(channel, request);
    process.reply(decision.response.clone())?;
    Ok(decision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_identity_a_channel_cannot_reach_another_channels_frame() {
        let mut broker = Broker::new();
        let victim = broker.open_channel().expect("channel");
        let attacker = broker.open_channel().expect("channel");
        let frame = broker.create_frame(victim).expect("frame");

        // The attacker names a frame it does not own. There is no field in
        // which it could instead claim to *be* the victim.
        let decision = broker.dispatch(attacker, Request::FrameHost { frame });
        assert_eq!(decision.response, Response::Denied);
        assert_eq!(decision.audit, Some(DenyReason::NotYourFrame));

        // And the owner is still served.
        assert_eq!(
            broker
                .dispatch(victim, Request::FrameHost { frame })
                .response,
            Response::FrameHost {
                host: FrameHost::Local
            }
        );
    }

    /// The refusal a peer sees must not distinguish "gone" from "not yours".
    /// It used to, which let a hostile channel sweep the handle space and read
    /// off every live frame in the browser, exactly, with no false positives.
    #[test]
    fn hostile_identity_denials_are_indistinguishable_on_the_wire() {
        let mut broker = Broker::new();
        let victim = broker.open_channel().expect("channel");
        let attacker = broker.open_channel().expect("channel");
        let live = broker.create_frame(victim).expect("frame");

        let someone_elses = broker.dispatch(attacker, Request::FrameHost { frame: live });
        let never_existed = broker.dispatch(
            attacker,
            Request::FrameHost {
                frame: FrameId::new(9_999, 0),
            },
        );

        assert_eq!(someone_elses.response, never_existed.response);
        assert_ne!(
            someone_elses.audit, never_existed.audit,
            "the log must still distinguish what the peer must not"
        );
    }

    #[test]
    fn hostile_identity_a_forged_frame_id_resolves_to_nothing() {
        let mut broker = Broker::new();
        let channel = broker.open_channel().expect("channel");
        for (index, generation) in [(0, 0), (0, 7), (u32::MAX, u32::MAX), (1, 0)] {
            let forged = FrameId::new(index, generation);
            assert_eq!(
                broker
                    .dispatch(channel, Request::FrameHost { frame: forged })
                    .response,
                Response::Denied,
                "forged handle {index}/{generation} must not resolve"
            );
        }
    }

    #[test]
    fn hostile_identity_a_stale_handle_does_not_survive_slot_reuse() {
        let mut broker = Broker::new();
        let first = broker.open_channel().expect("channel");
        let frame = broker.create_frame(first).expect("frame");
        broker.close_channel(first);

        // The slot is reused by a different channel. The old handle must not
        // address the new frame — this is the use-after-free §4.1 describes,
        // at the process boundary instead of in the DOM.
        let second = broker.open_channel().expect("channel");
        let reused = broker.create_frame(second).expect("frame");
        assert_eq!(frame.index(), reused.index(), "slot should be reused");
        assert_ne!(frame.generation(), reused.generation());

        let decision = broker.dispatch(second, Request::FrameHost { frame });
        assert_eq!(decision.response, Response::Denied);
        assert_eq!(decision.audit, Some(DenyReason::NoSuchFrame));
    }

    #[test]
    fn hostile_identity_echo_and_ping_grant_nothing() {
        let mut broker = Broker::new();
        let victim = broker.open_channel().expect("channel");
        let attacker = broker.open_channel().expect("channel");
        let frame = broker.create_frame(victim).expect("frame");

        // Whatever an attacker puts in a payload, it is bytes. There is no
        // parse step in which a payload could become an identity.
        let probe = broker.dispatch(
            attacker,
            Request::Echo {
                payload: victim.as_u64().to_le_bytes().to_vec(),
            },
        );
        assert!(matches!(probe.response, Response::Echo { .. }));
        assert_eq!(
            broker.dispatch(attacker, Request::Ping).response,
            Response::Pong
        );
        assert_eq!(
            broker
                .dispatch(attacker, Request::FrameHost { frame })
                .response,
            Response::Denied
        );
    }

    /// A closed channel must be refused everything, not merely stripped of its
    /// frames.
    #[test]
    fn hostile_identity_a_closed_channel_is_served_nothing() {
        let mut broker = Broker::new();
        let channel = broker.open_channel().expect("channel");
        assert_eq!(
            broker.dispatch(channel, Request::Ping).response,
            Response::Pong
        );

        broker.close_channel(channel);

        for request in [
            Request::Ping,
            Request::Echo {
                payload: b"still there?".to_vec(),
            },
            Request::FrameHost {
                frame: FrameId::new(0, 0),
            },
        ] {
            let decision = broker.dispatch(channel, request.clone());
            assert_eq!(decision.response, Response::Denied, "on {request:?}");
            assert_eq!(decision.audit, Some(DenyReason::UnknownChannel));
        }
    }

    #[test]
    fn crash_restart_the_old_channel_id_stops_being_served() {
        let mut broker = Broker::new();
        let first = broker.open_channel().expect("channel");
        broker.close_channel(first);
        let second = broker.open_channel().expect("channel");

        assert_eq!(
            broker.dispatch(first, Request::Ping).response,
            Response::Denied
        );
        assert_eq!(
            broker.dispatch(second, Request::Ping).response,
            Response::Pong
        );
    }

    #[test]
    fn create_frame_for_an_unknown_channel_leaks_no_slot() {
        let mut broker = Broker::new();
        let live = broker.open_channel().expect("channel");
        let dead = broker.open_channel().expect("channel");
        broker.close_channel(dead);

        for _ in 0..8 {
            assert!(broker.create_frame(dead).is_none());
        }
        assert!(
            broker.frames.slots.is_empty(),
            "a refused create_frame must not have grown the tree"
        );

        let frame = broker.create_frame(live).expect("frame");
        assert!(broker.holds_frame(live, frame));
        assert_eq!(broker.frames.slots.len(), 1);
    }

    #[test]
    fn closing_a_channel_releases_its_frames() {
        let mut broker = Broker::new();
        let channel = broker.open_channel().expect("channel");
        let frame = broker.create_frame(channel).expect("frame");
        assert!(broker.holds_frame(channel, frame));

        broker.close_channel(channel);
        assert!(!broker.holds_frame(channel, frame));
        assert_eq!(broker.channel_count(), 0);
    }

    #[test]
    fn channel_ids_are_not_reused() {
        let mut broker = Broker::new();
        let first = broker.open_channel().expect("channel");
        broker.close_channel(first);
        let second = broker.open_channel().expect("channel");
        assert_ne!(first, second);
    }

    /// Exhaustion must refuse, not hand the same id to two live processes.
    #[test]
    fn channel_ids_run_out_rather_than_repeat() {
        let mut broker = Broker::new();
        broker.next_channel = u64::MAX - 1;
        let last = broker
            .open_channel()
            .expect("the penultimate id is issuable");
        assert!(
            broker.open_channel().is_none(),
            "a saturating counter would repeat u64::MAX forever"
        );
        // The refusal must be clean: nothing registered for the id it declined
        // to issue, and the one before it still works.
        assert_eq!(broker.channel_count(), 1);
        assert_eq!(
            broker.dispatch(last, Request::Ping).response,
            Response::Pong
        );
    }

    /// One channel must not be able to free another's frame. Nothing reaches
    /// this yet — the point is that when a request does, the obvious
    /// implementation is already safe.
    #[test]
    fn destroy_requires_ownership() {
        let mut tree = FrameTree::default();
        let owner = ChannelId(0);
        let other = ChannelId(1);
        let frame = tree.create(owner).expect("frame");

        assert!(!tree.destroy(other, frame), "a stranger must not free it");
        assert_eq!(tree.owner(frame), Some(owner));

        assert!(tree.destroy(owner, frame));
        assert_eq!(tree.owner(frame), None);
    }

    /// §4.3: a panic during dispatch must not take the broker with it, and
    /// must not leave the panicking channel still served.
    #[test]
    fn a_panic_in_dispatch_is_caught_and_closes_the_channel() {
        let mut broker = Broker::new();
        let faulty = broker.open_channel().expect("channel");
        let bystander = broker.open_channel().expect("channel");
        let frame = broker.create_frame(faulty).expect("frame");
        assert_eq!(broker.channel_count(), 2);

        // The deliberate panic prints a message during this test. Suppressing
        // it with take_hook/set_hook is tempting and wrong: libtest tracks
        // panics through its own hook, and replacing it makes the harness fail
        // the test even though every assertion below passes.
        broker.panic_on_dispatch = true;
        let decision = broker.dispatch_guarded(faulty, Request::FrameHost { frame });
        broker.panic_on_dispatch = false;

        // Fail closed, and name the internal fault rather than blaming the
        // frame tree — the audit reason is what an investigator follows.
        assert_eq!(decision.response, Response::Denied);
        assert_eq!(decision.audit, Some(DenyReason::Internal));

        assert_eq!(broker.channel_count(), 1);
        assert!(!broker.holds_frame(faulty, frame));
        assert_eq!(
            broker.dispatch(bystander, Request::Ping).response,
            Response::Pong
        );
    }

    #[test]
    fn dispatch_guarded_answers_normally_when_nothing_panics() {
        let mut broker = Broker::new();
        let channel = broker.open_channel().expect("channel");
        let frame = broker.create_frame(channel).expect("frame");
        assert_eq!(
            broker
                .dispatch_guarded(channel, Request::FrameHost { frame })
                .response,
            Response::FrameHost {
                host: FrameHost::Local
            }
        );
        assert_eq!(broker.channel_count(), 1, "a clean dispatch closes nothing");
    }

    #[test]
    fn a_retired_slot_is_never_handed_out_again() {
        let mut tree = FrameTree::default();
        let channel = ChannelId(0);
        let frame = tree.create(channel).expect("first frame");
        tree.destroy(channel, frame);

        // Force the slot to the brink of overflow, then past it.
        if let Some(slot) = tree.slots.first_mut() {
            slot.generation = u32::MAX;
        }
        let next = tree.create(channel).expect("second frame");
        assert_ne!(
            next.index(),
            frame.index(),
            "an overflowing slot must be retired, not wrapped"
        );
        assert!(tree.slots.first().is_some_and(|s| s.retired));
    }
}
