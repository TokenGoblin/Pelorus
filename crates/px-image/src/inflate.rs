//! zlib / DEFLATE decoding, CRC-32 and Adler-32. Copied in structure from the
//! px-paint PNG decoder (see docs/backlog.md) and rewritten to satisfy the
//! panic lints: no indexing, bounded output, no allocation from input lengths.

use crate::DecodeError;

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        std::array::from_fn(|n| {
            let mut c = n as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    0xEDB8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            c
        })
    })
}

/// CRC-32 (IEEE) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    let table = crc_table();
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        let idx = ((c ^ u32::from(b)) & 0xFF) as usize;
        c = table.get(idx).copied().unwrap_or(0) ^ (c >> 8);
    }
    !c
}

/// Adler-32 of `data`.
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += u32::from(x);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

const LBASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEXT: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DBASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DEXT: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    n: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            acc: 0,
            n: 0,
        }
    }
    fn refill(&mut self) {
        while self.n <= 56 {
            let Some(&b) = self.data.get(self.pos) else {
                break;
            };
            self.pos += 1;
            self.acc |= u64::from(b) << self.n;
            self.n += 8;
        }
    }
    /// Read `n` (<= 16) bits, least significant first.
    fn bits(&mut self, n: u32) -> Result<u32, DecodeError> {
        if self.n < n {
            self.refill();
            if self.n < n {
                return Err(DecodeError::Truncated);
            }
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.n -= n;
        Ok(v)
    }
    fn bit(&mut self) -> Result<u32, DecodeError> {
        self.bits(1)
    }
    /// Drop the partial byte and hand prefetched whole bytes back.
    fn byte_align(&mut self) {
        let whole = (self.n / 8) as usize;
        self.pos = self.pos.saturating_sub(whole);
        self.acc = 0;
        self.n = 0;
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, DecodeError> {
        let mut count = [0u16; 16];
        for &l in lengths {
            if let Some(c) = count.get_mut(usize::from(l & 15)) {
                *c += 1;
            }
        }
        let mut left: i32 = 1;
        for &c in count.iter().skip(1) {
            left = (left << 1) - i32::from(c);
            if left < 0 {
                return Err(DecodeError::Malformed("over-subscribed Huffman code"));
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            let prev = offs.get(l).copied().unwrap_or(0);
            let c = count.get(l).copied().unwrap_or(0);
            if let Some(o) = offs.get_mut(l + 1) {
                *o = prev + c;
            }
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0
                && let Some(o) = offs.get_mut(usize::from(l & 15))
            {
                if let Some(slot) = symbol.get_mut(usize::from(*o)) {
                    *slot = s as u16;
                }
                *o += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, DecodeError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.bit()? as i32;
            let count = i32::from(self.count.get(len).copied().unwrap_or(0));
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or(DecodeError::Malformed("bad Huffman symbol"));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(DecodeError::Malformed("invalid Huffman code"))
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), DecodeError> {
    let mut l = [0u8; 288];
    for (i, v) in l.iter_mut().enumerate() {
        *v = match i {
            0..=143 => 8,
            144..=255 => 9,
            256..=279 => 7,
            _ => 8,
        };
    }
    Ok((Huffman::new(&l)?, Huffman::new(&[5u8; 30])?))
}

/// Output buffer that never grows beyond `limit` and reserves in steps.
struct Out {
    v: Vec<u8>,
    limit: usize,
}

impl Out {
    fn room(&mut self, extra: usize) -> Result<(), DecodeError> {
        if extra > self.limit.saturating_sub(self.v.len()) {
            return Err(DecodeError::Malformed(
                "inflated data exceeds expected size",
            ));
        }
        let need = self.v.len() + extra;
        if need > self.v.capacity() {
            let want = need
                .max(self.v.capacity().saturating_mul(2))
                .max(4096)
                .min(self.limit);
            self.v
                .try_reserve_exact(want - self.v.len())
                .map_err(|_| DecodeError::LimitExceeded("allocation"))?;
        }
        Ok(())
    }
}

fn inflate_codes(
    r: &mut BitReader,
    out: &mut Out,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), DecodeError> {
    loop {
        let sym = usize::from(lit.decode(r)?);
        if sym < 256 {
            out.room(1)?;
            out.v.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let li = sym - 257;
            let (Some(&lb), Some(&le)) = (LBASE.get(li), LEXT.get(li)) else {
                return Err(DecodeError::Malformed("bad length symbol"));
            };
            let len = usize::from(lb) + r.bits(u32::from(le))? as usize;
            let di = usize::from(dist.decode(r)?);
            let (Some(&db), Some(&de)) = (DBASE.get(di), DEXT.get(di)) else {
                return Err(DecodeError::Malformed("bad distance symbol"));
            };
            let d = usize::from(db) + r.bits(u32::from(de))? as usize;
            if d > out.v.len() {
                return Err(DecodeError::Malformed("distance too far back"));
            }
            out.room(len)?;
            let start = out.v.len() - d;
            for i in 0..len {
                let b = out.v.get(start + i).copied().unwrap_or(0);
                out.v.push(b);
            }
        }
    }
}

fn read_dynamic(r: &mut BitReader) -> Result<(Huffman, Huffman), DecodeError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = r.bits(5)? as usize + 257;
    let ndist = r.bits(5)? as usize + 1;
    let ncode = r.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(DecodeError::Malformed("too many Huffman codes"));
    }
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(ncode) {
        let v = r.bits(3)? as u8;
        if let Some(slot) = cl.get_mut(o) {
            *slot = v;
        }
    }
    let clh = Huffman::new(&cl)?;
    let mut lengths = [0u8; 316];
    let total = nlen + ndist;
    let mut i = 0;
    while i < total {
        let sym = clh.decode(r)?;
        if sym < 16 {
            if let Some(s) = lengths.get_mut(i) {
                *s = sym as u8;
            }
            i += 1;
        } else {
            let (val, rep) = match sym {
                16 => {
                    let prev = i
                        .checked_sub(1)
                        .and_then(|j| lengths.get(j))
                        .copied()
                        .ok_or(DecodeError::Malformed("repeat with no previous length"))?;
                    (prev, 3 + r.bits(2)? as usize)
                }
                17 => (0, 3 + r.bits(3)? as usize),
                _ => (0, 11 + r.bits(7)? as usize),
            };
            if i + rep > total {
                return Err(DecodeError::Malformed("code length repeat overruns"));
            }
            for _ in 0..rep {
                if let Some(s) = lengths.get_mut(i) {
                    *s = val;
                }
                i += 1;
            }
        }
    }
    if lengths.get(256).copied().unwrap_or(0) == 0 {
        return Err(DecodeError::Malformed("no end-of-block code"));
    }
    let lit = lengths
        .get(..nlen)
        .ok_or(DecodeError::Malformed("code lengths"))?;
    let dist = lengths
        .get(nlen..total)
        .ok_or(DecodeError::Malformed("code lengths"))?;
    Ok((Huffman::new(lit)?, Huffman::new(dist)?))
}

/// Inflate a raw DEFLATE stream, refusing to produce more than `limit` bytes.
pub fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    let mut r = BitReader::new(data);
    let mut out = Out {
        v: Vec::new(),
        limit,
    };
    loop {
        let last = r.bit()?;
        match r.bits(2)? {
            0 => {
                r.byte_align();
                let hdr = data
                    .get(r.pos..)
                    .and_then(|s| s.get(..4))
                    .ok_or(DecodeError::Truncated)?;
                let len = u16::from_le_bytes([at(hdr, 0), at(hdr, 1)]);
                let nlen = u16::from_le_bytes([at(hdr, 2), at(hdr, 3)]);
                if len != !nlen {
                    return Err(DecodeError::Malformed("stored block length check"));
                }
                let len = usize::from(len);
                let start = r.pos + 4;
                let body = data
                    .get(start..)
                    .and_then(|s| s.get(..len))
                    .ok_or(DecodeError::Truncated)?;
                out.room(len)?;
                out.v.extend_from_slice(body);
                r.pos = start + len;
            }
            1 => {
                let (l, d) = fixed_tables()?;
                inflate_codes(&mut r, &mut out, &l, &d)?;
            }
            2 => {
                let (l, d) = read_dynamic(&mut r)?;
                inflate_codes(&mut r, &mut out, &l, &d)?;
            }
            _ => return Err(DecodeError::Malformed("reserved block type")),
        }
        if last == 1 {
            return Ok(out.v);
        }
    }
}

fn at(d: &[u8], i: usize) -> u8 {
    crate::at(d, i)
}

/// Decompress a zlib stream into at most `limit` bytes and check its Adler-32.
pub fn zlib_decompress(z: &[u8], limit: usize) -> Result<Vec<u8>, DecodeError> {
    if z.len() < 6 {
        return Err(DecodeError::Truncated);
    }
    let (cmf, flg) = (at(z, 0), at(z, 1));
    if cmf & 0x0F != 8
        || cmf >> 4 > 7
        || !((u32::from(cmf) << 8) | u32::from(flg)).is_multiple_of(31)
    {
        return Err(DecodeError::Malformed("bad zlib header"));
    }
    if flg & 0x20 != 0 {
        return Err(DecodeError::Unsupported("zlib preset dictionary"));
    }
    let body = z.get(2..z.len() - 4).ok_or(DecodeError::Truncated)?;
    let out = inflate(body, limit)?;
    let want = crate::be32(z, z.len() - 4);
    if adler32(&out) != want {
        return Err(DecodeError::Malformed("zlib Adler-32 mismatch"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_known_answers() {
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn adler32_known_answers() {
        assert_eq!(adler32(b""), 1);
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
    }

    #[test]
    fn stored_and_fixed_by_hand() {
        // Stored block: BFINAL=1 BTYPE=00, LEN=3, NLEN=!3, "abc".
        let s = [0x01, 3, 0, 0xFC, 0xFF, b'a', b'b', b'c'];
        assert_eq!(inflate(&s, 3).unwrap(), b"abc");
        // zlib.compress(b"a"*10) with fixed Huffman: 4b 4c 84 01 00
        let f = [0x4B, 0x4C, 0x84, 0x01, 0x00];
        assert_eq!(inflate(&f, 10).unwrap(), vec![b'a'; 10]);
        assert!(inflate(&f, 9).is_err(), "limit is enforced");
    }

    #[test]
    fn rejects_garbage() {
        assert!(inflate(&[0x07], 10).is_err()); // reserved block type 3
        assert!(zlib_decompress(&[0; 8], 10).is_err());
    }
}
