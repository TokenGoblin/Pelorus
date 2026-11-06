#![no_main]

//! Untrusted image bytes through every decoder (Phase 15, §4.3, §10).
//!
//! The target sniffs the format from the magic bytes and decodes with limits
//! small enough to keep each execution fast. Whatever the bytes: decoding must
//! return `Ok` or `Err` without panicking or allocating beyond the limits, an
//! `Ok` result must be internally consistent (buffer length equals
//! `width * height * 4`, within the pixel cap, frame count within the cap), and
//! decoding the same bytes twice must give the same result (determinism). A
//! second pass tries the bytes behind each other format's magic so a decoder is
//! reached even when the corpus entry starts with another format's header.
//!
//! Seeds: the fixtures under `crates/px-image/tests/fixtures`.

use libfuzzer_sys::fuzz_target;
use px_image::{Decoded, Image, Limits, decode, sniff};

const LIMITS: Limits = Limits {
    max_pixels: 1 << 22,
    max_frames: 64,
    max_total_bytes: 64 << 20,
};

const MAGICS: [&[u8]; 6] = [
    &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
    b"GIF89a",
    &[0xFF, 0xD8, 0xFF],
    b"BM",
    &[0, 0, 1, 0],
    // Static SVG (ADR 049): the parser, style, path and raster code.
    b"<svg xmlns='http://www.w3.org/2000/svg' width='16' height='16'>",
];

fn consistent(i: &Image) {
    assert!(i.width > 0 && i.height > 0);
    assert!(u64::from(i.width) * u64::from(i.height) <= LIMITS.max_pixels);
    assert_eq!(i.rgba.len(), i.width as usize * i.height as usize * 4);
}

fn run(bytes: &[u8]) {
    let first = decode(bytes, &LIMITS);
    if let Ok(d) = &first {
        match d {
            Decoded::Still(i) => consistent(i),
            Decoded::Animated(a) => {
                assert!(!a.frames.is_empty() && a.frames.len() <= LIMITS.max_frames);
                for f in &a.frames {
                    consistent(&f.image);
                    assert_eq!((f.image.width, f.image.height), (a.width, a.height));
                }
            }
        }
    }
    assert_eq!(first, decode(bytes, &LIMITS));
}

fuzz_target!(|data: &[u8]| {
    run(data);
    if sniff(data).is_none() {
        // Reach each decoder with the raw bytes behind its magic.
        for magic in MAGICS {
            let mut v = magic.to_vec();
            v.extend_from_slice(data);
            run(&v);
        }
    }
});
