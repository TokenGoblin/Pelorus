//! A PNG encoder and decoder, std only.
//!
//! Baselines for pixel-diff tests are committed to git, so the encoder must be
//! small and *byte-for-byte deterministic*; the decoder reads those baselines back
//! and, being a decoder, is written as if its input were hostile.
//!
//! # Encoder
//!
//! * Colour type: **RGB (2)** when every pixel is opaque, else **RGBA (6)**; 8 bits
//!   per channel, no interlace, no ancillary chunks. Chunks: `IHDR`, one `IDAT`,
//!   `IEND`.
//! * Filter: chosen per row by the *minimum sum of absolute differences* heuristic
//!   (each filtered byte read as `i8`, absolute value, summed); candidates are
//!   evaluated in order None, Sub, Up, Average, Paeth and the first minimum wins.
//! * Compression: one zlib stream holding a single fixed-Huffman DEFLATE block over
//!   a greedy LZ77 parse (3-byte hash, hash chains of at most [`CHAIN_LIMIT`]
//!   candidates, 32 KiB window, matches 3..=258). Never stored blocks.
//!
//! # Decoder
//!
//! Non-interlaced, 8-bit grey (0), RGB (2), palette (3), grey+alpha (4), RGBA (6).
//! Full inflate (stored, fixed and dynamic Huffman). Output is always RGBA8.
//! Every chunk CRC and the zlib Adler-32 are verified. `tRNS` is honoured for
//! palette images and ignored for the others. Critical unknown chunks are rejected;
//! ancillary ones are skipped.
//!
//! Bounds: width and height are each at most [`MAX_DIMENSION`] and the pixel count
//! at most [`MAX_PIXELS`]. The decompressed size is computed from the header, and
//! inflate refuses to produce a byte beyond it, so no allocation is ever sized from
//! an untrusted length without that check first. Nothing panics; all failures are
//! [`PngError`].

use std::fmt;

/// Largest accepted width or height, in pixels.
pub const MAX_DIMENSION: u32 = 16384;
/// Largest accepted pixel count (2^25, 128 MiB as RGBA).
pub const MAX_PIXELS: u64 = 1 << 25;
/// Hash-chain candidates examined per position by the compressor.
pub const CHAIN_LIMIT: usize = 32;

const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// Why encoding or decoding failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PngError {
    /// Zero or over-limit dimensions.
    BadDimensions,
    /// Pixel buffer length does not match `width * height * 4`.
    BadBuffer,
    /// Input ended early.
    Truncated,
    /// Not a PNG, or a structural violation. The string says which.
    Malformed(&'static str),
    /// A chunk CRC did not match.
    BadCrc,
    /// The zlib Adler-32 did not match.
    BadAdler,
    /// Valid PNG using a feature this decoder does not implement.
    Unsupported(&'static str),
}

impl fmt::Display for PngError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PngError::BadDimensions => write!(f, "bad image dimensions"),
            PngError::BadBuffer => write!(f, "pixel buffer length mismatch"),
            PngError::Truncated => write!(f, "truncated PNG"),
            PngError::Malformed(m) => write!(f, "malformed PNG: {m}"),
            PngError::BadCrc => write!(f, "chunk CRC mismatch"),
            PngError::BadAdler => write!(f, "zlib Adler-32 mismatch"),
            PngError::Unsupported(m) => write!(f, "unsupported PNG feature: {m}"),
        }
    }
}

impl std::error::Error for PngError {}

/// A decoded image: RGBA8, row-major, `rgba.len() == width * height * 4`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

// ---------------------------------------------------------------- checksums

const fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        t[n] = c;
        n += 1;
    }
    t
}

static CRC_TABLE: [u32; 256] = crc_table();

/// CRC-32 (ISO 3309, as PNG uses it).
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    c ^ 0xFFFF_FFFF
}

/// Adler-32 (RFC 1950).
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    // 5552 is the largest run that cannot overflow u32 before the modulo.
    for chunk in data.chunks(5552) {
        for &x in chunk {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

// ------------------------------------------------------------------ tables

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

// ------------------------------------------------------------------ deflate

struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }
    /// Append `n` (<= 16) bits of `v`, least significant first.
    fn bits(&mut self, v: u32, n: u32) {
        self.acc |= v << self.n;
        self.n += n;
        while self.n >= 8 {
            self.out.push((self.acc & 0xFF) as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    /// Append a Huffman code (most significant bit first on the wire).
    fn code(&mut self, code: u32, len: u32) {
        let mut rev = 0;
        for i in 0..len {
            rev |= ((code >> i) & 1) << (len - 1 - i);
        }
        self.bits(rev, len);
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc & 0xFF) as u8);
        }
        self.out
    }
}

fn put_literal(w: &mut BitWriter, v: u32) {
    match v {
        0..=143 => w.code(0x30 + v, 8),
        144..=255 => w.code(0x190 + (v - 144), 9),
        256..=279 => w.code(v - 256, 7),
        _ => w.code(0xC0 + (v - 280), 8),
    }
}

fn put_match(w: &mut BitWriter, len: usize, dist: usize) {
    let mut li = 28;
    while LBASE[li] as usize > len {
        li -= 1;
    }
    put_literal(w, 257 + li as u32);
    w.bits((len - LBASE[li] as usize) as u32, LEXT[li] as u32);
    let mut di = 29;
    while DBASE[di] as usize > dist {
        di -= 1;
    }
    w.code(di as u32, 5);
    w.bits((dist - DBASE[di] as usize) as u32, DEXT[di] as u32);
}

const WINDOW: usize = 32768;
const HASH_BITS: u32 = 15;

fn hash3(d: &[u8], i: usize) -> usize {
    let v = ((d[i] as u32) << 16) | ((d[i + 1] as u32) << 8) | d[i + 2] as u32;
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_BITS)) as usize
}

/// DEFLATE-compress `data` as one fixed-Huffman block. Deterministic.
fn deflate(data: &[u8]) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.bits(1, 1); // BFINAL
    w.bits(1, 2); // BTYPE = fixed Huffman
    let n = data.len();
    // head[h] = position + 1 of the latest occurrence, 0 = none.
    let mut head = vec![0u32; 1 << HASH_BITS];
    let mut prev = vec![0u32; WINDOW];
    let insert = |head: &mut [u32], prev: &mut [u32], i: usize| {
        if i + 3 <= n {
            let h = hash3(data, i);
            prev[i & (WINDOW - 1)] = head[h];
            head[h] = i as u32 + 1;
        }
    };
    let mut i = 0;
    while i < n {
        let mut best_len = 0;
        let mut best_dist = 0;
        if i + 3 <= n {
            let max_len = (n - i).min(258);
            let mut cand = head[hash3(data, i)];
            let mut steps = 0;
            while cand != 0 && steps < CHAIN_LIMIT {
                let p = cand as usize - 1;
                if p >= i || i - p > WINDOW {
                    break;
                }
                let mut l = 0;
                while l < max_len && data[p + l] == data[i + l] {
                    l += 1;
                }
                if l > best_len {
                    best_len = l;
                    best_dist = i - p;
                    if l == max_len {
                        break;
                    }
                }
                let next = prev[p & (WINDOW - 1)];
                // Chains only ever point backwards; anything else is stale.
                if next != 0 && next as usize - 1 >= p {
                    break;
                }
                cand = next;
                steps += 1;
            }
        }
        if best_len >= 3 {
            put_match(&mut w, best_len, best_dist);
            for k in 0..best_len {
                insert(&mut head, &mut prev, i + k);
            }
            i += best_len;
        } else {
            put_literal(&mut w, data[i] as u32);
            insert(&mut head, &mut prev, i);
            i += 1;
        }
    }
    put_literal(&mut w, 256);
    w.finish()
}

// ------------------------------------------------------------------ encoder

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (a as i32, b as i32, c as i32);
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

/// Apply filter `ft` to `cur` given `prev` (the unfiltered row above, or zeros).
fn filter_row(ft: u8, bpp: usize, cur: &[u8], prev: &[u8], out: &mut Vec<u8>) {
    for i in 0..cur.len() {
        let x = cur[i];
        let a = if i >= bpp { cur[i - bpp] } else { 0 };
        let b = prev[i];
        let c = if i >= bpp { prev[i - bpp] } else { 0 };
        out.push(match ft {
            0 => x,
            1 => x.wrapping_sub(a),
            2 => x.wrapping_sub(b),
            3 => x.wrapping_sub(((a as u16 + b as u16) / 2) as u8),
            _ => x.wrapping_sub(paeth(a, b, c)),
        });
    }
}

fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    let crc = crc32(&out[start..]);
    out.extend_from_slice(&crc.to_be_bytes());
}

/// Encode RGBA8 pixels (`width * height * 4` bytes) as a PNG.
pub fn encode(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, PngError> {
    if width == 0
        || height == 0
        || width > MAX_DIMENSION
        || height > MAX_DIMENSION
        || width as u64 * height as u64 > MAX_PIXELS
    {
        return Err(PngError::BadDimensions);
    }
    let npix = width as usize * height as usize;
    if rgba.len() != npix * 4 {
        return Err(PngError::BadBuffer);
    }
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let bpp = if opaque { 3 } else { 4 };
    let row_len = width as usize * bpp;

    let mut raw = Vec::with_capacity((row_len + 1) * height as usize);
    let mut prev = vec![0u8; row_len];
    let mut cur = vec![0u8; row_len];
    let mut cand = Vec::with_capacity(row_len);
    for y in 0..height as usize {
        let src = &rgba[y * width as usize * 4..(y + 1) * width as usize * 4];
        if opaque {
            for (d, s) in cur.chunks_exact_mut(3).zip(src.chunks_exact(4)) {
                d.copy_from_slice(&s[..3]);
            }
        } else {
            cur.copy_from_slice(src);
        }
        let mut best = (u64::MAX, 0u8);
        for ft in 0..5u8 {
            cand.clear();
            filter_row(ft, bpp, &cur, &prev, &mut cand);
            let score: u64 = cand.iter().map(|&b| (b as i8).unsigned_abs() as u64).sum();
            if score < best.0 {
                best = (score, ft);
            }
        }
        raw.push(best.1);
        filter_row(best.1, bpp, &cur, &prev, &mut raw);
        std::mem::swap(&mut prev, &mut cur);
    }

    let mut z = vec![0x78, 0x01]; // deflate, 32K window, no preset dict; 0x7801 % 31 == 0
    z.extend_from_slice(&deflate(&raw));
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = Vec::with_capacity(z.len() + 64);
    out.extend_from_slice(&SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, if opaque { 2 } else { 6 }, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &z);
    push_chunk(&mut out, b"IEND", &[]);
    Ok(out)
}

// ------------------------------------------------------------------ inflate

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
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
    /// Read `n` (<= 16) bits, least significant first.
    fn bits(&mut self, n: u32) -> Result<u32, PngError> {
        while self.n < n {
            let b = *self.data.get(self.pos).ok_or(PngError::Truncated)?;
            self.pos += 1;
            self.acc |= (b as u32) << self.n;
            self.n += 8;
        }
        let v = self.acc & ((1u32 << n) - 1);
        self.acc >>= n;
        self.n -= n;
        Ok(v)
    }
    /// Discard the bits left in the current byte (whole bytes are never prefetched
    /// beyond what `bits` needed, so `pos` is the next unread byte).
    fn align(&mut self) {
        self.acc = 0;
        self.n = 0;
    }
}

struct Huffman {
    count: [u16; 16],
    symbol: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Result<Huffman, PngError> {
        let mut count = [0u16; 16];
        for &l in lengths {
            count[l as usize & 15] += 1;
        }
        let mut left: i32 = 1;
        for &c in count.iter().skip(1) {
            left = (left << 1) - c as i32;
            if left < 0 {
                return Err(PngError::Malformed("over-subscribed Huffman code"));
            }
        }
        let mut offs = [0u16; 16];
        for l in 1..15 {
            offs[l + 1] = offs[l] + count[l];
        }
        let mut symbol = vec![0u16; lengths.len()];
        for (s, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let o = &mut offs[l as usize];
                if let Some(slot) = symbol.get_mut(*o as usize) {
                    *slot = s as u16;
                }
                *o += 1;
            }
        }
        Ok(Huffman { count, symbol })
    }

    fn decode(&self, r: &mut BitReader) -> Result<u16, PngError> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= r.bits(1)? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return self
                    .symbol
                    .get((index + (code - first)) as usize)
                    .copied()
                    .ok_or(PngError::Malformed("bad Huffman symbol"));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(PngError::Malformed("invalid Huffman code"))
    }
}

fn fixed_tables() -> Result<(Huffman, Huffman), PngError> {
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

fn inflate_codes(
    r: &mut BitReader,
    out: &mut Vec<u8>,
    limit: usize,
    lit: &Huffman,
    dist: &Huffman,
) -> Result<(), PngError> {
    loop {
        let sym = lit.decode(r)? as usize;
        if sym < 256 {
            if out.len() >= limit {
                return Err(PngError::Malformed("inflated data exceeds expected size"));
            }
            out.push(sym as u8);
        } else if sym == 256 {
            return Ok(());
        } else {
            let li = sym - 257;
            if li >= 29 {
                return Err(PngError::Malformed("bad length symbol"));
            }
            let len = LBASE[li] as usize + r.bits(LEXT[li] as u32)? as usize;
            let di = dist.decode(r)? as usize;
            if di >= 30 {
                return Err(PngError::Malformed("bad distance symbol"));
            }
            let d = DBASE[di] as usize + r.bits(DEXT[di] as u32)? as usize;
            if d > out.len() {
                return Err(PngError::Malformed("distance too far back"));
            }
            if len > limit.saturating_sub(out.len()) {
                return Err(PngError::Malformed("inflated data exceeds expected size"));
            }
            for _ in 0..len {
                let b = out[out.len() - d];
                out.push(b);
            }
        }
    }
}

fn read_dynamic(r: &mut BitReader) -> Result<(Huffman, Huffman), PngError> {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let nlen = r.bits(5)? as usize + 257;
    let ndist = r.bits(5)? as usize + 1;
    let ncode = r.bits(4)? as usize + 4;
    if nlen > 286 || ndist > 30 {
        return Err(PngError::Malformed("too many Huffman codes"));
    }
    let mut cl = [0u8; 19];
    for &o in ORDER.iter().take(ncode) {
        cl[o] = r.bits(3)? as u8;
    }
    let clh = Huffman::new(&cl)?;
    let mut lengths = [0u8; 316];
    let mut i = 0;
    while i < nlen + ndist {
        let sym = clh.decode(r)?;
        if sym < 16 {
            lengths[i] = sym as u8;
            i += 1;
        } else {
            let (val, rep) = match sym {
                16 => {
                    if i == 0 {
                        return Err(PngError::Malformed("repeat with no previous length"));
                    }
                    (lengths[i - 1], 3 + r.bits(2)? as usize)
                }
                17 => (0, 3 + r.bits(3)? as usize),
                _ => (0, 11 + r.bits(7)? as usize),
            };
            if i + rep > nlen + ndist {
                return Err(PngError::Malformed("code length repeat overruns"));
            }
            for _ in 0..rep {
                lengths[i] = val;
                i += 1;
            }
        }
    }
    if lengths[256] == 0 {
        return Err(PngError::Malformed("no end-of-block code"));
    }
    Ok((
        Huffman::new(&lengths[..nlen])?,
        Huffman::new(&lengths[nlen..nlen + ndist])?,
    ))
}

/// Inflate a raw DEFLATE stream, refusing to produce more than `limit` bytes.
fn inflate(data: &[u8], limit: usize) -> Result<Vec<u8>, PngError> {
    let mut r = BitReader::new(data);
    let mut out: Vec<u8> = Vec::with_capacity(limit.min(1 << 16));
    loop {
        let last = r.bits(1)?;
        match r.bits(2)? {
            0 => {
                r.align();
                let hdr = data.get(r.pos..r.pos + 4).ok_or(PngError::Truncated)?;
                let len = u16::from_le_bytes([hdr[0], hdr[1]]);
                let nlen = u16::from_le_bytes([hdr[2], hdr[3]]);
                if len != !nlen {
                    return Err(PngError::Malformed("stored block length check"));
                }
                let len = len as usize;
                let start = r.pos + 4;
                let body = data.get(start..start + len).ok_or(PngError::Truncated)?;
                if len > limit.saturating_sub(out.len()) {
                    return Err(PngError::Malformed("inflated data exceeds expected size"));
                }
                out.extend_from_slice(body);
                r.pos = start + len;
            }
            1 => {
                let (l, d) = fixed_tables()?;
                inflate_codes(&mut r, &mut out, limit, &l, &d)?;
            }
            2 => {
                let (l, d) = read_dynamic(&mut r)?;
                inflate_codes(&mut r, &mut out, limit, &l, &d)?;
            }
            _ => return Err(PngError::Malformed("reserved block type")),
        }
        if last == 1 {
            return Ok(out);
        }
    }
}

/// Decompress a zlib stream into at most `limit` bytes and check its Adler-32.
fn zlib_decompress(z: &[u8], limit: usize) -> Result<Vec<u8>, PngError> {
    if z.len() < 6 {
        return Err(PngError::Truncated);
    }
    let (cmf, flg) = (z[0], z[1]);
    if cmf & 0x0F != 8 || cmf >> 4 > 7 || (((cmf as u32) << 8) | flg as u32) % 31 != 0 {
        return Err(PngError::Malformed("bad zlib header"));
    }
    if flg & 0x20 != 0 {
        return Err(PngError::Unsupported("zlib preset dictionary"));
    }
    let body = &z[2..z.len() - 4];
    let out = inflate(body, limit)?;
    let t = &z[z.len() - 4..];
    let want = u32::from_be_bytes([t[0], t[1], t[2], t[3]]);
    if adler32(&out) != want {
        return Err(PngError::BadAdler);
    }
    Ok(out)
}

// ------------------------------------------------------------------ decoder

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Decode a PNG to RGBA8. See the module docs for what is supported.
pub fn decode(data: &[u8]) -> Result<Image, PngError> {
    if data.len() < 8 {
        return Err(PngError::Truncated);
    }
    if data[..8] != SIGNATURE {
        return Err(PngError::Malformed("bad signature"));
    }
    let mut pos = 8;
    let mut hdr: Option<(u32, u32, u8)> = None; // width, height, colour type
    let mut palette: Vec<[u8; 3]> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    let mut seen_idat = false;
    let mut idat_ended = false;
    let mut seen_end = false;
    while pos < data.len() {
        let head = data.get(pos..pos + 8).ok_or(PngError::Truncated)?;
        let len = be32(head) as usize;
        let kind = [head[4], head[5], head[6], head[7]];
        // Check the claimed length against what is really there before using it.
        let end = (pos + 8)
            .checked_add(len)
            .and_then(|e| e.checked_add(4))
            .ok_or(PngError::Truncated)?;
        if end > data.len() {
            return Err(PngError::Truncated);
        }
        let body = &data[pos + 8..pos + 8 + len];
        let crc = be32(&data[pos + 8 + len..end]);
        if crc32(&data[pos + 4..pos + 8 + len]) != crc {
            return Err(PngError::BadCrc);
        }
        pos = end;
        if hdr.is_none() && &kind != b"IHDR" {
            return Err(PngError::Malformed("IHDR must be first"));
        }
        match &kind {
            b"IHDR" => {
                if hdr.is_some() || body.len() != 13 {
                    return Err(PngError::Malformed("bad IHDR"));
                }
                let (w, h) = (be32(body), be32(&body[4..]));
                if w == 0
                    || h == 0
                    || w > MAX_DIMENSION
                    || h > MAX_DIMENSION
                    || w as u64 * h as u64 > MAX_PIXELS
                {
                    return Err(PngError::BadDimensions);
                }
                if body[8] != 8 {
                    return Err(PngError::Unsupported("bit depth other than 8"));
                }
                if !matches!(body[9], 0 | 2 | 3 | 4 | 6) {
                    return Err(PngError::Malformed("bad colour type"));
                }
                if body[10] != 0 || body[11] != 0 {
                    return Err(PngError::Malformed("bad compression or filter method"));
                }
                if body[12] != 0 {
                    return Err(PngError::Unsupported("interlace"));
                }
                hdr = Some((w, h, body[9]));
            }
            b"PLTE" => {
                if body.len() % 3 != 0 || body.is_empty() || body.len() / 3 > 256 || seen_idat {
                    return Err(PngError::Malformed("bad PLTE"));
                }
                palette = body.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect();
            }
            b"tRNS" => {
                if body.len() > 256 {
                    return Err(PngError::Malformed("bad tRNS"));
                }
                trns = body.to_vec();
            }
            b"IDAT" => {
                if idat_ended {
                    return Err(PngError::Malformed("IDAT chunks not consecutive"));
                }
                seen_idat = true;
                idat.extend_from_slice(body);
            }
            b"IEND" => {
                seen_end = true;
                break;
            }
            _ => {
                if kind[0] & 0x20 == 0 {
                    return Err(PngError::Unsupported("unknown critical chunk"));
                }
            }
        }
        if seen_idat && &kind != b"IDAT" {
            idat_ended = true;
        }
    }
    if !seen_end {
        return Err(PngError::Truncated);
    }
    let (w, h, ct) = hdr.ok_or(PngError::Truncated)?;
    if !seen_idat {
        return Err(PngError::Malformed("no IDAT"));
    }
    let bpp = match ct {
        0 | 3 => 1,
        4 => 2,
        2 => 3,
        _ => 4,
    };
    if ct == 3 && palette.is_empty() {
        return Err(PngError::Malformed("palette image without PLTE"));
    }
    let (w, h) = (w as usize, h as usize);
    let row_len = w * bpp;
    let expect = (row_len + 1) * h;
    let raw = zlib_decompress(&idat, expect)?;
    if raw.len() != expect {
        return Err(PngError::Malformed("image data length mismatch"));
    }

    // Unfilter into `pix` (rows without the filter byte).
    let mut pix = vec![0u8; row_len * h];
    let zero = vec![0u8; row_len];
    for y in 0..h {
        let ft = raw[y * (row_len + 1)];
        let src = &raw[y * (row_len + 1) + 1..(y + 1) * (row_len + 1)];
        let (done, rest) = pix.split_at_mut(y * row_len);
        let cur = &mut rest[..row_len];
        let prev: &[u8] = if y == 0 {
            &zero
        } else {
            &done[(y - 1) * row_len..]
        };
        for i in 0..row_len {
            let a = if i >= bpp { cur[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            let add = match ft {
                0 => 0,
                1 => a,
                2 => b,
                3 => ((a as u16 + b as u16) / 2) as u8,
                4 => paeth(a, b, c),
                _ => return Err(PngError::Malformed("bad filter type")),
            };
            cur[i] = src[i].wrapping_add(add);
        }
    }

    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in pix.chunks_exact(bpp) {
        match ct {
            0 => rgba.extend_from_slice(&[p[0], p[0], p[0], 255]),
            2 => rgba.extend_from_slice(&[p[0], p[1], p[2], 255]),
            3 => {
                let c = palette
                    .get(p[0] as usize)
                    .ok_or(PngError::Malformed("palette index out of range"))?;
                let a = trns.get(p[0] as usize).copied().unwrap_or(255);
                rgba.extend_from_slice(&[c[0], c[1], c[2], a]);
            }
            4 => rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]),
            _ => rgba.extend_from_slice(p),
        }
    }
    Ok(Image {
        width: w as u32,
        height: h as u32,
        rgba,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    fn noisy(w: u32, h: u32, seed: u64, alpha: bool) -> Vec<u8> {
        let mut r = Rng(seed);
        let mut v = Vec::new();
        for i in 0..(w * h) {
            // Mix of runs, gradients and noise so every filter and match path runs.
            let base = (i % 251) as u8;
            let n = (r.next() & 0xFF) as u8;
            let px = match r.next() % 4 {
                0 => [n, n, n],
                1 => [base, base, base],
                2 => [10, 20, 30],
                _ => [base, n, 7],
            };
            v.extend_from_slice(&px);
            v.push(if alpha { (r.next() & 0xFF) as u8 } else { 255 });
        }
        v
    }

    #[test]
    fn crc32_known_answers() {
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
        assert_eq!(crc32(b"IEND"), 0xAE42_6082);
    }

    #[test]
    fn adler32_known_answers() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E6_0398);
        assert_eq!(adler32(b""), 1);
        // Long input crosses the 5552-byte modulo batching.
        let (mut a, mut b) = (1u64, 0u64);
        for _ in 0..100_000 {
            a = (a + 255) % 65521;
            b = (b + a) % 65521;
        }
        assert_eq!(adler32(&vec![0xFFu8; 100_000]), ((b << 16) | a) as u32);
    }

    fn chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        push_chunk(&mut v, kind, body);
        v
    }

    /// A 2x1 RGB PNG assembled by hand: stored zlib block, filter None.
    fn hand_made() -> Vec<u8> {
        let mut p = SIGNATURE.to_vec();
        p.extend(chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 2, 0, 0, 0]));
        let raw = [0u8, 255, 0, 0, 0, 0, 255];
        let mut z = vec![0x78, 0x01, 0x01, 7, 0, !7, 0xFF];
        z.extend_from_slice(&raw);
        z.extend_from_slice(&adler32(&raw).to_be_bytes());
        p.extend(chunk(b"IDAT", &z));
        p.extend(chunk(b"IEND", &[]));
        p
    }

    #[test]
    fn decodes_hand_made_png() {
        let png = hand_made();
        let img = decode(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, [255, 0, 0, 255, 0, 0, 255, 255]);
        // The IEND chunk is the fixed 12 bytes every PNG ends with.
        assert_eq!(
            png[png.len() - 12..],
            [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
        );
    }

    #[test]
    fn round_trips() {
        for (i, &(w, h, alpha)) in [
            (1, 1, false),
            (1, 1, true),
            (7, 5, false),
            (33, 17, true),
            (64, 64, false),
            (200, 3, true),
            (3, 200, false),
        ]
        .iter()
        .enumerate()
        {
            let px = noisy(w, h, 0x9E37_79B9_7F4A_7C15 ^ i as u64, alpha);
            let png = encode(w, h, &px).unwrap();
            let img = decode(&png).unwrap();
            assert_eq!((img.width, img.height), (w, h));
            assert_eq!(img.rgba, px, "{w}x{h} alpha={alpha}");
            assert_eq!(png, encode(w, h, &px).unwrap(), "deterministic");
        }
    }

    #[test]
    fn colour_type_follows_alpha() {
        let opaque = encode(1, 1, &[1, 2, 3, 255]).unwrap();
        let clear = encode(1, 1, &[1, 2, 3, 254]).unwrap();
        assert_eq!(opaque[25], 2);
        assert_eq!(clear[25], 6);
    }

    #[test]
    fn flat_image_compresses_well() {
        let px = vec![200u8; 400 * 300 * 4];
        let png = encode(400, 300, &px).unwrap();
        assert!(png.len() < 4500, "{}", png.len());
        assert_eq!(decode(&png).unwrap().rgba, px);
    }

    #[test]
    fn long_runs_use_max_length_matches() {
        let px = vec![0u8; 1000 * 4];
        let png = encode(1000, 1, &px).unwrap();
        assert_eq!(decode(&png).unwrap().rgba, px);
    }

    #[test]
    fn encoder_rejects_bad_input() {
        assert_eq!(encode(0, 1, &[]), Err(PngError::BadDimensions));
        assert_eq!(encode(1, 1, &[0; 3]), Err(PngError::BadBuffer));
        assert_eq!(
            encode(MAX_DIMENSION + 1, 1, &[]),
            Err(PngError::BadDimensions)
        );
    }

    #[test]
    fn every_truncation_is_an_error() {
        let png = encode(9, 9, &noisy(9, 9, 5, true)).unwrap();
        for n in 0..png.len() {
            assert!(decode(&png[..n]).is_err(), "prefix {n} decoded");
        }
        assert!(decode(&png).is_ok());
    }

    #[test]
    fn every_single_byte_corruption_is_an_error_or_equal() {
        let px = noisy(6, 6, 9, false);
        let png = encode(6, 6, &px).unwrap();
        for i in 0..png.len() {
            let mut bad = png.clone();
            bad[i] ^= 0x55;
            // Must never panic; CRCs make nearly everything an error.
            if let Ok(img) = decode(&bad) {
                assert_eq!(img.rgba, px, "byte {i} changed pixels silently");
            }
        }
    }

    #[test]
    fn corrupted_crc_is_rejected() {
        let mut png = encode(2, 2, &[9; 16]).unwrap();
        let n = png.len();
        png[n - 1] ^= 1; // IEND CRC
        assert_eq!(decode(&png), Err(PngError::BadCrc));
        let mut png = encode(2, 2, &[9; 16]).unwrap();
        png[29] ^= 1; // IHDR CRC
        assert_eq!(decode(&png), Err(PngError::BadCrc));
    }

    #[test]
    fn huge_claimed_dimensions_are_rejected_without_allocating() {
        for (w, h) in [
            (0xFFFF_FFFFu32, 1u32),
            (16384, 16384),
            (1, 0),
            (0, 5),
            (20000, 1),
        ] {
            let mut ihdr = Vec::new();
            ihdr.extend_from_slice(&w.to_be_bytes());
            ihdr.extend_from_slice(&h.to_be_bytes());
            ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
            let mut p = SIGNATURE.to_vec();
            p.extend(chunk(b"IHDR", &ihdr));
            p.extend(chunk(b"IEND", &[]));
            assert_eq!(decode(&p), Err(PngError::BadDimensions), "{w}x{h}");
        }
    }

    #[test]
    fn huge_chunk_length_is_truncation_not_allocation() {
        let mut p = SIGNATURE.to_vec();
        p.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, b'I', b'D', b'A', b'T']);
        assert_eq!(decode(&p), Err(PngError::Truncated));
    }

    #[test]
    fn decompression_bomb_is_bounded() {
        // 1x1 image whose zlib stream inflates to far more than the 5 bytes it needs.
        let big = vec![0u8; 100_000];
        let mut z = vec![0x78, 0x01];
        z.extend(deflate(&big));
        z.extend_from_slice(&adler32(&big).to_be_bytes());
        let mut p = SIGNATURE.to_vec();
        p.extend(chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]));
        p.extend(chunk(b"IDAT", &z));
        p.extend(chunk(b"IEND", &[]));
        assert!(matches!(decode(&p), Err(PngError::Malformed(_))));
    }

    #[test]
    fn garbage_never_panics() {
        let mut r = Rng(0x1234_5678_9ABC_DEF1);
        for _ in 0..300 {
            let n = (r.next() % 200) as usize;
            let v: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
            let _ = decode(&v);
            let _ = inflate(&v, 4096);
            let mut with_sig = SIGNATURE.to_vec();
            with_sig.extend_from_slice(&v);
            let _ = decode(&with_sig);
        }
    }

    #[test]
    fn inflate_stored_and_fixed_by_hand() {
        // Stored block "abc".
        let s = [0x01, 3, 0, 0xFC, 0xFF, b'a', b'b', b'c'];
        assert_eq!(inflate(&s, 10).unwrap(), b"abc");
        assert!(inflate(&s, 2).is_err());
        // Fixed block for "a" (what zlib emits: 4b 04 00).
        assert_eq!(inflate(&[0x4B, 0x04, 0x00], 10).unwrap(), b"a");
    }

    #[test]
    fn palette_decodes_with_transparency() {
        // 2x1 palette image, indices 1 and 0, tRNS making entry 0 transparent.
        let raw = [0u8, 1, 0];
        let mut z = vec![0x78, 0x01, 0x01, 3, 0, !3, 0xFF];
        z.extend_from_slice(&raw);
        z.extend_from_slice(&adler32(&raw).to_be_bytes());
        let mut p = SIGNATURE.to_vec();
        p.extend(chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 3, 0, 0, 0]));
        p.extend(chunk(b"PLTE", &[1, 2, 3, 4, 5, 6]));
        p.extend(chunk(b"tRNS", &[0]));
        p.extend(chunk(b"IDAT", &z));
        p.extend(chunk(b"IEND", &[]));
        assert_eq!(decode(&p).unwrap().rgba, [4, 5, 6, 255, 1, 2, 3, 0]);
    }

    #[test]
    fn all_filter_types_decode() {
        let rows: [[u8; 9]; 3] = [
            [10, 20, 30, 40, 50, 60, 70, 80, 90],
            [15, 25, 35, 45, 55, 65, 75, 85, 95],
            [1, 2, 3, 250, 251, 252, 100, 101, 102],
        ];
        for fts in [[1u8, 2, 3], [4, 3, 1], [3, 4, 2], [0, 4, 4]] {
            let mut raw = Vec::new();
            let mut prev = vec![0u8; 9];
            for (row, &ft) in rows.iter().zip(fts.iter()) {
                raw.push(ft);
                filter_row(ft, 3, row, &prev, &mut raw);
                prev = row.to_vec();
            }
            let mut z = vec![0x78, 0x9C];
            z.extend(deflate(&raw));
            z.extend_from_slice(&adler32(&raw).to_be_bytes());
            let mut p = SIGNATURE.to_vec();
            p.extend(chunk(b"IHDR", &[0, 0, 0, 3, 0, 0, 0, 3, 8, 2, 0, 0, 0]));
            p.extend(chunk(b"IDAT", &z));
            p.extend(chunk(b"IEND", &[]));
            let img = decode(&p).unwrap();
            for (y, row) in rows.iter().enumerate() {
                for x in 0..3 {
                    let o = (y * 3 + x) * 4;
                    assert_eq!(img.rgba[o..o + 3], row[x * 3..x * 3 + 3]);
                }
            }
        }
    }
}
