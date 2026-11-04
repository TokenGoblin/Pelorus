//! Font transfer for the GPU process: the sender's chunking and the receiver's
//! reassembly (Phase 9 unit 4).
//!
//! A font does not fit in one message (and may be many megabytes), so it
//! crosses as one [`GpuRequest::FontBegin`] naming the font and its exact
//! length, then [`GpuRequest::FontChunk`]s. As with the frame's strips, **the
//! receiver does the arithmetic**: every chunk but the last is exactly
//! [`MAX_FONT_CHUNK_BYTES`], the last is exactly the remainder, the offsets are
//! consecutive from zero, and the receiver knows all of that from `len` alone.
//! A sender that splits differently is malformed however plausible its bytes.
//! That leaves exactly one acceptable sequence for a given font, which is what
//! "canonical" means here.
//!
//! The assembler is bytes only: it does not know what a font is. Parsing is the
//! caller's job, in the sandboxed process, on the completed bytes.
//!
//! Memory follows chunks as they arrive, never a declared `len`: a sender that
//! promises 16 MiB and stops costs what it sent. Fonts are sent one at a time:
//! a new font may not begin until the previous one is complete.
//!
//! Caps (all in [`crate::gpu`]): [`MAX_FONT_BYTES`] per font, [`MAX_FONTS`]
//! fonts, [`MAX_TOTAL_FONT_BYTES`] in total, counting every font ever begun.

use crate::gpu::{
    FontBytes, GpuRequest, MAX_FONT_BYTES, MAX_FONT_CHUNK_BYTES, MAX_FONTS, MAX_TOTAL_FONT_BYTES,
};

/// Why a font transfer message was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontTransferError {
    /// `len` is zero or over [`MAX_FONT_BYTES`].
    BadLength,
    /// A font with this id was already begun (complete or not).
    Duplicate,
    /// More than [`MAX_FONTS`] fonts, or more than [`MAX_TOTAL_FONT_BYTES`].
    OverCap,
    /// A font began while another was incomplete.
    Interleaved,
    /// A chunk for a font that was never begun, or is already complete.
    UnknownFont,
    /// A chunk whose offset is not the bytes received so far.
    WrongOffset,
    /// A chunk whose size is not what the receiver's arithmetic says.
    WrongSize,
}

/// What a font message did: `Ok(Some((font, bytes)))` completed a font.
pub type FontStep = Result<Option<(u32, Vec<u8>)>, FontTransferError>;

struct InProgress {
    font: u32,
    len: usize,
    bytes: Vec<u8>,
}

/// Reassembles fonts from [`GpuRequest::FontBegin`] and
/// [`GpuRequest::FontChunk`], accepting only the one valid sequence.
#[derive(Default)]
pub struct FontAssembler {
    current: Option<InProgress>,
    /// Ids of every font begun, for duplicate detection. At most [`MAX_FONTS`].
    seen: Vec<u32>,
    total: u64,
}

impl FontAssembler {
    /// A receiver that has seen nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Fonts begun so far (complete or not).
    pub fn fonts_begun(&self) -> usize {
        self.seen.len()
    }

    /// Whether a font is partway through transfer.
    pub fn in_progress(&self) -> bool {
        self.current.is_some()
    }

    /// Accept a `FontBegin`.
    pub fn begin(&mut self, font: u32, len: u32) -> Result<(), FontTransferError> {
        if self.current.is_some() {
            return Err(FontTransferError::Interleaved);
        }
        if len == 0 || len > MAX_FONT_BYTES {
            return Err(FontTransferError::BadLength);
        }
        if self.seen.contains(&font) {
            return Err(FontTransferError::Duplicate);
        }
        if self.seen.len() >= MAX_FONTS
            || self.total.saturating_add(u64::from(len)) > MAX_TOTAL_FONT_BYTES
        {
            return Err(FontTransferError::OverCap);
        }
        let len = usize::try_from(len).map_err(|_| FontTransferError::BadLength)?;
        self.seen.push(font);
        self.total += len as u64;
        self.current = Some(InProgress {
            font,
            len,
            // Not sized from `len`: memory follows the chunks that arrive.
            bytes: Vec::new(),
        });
        Ok(())
    }

    /// Accept a `FontChunk`. `Ok(Some((font, bytes)))` when it completes the
    /// font.
    pub fn chunk(
        &mut self,
        font: u32,
        offset: u32,
        bytes: &[u8],
    ) -> Result<Option<(u32, Vec<u8>)>, FontTransferError> {
        let current = self
            .current
            .as_mut()
            .filter(|c| c.font == font)
            .ok_or(FontTransferError::UnknownFont)?;
        if usize::try_from(offset).ok() != Some(current.bytes.len()) {
            return Err(FontTransferError::WrongOffset);
        }
        let remaining = current.len - current.bytes.len();
        if bytes.len() != remaining.min(MAX_FONT_CHUNK_BYTES) {
            return Err(FontTransferError::WrongSize);
        }
        current.bytes.extend_from_slice(bytes);
        if current.bytes.len() == current.len {
            return Ok(self.current.take().map(|c| (c.font, c.bytes)));
        }
        Ok(None)
    }

    /// Apply a request if it is a font message; `None` for any other request.
    pub fn accept(&mut self, request: &GpuRequest) -> Option<FontStep> {
        match request {
            GpuRequest::FontBegin { font, len } => Some(self.begin(*font, *len).map(|()| None)),
            GpuRequest::FontChunk {
                font,
                offset,
                bytes,
            } => Some(self.chunk(*font, *offset, bytes.as_slice())),
            _ => None,
        }
    }
}

/// The requests that carry `bytes` as font `font`, in order, or `None` if the
/// font is empty or over [`MAX_FONT_BYTES`]. Lazy: no chunk is copied until it
/// is asked for.
pub fn font_requests(font: u32, bytes: &[u8]) -> Option<impl Iterator<Item = GpuRequest> + '_> {
    let len = u32::try_from(bytes.len()).ok()?;
    if len == 0 || len > MAX_FONT_BYTES {
        return None;
    }
    let begin = std::iter::once(GpuRequest::FontBegin { font, len });
    let chunks = bytes
        .chunks(MAX_FONT_CHUNK_BYTES)
        .enumerate()
        .filter_map(move |(i, chunk)| {
            let offset = u32::try_from(i.checked_mul(MAX_FONT_CHUNK_BYTES)?).ok()?;
            Some(GpuRequest::FontChunk {
                font,
                offset,
                bytes: FontBytes::new(chunk.to_vec()).ok()?,
            })
        });
    Some(begin.chain(chunks))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i % 253) as u8).collect()
    }

    /// Feed every request of `font_requests` to an assembler.
    fn transfer(a: &mut FontAssembler, id: u32, bytes: &[u8]) -> Option<(u32, Vec<u8>)> {
        let mut done = None;
        for request in font_requests(id, bytes).expect("requests") {
            if let Some(r) = a.accept(&request)
                && let Some(font) = r.expect("accepted")
            {
                done = Some(font);
            }
        }
        done
    }

    #[test]
    fn a_font_roundtrips_at_the_chunk_boundaries() {
        for n in [
            1,
            MAX_FONT_CHUNK_BYTES - 1,
            MAX_FONT_CHUNK_BYTES,
            MAX_FONT_CHUNK_BYTES + 1,
            3 * MAX_FONT_CHUNK_BYTES,
            3 * MAX_FONT_CHUNK_BYTES + 17,
        ] {
            let bytes = data(n);
            let mut a = FontAssembler::new();
            assert_eq!(transfer(&mut a, 5, &bytes), Some((5, bytes.clone())), "{n}");
            assert!(!a.in_progress());
        }
    }

    #[test]
    fn the_sender_splits_exactly_as_the_receiver_expects() {
        let n = 2 * MAX_FONT_CHUNK_BYTES + 10;
        let bytes = data(n);
        let sizes: Vec<usize> = font_requests(1, &bytes)
            .expect("requests")
            .filter_map(|r| match r {
                GpuRequest::FontChunk { bytes, .. } => Some(bytes.len()),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, vec![MAX_FONT_CHUNK_BYTES, MAX_FONT_CHUNK_BYTES, 10]);
    }

    #[test]
    fn the_sender_refuses_what_the_receiver_would() {
        assert!(font_requests(1, &[]).is_none());
        let big = vec![0u8; MAX_FONT_BYTES as usize + 1];
        assert!(font_requests(1, &big).is_none());
    }

    #[test]
    fn lengths_outside_the_cap_are_refused() {
        let mut a = FontAssembler::new();
        assert_eq!(a.begin(1, 0), Err(FontTransferError::BadLength));
        assert_eq!(
            a.begin(1, MAX_FONT_BYTES + 1),
            Err(FontTransferError::BadLength)
        );
        assert_eq!(a.begin(1, u32::MAX), Err(FontTransferError::BadLength));
        assert_eq!(a.fonts_begun(), 0);
        assert_eq!(a.begin(1, MAX_FONT_BYTES), Ok(()));
    }

    #[test]
    fn a_chunk_for_an_unknown_font_is_refused() {
        let mut a = FontAssembler::new();
        assert_eq!(a.chunk(1, 0, &[1]), Err(FontTransferError::UnknownFont));
        a.begin(1, 4).expect("begin");
        assert_eq!(
            a.chunk(2, 0, &[1, 2, 3, 4]),
            Err(FontTransferError::UnknownFont)
        );
        // A completed font takes no more chunks.
        assert!(a.chunk(1, 0, &[1, 2, 3, 4]).expect("chunk").is_some());
        assert_eq!(a.chunk(1, 4, &[1]), Err(FontTransferError::UnknownFont));
    }

    #[test]
    fn a_duplicate_font_id_is_refused_even_after_completion() {
        let mut a = FontAssembler::new();
        a.begin(9, 2).expect("begin");
        assert_eq!(a.begin(9, 2), Err(FontTransferError::Interleaved));
        a.chunk(9, 0, &[1, 2]).expect("done");
        assert_eq!(a.begin(9, 2), Err(FontTransferError::Duplicate));
        assert_eq!(a.begin(10, 2), Ok(()));
    }

    #[test]
    fn chunks_out_of_order_or_the_wrong_size_are_refused() {
        let n = 2 * MAX_FONT_CHUNK_BYTES + 3;
        let bytes = data(n);
        let c = MAX_FONT_CHUNK_BYTES;
        let fresh = || {
            let mut a = FontAssembler::new();
            a.begin(1, n as u32).expect("begin");
            a
        };
        // The second chunk first.
        assert_eq!(
            fresh().chunk(1, c as u32, &bytes[c..2 * c]),
            Err(FontTransferError::WrongOffset)
        );
        // Right offset, short and long.
        assert_eq!(
            fresh().chunk(1, 0, &bytes[..c - 1]),
            Err(FontTransferError::WrongSize)
        );
        assert_eq!(
            fresh().chunk(1, 0, &bytes[..c + 1]),
            Err(FontTransferError::WrongSize)
        );
        // A repeated chunk.
        let mut a = fresh();
        assert_eq!(a.chunk(1, 0, &bytes[..c]), Ok(None));
        assert_eq!(
            a.chunk(1, 0, &bytes[..c]),
            Err(FontTransferError::WrongOffset)
        );
        // The last chunk must be exactly the remainder.
        assert_eq!(a.chunk(1, c as u32, &bytes[c..2 * c]), Ok(None));
        assert_eq!(
            a.chunk(1, 2 * c as u32, &bytes[2 * c..2 * c + 2]),
            Err(FontTransferError::WrongSize)
        );
        assert!(
            a.chunk(1, 2 * c as u32, &bytes[2 * c..])
                .expect("last")
                .is_some()
        );
    }

    #[test]
    fn the_font_count_and_total_caps_hold() {
        let mut a = FontAssembler::new();
        for id in 0..MAX_FONTS as u32 {
            a.begin(id, 1).expect("begin");
            a.chunk(id, 0, &[0]).expect("chunk");
        }
        assert_eq!(a.begin(1000, 1), Err(FontTransferError::OverCap));

        let mut a = FontAssembler::new();
        let per = MAX_FONT_BYTES;
        let fits = (MAX_TOTAL_FONT_BYTES / u64::from(per)) as u32;
        for id in 0..fits {
            a.begin(id, per).expect("begin");
            // Abandoning is not possible: complete it by sending the bytes.
            let bytes = vec![0u8; per as usize];
            for request in font_requests(id, &bytes).expect("requests").skip(1) {
                a.accept(&request).expect("font message").expect("chunk");
            }
        }
        assert_eq!(a.begin(fits, 1), Err(FontTransferError::OverCap));
    }

    #[test]
    fn a_declared_length_allocates_nothing() {
        let mut a = FontAssembler::new();
        a.begin(1, MAX_FONT_BYTES).expect("begin");
        assert_eq!(a.current.as_ref().map(|c| c.bytes.capacity()), Some(0));
    }

    #[test]
    fn non_font_requests_are_not_ours() {
        assert!(FontAssembler::new().accept(&GpuRequest::Draw).is_none());
    }
}
