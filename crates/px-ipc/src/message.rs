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
//!
//! # Nothing here explains a refusal
//!
//! [`Response::Denied`] carries no reason. It used to distinguish "no such
//! frame" from "not yours", which let a hostile content process sweep the
//! `FrameId` space and read off — exactly, with no false positives — which
//! slots hold live frames belonging to other sites, and how often those slots
//! churn as tabs open and close. That is a cross-site side channel in a
//! browser whose thesis is site isolation. The reason still exists broker-side
//! for the audit log; it does not cross the boundary.

use serde::{Deserialize, Serialize};

/// A handle to a frame in the broker's frame tree.
///
/// Generational, for the reason §4.1 gives for `NodeId`: a plain index with
/// slot reuse means a stale handle silently addresses whatever now occupies
/// that slot. §4.1 is written about the DOM, but the bug is a property of
/// index-plus-reuse rather than of the DOM, and a frame handle crosses a
/// process boundary — where the holder of a stale one may be hostile.
///
/// Forging one is trivial: it is two integers on the wire. That is expected.
/// Possession is never authority; the broker checks ownership.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct FrameId {
    index: u32,
    generation: u32,
}

impl FrameId {
    /// Mint a handle. Broker-side in practice; a content process gains nothing
    /// by calling it, since the broker checks ownership rather than trusting
    /// the value.
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

/// Content process to broker.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Liveness.
    Ping,
    /// Return the payload unchanged. The whole of Phase 1's traffic.
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
    /// Refused. Fail closed: anything the broker cannot decide affirmatively
    /// arrives here, and it says nothing about why — see the module docs.
    Denied,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `FrameId` is two integers on the wire, so a hostile peer can produce
    /// any value it likes. This test records that this is *expected*: the
    /// defence is that the broker checks ownership, never that the handle is
    /// unforgeable.
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

    /// The oracle this variant used to be. If a reason ever returns to the
    /// wire, it must not let a peer distinguish "gone" from "not yours".
    #[test]
    fn a_denial_carries_no_information() {
        let encoded = postcard::to_allocvec(&Response::Denied).expect("encode");
        assert_eq!(
            encoded.len(),
            1,
            "Denied must be a bare discriminant; it encoded to {encoded:?}"
        );
    }
}
