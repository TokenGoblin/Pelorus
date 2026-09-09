#![forbid(unsafe_code)]

//! Parent process, capability broker, policy, frame tree.
//!
//! # The one rule this crate exists to enforce
//!
//! [`Broker::dispatch`] takes the channel as a separate argument from the
//! message:
//!
//! ```ignore
//! fn dispatch(&mut self, channel: ChannelId, request: Request) -> Response
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

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use px_ipc::{Channel, DenyReason, FrameHost, FrameId, IpcError, Request, Response};

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

    /// Destroy a frame, freeing its slot for reuse at a higher generation.
    pub fn destroy(&mut self, frame: FrameId) {
        if let Some(slot) = self.slot_mut(frame) {
            slot.owner = None;
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
    /// outlives its channel cannot land on a successor.
    pub fn open_channel(&mut self) -> ChannelId {
        let id = ChannelId(self.next_channel);
        self.next_channel = self.next_channel.saturating_add(1);
        self.capabilities.insert(id, ChannelCaps::default());
        id
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
    /// What happens after a caught panic is the part worth stating. Broker
    /// state may be inconsistent — that is what a panic mid-mutation means —
    /// so this **closes the channel** rather than answering as if nothing
    /// occurred. Fail closed: a panic while deciding a capability question is
    /// exactly the case where continuing to serve that channel is least
    /// justified.
    ///
    /// `AssertUnwindSafe` is required because `&mut self` is not
    /// `UnwindSafe`, and that is not a formality being waived: it is the
    /// compiler pointing at the inconsistent-state problem. Closing the
    /// channel is the answer to it.
    pub fn dispatch_guarded(&mut self, channel: ChannelId, request: Request) -> Response {
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.dispatch(channel, request)
        }));
        match caught {
            Ok(response) => response,
            Err(_) => {
                self.close_channel(channel);
                Response::Denied {
                    reason: DenyReason::Internal,
                }
            }
        }
    }

    /// Handle one request from one channel.
    ///
    /// The channel argument comes from the transport, never from `request`.
    /// See the crate documentation. Callers on the IPC path should use
    /// [`Broker::dispatch_guarded`].
    pub fn dispatch(&mut self, channel: ChannelId, request: Request) -> Response {
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
            return Response::Denied {
                reason: DenyReason::UnknownChannel,
            };
        }

        match request {
            Request::Ping => Response::Pong,
            Request::Echo { payload } => Response::Echo { payload },
            Request::FrameHost { frame } => match self.frames.owner(frame) {
                None => Response::Denied {
                    reason: DenyReason::NoSuchFrame,
                },
                Some(owner) if owner == channel => {
                    if self.holds_frame(channel, frame) {
                        Response::FrameHost {
                            host: FrameHost::Local,
                        }
                    } else {
                        Response::Denied {
                            reason: DenyReason::NotYourFrame,
                        }
                    }
                }
                // Owned by another process. The asker is told only that it is
                // not theirs — never which process holds it.
                Some(_) => Response::Denied {
                    reason: DenyReason::NotYourFrame,
                },
            },
        }
    }
}

/// A content process and the channel to it.
///
/// The channel is the process's inherited stdin and stdout (ADR 005). The
/// broker holds both ends' handles, which is what makes "authority comes from
/// the channel" true in the strongest sense: it is not trusting a claim, it is
/// holding the pipe.
pub struct ContentProcess {
    child: Child,
    channel: Channel<BufReader<ChildStdout>, BufWriter<ChildStdin>>,
    id: ChannelId,
    executable: std::path::PathBuf,
}

impl ContentProcess {
    /// Spawn a content process and register its channel with the broker.
    pub fn spawn(broker: &mut Broker, executable: &Path) -> std::io::Result<Self> {
        let mut child = Command::new(executable)
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

        Ok(Self {
            child,
            channel: Channel::new(BufReader::new(stdout), BufWriter::new(stdin)),
            id: broker.open_channel(),
            executable: executable.to_path_buf(),
        })
    }

    /// This process's channel.
    pub fn channel_id(&self) -> ChannelId {
        self.id
    }

    /// Send a request and read the reply.
    pub fn round_trip(&mut self, request: &Request) -> Result<Response, IpcError> {
        self.channel.send(request)?;
        self.channel.recv()
    }

    /// Whether the child is still running.
    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Kill the child. Used by the crash-recovery tests, and by the broker
    /// when a process misbehaves.
    pub fn kill(&mut self) -> std::io::Result<()> {
        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
    }

    /// Replace a dead process with a fresh one.
    ///
    /// The old channel is closed first, which drops every capability it held.
    /// The replacement gets a **new** [`ChannelId`] and starts with nothing —
    /// the same rule §7.3 states for `px-mcp`, for the same reason: a process
    /// that can crash its way back to its old authority can crash its way into
    /// somebody else's.
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
        // A content process that outlives its broker is an orphan holding a
        // pipe nobody reads.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_identity_a_channel_cannot_reach_another_channels_frame() {
        let mut broker = Broker::new();
        let victim = broker.open_channel();
        let attacker = broker.open_channel();
        let frame = broker.create_frame(victim).expect("frame");

        // The attacker names a frame it does not own. There is no field in
        // which it could instead claim to *be* the victim.
        let response = broker.dispatch(attacker, Request::FrameHost { frame });
        assert_eq!(
            response,
            Response::Denied {
                reason: DenyReason::NotYourFrame
            }
        );

        // And the owner is still served.
        assert_eq!(
            broker.dispatch(victim, Request::FrameHost { frame }),
            Response::FrameHost {
                host: FrameHost::Local
            }
        );
    }

    #[test]
    fn hostile_identity_a_forged_frame_id_resolves_to_nothing() {
        let mut broker = Broker::new();
        let channel = broker.open_channel();
        for (index, generation) in [(0, 0), (0, 7), (u32::MAX, u32::MAX), (1, 0)] {
            let forged = FrameId::new(index, generation);
            assert_eq!(
                broker.dispatch(channel, Request::FrameHost { frame: forged }),
                Response::Denied {
                    reason: DenyReason::NoSuchFrame
                },
                "forged handle {index}/{generation} must not resolve"
            );
        }
    }

    #[test]
    fn hostile_identity_a_stale_handle_does_not_survive_slot_reuse() {
        let mut broker = Broker::new();
        let first = broker.open_channel();
        let frame = broker.create_frame(first).expect("frame");
        broker.close_channel(first);

        // The slot is reused by a different channel. The old handle must not
        // address the new frame — this is the use-after-free §4.1 describes,
        // at the process boundary instead of in the DOM.
        let second = broker.open_channel();
        let reused = broker.create_frame(second).expect("frame");
        assert_eq!(frame.index(), reused.index(), "slot should be reused");
        assert_ne!(frame.generation(), reused.generation());

        assert_eq!(
            broker.dispatch(second, Request::FrameHost { frame }),
            Response::Denied {
                reason: DenyReason::NoSuchFrame
            }
        );
    }

    #[test]
    fn hostile_identity_echo_and_ping_grant_nothing() {
        let mut broker = Broker::new();
        let victim = broker.open_channel();
        let attacker = broker.open_channel();
        let frame = broker.create_frame(victim).expect("frame");

        // Whatever an attacker puts in a payload, it is bytes. There is no
        // parse step in which a payload could become an identity.
        let probe = broker.dispatch(
            attacker,
            Request::Echo {
                payload: victim.as_u64().to_le_bytes().to_vec(),
            },
        );
        assert!(matches!(probe, Response::Echo { .. }));
        assert_eq!(broker.dispatch(attacker, Request::Ping), Response::Pong);

        assert_eq!(
            broker.dispatch(attacker, Request::FrameHost { frame }),
            Response::Denied {
                reason: DenyReason::NotYourFrame
            }
        );
    }

    #[test]
    fn closing_a_channel_releases_its_frames() {
        let mut broker = Broker::new();
        let channel = broker.open_channel();
        let frame = broker.create_frame(channel).expect("frame");
        assert!(broker.holds_frame(channel, frame));

        broker.close_channel(channel);
        assert!(!broker.holds_frame(channel, frame));
        assert_eq!(broker.channel_count(), 0);
    }

    #[test]
    fn channel_ids_are_not_reused() {
        let mut broker = Broker::new();
        let first = broker.open_channel();
        broker.close_channel(first);
        let second = broker.open_channel();
        assert_ne!(first, second);
    }

    /// §4.3: a panic during dispatch must not take the broker with it, and
    /// must not leave the panicking channel still served.
    #[test]
    fn a_panic_in_dispatch_is_caught_and_closes_the_channel() {
        let mut broker = Broker::new();
        let faulty = broker.open_channel();
        let bystander = broker.open_channel();
        let frame = broker.create_frame(faulty).expect("frame");
        assert_eq!(broker.channel_count(), 2);

        // The deliberate panic prints a message during this test. Suppressing
        // it with take_hook/set_hook is tempting and wrong: libtest tracks
        // panics through its own hook, and replacing it makes the harness fail
        // the test even though every assertion below passes. Noise in the
        // output is the cheaper problem.
        broker.panic_on_dispatch = true;
        let response = broker.dispatch_guarded(faulty, Request::FrameHost { frame });
        broker.panic_on_dispatch = false;

        // Fail closed: the answer is a denial, not an invented success, and
        // it names the internal fault rather than blaming the frame tree.
        assert_eq!(
            response,
            Response::Denied {
                reason: DenyReason::Internal
            }
        );

        // The panicking channel lost everything it held.
        assert_eq!(broker.channel_count(), 1);
        assert!(!broker.holds_frame(faulty, frame));

        // And the broker is alive, with the bystander untouched.
        assert_eq!(broker.dispatch(bystander, Request::Ping), Response::Pong);
    }

    #[test]
    fn dispatch_guarded_answers_normally_when_nothing_panics() {
        let mut broker = Broker::new();
        let channel = broker.open_channel();
        let frame = broker.create_frame(channel).expect("frame");
        assert_eq!(
            broker.dispatch_guarded(channel, Request::FrameHost { frame }),
            Response::FrameHost {
                host: FrameHost::Local
            }
        );
        assert_eq!(broker.channel_count(), 1, "a clean dispatch closes nothing");
    }

    /// A closed channel must be refused everything, not merely stripped of its
    /// frames. Before this, `close_channel` revoked capabilities while `Ping`
    /// still answered and `Echo` still echoed — a channel the broker believed
    /// it had cut off was still being served.
    #[test]
    fn hostile_identity_a_closed_channel_is_served_nothing() {
        let mut broker = Broker::new();
        let channel = broker.open_channel();
        assert_eq!(broker.dispatch(channel, Request::Ping), Response::Pong);

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
            assert_eq!(
                broker.dispatch(channel, request.clone()),
                Response::Denied {
                    reason: DenyReason::UnknownChannel
                },
                "a closed channel must be refused {request:?}"
            );
        }
    }

    /// The same, for the channel a restarted process left behind. A caller
    /// holding the old id must not keep being answered.
    #[test]
    fn crash_restart_the_old_channel_id_stops_being_served() {
        let mut broker = Broker::new();
        let first = broker.open_channel();
        broker.close_channel(first);
        let second = broker.open_channel();

        assert_eq!(
            broker.dispatch(first, Request::Ping),
            Response::Denied {
                reason: DenyReason::UnknownChannel
            }
        );
        assert_eq!(broker.dispatch(second, Request::Ping), Response::Pong);
    }

    /// Creating a frame for a channel that is not open must touch nothing.
    /// Mutating first and validating second left the slot owned by a channel
    /// that can never be closed again, so nothing could reclaim it.
    #[test]
    fn create_frame_for_an_unknown_channel_leaks_no_slot() {
        let mut broker = Broker::new();
        let live = broker.open_channel();
        let dead = broker.open_channel();
        broker.close_channel(dead);

        for _ in 0..8 {
            assert!(broker.create_frame(dead).is_none());
        }
        assert!(
            broker.frames.slots.is_empty(),
            "a refused create_frame must not have grown the tree"
        );

        // And the tree still works for a channel that is open.
        let frame = broker.create_frame(live).expect("frame");
        assert!(broker.holds_frame(live, frame));
        assert_eq!(broker.frames.slots.len(), 1);
    }

    /// §4.3's audit log has to point at the right thing: a panic is an internal
    /// fault, not a frame-resolution failure.
    #[test]
    fn a_caught_panic_is_reported_as_internal() {
        let mut broker = Broker::new();
        let channel = broker.open_channel();

        broker.panic_on_dispatch = true;
        let response = broker.dispatch_guarded(channel, Request::Ping);
        broker.panic_on_dispatch = false;

        assert_eq!(
            response,
            Response::Denied {
                reason: DenyReason::Internal
            },
            "a panic while handling Ping must not be logged as a frame problem"
        );
    }

    #[test]
    fn a_retired_slot_is_never_handed_out_again() {
        let mut tree = FrameTree::default();
        let channel = ChannelId(0);
        let frame = tree.create(channel).expect("first frame");
        tree.destroy(frame);

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
