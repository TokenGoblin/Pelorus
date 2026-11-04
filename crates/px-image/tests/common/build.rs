//! Helpers that hand-build container bytes for tests.
#![allow(dead_code)]

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// zlib stream of stored blocks.
pub fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut z = vec![0x78, 0x01];
    let mut chunks = data.chunks(65535).peekable();
    if data.is_empty() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend_from_slice(&(c.len() as u16).to_le_bytes());
        z.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        z.extend_from_slice(c);
    }
    z.extend_from_slice(&adler32(data).to_be_bytes());
    z
}

pub fn png_chunk(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = (body.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(body);
    let mut c = kind.to_vec();
    c.extend_from_slice(body);
    v.extend_from_slice(&crc32(&c).to_be_bytes());
    v
}

/// A PNG with the given IHDR fields and raw (filtered) scanline bytes.
pub fn png_with(
    w: u32,
    h: u32,
    depth: u8,
    ct: u8,
    interlace: u8,
    raw: &[u8],
    extra: &[Vec<u8>],
) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[depth, ct, 0, 0, interlace]);
    v.extend(png_chunk(b"IHDR", &ihdr));
    for e in extra {
        v.extend_from_slice(e);
    }
    v.extend(png_chunk(b"IDAT", &zlib_stored(raw)));
    v.extend(png_chunk(b"IEND", &[]));
    v
}
