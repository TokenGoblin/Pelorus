//! A Brotli (RFC 7932) decompressor, written for hostile input.
//!
//! Safe Rust, no dependency. WOFF2 is the only caller: a font file arrives from
//! the network, so every property below is enforced, not hoped for.
//!
//! * No panic: nothing indexes a slice, nothing unwraps, and every arithmetic
//!   step that input can steer is checked or saturating.
//! * Bounded memory: the output is capped by the `limit` the caller passes (for
//!   WOFF2, the size the header declares, itself capped), and checked **before**
//!   every append, so a decompression bomb stops at the cap. Prefix-code tables
//!   are bounded by the alphabet sizes of the format (at most 704 symbols per
//!   tree and 256 trees per kind); nothing is allocated from a length field
//!   without that bound.
//! * Bounded time: every loop consumes input bits or is counted by a bounded
//!   field (a copy or insert is at most the meta-block length, itself checked
//!   against the cap).
//! * The static dictionary (`data/brotli-dictionary.bin`, 122,784 bytes, the RFC
//!   7932 Appendix A dictionary, SHA-256 `20e42eb1...f35c70`) is embedded; a unit
//!   test pins its size and digest-independent spot checks.
//!
//! Not supported: the large-window extension (rejected as malformed), as no
//! WOFF2 encoder uses it.
//!
//! The code follows the RFC's structure: stream header, meta-block header,
//! block-type/-count codes, context maps, prefix codes, then the command loop.

use crate::brotli_tables::{CTX_SIGNED, CTX_UTF8, PREFIX_SUFFIX, TRANSFORMS};

/// Why decompression stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrotliError {
    /// The input ended inside a structure.
    Truncated,
    /// The stream breaks a rule of the format.
    Malformed(&'static str),
    /// The output would exceed the cap.
    TooLarge,
}

const DICT: &[u8] = include_bytes!("../data/brotli-dictionary.bin");

/// log2 of the number of words of each length 4..=24 (index = length).
const NDBITS: [u8; 25] = [
    0, 0, 0, 0, 10, 10, 11, 11, 10, 10, 10, 10, 10, 9, 9, 8, 7, 7, 8, 7, 7, 6, 6, 5, 5,
];

const INSERT_BASE: [u32; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
const INSERT_EXTRA: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];
const COPY_BASE: [u32; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];
const COPY_EXTRA: [u8; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];
/// Insert-length code base and copy-length code base per command cell.
const CELL_INSERT: [usize; 11] = [0, 0, 0, 0, 8, 8, 0, 16, 8, 16, 16];
const CELL_COPY: [usize; 11] = [0, 8, 0, 8, 0, 8, 16, 0, 16, 8, 16];

const BLOCK_COUNT_BASE: [u32; 26] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_COUNT_EXTRA: [u8; 26] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];

/// Order in which the code length code lengths are stored.
const CL_ORDER: [usize; 18] = [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// The fixed code of the code length code lengths, indexed by a 4-bit peek:
/// `(bits consumed, value)`.
const CL_FIXED: [(u8, u8); 16] = [
    (2, 0),
    (2, 4),
    (2, 3),
    (3, 2),
    (2, 0),
    (2, 4),
    (2, 3),
    (4, 1),
    (2, 0),
    (2, 4),
    (2, 3),
    (3, 2),
    (2, 0),
    (2, 4),
    (2, 3),
    (4, 5),
];

/// Maximum prefix-code length.
const MAX_BITS: usize = 15;

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn bit(&mut self) -> Result<u32, BrotliError> {
        let byte = self
            .data
            .get(self.pos >> 3)
            .copied()
            .ok_or(BrotliError::Truncated)?;
        let b = (byte >> (self.pos & 7)) & 1;
        self.pos += 1;
        Ok(u32::from(b))
    }

    /// `n <= 24` bits, least significant first.
    fn read(&mut self, n: u32) -> Result<u32, BrotliError> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Ok(v)
    }

    fn peek4(&self) -> u32 {
        let mut v = 0u32;
        for i in 0..4usize {
            let p = self.pos + i;
            let byte = self.data.get(p >> 3).copied().unwrap_or(0);
            v |= u32::from((byte >> (p & 7)) & 1) << i;
        }
        v
    }

    fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    fn bytes_left(&self) -> usize {
        self.data.len().saturating_sub(self.pos >> 3)
    }
}

/// A canonical prefix code. Codes are assigned in (length, symbol) order; the
/// first bit read is the most significant bit of the code.
struct Huff {
    counts: [u16; MAX_BITS + 1],
    symbols: Vec<u16>,
    /// A code with one symbol reads no bits.
    single: Option<u16>,
}

impl Huff {
    fn build(lengths: &[u8]) -> Result<Huff, BrotliError> {
        let mut counts = [0u16; MAX_BITS + 1];
        for &l in lengths {
            let c = counts
                .get_mut(usize::from(l))
                .ok_or(BrotliError::Malformed("code length"))?;
            *c += 1;
        }
        counts[0] = 0;
        // Over-subscription check.
        let mut left: i32 = 1;
        for len in 1..=MAX_BITS {
            left <<= 1;
            left -= i32::from(counts.get(len).copied().unwrap_or(0));
            if left < 0 {
                return Err(BrotliError::Malformed("over-subscribed prefix code"));
            }
        }
        let mut offs = [0usize; MAX_BITS + 2];
        for len in 1..=MAX_BITS {
            let prev = offs.get(len).copied().unwrap_or(0);
            let c = usize::from(counts.get(len).copied().unwrap_or(0));
            if let Some(o) = offs.get_mut(len + 1) {
                *o = prev + c;
            }
        }
        let used = offs.get(MAX_BITS + 1).copied().unwrap_or(0);
        let mut symbols = vec![0u16; used];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let slot = offs
                    .get_mut(usize::from(l))
                    .ok_or(BrotliError::Malformed("code length"))?;
                if let Some(s) = symbols.get_mut(*slot) {
                    *s = u16::try_from(sym).map_err(|_| BrotliError::Malformed("alphabet"))?;
                }
                *slot += 1;
            }
        }
        let single = if used == 1 {
            symbols.first().copied()
        } else {
            None
        };
        Ok(Huff {
            counts,
            symbols,
            single,
        })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Result<usize, BrotliError> {
        if let Some(s) = self.single {
            return Ok(usize::from(s));
        }
        let mut code: i32 = 0;
        let mut first: i32 = 0;
        let mut index: i32 = 0;
        for len in 1..=MAX_BITS {
            code |= bits.bit()? as i32;
            let count = i32::from(self.counts.get(len).copied().unwrap_or(0));
            if code - count < first {
                let i = usize::try_from(index + (code - first))
                    .map_err(|_| BrotliError::Malformed("prefix code"))?;
                return self
                    .symbols
                    .get(i)
                    .map(|&s| usize::from(s))
                    .ok_or(BrotliError::Malformed("prefix code"));
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err(BrotliError::Malformed("invalid prefix code"))
    }
}

fn bit_length(n: usize) -> u32 {
    usize::BITS - n.leading_zeros()
}

/// RFC 7932 section 3.4 and 3.5: one prefix code over `alphabet` symbols.
fn read_prefix_code(bits: &mut Bits<'_>, alphabet: usize) -> Result<Huff, BrotliError> {
    let hskip = bits.read(2)?;
    let mut lengths = vec![0u8; alphabet];
    if hskip == 1 {
        let nsym = bits.read(2)? as usize + 1;
        let sym_bits = bit_length(alphabet.saturating_sub(1));
        let mut syms = [0usize; 4];
        for i in 0..nsym {
            let s = bits.read(sym_bits)? as usize;
            if s >= alphabet {
                return Err(BrotliError::Malformed("symbol outside alphabet"));
            }
            if syms.get(..i).is_some_and(|p| p.contains(&s)) {
                return Err(BrotliError::Malformed("duplicate symbol"));
            }
            if let Some(slot) = syms.get_mut(i) {
                *slot = s;
            }
        }
        let shape: &[u8] = match nsym {
            1 => &[1],
            2 => &[1, 1],
            3 => &[1, 2, 2],
            _ => {
                if bits.bit()? == 1 {
                    &[1, 2, 3, 3]
                } else {
                    &[2, 2, 2, 2]
                }
            }
        };
        for (i, &l) in shape.iter().enumerate() {
            let s = syms.get(i).copied().unwrap_or(0);
            if let Some(slot) = lengths.get_mut(s) {
                *slot = l;
            }
        }
        // A one-symbol code is `single`, which `build` derives from `used == 1`.
        return Huff::build(&lengths);
    }

    // Complex prefix code: first the code length code lengths.
    let mut cl_lengths = [0u8; 18];
    let mut space: i32 = 32;
    let mut num_codes = 0;
    for &idx in CL_ORDER.iter().skip(hskip as usize) {
        let (n, v) = CL_FIXED
            .get(bits.peek4() as usize)
            .copied()
            .unwrap_or((2, 0));
        bits.read(u32::from(n))?;
        if let Some(slot) = cl_lengths.get_mut(idx) {
            *slot = v;
        }
        if v != 0 {
            space -= 32 >> v;
            num_codes += 1;
            if space <= 0 {
                break;
            }
        }
    }
    if !(num_codes == 1 || space == 0) {
        return Err(BrotliError::Malformed("code length code lengths"));
    }
    let cl_code = Huff::build(&cl_lengths)?;

    let mut sym = 0usize;
    let mut prev_len: u8 = 8;
    let mut repeat: u32 = 0;
    let mut repeat_len: u8 = 0;
    let mut space: i32 = 32768;
    while sym < alphabet && space > 0 {
        let code = cl_code.decode(bits)?;
        if code < 16 {
            repeat = 0;
            let l = code as u8;
            if let Some(slot) = lengths.get_mut(sym) {
                *slot = l;
            }
            sym += 1;
            if l != 0 {
                prev_len = l;
                space -= 32768 >> l;
            }
        } else {
            let (extra_bits, new_len) = if code == 16 { (2, prev_len) } else { (3, 0) };
            if repeat_len != new_len {
                repeat = 0;
                repeat_len = new_len;
            }
            let old = repeat;
            if repeat > 0 {
                repeat = (repeat - 2) << extra_bits;
            }
            repeat += bits.read(extra_bits)? + 3;
            let delta = repeat - old;
            if sym + delta as usize > alphabet {
                return Err(BrotliError::Malformed("repeat past the alphabet"));
            }
            for _ in 0..delta {
                if let Some(slot) = lengths.get_mut(sym) {
                    *slot = new_len;
                }
                sym += 1;
                if new_len != 0 {
                    space -= 32768 >> new_len;
                }
            }
        }
    }
    if space != 0 {
        return Err(BrotliError::Malformed("incomplete prefix code"));
    }
    Huff::build(&lengths)
}

/// `VarLenUint8`: 1..=256 as used for NBLTYPES and NTREES (value + 1 form).
fn read_var_u8(bits: &mut Bits<'_>) -> Result<u32, BrotliError> {
    if bits.bit()? == 0 {
        return Ok(0);
    }
    let n = bits.read(3)?;
    if n == 0 {
        return Ok(1);
    }
    Ok((1u32 << n) + bits.read(n)?)
}

/// Block-type switching state for one of literals, commands, distances.
struct Blocks {
    ntypes: usize,
    type_code: Option<Huff>,
    count_code: Option<Huff>,
    cur: usize,
    rb: [usize; 2],
    remaining: u32,
}

impl Blocks {
    fn read(bits: &mut Bits<'_>) -> Result<Blocks, BrotliError> {
        let ntypes = read_var_u8(bits)? as usize + 1;
        let mut b = Blocks {
            ntypes,
            type_code: None,
            count_code: None,
            cur: 0,
            rb: [1, 0],
            remaining: u32::MAX,
        };
        if ntypes >= 2 {
            b.type_code = Some(read_prefix_code(bits, ntypes + 2)?);
            b.count_code = Some(read_prefix_code(bits, 26)?);
            b.remaining = b.read_count(bits)?;
        }
        Ok(b)
    }

    fn read_count(&self, bits: &mut Bits<'_>) -> Result<u32, BrotliError> {
        let code = self
            .count_code
            .as_ref()
            .ok_or(BrotliError::Malformed("block count"))?
            .decode(bits)?;
        let base = BLOCK_COUNT_BASE
            .get(code)
            .copied()
            .ok_or(BrotliError::Malformed("block count"))?;
        let extra = BLOCK_COUNT_EXTRA.get(code).copied().unwrap_or(0);
        Ok(base.saturating_add(bits.read(u32::from(extra))?))
    }

    fn switch(&mut self, bits: &mut Bits<'_>) -> Result<(), BrotliError> {
        let code = self
            .type_code
            .as_ref()
            .ok_or(BrotliError::Malformed("block switch"))?
            .decode(bits)?;
        self.remaining = self.read_count(bits)?;
        let mut t = match code {
            0 => self.rb[0],
            1 => self.rb[1] + 1,
            n => n - 2,
        };
        if t >= self.ntypes {
            t -= self.ntypes;
        }
        if t >= self.ntypes {
            return Err(BrotliError::Malformed("block type"));
        }
        self.rb = [self.rb[1], t];
        self.cur = t;
        Ok(())
    }

    /// Account for one symbol, switching type first when the block is spent.
    fn take(&mut self, bits: &mut Bits<'_>) -> Result<(), BrotliError> {
        if self.remaining == 0 {
            self.switch(bits)?;
        }
        self.remaining = self.remaining.saturating_sub(1);
        Ok(())
    }
}

/// RFC 7932 section 7.3.
fn read_context_map(bits: &mut Bits<'_>, size: usize) -> Result<(Vec<u8>, usize), BrotliError> {
    let ntrees = read_var_u8(bits)? as usize + 1;
    let mut map = vec![0u8; size];
    if ntrees == 1 {
        return Ok((map, 1));
    }
    let rle_max = if bits.bit()? == 1 {
        bits.read(4)? as usize + 1
    } else {
        0
    };
    let code = read_prefix_code(bits, ntrees + rle_max)?;
    let mut i = 0usize;
    while i < size {
        let s = code.decode(bits)?;
        if s == 0 {
            i += 1;
        } else if s <= rle_max {
            let run = (1usize << s) + bits.read(s as u32)? as usize;
            if i + run > size {
                return Err(BrotliError::Malformed("context map run"));
            }
            i += run;
        } else {
            let v = u8::try_from(s - rle_max).map_err(|_| BrotliError::Malformed("context map"))?;
            if let Some(slot) = map.get_mut(i) {
                *slot = v;
            }
            i += 1;
        }
    }
    if bits.bit()? == 1 {
        let mut mtf: [u8; 256] = std::array::from_fn(|n| n as u8);
        for v in &mut map {
            let idx = usize::from(*v);
            let val = mtf.get(idx).copied().unwrap_or(0);
            *v = val;
            // Move to front.
            if let Some(head) = mtf.get_mut(..=idx) {
                head.rotate_right(1);
            }
        }
    }
    if map.iter().any(|&v| usize::from(v) >= ntrees) {
        return Err(BrotliError::Malformed("context map value"));
    }
    Ok((map, ntrees))
}

fn literal_context(mode: u8, p1: u8, p2: u8) -> usize {
    match mode {
        0 => usize::from(p1 & 0x3f),
        1 => usize::from(p1 >> 2),
        2 => {
            let a = CTX_UTF8.get(usize::from(p1)).copied().unwrap_or(0);
            let b = CTX_UTF8.get(256 + usize::from(p2)).copied().unwrap_or(0);
            usize::from(a | b)
        }
        _ => {
            let a = CTX_SIGNED.get(usize::from(p1)).copied().unwrap_or(0);
            let b = CTX_SIGNED.get(256 + usize::from(p2)).copied().unwrap_or(0);
            usize::from(a | b)
        }
    }
}

fn upper(p: &mut [u8]) -> usize {
    match p.first().copied() {
        Some(b) if b < 0xc0 => {
            if b.is_ascii_lowercase()
                && let Some(x) = p.first_mut()
            {
                *x ^= 32;
            }
            1
        }
        Some(b) if b < 0xe0 => {
            if let Some(x) = p.get_mut(1) {
                *x ^= 32;
            }
            2
        }
        Some(_) => {
            if let Some(x) = p.get_mut(2) {
                *x ^= 5;
            }
            3
        }
        None => 1,
    }
}

/// Apply dictionary transform `t` (0..121) to `word`.
fn transform(word: &[u8], t: usize) -> Result<Vec<u8>, BrotliError> {
    let (prefix, kind, suffix) = TRANSFORMS
        .get(t)
        .copied()
        .ok_or(BrotliError::Malformed("transform id"))?;
    let affix = |start: u8| -> &[u8] {
        let tail = PREFIX_SUFFIX.get(usize::from(start)..).unwrap_or(&[]);
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        tail.get(..end).unwrap_or(&[])
    };
    let mut out = Vec::with_capacity(word.len() + 16);
    out.extend_from_slice(affix(prefix));
    let skip = if kind >= 12 {
        usize::from(kind) - 11
    } else {
        0
    }
    .min(word.len());
    let mut body = word.get(skip..).unwrap_or(&[]);
    if kind <= 9 {
        let cut = body.len().saturating_sub(usize::from(kind));
        body = body.get(..cut).unwrap_or(&[]);
    }
    let start = out.len();
    out.extend_from_slice(body);
    if kind == 10 {
        if let Some(region) = out.get_mut(start..) {
            upper(region);
        }
    } else if kind == 11 {
        let mut at = start;
        while at < start + body.len() {
            let Some(region) = out.get_mut(at..) else {
                break;
            };
            at += upper(region);
        }
    }
    out.extend_from_slice(affix(suffix));
    Ok(out)
}

/// Decompress a Brotli stream. The output is never larger than `limit`.
pub fn decompress(input: &[u8], limit: usize) -> Result<Vec<u8>, BrotliError> {
    let mut bits = Bits {
        data: input,
        pos: 0,
    };
    // Stream header: WBITS.
    let wbits = if bits.bit()? == 0 {
        16
    } else {
        let n = bits.read(3)?;
        if n != 0 {
            17 + n
        } else {
            let m = bits.read(3)?;
            match m {
                0 => 17,
                1 => return Err(BrotliError::Malformed("large window")),
                m => 8 + m,
            }
        }
    };
    let window = (1usize << wbits) - 16;
    let mut out: Vec<u8> = Vec::new();
    let mut dist_rb: [i64; 4] = [16, 15, 11, 4]; // index 3 is the last distance

    loop {
        let is_last = bits.bit()? == 1;
        if is_last && bits.bit()? == 1 {
            break;
        }
        let nibbles = bits.read(2)?;
        if nibbles == 3 {
            // Metadata: skipped.
            if bits.bit()? != 0 {
                return Err(BrotliError::Malformed("reserved bit"));
            }
            let nbytes = bits.read(2)?;
            let mut skip = 0usize;
            for i in 0..nbytes {
                skip |= (bits.read(8)? as usize) << (8 * i);
            }
            if nbytes > 0 {
                skip += 1;
            }
            bits.align();
            if skip > bits.bytes_left() {
                return Err(BrotliError::Truncated);
            }
            bits.pos += skip * 8;
            if is_last {
                break;
            }
            continue;
        }
        let nibbles = 4 + nibbles;
        let mlen = bits.read(nibbles * 4)? as usize + 1;
        if nibbles > 4 && (mlen - 1) >> ((nibbles - 1) * 4) == 0 {
            return Err(BrotliError::Malformed(
                "meta-block length has a zero top nibble",
            ));
        }
        if out.len().saturating_add(mlen) > limit {
            return Err(BrotliError::TooLarge);
        }
        if !is_last && bits.bit()? == 1 {
            bits.align();
            let start = bits.pos >> 3;
            let body = input
                .get(start..start + mlen)
                .ok_or(BrotliError::Truncated)?;
            out.extend_from_slice(body);
            bits.pos += mlen * 8;
            continue;
        }

        // Compressed meta-block.
        let mut lit = Blocks::read(&mut bits)?;
        let mut cmd = Blocks::read(&mut bits)?;
        let mut dst = Blocks::read(&mut bits)?;
        let npostfix = bits.read(2)?;
        let ndirect = (bits.read(4)? as usize) << npostfix;
        let mut modes = Vec::with_capacity(lit.ntypes);
        for _ in 0..lit.ntypes {
            modes.push(bits.read(2)? as u8);
        }
        let (lit_map, nlit_trees) = read_context_map(&mut bits, lit.ntypes * 64)?;
        let (dist_map, ndist_trees) = read_context_map(&mut bits, dst.ntypes * 4)?;
        let mut lit_codes = Vec::with_capacity(nlit_trees);
        for _ in 0..nlit_trees {
            lit_codes.push(read_prefix_code(&mut bits, 256)?);
        }
        let mut cmd_codes = Vec::with_capacity(cmd.ntypes);
        for _ in 0..cmd.ntypes {
            cmd_codes.push(read_prefix_code(&mut bits, 704)?);
        }
        let dist_alphabet = 16 + ndirect + (48usize << npostfix);
        let mut dist_codes = Vec::with_capacity(ndist_trees);
        for _ in 0..ndist_trees {
            dist_codes.push(read_prefix_code(&mut bits, dist_alphabet)?);
        }

        let end = out.len() + mlen;
        while out.len() < end {
            cmd.take(&mut bits)?;
            let sym = cmd_codes
                .get(cmd.cur)
                .ok_or(BrotliError::Malformed("command tree"))?
                .decode(&mut bits)?;
            let cell = sym >> 6;
            let ic = CELL_INSERT.get(cell).copied().unwrap_or(0) + ((sym >> 3) & 7);
            let cc = CELL_COPY.get(cell).copied().unwrap_or(0) + (sym & 7);
            let ins_len =
                INSERT_BASE.get(ic).copied().unwrap_or(0).saturating_add(
                    bits.read(u32::from(INSERT_EXTRA.get(ic).copied().unwrap_or(0)))?,
                ) as usize;
            let copy_len = COPY_BASE
                .get(cc)
                .copied()
                .unwrap_or(0)
                .saturating_add(bits.read(u32::from(COPY_EXTRA.get(cc).copied().unwrap_or(0)))?)
                as usize;
            if ins_len > end - out.len() {
                return Err(BrotliError::Malformed("insert past the meta-block"));
            }
            for _ in 0..ins_len {
                lit.take(&mut bits)?;
                let n = out.len();
                let p1 = if n >= 1 {
                    out.get(n - 1).copied().unwrap_or(0)
                } else {
                    0
                };
                let p2 = if n >= 2 {
                    out.get(n - 2).copied().unwrap_or(0)
                } else {
                    0
                };
                let mode = modes.get(lit.cur).copied().unwrap_or(0);
                let ctx = literal_context(mode, p1, p2);
                let tree = lit_map
                    .get(lit.cur * 64 + ctx)
                    .copied()
                    .map(usize::from)
                    .unwrap_or(0);
                let s = lit_codes
                    .get(tree)
                    .ok_or(BrotliError::Malformed("literal tree"))?
                    .decode(&mut bits)?;
                out.push(s as u8);
            }
            if out.len() >= end {
                break;
            }

            // The distance.
            let mut dist_code = 0usize;
            let explicit = sym >= 128;
            if explicit {
                dst.take(&mut bits)?;
                let ctx = match copy_len {
                    2 => 0,
                    3 => 1,
                    4 => 2,
                    _ => 3,
                };
                let tree = dist_map
                    .get(dst.cur * 4 + ctx)
                    .copied()
                    .map(usize::from)
                    .unwrap_or(0);
                dist_code = dist_codes
                    .get(tree)
                    .ok_or(BrotliError::Malformed("distance tree"))?
                    .decode(&mut bits)?;
            }
            let last = |k: usize| dist_rb.get(3 - k).copied().unwrap_or(0);
            let distance: i64 = if dist_code < 16 {
                let (base, delta): (i64, i64) = match dist_code {
                    0 => (last(0), 0),
                    1 => (last(1), 0),
                    2 => (last(2), 0),
                    3 => (last(3), 0),
                    4 => (last(0), -1),
                    5 => (last(0), 1),
                    6 => (last(0), -2),
                    7 => (last(0), 2),
                    8 => (last(0), -3),
                    9 => (last(0), 3),
                    10 => (last(1), -1),
                    11 => (last(1), 1),
                    12 => (last(1), -2),
                    13 => (last(1), 2),
                    14 => (last(1), -3),
                    _ => (last(1), 3),
                };
                base + delta
            } else if dist_code < 16 + ndirect {
                (dist_code - 15) as i64
            } else {
                let d = dist_code - ndirect - 16;
                let ndistbits = 1 + (d >> (npostfix + 1));
                let hcode = d >> npostfix;
                let lcode = d & ((1usize << npostfix) - 1);
                let offset = ((2 + (hcode & 1)) << ndistbits) - 4;
                let extra = bits.read(ndistbits as u32)? as usize;
                (((offset + extra) << npostfix) + lcode + ndirect + 1) as i64
            };
            if distance <= 0 {
                return Err(BrotliError::Malformed("non-positive distance"));
            }
            let max_distance = i64::try_from(out.len().min(window)).unwrap_or(i64::MAX);
            if distance <= max_distance {
                if dist_code != 0 {
                    dist_rb = [dist_rb[1], dist_rb[2], dist_rb[3], distance];
                }
                if copy_len > end - out.len() {
                    return Err(BrotliError::Malformed("copy past the meta-block"));
                }
                let from = out.len() - distance as usize;
                for k in 0..copy_len {
                    let b = out.get(from + k).copied().unwrap_or(0);
                    out.push(b);
                }
            } else {
                // A static dictionary reference.
                if !(4..=24).contains(&copy_len) {
                    return Err(BrotliError::Malformed("dictionary word length"));
                }
                let word_id = (distance - max_distance - 1) as usize;
                let nbits = u32::from(NDBITS.get(copy_len).copied().unwrap_or(0));
                let idx = word_id & ((1usize << nbits) - 1);
                let t = word_id >> nbits;
                // The offset of length `n` is the sum of (count * length) below.
                let mut offset = 0usize;
                for l in 4..copy_len {
                    offset += l << NDBITS.get(l).copied().unwrap_or(0);
                }
                let start = offset + idx * copy_len;
                let word = DICT
                    .get(start..start + copy_len)
                    .ok_or(BrotliError::Malformed("dictionary word"))?;
                let w = transform(word, t)?;
                if w.len() > end - out.len() {
                    return Err(BrotliError::Malformed(
                        "dictionary word past the meta-block",
                    ));
                }
                out.extend_from_slice(&w);
            }
        }
        if is_last {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dictionary_is_the_rfc_dictionary() {
        assert_eq!(DICT.len(), 122_784);
        // First and last words: "time" and the final 24-letter word.
        assert_eq!(DICT.get(..4), Some(&b"time"[..]));
        let total: usize = (4..=24).map(|l| l << NDBITS[l]).sum();
        assert_eq!(total, 122_784);
    }

    #[test]
    fn an_empty_stream_and_an_uncompressed_block() {
        // WBITS=16 (0), ISLAST=1, ISLASTEMPTY=1.
        assert_eq!(decompress(&[0x06], 10), Ok(vec![]));
        // "hi": WBITS 0, ISLAST 0, MNIBBLES 00 (4), MLEN-1 = 1 (16 bits),
        // ISUNCOMPRESSED 1, align, bytes, then ISLAST+ISLASTEMPTY.
        let mut bytes = vec![0u8; 0];
        // bits: 0 | 0 | 00 | 0000000000000001 | 1  => pack LSB-first
        let mut acc: u64 = 0;
        let mut n = 0;
        let push = |v: u64, w: u32, acc: &mut u64, n: &mut u32| {
            *acc |= v << *n;
            *n += w;
        };
        push(0, 1, &mut acc, &mut n);
        push(0, 1, &mut acc, &mut n);
        push(0, 2, &mut acc, &mut n);
        push(1, 16, &mut acc, &mut n);
        push(1, 1, &mut acc, &mut n);
        while n % 8 != 0 {
            n += 1;
        }
        for i in 0..n / 8 {
            bytes.push((acc >> (8 * i)) as u8);
        }
        bytes.extend_from_slice(b"hi");
        bytes.push(0b11); // ISLAST=1, ISLASTEMPTY=1
        assert_eq!(decompress(&bytes, 10), Ok(b"hi".to_vec()));
        assert_eq!(decompress(&bytes, 1), Err(BrotliError::TooLarge));
    }

    macro_rules! vector {
        ($name:literal) => {
            (
                &include_bytes!(concat!("../tests/data/brotli/", $name, ".br"))[..],
                &include_bytes!(concat!("../tests/data/brotli/", $name, ".raw"))[..],
            )
        };
    }

    /// Streams made by the reference encoder (see tests/data/brotli/README):
    /// dictionary words and their transforms (text, html), context modelling,
    /// an uncompressed meta-block (rand), long runs (zeros) and several
    /// meta-blocks with a 16 MiB window (mixed).
    #[test]
    fn reference_encoder_streams_decode_exactly() {
        for (name, (z, raw)) in [
            ("text", vector!("text")),
            ("tiny", vector!("tiny")),
            ("html", vector!("html")),
            ("rand", vector!("rand")),
            ("zeros", vector!("zeros")),
            ("mixed", vector!("mixed")),
        ] {
            let out = decompress(z, raw.len()).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(out, raw, "{name}");
            // One byte under the cap is refused, not truncated.
            assert_eq!(
                decompress(z, raw.len() - 1),
                Err(BrotliError::TooLarge),
                "{name}"
            );
        }
    }

    #[test]
    fn hostile_streams_never_panic() {
        // Every prefix of a valid stream, and bit flips, must return, not panic.
        let z = include_bytes!("../tests/data/brotli/html.br");
        for n in 0..z.len() {
            let _ = decompress(&z[..n], 1000);
        }
        for i in 0..z.len() * 8 {
            let mut m = z.to_vec();
            m[i / 8] ^= 1 << (i % 8);
            let _ = decompress(&m, 1000);
        }
        // Pseudo-random garbage.
        let mut x: u32 = 12345;
        for _ in 0..300 {
            let buf: Vec<u8> = (0..64)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (x >> 24) as u8
                })
                .collect();
            let _ = decompress(&buf, 4096);
        }
    }
}
