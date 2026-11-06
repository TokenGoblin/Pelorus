#![forbid(unsafe_code)]

//! First-party, std-only image decoders (ADR 042).
//!
//! Every function here decodes hostile bytes. Nothing panics, every length is
//! checked before it sizes an allocation, and every allocation that depends on
//! the input goes through [`limits::Budget`] first. Output is deterministic:
//! no floating point is used in any decode path.
//!
//! # Output
//!
//! Always RGBA8, straight (non-premultiplied) alpha, row-major. Animations are
//! returned as full-canvas, already composited frames.
//!
//! # Formats
//!
//! | format | supported | rejected as `Unsupported` |
//! |--------|-----------|----------------------------|
//! | PNG    | all colour types and depths, Adam7, tRNS, APNG | none known |
//! | GIF    | 87a/89a, interlace, local/global palettes, transparency, disposal 0-3, NETSCAPE loop | none |
//! | JPEG   | baseline and progressive Huffman, 8-bit, grey and YCbCr, any 1/2/4 sampling, restart markers | arithmetic, 12-bit, lossless, CMYK/YCCK, hierarchical |
//! | BMP    | 1/4/8/16/24/32 bit, RLE4/RLE8, bit fields, top-down and bottom-up, core/V3/V4/V5 headers | JPEG/PNG-in-BMP |
//! | ICO    | the largest frame, BMP or PNG encoded | cursors |
//!
//! WebP is recognised by [`sniff`] and rejected with `Unsupported`; a later ADR
//! decides whether to write VP8/VP8L.
//!
//! # Limits
//!
//! See [`Limits`]. The defaults are 2^25 pixels per image (8192 x 4096), 4096
//! frames, and 512 MiB of live decoder memory (output plus scratch, summed over
//! all frames). The 512 MiB covers the worst single-image case, a 16-bit RGBA
//! PNG at the pixel cap (256 MiB of inflated rows plus 128 MiB of output).

pub mod bmp;
pub mod gif;
pub mod ico;
pub mod inflate;
pub mod jpeg;
pub mod limits;
pub mod png;

pub use limits::Limits;

use std::fmt;

/// A decoded still image: RGBA8, row-major, `rgba.len() == width * height * 4`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// One animation frame: a full-canvas composited image and how long it shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub image: Image,
    /// Display time in milliseconds. May be 0; clamping is the caller's policy.
    pub delay_ms: u32,
}

/// An animation. Every frame has the canvas size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Animation {
    pub width: u32,
    pub height: u32,
    pub frames: Vec<Frame>,
    /// Total number of times to play the animation; 0 means forever. A GIF
    /// without a NETSCAPE loop extension plays once (1); one whose extension
    /// says N plays N + 1 times (N repeats after the first play), N = 0 forever.
    pub loop_count: u32,
}

/// The result of a decode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decoded {
    Still(Image),
    Animated(Animation),
}

impl Decoded {
    /// The first frame (the only one of a still).
    pub fn first_image(&self) -> Option<&Image> {
        match self {
            Decoded::Still(i) => Some(i),
            Decoded::Animated(a) => a.frames.first().map(|f| &f.image),
        }
    }
}

/// Why decoding failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DecodeError {
    /// The input ended early.
    Truncated,
    /// Structurally invalid. The string says what.
    Malformed(&'static str),
    /// Zero or absurd dimensions.
    BadDimensions,
    /// A [`Limits`] bound would have been exceeded.
    LimitExceeded(&'static str),
    /// Valid, but uses a feature this crate does not implement.
    Unsupported(&'static str),
    /// The magic bytes match no supported format.
    UnknownFormat,
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => write!(f, "truncated image"),
            DecodeError::Malformed(m) => write!(f, "malformed image: {m}"),
            DecodeError::BadDimensions => write!(f, "bad image dimensions"),
            DecodeError::LimitExceeded(m) => write!(f, "image limit exceeded: {m}"),
            DecodeError::Unsupported(m) => write!(f, "unsupported image feature: {m}"),
            DecodeError::UnknownFormat => write!(f, "unknown image format"),
        }
    }
}

impl std::error::Error for DecodeError {}

/// An image format recognised by [`sniff`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Gif,
    Jpeg,
    Bmp,
    Ico,
    /// Recognised, never decoded.
    Webp,
}

/// Identify a format from its leading magic bytes only.
pub fn sniff(data: &[u8]) -> Option<Format> {
    if data.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some(Format::Png)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if data.starts_with(b"BM") {
        Some(Format::Bmp)
    } else if data.starts_with(&[0, 0, 1, 0]) {
        Some(Format::Ico)
    } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
        Some(Format::Webp)
    } else {
        None
    }
}

/// Sniff and decode.
pub fn decode(data: &[u8], limits: &Limits) -> Result<Decoded, DecodeError> {
    match sniff(data) {
        Some(Format::Png) => png::decode(data, limits),
        Some(Format::Gif) => gif::decode(data, limits),
        Some(Format::Jpeg) => jpeg::decode(data, limits).map(Decoded::Still),
        Some(Format::Bmp) => bmp::decode(data, limits).map(Decoded::Still),
        Some(Format::Ico) => ico::decode(data, limits).map(Decoded::Still),
        Some(Format::Webp) => Err(DecodeError::Unsupported("WebP")),
        None => Err(DecodeError::UnknownFormat),
    }
}

/// Little-endian readers over a slice that never panic; short reads give 0 and
/// callers check lengths before relying on them.
pub(crate) fn le16(d: &[u8], at: usize) -> u16 {
    match d.get(at..).and_then(|s| s.get(..2)) {
        Some(&[a, b]) => u16::from_le_bytes([a, b]),
        _ => 0,
    }
}
pub(crate) fn le32(d: &[u8], at: usize) -> u32 {
    match d.get(at..).and_then(|s| s.get(..4)) {
        Some(&[a, b, c, e]) => u32::from_le_bytes([a, b, c, e]),
        _ => 0,
    }
}
pub(crate) fn be16(d: &[u8], at: usize) -> u16 {
    match d.get(at..).and_then(|s| s.get(..2)) {
        Some(&[a, b]) => u16::from_be_bytes([a, b]),
        _ => 0,
    }
}
pub(crate) fn be32(d: &[u8], at: usize) -> u32 {
    match d.get(at..).and_then(|s| s.get(..4)) {
        Some(&[a, b, c, e]) => u32::from_be_bytes([a, b, c, e]),
        _ => 0,
    }
}
/// Byte at `at`, or 0 past the end.
pub(crate) fn at(d: &[u8], i: usize) -> u8 {
    d.get(i).copied().unwrap_or(0)
}
/// Store `v` at `i` if in range.
pub(crate) fn put(d: &mut [u8], i: usize, v: u8) {
    if let Some(x) = d.get_mut(i) {
        *x = v;
    }
}
