#![forbid(unsafe_code)]

//! Typed channels, postcard codec, handle passing.
//!
//! This crate is the trust boundary. Every byte it decodes was written by a
//! process that may be entirely under an attacker's control, so it is held to
//! the panic lints §4.3 specifies for `px-content`, `px-net` and `px-mcp`
//! even though it is not named in that list — see this crate's `CLAUDE.md`.
//!
//! # Authority
//!
//! Nothing in [`Request`] identifies its sender, and there is no way to add
//! such a field without `ci/gate-ipc.sh` failing. The broker learns who sent a
//! message from which channel it arrived on (invariant 9). A message may
//! *reference* a resource — a [`FrameId`] it wants to act on — and the broker
//! decides whether the channel it came from is entitled to it. That is a
//! capability check, not an identity claim.
//!
//! # Handle passing
//!
//! Not implemented. ADR 005 defers it to Phase 2, where `px-sandbox` exists;
//! sending a file descriptor needs `SCM_RIGHTS` and this crate forbids
//! `unsafe`. The transport is generic over [`Read`] and [`Write`] so that
//! swapping inherited pipes for a socket changes the transport and not its
//! callers.
//!
//! [`Read`]: std::io::Read
//! [`Write`]: std::io::Write

use std::io::{Read, Write};

use serde::{Serialize, de::DeserializeOwned};

pub mod message;

pub use message::{DenyReason, FrameHost, FrameId, Request, Response};

/// Hard ceiling on one encoded message, in bytes.
///
/// §4.4: no length prefix from untrusted bytes drives an allocation without a
/// bound check. This is that bound. It is checked *before* any allocation, so
/// a peer claiming `u32::MAX` costs four bytes of read and nothing else.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// Bytes of length prefix preceding every frame.
const LENGTH_PREFIX_BYTES: usize = 4;

/// What can go wrong on the wire.
#[derive(Debug)]
pub enum IpcError {
    /// The peer closed the channel. Routine, not an error path to be surprised
    /// by: for the broker this is how a content process reports its own death.
    PeerClosed,
    /// A length prefix exceeded [`MAX_MESSAGE_BYTES`]. Nothing was allocated.
    MessageTooLarge {
        /// What the peer claimed, for the log. Never used to size anything.
        claimed: u64,
    },
    /// The payload was not a valid encoding of the expected type.
    Malformed,
    /// The underlying transport failed.
    Io(std::io::Error),
}

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PeerClosed => write!(f, "peer closed the channel"),
            Self::MessageTooLarge { claimed } => write!(
                f,
                "message length {claimed} exceeds the {MAX_MESSAGE_BYTES} byte limit"
            ),
            Self::Malformed => write!(f, "payload is not a valid message"),
            Self::Io(e) => write!(f, "transport error: {e}"),
        }
    }
}

impl std::error::Error for IpcError {}

impl From<std::io::Error> for IpcError {
    fn from(e: std::io::Error) -> Self {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::PeerClosed
        } else {
            Self::Io(e)
        }
    }
}

/// A typed request/response channel over any byte transport.
///
/// Generic over the transport so that the inherited pipes of Phase 1 can
/// become a socket later without touching a caller, and so that tests and fuzz
/// targets can drive the codec with an in-memory hostile stream at full speed
/// rather than through a process spawn per case.
pub struct Channel<R: Read, W: Write> {
    reader: R,
    writer: W,
    /// Reused across receives so a steady stream of messages does not allocate
    /// per message. Never grown beyond [`MAX_MESSAGE_BYTES`].
    scratch: Vec<u8>,
}

impl<R: Read, W: Write> Channel<R, W> {
    /// Wrap a reader and a writer as one channel.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            scratch: Vec::new(),
        }
    }

    /// Encode and send one message.
    ///
    /// Refuses to send anything over [`MAX_MESSAGE_BYTES`] rather than
    /// producing a frame the peer is obliged to reject. A limit enforced on
    /// only one side gets discovered as a mysterious disconnect.
    pub fn send<T: Serialize>(&mut self, message: &T) -> Result<(), IpcError> {
        let payload = postcard::to_allocvec(message).map_err(|_| IpcError::Malformed)?;
        if payload.len() > MAX_MESSAGE_BYTES {
            return Err(IpcError::MessageTooLarge {
                claimed: payload.len() as u64,
            });
        }
        // `payload.len()` is bounded above by MAX_MESSAGE_BYTES, which fits in
        // u32, so this conversion cannot truncate.
        let length = u32::try_from(payload.len()).map_err(|_| IpcError::Malformed)?;
        self.writer.write_all(&length.to_le_bytes())?;
        self.writer.write_all(&payload)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Receive and decode one message.
    ///
    /// The order of operations here is the whole point: read the prefix, check
    /// it against the limit, and only then allocate. Reversing those last two
    /// is how a four-byte message becomes a four-gigabyte allocation.
    pub fn recv<T: DeserializeOwned>(&mut self) -> Result<T, IpcError> {
        let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
        match self.reader.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(IpcError::PeerClosed);
            }
            Err(e) => return Err(IpcError::Io(e)),
        }

        let claimed = u32::from_le_bytes(prefix);
        let length = claimed as usize;
        if length > MAX_MESSAGE_BYTES {
            return Err(IpcError::MessageTooLarge {
                claimed: u64::from(claimed),
            });
        }

        self.scratch.clear();
        self.scratch.resize(length, 0);
        self.reader.read_exact(self.scratch.as_mut_slice())?;

        postcard::from_bytes(self.scratch.as_slice()).map_err(|_| IpcError::Malformed)
    }
}

/// Decode one message from a complete frame, prefix included.
///
/// The exact path a hostile peer drives, exposed for the fuzz targets so they
/// attack framing and payload decoding together rather than one at a time.
pub fn decode_frame<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, IpcError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut channel = Channel::new(&mut cursor, std::io::sink());
    channel.recv()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loopback(bytes: Vec<u8>) -> Channel<std::io::Cursor<Vec<u8>>, std::io::Sink> {
        Channel::new(std::io::Cursor::new(bytes), std::io::sink())
    }

    #[test]
    fn roundtrip_preserves_the_message() {
        let mut buffer = Vec::new();
        {
            let mut channel = Channel::new(std::io::empty(), &mut buffer);
            channel
                .send(&Request::Echo {
                    payload: b"hello".to_vec(),
                })
                .expect("send");
        }
        let decoded: Request = decode_frame(&buffer).expect("decode");
        assert_eq!(
            decoded,
            Request::Echo {
                payload: b"hello".to_vec()
            }
        );
    }

    /// The hostile length prefix Phase 1's gate names explicitly. A peer
    /// claiming four gigabytes must cost four bytes, not four gigabytes.
    #[test]
    fn size_limit_rejects_a_hostile_length_prefix() {
        let mut frame = u32::MAX.to_le_bytes().to_vec();
        frame.extend_from_slice(b"not four gigabytes of anything");
        match decode_frame::<Request>(&frame) {
            Err(IpcError::MessageTooLarge { claimed }) => {
                assert_eq!(claimed, u64::from(u32::MAX));
            }
            other => panic!("expected MessageTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn size_limit_rejects_one_byte_over_the_maximum() {
        let over = u32::try_from(MAX_MESSAGE_BYTES + 1).expect("fits in u32");
        let frame = over.to_le_bytes().to_vec();
        assert!(matches!(
            decode_frame::<Request>(&frame),
            Err(IpcError::MessageTooLarge { .. })
        ));
    }

    #[test]
    fn size_limit_accepts_exactly_the_maximum_prefix() {
        // At the boundary the length is allowed, so the failure must come from
        // the payload being absent rather than from the limit. An off-by-one
        // in the check would show up here and nowhere else.
        let at = u32::try_from(MAX_MESSAGE_BYTES).expect("fits in u32");
        let frame = at.to_le_bytes().to_vec();
        assert!(matches!(
            decode_frame::<Request>(&frame),
            Err(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn size_limit_refuses_to_send_an_oversized_message() {
        let mut sink = Vec::new();
        let mut channel = Channel::new(std::io::empty(), &mut sink);
        let result = channel.send(&Request::Echo {
            payload: vec![0u8; MAX_MESSAGE_BYTES + 1],
        });
        assert!(matches!(result, Err(IpcError::MessageTooLarge { .. })));
        assert!(sink.is_empty(), "nothing may be written for a refused send");
    }

    #[test]
    fn a_truncated_frame_is_a_closed_peer_not_a_hang() {
        let frame = 64u32.to_le_bytes().to_vec();
        assert!(matches!(
            decode_frame::<Request>(&frame),
            Err(IpcError::PeerClosed)
        ));
    }

    #[test]
    fn garbage_payload_is_malformed_not_a_panic() {
        let payload = vec![0xFFu8; 32];
        let mut frame = u32::try_from(payload.len())
            .expect("fits")
            .to_le_bytes()
            .to_vec();
        frame.extend_from_slice(&payload);
        assert!(matches!(
            decode_frame::<Request>(&frame),
            Err(IpcError::Malformed)
        ));
    }

    #[test]
    fn an_empty_stream_reports_a_closed_peer() {
        let mut channel = loopback(Vec::new());
        assert!(matches!(
            channel.recv::<Request>(),
            Err(IpcError::PeerClosed)
        ));
    }
}
