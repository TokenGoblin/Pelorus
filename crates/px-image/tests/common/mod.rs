//! Shared test support: SHA-256, the fixture manifest, and the hostile-input
//! battery every format runs.
#![allow(dead_code)]

pub mod build;
#[allow(unused_imports)]
pub use build::*;

/// A decoder under test.
pub type DecodeFn<'a> = &'a dyn Fn(&[u8], &Limits) -> Result<Decoded, DecodeError>;

use px_image::{DecodeError, Decoded, Limits};
use std::time::{Duration, Instant};

/// SHA-256, written out so the tests need no dependency.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                block[4 * i],
                block[4 * i + 1],
                block[4 * i + 2],
                block[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v = [
                t1.wrapping_add(t2),
                v[0],
                v[1],
                v[2],
                v[3].wrapping_add(t1),
                v[4],
                v[5],
                v[6],
            ];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn sha256_known_answers() {
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

/// One manifest line.
pub struct Entry {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub sha: String,
    pub plays: u32,
    pub delays: Vec<u32>,
}

pub fn parse_manifest(text: &str) -> Vec<Entry> {
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Entry {
                name: f[0].to_string(),
                width: f[1].parse().unwrap(),
                height: f[2].parse().unwrap(),
                frames: f[3].parse().unwrap(),
                sha: f[4].to_string(),
                plays: f[5].parse().unwrap(),
                delays: if f.get(6).copied().unwrap_or("-") == "-" {
                    vec![]
                } else {
                    f[6].split(',').map(|d| d.parse().unwrap()).collect()
                },
            }
        })
        .collect()
}

/// All frames' RGBA concatenated, as the manifest hashes them.
pub fn all_rgba(d: &Decoded) -> Vec<u8> {
    match d {
        Decoded::Still(i) => i.rgba.clone(),
        Decoded::Animated(a) => a.frames.iter().flat_map(|f| f.image.rgba.clone()).collect(),
    }
}

pub fn dims(d: &Decoded) -> (u32, u32, usize) {
    match d {
        Decoded::Still(i) => (i.width, i.height, 1),
        Decoded::Animated(a) => (a.width, a.height, a.frames.len()),
    }
}

/// xorshift64*, deterministic.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Small limits so hostile loops run fast and the budget is exercised.
pub fn small_limits() -> Limits {
    Limits {
        max_pixels: 1 << 20,
        max_frames: 64,
        max_total_bytes: 64 << 20,
    }
}

/// A decode result must be Err, or Ok with consistent buffers within limits.
pub fn check_result(r: &Result<Decoded, DecodeError>, limits: &Limits) {
    if let Ok(d) = r {
        let mut total = 0usize;
        let frames: Vec<&px_image::Image> = match d {
            Decoded::Still(i) => vec![i],
            Decoded::Animated(a) => {
                assert!(a.frames.len() <= limits.max_frames);
                a.frames.iter().map(|f| &f.image).collect()
            }
        };
        for i in frames {
            assert!(i.width > 0 && i.height > 0);
            assert_eq!(i.rgba.len(), i.width as usize * i.height as usize * 4);
            assert!(i.rgba.capacity() <= limits.max_total_bytes);
            assert!((i.width as u64) * (i.height as u64) <= limits.max_pixels);
            total += i.rgba.capacity();
        }
        assert!(total <= limits.max_total_bytes);
    }
}

/// The battery: truncation at every byte, byte corruption at every position
/// (stride-capped for big inputs), and a time bound for the whole run.
pub fn hostile_battery(name: &str, data: &[u8], decode: DecodeFn) {
    let limits = small_limits();
    let start = Instant::now();
    for n in 0..data.len() {
        let r = decode(&data[..n], &limits);
        check_result(&r, &limits);
    }
    let stride = (data.len() / 4000).max(1);
    let mut buf = data.to_vec();
    let mut i = 0;
    while i < data.len() {
        let orig = buf[i];
        for flip in [0xFFu8, 0x01, 0x80] {
            buf[i] = orig ^ flip;
            let r = decode(&buf, &limits);
            check_result(&r, &limits);
        }
        buf[i] = orig;
        i += stride;
    }
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "{name}: hostile battery too slow"
    );
}

pub fn random_bytes_battery(prefix: &[u8], seed: u64, decode: DecodeFn) {
    let limits = small_limits();
    let mut rng = Rng(seed);
    let start = Instant::now();
    for round in 0..3000 {
        let len = (rng.next() % 600) as usize;
        let mut v = prefix.to_vec();
        for _ in 0..len {
            v.push(rng.next() as u8);
        }
        let r = decode(&v, &limits);
        check_result(&r, &limits);
        let _ = round;
    }
    assert!(start.elapsed() < Duration::from_secs(60));
}
