//! The wire vocabulary.
//!
//! # Nothing here identifies its sender
//!
//! [`Request`] has no field naming the process that sent it — no channel, no
//! pid, no tab, no origin, no partition key. This is invariant 9 expressed as
//! a type rather than as a rule to be remembered: the broker takes the sender
//! from the connection a message arrived on, so a confused-deputy attack has
//! nothing to say. `ci/gate-ipc.sh` fails if such a field appears.
//!
//! A request may still *name a resource* — [`Request::FrameHost`] carries the
//! [`FrameId`] it asks about. That is not an identity claim: the broker checks
//! whether the channel it arrived on holds that frame, and denies it
//! otherwise. The distinction is the whole design. "I am tab 7" is
//! unrepresentable; "tell me about frame 7" is a question the broker is
//! entitled to refuse.

use serde::{Deserialize, Serialize};

/// A handle to a frame in the broker's frame tree.
///
/// Generational, for the reason §4.1 gives for `NodeId`: a plain index with
/// slot reuse means a stale handle silently addresses whatever now occupies
/// that slot. §4.1 is written about the DOM, but the bug is a property of
/// index-plus-reuse rather than of the DOM, and a frame handle crosses a
/// process boundary — where the holder of a stale one may be hostile.
///
/// The fields are private and there is no constructor here. A content process
/// cannot mint a `FrameId`; it can only echo back one the broker gave it, and
/// the broker checks the generation. Forging one is possible on the wire —
/// it is two integers — which is exactly why possession is never authority.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameId {
    index: u32,
    generation: u32,
}

impl FrameId {
    /// Mint a handle. Broker-side only in practice; a content process has no
    /// reason to call this and gains nothing by it, since the broker checks
    /// ownership rather than trusting the value.
    pub fn new(index: u32, generation: u32) -> Self {
        Self { index, generation }
    }

    /// Slot this handle refers to.
    pub fn index(self) -> u32 {
        self.index
    }

    /// Generation this handle was minted at.
    pub fn generation(self) -> u32 {
        self.generation
    }
}

/// Where a frame is hosted, as told to a content process.
///
/// Deliberately opaque. A content process learns that a frame is somewhere
/// else, never *where* — no channel id, no pid. Routing is the broker's job,
/// and an identifier a process does not have is one it cannot be tricked into
/// repeating.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum FrameHost {
    /// Hosted by the process that asked.
    Local,
    /// Hosted by some other process. Which one is not disclosed.
    Remote,
}

/// Why the broker refused.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// The channel does not hold the frame it named.
    NotYourFrame,
    /// The frame does not exist, or its slot has been reused since.
    NoSuchFrame,
}

/// Content process to broker.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Liveness.
    Ping,
    /// Return the payload unchanged. The whole of Phase 1's content process.
    Echo {
        /// Bounded by `MAX_MESSAGE_BYTES` at the framing layer.
        payload: Vec<u8>,
    },
    /// Ask where a frame lives. Answered only if this channel holds it.
    FrameHost {
        /// The frame being asked about — a resource reference, not a claim of
        /// identity. See the module documentation.
        frame: FrameId,
    },
}

/// Broker to content process.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// Liveness.
    Pong,
    /// The payload, unchanged.
    Echo {
        /// As sent.
        payload: Vec<u8>,
    },
    /// Where the frame lives.
    FrameHost {
        /// Local or elsewhere. Never which elsewhere.
        host: FrameHost,
    },
    /// Refused, and why. Fail closed: anything the broker cannot decide
    /// affirmatively arrives here.
    Denied {
        /// The reason, for the audit log.
        reason: DenyReason,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `FrameId` is two integers on the wire, so a hostile peer can produce
    /// any value it likes. This test exists to record that this is *expected*:
    /// the defence is that the broker checks ownership, never that the handle
    /// is unforgeable.
    #[test]
    fn a_frame_id_is_forgeable_and_that_is_fine() {
        let forged = FrameId::new(u32::MAX, u32::MAX);
        assert_eq!(forged.index(), u32::MAX);
        assert_eq!(forged.generation(), u32::MAX);
    }

    #[test]
    fn frame_ids_of_different_generations_are_different_handles() {
        assert_ne!(FrameId::new(3, 1), FrameId::new(3, 2));
    }
}
