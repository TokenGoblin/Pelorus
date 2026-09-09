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
//! # What a hostile peer cannot do here
//!
//! Each of these is a defence that an adversarial review found missing:
//!
//! - **Claim a length and not send it.** The payload is read incrementally,
//!   so declaring a megabyte costs a chunk buffer rather than a megabyte.
//! - **Leave a buffer permanently committed.** Scratch is shrunk back after an
//!   oversized message, so one large frame does not cost the broker a megabyte
//!   per channel for the life of the process.
//! - **Send a valid prefix with junk after it.** Trailing bytes are rejected,
//!   so the frame-to-message mapping is injective and the size limit keeps
//!   bounding useful work.
//! - **Reflect the broker's own bytes back.** Each direction has its own tag,
//!   so a content process that is `cat` produces `Malformed`, not an apparently
//!   valid reply.
//! - **Desynchronise the stream and keep talking.** Any error that could leave
//!   the stream misaligned poisons the channel permanently.
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

pub use message::{FrameHost, FrameId, Request, Response};

/// Hard ceiling on one encoded message, in bytes.
///
/// §4.4: no length prefix from untrusted bytes drives an allocation without a
/// bound check. This is that bound.
pub const MAX_MESSAGE_BYTES: usize = 1 << 20;

/// Bytes of length prefix preceding every frame.
const LENGTH_PREFIX_BYTES: usize = 4;

/// How much of a declared payload is read at a time.
///
/// The reason this constant exists at all: `resize(length)` followed by
/// `read_exact` lets four bytes on the wire buy a megabyte of committed,
/// zeroed memory from a peer that then sends nothing. Reading in chunks means
/// a peer pays for the bytes it actually sends.
const READ_CHUNK_BYTES: usize = 8 * 1024;

/// Scratch capacity retained between messages.
///
/// Above this, the buffer is shrunk after use. `Vec::clear` does not release
/// capacity, so without this one large message costs the broker that much
/// resident memory per channel forever — and §4.4's heap caps are per content
/// process, so nothing else would ever reclaim it.
const SCRATCH_RETAIN_BYTES: usize = 64 * 1024;

/// A message that can travel on a channel, in a known direction.
///
/// The tag is what stops a peer replaying the other direction's bytes. Without
/// it `Request` and `Response` are indistinguishable on the wire — a content
/// process that echoes its input verbatim, decoding nothing, produces frames
/// the broker accepts as valid replies.
pub trait Message: Serialize + DeserializeOwned {
    /// Distinguishes this direction on the wire.
    const TAG: u8;
}

impl Message for Request {
    const TAG: u8 = 0xA1;
}

impl Message for Response {
    const TAG: u8 = 0xA2;
}

/// What can go wrong on the wire.
#[derive(Debug)]
pub enum IpcError {
    /// The peer closed the channel. Routine, not an error path to be surprised
    /// by: for the broker this is how a content process reports its own death.
    PeerClosed,
    /// A length prefix exceeded [`MAX_MESSAGE_BYTES`].
    MessageTooLarge {
        /// What the peer claimed, for the log. Never used to size anything.
        claimed: u64,
    },
    /// The payload was not a valid, complete, correctly-directed encoding of
    /// the expected type.
    Malformed,
    /// The channel is unusable because an earlier error may have left the
    /// stream misaligned. Permanent: the caller must tear the channel down.
    Poisoned,
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
            Self::Poisoned => write!(f, "channel is poisoned by an earlier error"),
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
    /// per message, and shrunk past [`SCRATCH_RETAIN_BYTES`] so one large
    /// message does not cost that memory forever.
    scratch: Vec<u8>,
    /// Set once the stream may be misaligned. Never cleared.
    poisoned: bool,
}

impl<R: Read, W: Write> Channel<R, W> {
    /// Wrap a reader and a writer as one channel.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            scratch: Vec::new(),
            poisoned: false,
        }
    }

    /// Whether an earlier error may have left the stream misaligned.
    pub fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Encode and send one message.
    ///
    /// Refuses to send anything over [`MAX_MESSAGE_BYTES`] rather than
    /// producing a frame the peer is obliged to reject — and refuses *before*
    /// writing a byte, so a rejected send cannot desynchronise the stream.
    pub fn send<T: Message>(&mut self, message: &T) -> Result<(), IpcError> {
        if self.poisoned {
            return Err(IpcError::Poisoned);
        }

        let payload = postcard::to_allocvec(message).map_err(|_| IpcError::Malformed)?;
        // The tag occupies one byte of the budget, so account for it here
        // rather than letting the framed size exceed the cap the peer enforces.
        let framed = payload.len().saturating_add(1);
        if framed > MAX_MESSAGE_BYTES {
            return Err(IpcError::MessageTooLarge {
                claimed: framed as u64,
            });
        }
        let length = u32::try_from(framed).map_err(|_| IpcError::Malformed)?;

        // From here on, a failure may have left part of a frame on the wire —
        // a complete length prefix and half a payload, which the peer would
        // consume as the head of the next message. There is no way to retract
        // it, so the channel is finished.
        self.write_all_poisoning(&length.to_le_bytes())?;
        self.write_all_poisoning(&[T::TAG])?;
        self.write_all_poisoning(&payload)?;
        self.writer.flush().map_err(|e| {
            self.poisoned = true;
            IpcError::from(e)
        })
    }

    fn write_all_poisoning(&mut self, bytes: &[u8]) -> Result<(), IpcError> {
        self.writer.write_all(bytes).map_err(|e| {
            self.poisoned = true;
            IpcError::from(e)
        })
    }

    /// Receive and decode one message.
    pub fn recv<T: Message>(&mut self) -> Result<T, IpcError> {
        if self.poisoned {
            return Err(IpcError::Poisoned);
        }

        let mut prefix = [0u8; LENGTH_PREFIX_BYTES];
        match self.reader.read_exact(&mut prefix) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Err(IpcError::PeerClosed);
            }
            Err(e) => {
                self.poisoned = true;
                return Err(IpcError::Io(e));
            }
        }

        let claimed = u32::from_le_bytes(prefix);
        let length = claimed as usize;
        if length > MAX_MESSAGE_BYTES {
            // The payload is not consumed, so the stream is now misaligned at
            // an offset the attacker chose. Nothing after this is trustworthy.
            self.poisoned = true;
            return Err(IpcError::MessageTooLarge {
                claimed: u64::from(claimed),
            });
        }

        let result = self.read_payload(length);
        if result.is_err() {
            self.poisoned = true;
        }
        self.shrink_scratch();
        result
    }

    /// Read exactly `length` bytes into scratch and decode them.
    ///
    /// Incremental on purpose. `scratch.resize(length, 0)` before reading is
    /// what makes four bytes on the wire cost a megabyte of committed memory
    /// from a peer that then sends nothing at all.
    fn read_payload<T: Message>(&mut self, length: usize) -> Result<T, IpcError> {
        self.scratch.clear();
        let mut chunk = [0u8; READ_CHUNK_BYTES];
        let mut remaining = length;
        while remaining > 0 {
            let want = remaining.min(READ_CHUNK_BYTES);
            let slot = chunk.get_mut(..want).ok_or(IpcError::Malformed)?;
            match self.reader.read_exact(slot) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Err(IpcError::PeerClosed);
                }
                Err(e) => return Err(IpcError::Io(e)),
            }
            self.scratch.extend_from_slice(slot);
            remaining -= want;
        }

        let (tag, body) = self.scratch.split_first().ok_or(IpcError::Malformed)?;
        if *tag != T::TAG {
            // Either a peer replaying the other direction's bytes, or a
            // desynchronised stream. Both mean this channel is finished.
            return Err(IpcError::Malformed);
        }

        // take_from_bytes, not from_bytes: a valid encoding followed by junk
        // must not decode. Otherwise the frame-to-message mapping is not
        // injective, and a megabyte of wire can buy a one-byte Ping.
        let (message, rest) =
            postcard::take_from_bytes::<T>(body).map_err(|_| IpcError::Malformed)?;
        if !rest.is_empty() {
            return Err(IpcError::Malformed);
        }
        Ok(message)
    }

    fn shrink_scratch(&mut self) {
        if self.scratch.capacity() > SCRATCH_RETAIN_BYTES {
            self.scratch.clear();
            self.scratch.shrink_to(SCRATCH_RETAIN_BYTES);
        }
    }
}

/// Decode one message from a complete frame, prefix included.
///
/// The exact path a hostile peer drives, exposed for the fuzz targets so they
/// attack framing, tagging and payload decoding together rather than one at a
/// time.
pub fn decode_frame<T: Message>(bytes: &[u8]) -> Result<T, IpcError> {
    let mut cursor = std::io::Cursor::new(bytes);
    let mut channel = Channel::new(&mut cursor, std::io::sink());
    channel.recv()
}

/// Encode one complete frame, prefix included.
///
/// Used by anything that must know a message is sendable *before* committing
/// to send it. A queue that accepts a message the writer will later refuse
/// turns a caller's error into a silent loss.
pub fn encode_frame<T: Message>(message: &T) -> Result<Vec<u8>, IpcError> {
    let mut buffer = Vec::new();
    let mut channel = Channel::new(std::io::empty(), &mut buffer);
    channel.send(message)?;
    Ok(buffer)
}

/// Write an already-encoded frame.
///
/// The counterpart to [`encode_frame`], for a writer that validated the
/// message earlier and elsewhere. Takes bytes rather than a message so that
/// encoding — and therefore the size decision — happens once, at the point
/// where a caller can still be told about it.
pub fn write_frame<W: Write>(writer: &mut W, frame: &[u8]) -> Result<(), IpcError> {
    writer.write_all(frame)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_the_message() {
        let frame = encode_frame(&Request::Echo {
            payload: b"hello".to_vec(),
        })
        .expect("encode");
        let decoded: Request = decode_frame(&frame).expect("decode");
        assert_eq!(
            decoded,
            Request::Echo {
                payload: b"hello".to_vec()
            }
        );
    }

    /// The hostile length prefix Phase 1's gate names explicitly.
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

    /// A peer that declares the maximum and sends nothing must not cost the
    /// maximum. Before this, four bytes on the wire bought a committed,
    /// zeroed megabyte — 262144x amplification, inside the size limit.
    #[test]
    fn size_limit_a_declared_payload_costs_only_what_arrives() {
        let at = u32::try_from(MAX_MESSAGE_BYTES).expect("fits in u32");
        let frame = at.to_le_bytes().to_vec();

        let mut cursor = std::io::Cursor::new(frame);
        let mut channel = Channel::new(&mut cursor, std::io::sink());
        assert!(matches!(
            channel.recv::<Request>(),
            Err(IpcError::PeerClosed)
        ));

        assert!(
            channel.scratch.capacity() <= SCRATCH_RETAIN_BYTES,
            "a declared-but-unsent megabyte must not commit a megabyte; \
             capacity was {}",
            channel.scratch.capacity()
        );
    }

    /// One large message must not cost the broker that memory forever.
    #[test]
    fn size_limit_scratch_is_returned_after_a_large_message() {
        let big = encode_frame(&Request::Echo {
            payload: vec![7u8; MAX_MESSAGE_BYTES / 2],
        })
        .expect("encode");

        let mut cursor = std::io::Cursor::new(big);
        let mut channel = Channel::new(&mut cursor, std::io::sink());
        let decoded: Request = channel.recv().expect("decode");
        assert!(matches!(decoded, Request::Echo { .. }));

        assert!(
            channel.scratch.capacity() <= SCRATCH_RETAIN_BYTES,
            "scratch was not returned; capacity is {}",
            channel.scratch.capacity()
        );
    }

    #[test]
    fn size_limit_refuses_to_send_an_oversized_message() {
        let mut sink = Vec::new();
        let poisoned = {
            let mut channel = Channel::new(std::io::empty(), &mut sink);
            let result = channel.send(&Request::Echo {
                payload: vec![0u8; MAX_MESSAGE_BYTES + 1],
            });
            assert!(matches!(result, Err(IpcError::MessageTooLarge { .. })));
            channel.is_poisoned()
        };
        assert!(sink.is_empty(), "nothing may be written for a refused send");
        assert!(!poisoned, "a pre-write refusal is recoverable");
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
        let mut cursor = std::io::Cursor::new(Vec::new());
        let mut channel = Channel::new(&mut cursor, std::io::sink());
        assert!(matches!(
            channel.recv::<Request>(),
            Err(IpcError::PeerClosed)
        ));
    }

    /// A valid encoding with junk appended must not decode. Otherwise a
    /// megabyte of wire buys a one-byte Ping and the size limit stops bounding
    /// anything useful.
    #[test]
    fn trailing_bytes_are_rejected() {
        let mut frame = encode_frame(&Request::Ping).expect("encode");
        let junk = vec![0u8; 99];
        // Extend the declared length so the trailing bytes are inside the
        // frame rather than after it.
        let new_len =
            u32::try_from(frame.len() - LENGTH_PREFIX_BYTES + junk.len()).expect("fits in u32");
        frame.splice(..LENGTH_PREFIX_BYTES, new_len.to_le_bytes());
        frame.extend_from_slice(&junk);

        assert!(
            matches!(decode_frame::<Request>(&frame), Err(IpcError::Malformed)),
            "a valid prefix followed by junk must not decode"
        );
    }

    /// A content process that is `cat` — reflecting the broker's own bytes
    /// verbatim, decoding nothing — must not produce a valid-looking reply.
    #[test]
    fn hostile_identity_a_reflected_request_is_not_a_valid_response() {
        for request in [
            Request::Ping,
            Request::Echo {
                payload: b"hi".to_vec(),
            },
            Request::FrameHost {
                frame: FrameId::new(1, 2),
            },
        ] {
            let frame = encode_frame(&request).expect("encode");
            assert!(
                matches!(decode_frame::<Response>(&frame), Err(IpcError::Malformed)),
                "reflecting {request:?} must not decode as a Response"
            );
        }
    }

    #[test]
    fn size_limit_an_oversized_prefix_poisons_the_channel() {
        let mut frame = u32::MAX.to_le_bytes().to_vec();
        frame.extend_from_slice(&encode_frame(&Request::Ping).expect("encode"));

        let mut cursor = std::io::Cursor::new(frame);
        let mut channel = Channel::new(&mut cursor, std::io::sink());
        assert!(matches!(
            channel.recv::<Request>(),
            Err(IpcError::MessageTooLarge { .. })
        ));

        // The payload was never consumed, so the stream is misaligned at an
        // offset the attacker chose. Resynchronising there would parse
        // attacker-framed bytes as a message.
        assert!(channel.is_poisoned());
        assert!(matches!(channel.recv::<Request>(), Err(IpcError::Poisoned)));
        assert!(matches!(
            channel.send(&Request::Ping),
            Err(IpcError::Poisoned)
        ));
    }

    #[test]
    fn a_failed_write_poisons_the_channel() {
        /// Accepts the length prefix, then fails — the shape a pipe takes when
        /// the peer goes away mid-frame.
        struct FailsAfter(usize);
        impl Write for FailsAfter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.0 == 0 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "peer gone",
                    ));
                }
                let n = buf.len().min(self.0);
                self.0 -= n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut channel = Channel::new(std::io::empty(), FailsAfter(6));
        let result = channel.send(&Request::Echo {
            payload: vec![0u8; 64],
        });
        assert!(result.is_err());
        assert!(
            channel.is_poisoned(),
            "a partial frame on the wire must end the channel"
        );
        assert!(matches!(
            channel.send(&Request::Ping),
            Err(IpcError::Poisoned)
        ));
    }
}
