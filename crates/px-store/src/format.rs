//! The on-disk log format. Pure: bytes in, bytes out, no filesystem.
//!
//! # Layout (all integers little-endian)
//!
//! ```text
//! header, 16 bytes:
//!   0..4   magic  "PXSL"
//!   4..8   version (u32)            this reader accepts exactly 1
//!   8..12  flags   (u32)            must be 0; unknown flags are refused
//!   12..16 CRC-32 of bytes 0..12
//!
//! record, repeated until end of file:
//!   0..4   body length N (u32)      N <= MAX_BODY_BYTES
//!   4..8   CRC-32 of (the 4 length bytes ++ the N body bytes)
//!   8..    body: op (u8) ns (u8) pid_len (u16) pid key_len (u16) key [value]
//!          op 1 = Put (value is the rest of the body), op 2 = Delete (no value)
//! ```
//!
//! # Recovery
//!
//! A reader scans records from the header on and stops at the first one that
//! is incomplete, over-long, fails its checksum, does not parse, or is
//! rejected by the engine (quota, unknown namespace, undecodable partition
//! id). Everything before that point is the *consistent prefix* and is what the
//! store exposes; everything from that point on is discarded (and truncated off
//! when the file is opened for writing). A partially written record is
//! therefore never visible: it either checks out whole or it is not there.
//!
//! The one place this can lose acknowledged data is genuine corruption in the
//! middle of the log: records after the damage are dropped too. That is the
//! price of a log with no resynchronisation marker, and it fails closed.
//!
//! A header that is a strict prefix of the canonical header is a store whose
//! creation was torn; it is treated as empty.

use crate::checksum::{crc32, crc32_update};
use crate::error::FormatError;

/// File magic.
pub const MAGIC: [u8; 4] = *b"PXSL";
/// The one version this code reads and writes.
pub const VERSION: u32 = 1;
/// Header size.
pub const HEADER_LEN: usize = 16;
/// Per-record framing (length + checksum).
pub const FRAME_LEN: usize = 8;

/// Largest key, bytes.
pub const MAX_KEY_BYTES: usize = 1024;
/// Largest value, bytes.
pub const MAX_VALUE_BYTES: usize = 1 << 20;
/// Largest encoded partition id, bytes.
pub const MAX_PARTITION_ID_BYTES: usize = 1024;
/// Largest record body: op, ns, two length fields, and the three payloads.
pub const MAX_BODY_BYTES: usize =
    2 + 2 + MAX_PARTITION_ID_BYTES + 2 + MAX_KEY_BYTES + MAX_VALUE_BYTES;

const OP_PUT: u8 = 1;
const OP_DELETE: u8 = 2;

/// The canonical header.
pub fn header_bytes() -> Vec<u8> {
    let mut h = Vec::with_capacity(HEADER_LEN);
    h.extend_from_slice(&MAGIC);
    h.extend_from_slice(&VERSION.to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    let crc = crc32(&h);
    h.extend_from_slice(&crc.to_le_bytes());
    h
}

/// What a file's first bytes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Header {
    /// Empty, or a torn creation: start a fresh log.
    Fresh,
    /// A valid header; records start at [`HEADER_LEN`].
    Valid,
}

/// Classify the start of a file.
pub fn check_header(bytes: &[u8]) -> Result<Header, FormatError> {
    if bytes.is_empty() {
        return Ok(Header::Fresh);
    }
    let canonical = header_bytes();
    if bytes.len() < HEADER_LEN {
        return if canonical.starts_with(bytes) {
            Ok(Header::Fresh)
        } else {
            Err(FormatError::NotAStore)
        };
    }
    let mut r = Reader::new(bytes);
    if r.take(4) != Some(&MAGIC[..]) {
        return Err(FormatError::NotAStore);
    }
    let version = r.u32().ok_or(FormatError::BadHeader)?;
    let flags = r.u32().ok_or(FormatError::BadHeader)?;
    if version != VERSION || flags != 0 {
        return Err(FormatError::UnsupportedVersion { version, flags });
    }
    let crc = r.u32().ok_or(FormatError::BadHeader)?;
    let covered = bytes.get(..12).ok_or(FormatError::BadHeader)?;
    if crc != crc32(covered) {
        return Err(FormatError::BadHeader);
    }
    Ok(Header::Valid)
}

/// One logged operation, borrowing from the buffer it was decoded from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op<'a> {
    /// Set `key` to `value` in namespace `ns` of partition `pid`.
    Put {
        /// Namespace byte.
        ns: u8,
        /// Encoded partition id.
        pid: &'a [u8],
        /// Key.
        key: &'a [u8],
        /// Value.
        value: &'a [u8],
    },
    /// Remove `key`.
    Delete {
        /// Namespace byte.
        ns: u8,
        /// Encoded partition id.
        pid: &'a [u8],
        /// Key.
        key: &'a [u8],
    },
}

/// Encode one record (framing included).
pub fn encode_record(op: &Op<'_>) -> Result<Vec<u8>, FormatError> {
    let (code, ns, pid, key, value): (u8, u8, &[u8], &[u8], &[u8]) = match op {
        Op::Put {
            ns,
            pid,
            key,
            value,
        } => (OP_PUT, *ns, pid, key, value),
        Op::Delete { ns, pid, key } => (OP_DELETE, *ns, pid, key, &[]),
    };
    if pid.len() > MAX_PARTITION_ID_BYTES
        || key.len() > MAX_KEY_BYTES
        || value.len() > MAX_VALUE_BYTES
    {
        return Err(FormatError::RecordTooLarge);
    }
    let mut body = Vec::with_capacity(6 + pid.len() + key.len() + value.len());
    body.push(code);
    body.push(ns);
    body.extend_from_slice(&u16::try_from(pid.len()).map_err(|_| FormatError::RecordTooLarge)?.to_le_bytes());
    body.extend_from_slice(pid);
    body.extend_from_slice(&u16::try_from(key.len()).map_err(|_| FormatError::RecordTooLarge)?.to_le_bytes());
    body.extend_from_slice(key);
    body.extend_from_slice(value);
    let len = u32::try_from(body.len()).map_err(|_| FormatError::RecordTooLarge)?;
    let len_bytes = len.to_le_bytes();
    let crc = crc32_update(crc32(&len_bytes), &body);
    let mut out = Vec::with_capacity(FRAME_LEN + body.len());
    out.extend_from_slice(&len_bytes);
    out.extend_from_slice(&crc.to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Result of decoding at one position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next<'a> {
    /// Nothing left: the buffer ended exactly on a record boundary.
    End,
    /// A whole, checksummed, well-formed record, and how many bytes it took.
    Record(Op<'a>, usize),
    /// Incomplete, over-long, corrupt or malformed. Stop here.
    Torn,
}

/// Decode the record at the start of `buf`.
pub fn decode_record(buf: &[u8]) -> Next<'_> {
    if buf.is_empty() {
        return Next::End;
    }
    let mut r = Reader::new(buf);
    let (Some(len), Some(crc)) = (r.u32(), r.u32()) else {
        return Next::Torn;
    };
    let Ok(len) = usize::try_from(len) else {
        return Next::Torn;
    };
    if len > MAX_BODY_BYTES {
        return Next::Torn;
    }
    let Some(body) = r.take(len) else {
        return Next::Torn;
    };
    if crc != crc32_update(crc32(&(len as u32).to_le_bytes()), body) {
        return Next::Torn;
    }
    match parse_body(body) {
        Some(op) => Next::Record(op, FRAME_LEN + len),
        None => Next::Torn,
    }
}

fn parse_body(body: &[u8]) -> Option<Op<'_>> {
    let mut r = Reader::new(body);
    let code = r.u8()?;
    let ns = r.u8()?;
    let pid_len = usize::from(r.u16()?);
    let pid = r.take(pid_len)?;
    let key_len = usize::from(r.u16()?);
    let key = r.take(key_len)?;
    if pid.len() > MAX_PARTITION_ID_BYTES || key.len() > MAX_KEY_BYTES {
        return None;
    }
    let value = r.rest();
    match code {
        OP_PUT if value.len() <= MAX_VALUE_BYTES => Some(Op::Put {
            ns,
            pid,
            key,
            value,
        }),
        OP_DELETE if value.is_empty() => Some(Op::Delete { ns, pid, key }),
        _ => None,
    }
}

/// A bounds-checked cursor. Every read returns `None` rather than panicking.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Start at the beginning of `buf`.
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Take `n` bytes.
    pub fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    /// Take everything that is left.
    pub fn rest(&mut self) -> &'a [u8] {
        let out = self.buf.get(self.pos..).unwrap_or_default();
        self.pos = self.buf.len();
        out
    }

    /// One byte.
    pub fn u8(&mut self) -> Option<u8> {
        self.take(1)?.first().copied()
    }

    /// Little-endian u16.
    pub fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    /// Little-endian u32.
    pub fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    /// Whether the cursor is at the end.
    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_sixteen_bytes_and_valid() {
        let h = header_bytes();
        assert_eq!(h.len(), HEADER_LEN);
        assert_eq!(check_header(&h), Ok(Header::Valid));
        // magic, version 1, flags 0 spelled out by hand.
        assert_eq!(h.get(..12), Some(&b"PXSL\x01\x00\x00\x00\x00\x00\x00\x00"[..]));
    }

    #[test]
    fn every_header_prefix_is_fresh_and_nothing_panics() {
        let h = header_bytes();
        for n in 0..HEADER_LEN {
            assert_eq!(check_header(h.get(..n).unwrap()), Ok(Header::Fresh), "n={n}");
        }
    }

    #[test]
    fn foreign_file_is_not_a_store() {
        assert_eq!(check_header(b"SQLite format 3\0"), Err(FormatError::NotAStore));
        assert_eq!(check_header(b"SQ"), Err(FormatError::NotAStore));
    }

    #[test]
    fn newer_version_and_unknown_flags_are_refused() {
        let mut h = header_bytes();
        h.get_mut(4).map(|b| *b = 2);
        assert_eq!(
            check_header(&h),
            Err(FormatError::UnsupportedVersion { version: 2, flags: 0 })
        );
        let mut h = header_bytes();
        h.get_mut(8).map(|b| *b = 1);
        assert_eq!(
            check_header(&h),
            Err(FormatError::UnsupportedVersion { version: 1, flags: 1 })
        );
    }

    #[test]
    fn damaged_header_checksum_is_refused() {
        let mut h = header_bytes();
        h.get_mut(15).map(|b| *b ^= 0xff);
        assert_eq!(check_header(&h), Err(FormatError::BadHeader));
    }

    fn put<'a>(pid: &'a [u8], key: &'a [u8], value: &'a [u8]) -> Op<'a> {
        Op::Put {
            ns: 0,
            pid,
            key,
            value,
        }
    }

    #[test]
    fn record_layout_by_hand() {
        let rec = encode_record(&put(b"P", b"k", b"vv")).unwrap();
        // body = op, ns, pid_len(2), "P", key_len(2), "k", "vv" = 10 bytes.
        let body = b"\x01\x00\x01\x00P\x01\x00kvv";
        assert_eq!(body.len(), 10);
        assert_eq!(rec.get(..4), Some(&10u32.to_le_bytes()[..]));
        assert_eq!(rec.get(8..), Some(&body[..]));
        let crc = crc32_update(crc32(&10u32.to_le_bytes()), body);
        assert_eq!(rec.get(4..8), Some(&crc.to_le_bytes()[..]));
        assert_eq!(rec.len(), 18);
    }

    #[test]
    fn put_and_delete_round_trip() {
        for op in [
            put(b"pid", b"key", b"value"),
            put(b"pid", b"", b""),
            Op::Delete {
                ns: 3,
                pid: b"pid",
                key: b"key",
            },
        ] {
            let rec = encode_record(&op).unwrap();
            assert_eq!(decode_record(&rec), Next::Record(op, rec.len()));
        }
        assert_eq!(decode_record(b""), Next::End);
    }

    #[test]
    fn every_truncation_of_a_record_is_torn() {
        let rec = encode_record(&put(b"pid", b"key", b"some value")).unwrap();
        for n in 1..rec.len() {
            assert_eq!(decode_record(rec.get(..n).unwrap()), Next::Torn, "n={n}");
        }
    }

    #[test]
    fn every_single_bit_flip_is_detected() {
        let rec = encode_record(&put(b"pid", b"key", b"some value")).unwrap();
        for i in 0..rec.len() {
            for bit in 0..8 {
                let mut bad = rec.clone();
                bad.get_mut(i).map(|b| *b ^= 1 << bit);
                assert_eq!(decode_record(&bad), Next::Torn, "byte {i} bit {bit}");
            }
        }
    }

    #[test]
    fn oversized_fields_are_refused_on_encode_and_decode() {
        let big = vec![0u8; MAX_VALUE_BYTES + 1];
        assert_eq!(
            encode_record(&put(b"p", b"k", &big)),
            Err(FormatError::RecordTooLarge)
        );
        let long_key = vec![0u8; MAX_KEY_BYTES + 1];
        assert_eq!(
            encode_record(&put(b"p", &long_key, b"")),
            Err(FormatError::RecordTooLarge)
        );
        // A length field larger than any legal body is torn without reading on.
        let mut hostile = Vec::new();
        hostile.extend_from_slice(&u32::MAX.to_le_bytes());
        hostile.extend_from_slice(&0u32.to_le_bytes());
        assert_eq!(decode_record(&hostile), Next::Torn);
    }

    #[test]
    fn well_checksummed_garbage_body_is_torn() {
        // Valid frame, body claims a 500-byte pid it does not have.
        let body = [1u8, 0, 0xF4, 0x01, 0];
        let len = (body.len() as u32).to_le_bytes();
        let crc = crc32_update(crc32(&len), &body);
        let mut rec = len.to_vec();
        rec.extend_from_slice(&crc.to_le_bytes());
        rec.extend_from_slice(&body);
        assert_eq!(decode_record(&rec), Next::Torn);
    }
}
