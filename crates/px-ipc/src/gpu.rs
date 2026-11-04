//! The GPU process vocabulary (Phase 8, ADR 033 (d) and (e)).
//!
//! Two more typed enums on the same channel machinery: [`GpuRequest`] travels
//! broker to GPU process, [`GpuResponse`] travels back. Each has its own
//! direction tag, so the codec's reflection defence applies to this pair too.
//!
//! # The GPU process is the less trusted side of this pair
//!
//! A GPU process runs a rasterizer (and eventually a driver stack) on data
//! derived from pages. It is sandboxed because it is expected to be
//! compromised some day. So the broker decodes [`GpuResponse`] as hostile
//! bytes, exactly as it decodes a content process's `Request`, and the types
//! below are shaped for that:
//!
//! - Every variable-length field is a [`Bounded`] vector whose decoder checks
//!   the declared element count against its own cap **before** it stores a
//!   single element. The 1 MiB message ceiling already bounds the bytes; the
//!   cap bounds the *amplification*, which is the other half. A `PopClip` item
//!   is one byte on the wire and tens of bytes in memory, so without a
//!   per-field cap a megabyte of wire would buy tens of megabytes of heap.
//! - A frame does not fit in one message, so it crosses in row strips of at
//!   most [`MAX_STRIP_BYTES`], and the receiver (not the sender) works out how
//!   many strips and how many rows each must carry. A sender that disagrees
//!   with the receiver's arithmetic is malformed.
//! - Nothing here names its sender. A request carries a `serial`, a counter the
//!   broker chose, which the header echoes so a stale frame cannot be taken for
//!   a fresh one. A serial is not identity: it authenticates nothing.
//!
//! # Canonical encoding
//!
//! [`Bounded`] serializes exactly as the `Vec` it wraps, so the codec's rule
//! (decode, re-encode, require byte equality) holds without change.
//!
//! # The testing vocabulary
//!
//! [`GpuRequest::Probe`], [`GpuRequest::Arm`] and the types they use exist only
//! under `cfg(feature = "testing")` (§14.4). They let a broker test make a real
//! GPU process attempt forbidden operations and misbehave on demand. A release
//! build has no such variant, so a hostile peer cannot name one: it fails to
//! decode.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Message;

/// Largest pixel payload of one [`GpuResponse::Strip`], in bytes.
///
/// A quarter of [`crate::MAX_MESSAGE_BYTES`]: a strip plus its few bytes of
/// framing is far inside the ceiling.
pub const MAX_STRIP_BYTES: usize = 256 * 1024;

/// Most display items one [`GpuRequest::Items`] may carry.
pub const MAX_ITEMS_PER_MESSAGE: usize = 1024;

/// Most display items one frame may declare.
///
/// 262,144 items at under 100 bytes each is a list in the low tens of
/// megabytes: more than any Phase 8 page needs by two orders of magnitude, and
/// small enough that a hostile peer cannot make the GPU process allocate
/// without bound by declaring a huge count and then feeding it.
pub const MAX_DISPLAY_ITEMS: u32 = 1 << 18;

/// Most glyphs one [`GpuRequest::Glyphs`] may carry. A glyph is at most 15 bytes
/// on the wire (three varints), so a full message is about 60 KiB.
pub const MAX_GLYPHS_PER_MESSAGE: usize = 4096;

/// Most glyphs one frame's arena may declare: the display list's own
/// `MAX_GLYPHS` (a test in `px-gpu` keeps the two equal). 3 MiB in memory.
pub const MAX_DISPLAY_GLYPHS: u32 = 1 << 18;

/// Most bytes of one [`GpuRequest::FontChunk`]. A quarter of the message
/// ceiling, as strips are, so a chunk plus framing is far inside it.
pub const MAX_FONT_CHUNK_BYTES: usize = 256 * 1024;

/// Largest single font. 16 MiB admits a large CJK face; a font is parsed by
/// the sandboxed process, so the cap is also the most data one hostile font
/// can make it parse.
pub const MAX_FONT_BYTES: u32 = 16 * 1024 * 1024;

/// Most fonts one process holds over its life. Fonts are never evicted, so
/// this is also the most a process is ever asked to retain.
pub const MAX_FONTS: usize = 64;

/// Most font bytes one process holds in total (64 MiB).
pub const MAX_TOTAL_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// Largest frame edge, matching the software rasterizer's own limit.
pub const MAX_FRAME_DIMENSION: u32 = 16384;

/// Largest frame pixel count, matching the software rasterizer's own limit.
pub const MAX_FRAME_PIXELS: u64 = 1 << 25;

/// Most probe results one reply may carry.
#[cfg(feature = "testing")]
pub const MAX_PROBE_ATTEMPTS: usize = 32;

/// A vector that refuses to be longer than `MAX` on the wire.
///
/// The check is made from the sequence's declared length, before any element is
/// stored, in both directions. postcard encodes a sequence as a varint count
/// followed by the elements, and serde's own `Vec` visitor would take that
/// count's word for how much to reserve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bounded<T, const MAX: usize>(Vec<T>);

impl<T, const MAX: usize> Bounded<T, MAX> {
    /// Wrap a vector, or hand it back if it is longer than `MAX`, so a caller
    /// can chunk it.
    pub fn new(items: Vec<T>) -> Result<Self, Vec<T>> {
        if items.len() > MAX {
            Err(items)
        } else {
            Ok(Self(items))
        }
    }

    /// Wrap a vector, dropping what does not fit. For a producer that would
    /// rather say less than refuse (a log).
    pub fn truncated(mut items: Vec<T>) -> Self {
        items.truncate(MAX);
        Self(items)
    }

    /// The elements.
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Take the elements.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    /// How many elements.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl<T: Serialize, const MAX: usize> Serialize for Bounded<T, MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.len() > MAX {
            return Err(serde::ser::Error::custom("sequence exceeds its bound"));
        }
        serializer.collect_seq(&self.0)
    }
}

struct BoundedVisitor<T, const MAX: usize>(PhantomData<T>);

impl<'de, T: Deserialize<'de>, const MAX: usize> Visitor<'de> for BoundedVisitor<T, MAX> {
    type Value = Bounded<T, MAX>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a sequence of at most {MAX} elements")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        // The declared count, checked before a single element is stored. A
        // format that cannot say how long the sequence is cannot be bounded up
        // front, so it is refused: fail closed.
        let declared = seq
            .size_hint()
            .ok_or_else(|| serde::de::Error::custom("sequence has no declared length"))?;
        if declared > MAX {
            return Err(serde::de::Error::custom("sequence exceeds its bound"));
        }
        let mut out = Vec::with_capacity(declared);
        while let Some(element) = seq.next_element()? {
            if out.len() >= MAX {
                return Err(serde::de::Error::custom("sequence exceeds its bound"));
            }
            out.push(element);
        }
        Ok(Bounded(out))
    }
}

impl<'de, T: Deserialize<'de>, const MAX: usize> Deserialize<'de> for Bounded<T, MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(BoundedVisitor::<T, MAX>(PhantomData))
    }
}

/// One chunk of display items.
pub type ItemChunk = Bounded<WireItem, MAX_ITEMS_PER_MESSAGE>;

/// One strip's pixels: RGBA8, rows top to bottom.
pub type StripPixels = Bounded<u8, MAX_STRIP_BYTES>;

/// One chunk of the frame's glyph arena.
pub type GlyphChunk = Bounded<WireGlyph, MAX_GLYPHS_PER_MESSAGE>;

/// One chunk of a font file.
pub type FontBytes = Bounded<u8, MAX_FONT_CHUNK_BYTES>;

/// One positioned glyph: the glyph's index in its font and its origin in app
/// units, page space.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WireGlyph {
    /// Glyph index.
    pub glyph: u32,
    /// Origin x.
    pub x: i32,
    /// Origin y (baseline).
    pub y: i32,
}

/// A rectangle in app units (1/60 px), page space.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WireRect {
    /// Left edge.
    pub x: i32,
    /// Top edge.
    pub y: i32,
    /// Width.
    pub width: i32,
    /// Height.
    pub height: i32,
}

/// Non-premultiplied sRGB with 8-bit alpha.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WireColor {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
    /// Alpha.
    pub a: u8,
}

/// Border line style.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum WireBorderStyle {
    /// No border.
    None,
    /// A solid line.
    Solid,
}

/// One border side.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct WireBorderSide {
    /// Width in app units.
    pub width: i32,
    /// Style.
    pub style: WireBorderStyle,
    /// Colour.
    pub color: WireColor,
}

/// One paint operation, mirroring the display list's `Item`.
///
/// A mirror rather than the display list's own type so that this crate depends
/// on nothing but serde, and the wire shape does not move when the in-process
/// type does.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum WireItem {
    /// Fill a rectangle.
    FillRect {
        /// Where.
        rect: WireRect,
        /// With what.
        color: WireColor,
    },
    /// A border, sides in CSS order: top, right, bottom, left.
    Border {
        /// Around what.
        rect: WireRect,
        /// The four sides.
        sides: [WireBorderSide; 4],
    },
    /// A placeholder block for a run of text.
    TextRun {
        /// Where.
        rect: WireRect,
        /// With what.
        color: WireColor,
    },
    /// Begin a clip.
    PushClip {
        /// The clip rectangle.
        rect: WireRect,
    },
    /// End the innermost clip.
    PopClip,
    /// Draw glyphs `start .. start + len` of the frame's glyph arena
    /// ([`GpuRequest::Glyphs`]) from a font previously transferred.
    GlyphRun {
        /// The font, by the number the broker gave it in [`GpuRequest::FontBegin`].
        font: u32,
        /// Em size in app units.
        size: i32,
        /// Colour.
        color: WireColor,
        /// First glyph.
        start: u32,
        /// How many.
        len: u32,
    },
}

/// Why the GPU process declined to produce a frame.
///
/// Coarse on purpose. These are for the broker's log; the broker's reaction to
/// every one of them is the same (render in software), so none carries text a
/// hostile process could use to say something.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum GpuFailure {
    /// A zero or over-limit viewport.
    BadViewport,
    /// A declared item count over [`MAX_DISPLAY_ITEMS`], or more or fewer
    /// items arrived than were declared.
    BadItemCount,
    /// The list's clips do not balance, or nest too deeply.
    BadClips,
    /// A message arrived in the wrong order for the exchange.
    OutOfOrder,
    /// A coordinate outside the range layout can produce.
    BadCoordinate,
    /// The backend failed for a reason of its own.
    Backend,
    /// A font transfer broke the protocol: a chunk for no font, out of order,
    /// of the wrong size, a duplicate id, or over a cap.
    BadFontTransfer,
    /// A completed font did not parse. The process exits after saying so: a
    /// malformed font costs the process that met it and nothing else.
    BadFont,
    /// A declared glyph count over [`MAX_DISPLAY_GLYPHS`], more or fewer glyphs
    /// than declared, or a glyph run outside the arena.
    BadGlyphs,
    /// A glyph run names a font that was never transferred.
    UnknownFont,
}

/// Broker to GPU process.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum GpuRequest {
    /// Start a frame: its size and exactly how many items will follow.
    Begin {
        /// A counter the broker chose; echoed by the header. Not identity.
        serial: u32,
        /// Viewport width in pixels.
        width: u32,
        /// Viewport height in pixels.
        height: u32,
        /// How many items the following [`GpuRequest::Items`] will carry in
        /// total. At most [`MAX_DISPLAY_ITEMS`].
        item_count: u32,
        /// How many glyphs the following [`GpuRequest::Glyphs`] will carry in
        /// total (the arena the frame's glyph runs window into). At most
        /// [`MAX_DISPLAY_GLYPHS`].
        glyph_count: u32,
    },
    /// The next items, in paint order.
    Items {
        /// At most [`MAX_ITEMS_PER_MESSAGE`].
        items: ItemChunk,
    },
    /// All items sent; rasterize and reply.
    Draw,
    /// The next glyphs of the frame's arena, in arena order. All arrive after
    /// `Begin` and before `Draw`.
    Glyphs {
        /// At most [`MAX_GLYPHS_PER_MESSAGE`].
        glyphs: GlyphChunk,
    },
    /// Start transferring a font: its id and its exact length. The font's bytes
    /// follow in [`GpuRequest::FontChunk`]s, each of exactly
    /// `min(MAX_FONT_CHUNK_BYTES, remaining)` bytes, in order. The receiver
    /// works the arithmetic out; see [`crate::gpu_fonts::FontAssembler`].
    FontBegin {
        /// The id display lists will use for this font. Chosen by the broker;
        /// the receiver refuses a duplicate.
        font: u32,
        /// Total bytes, `1 ..= MAX_FONT_BYTES`.
        len: u32,
    },
    /// The next bytes of the font being transferred.
    FontChunk {
        /// Which font; must be the one `FontBegin` started.
        font: u32,
        /// Offset of this chunk: must equal the bytes received so far.
        offset: u32,
        /// At most [`MAX_FONT_CHUNK_BYTES`].
        bytes: FontBytes,
    },
    /// Testing only: attempt the forbidden operations and report.
    #[cfg(feature = "testing")]
    Probe,
    /// Testing only: misbehave during the next [`GpuRequest::Draw`].
    #[cfg(feature = "testing")]
    Arm {
        /// How.
        fault: GpuFault,
    },
}

/// GPU process to broker.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum GpuResponse {
    /// Startup finished and the process has confined itself.
    ///
    /// The broker does not believe this: it verifies the confinement from the
    /// operating system's view of the process before sending anything. The
    /// field is a report, kept for the log.
    Ready {
        /// The Landlock ABI the process says it enforced; 0 if none.
        landlock_abi: u32,
    },
    /// A frame follows, in strips.
    FrameHeader {
        /// The serial of the `Begin` this answers.
        serial: u32,
        /// Frame width in pixels.
        width: u32,
        /// Frame height in pixels.
        height: u32,
    },
    /// Rows `first_row .. first_row + rows` of the frame.
    Strip {
        /// First row in this strip.
        first_row: u32,
        /// How many rows.
        rows: u32,
        /// `rows * width * 4` bytes, no more than [`MAX_STRIP_BYTES`].
        pixels: StripPixels,
    },
    /// No frame.
    Failed {
        /// Why, coarsely.
        why: GpuFailure,
    },
    /// Testing only: what the forbidden operations did.
    #[cfg(feature = "testing")]
    Probe {
        /// One entry per attempt.
        attempts: Bounded<ProbeAttempt, MAX_PROBE_ATTEMPTS>,
    },
}

/// Testing only: a forbidden operation a probe attempts.
#[cfg(feature = "testing")]
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum ProbeOp {
    /// Open a world-readable system file.
    OpenSystemFile,
    /// Open the working directory for reading.
    OpenWorkingDirectory,
    /// Create an Internet-domain socket.
    Socket,
    /// Connect, on a descriptor that does not exist: only a policy that denies
    /// the call outright answers differently from "bad descriptor".
    Connect,
    /// Bind, probed the same way.
    Bind,
    /// Replace the process image.
    Exec,
}

/// Testing only: one probe result.
#[cfg(feature = "testing")]
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub struct ProbeAttempt {
    /// What was attempted.
    pub op: ProbeOp,
    /// Whether it worked. Anything but `false` under the profile is a failure
    /// of the profile.
    pub succeeded: bool,
    /// `errno` when it failed, else 0.
    pub errno: i32,
}

/// Testing only: how the process misbehaves during the next draw.
#[cfg(feature = "testing")]
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum GpuFault {
    /// Exit at once, before replying.
    Exit,
    /// Write bytes that are not a frame.
    Malformed,
    /// Write a length prefix over the message ceiling.
    Oversized,
    /// Never reply.
    Hang,
    /// Send the header and one strip, then stop.
    StallMidFrame,
    /// Send strips without end.
    Flood,
    /// Answer with a strip whose rows do not follow the header's arithmetic.
    WrongRows,
    /// Answer with a header carrying the wrong serial.
    WrongSerial,
}

impl Message for GpuRequest {
    const TAG: u8 = 0xA3;
}

impl Message for GpuResponse {
    const TAG: u8 = 0xA4;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IpcError, decode_frame, encode_frame};

    #[test]
    fn requests_and_responses_roundtrip() {
        let begin = GpuRequest::Begin {
            serial: 7,
            width: 800,
            height: 600,
            item_count: 2,
            glyph_count: 3,
        };
        let frame = encode_frame(&begin).expect("encode");
        assert_eq!(decode_frame::<GpuRequest>(&frame).expect("decode"), begin);

        let items = ItemChunk::new(vec![
            WireItem::PopClip,
            WireItem::FillRect {
                rect: WireRect {
                    x: 1,
                    y: -2,
                    width: 3,
                    height: 4,
                },
                color: WireColor {
                    r: 1,
                    g: 2,
                    b: 3,
                    a: 4,
                },
            },
        ])
        .expect("bounded");
        let request = GpuRequest::Items { items };
        let frame = encode_frame(&request).expect("encode");
        assert_eq!(decode_frame::<GpuRequest>(&frame).expect("decode"), request);
    }

    #[test]
    fn a_bounded_vec_encodes_as_the_plain_vec_does() {
        let bounded = StripPixels::new(vec![1, 2, 3]).expect("bounded");
        let a = postcard::to_allocvec(&bounded).expect("a");
        let b = postcard::to_allocvec(&vec![1u8, 2, 3]).expect("b");
        assert_eq!(a, b);
    }

    #[test]
    fn the_two_directions_are_tagged_differently() {
        assert_ne!(GpuRequest::TAG, GpuResponse::TAG);
        assert_ne!(GpuRequest::TAG, crate::Request::TAG);
        assert_ne!(GpuResponse::TAG, crate::Response::TAG);
        // A request's bytes are not a valid response.
        let frame = encode_frame(&GpuRequest::Draw).expect("encode");
        assert!(matches!(
            decode_frame::<GpuResponse>(&frame),
            Err(IpcError::Malformed)
        ));
    }
}
