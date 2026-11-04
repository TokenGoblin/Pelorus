//! PNG / APNG tests. Fixtures under tests/fixtures/png; see PROVENANCE.txt there.
//! Expectations are SHA-256 hashes of the RGBA computed by an independent
//! decoder (Pillow) and, for the baseline, by a hand-written Python model.

#![cfg(test)]
mod common;
use common::*;
use px_image::{DecodeError, Decoded, Limits, png};

fn dec(d: &[u8], l: &Limits) -> Result<Decoded, DecodeError> {
    png::decode(d, l)
}

fn fixtures() -> Vec<(Entry, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/png");
    let m = std::fs::read_to_string(format!("{dir}/MANIFEST")).unwrap();
    parse_manifest(&m)
        .into_iter()
        .map(|e| {
            let b = std::fs::read(format!("{dir}/{}", e.name)).unwrap();
            (e, b)
        })
        .collect()
}

#[test]
fn every_fixture_matches_the_oracle_hash() {
    let fx = fixtures();
    assert!(fx.len() >= 40);
    for (e, bytes) in &fx {
        let d = dec(bytes, &Limits::default()).unwrap_or_else(|err| panic!("{}: {err}", e.name));
        let (w, h, n) = dims(&d);
        assert_eq!((w, h, n), (e.width, e.height, e.frames), "{}", e.name);
        assert_eq!(hex(&sha256(&all_rgba(&d))), e.sha, "{}", e.name);
        if let Decoded::Animated(a) = &d {
            assert_eq!(a.loop_count, e.plays, "{}", e.name);
            let delays: Vec<u32> = a.frames.iter().map(|f| f.delay_ms).collect();
            assert_eq!(delays, e.delays, "{}", e.name);
        }
    }
}

#[test]
fn hand_derived_pixels() {
    let l = Limits::default();
    // 2x1 RGB, filter None: (1,2,3) (4,5,6).
    let png = png_with(2, 1, 8, 2, 0, &[0, 1, 2, 3, 4, 5, 6], &[]);
    let d = dec(&png, &l).unwrap();
    assert_eq!(
        d.first_image().unwrap().rgba,
        vec![1, 2, 3, 255, 4, 5, 6, 255]
    );
    // 1-bit grey 8x1 = 0b10100000 -> 255,0,255,0,0,0,0,0 (bit 7 first).
    let png = png_with(8, 1, 1, 0, 0, &[0, 0b1010_0000], &[]);
    let d = dec(&png, &l).unwrap();
    let g: Vec<u8> = d
        .first_image()
        .unwrap()
        .rgba
        .chunks(4)
        .map(|p| p[0])
        .collect();
    assert_eq!(g, vec![255, 0, 255, 0, 0, 0, 0, 0]);
    // 16-bit grey, Sub filter, bpp 2: bytes 2,3 += bytes 0,1 -> 0x12,0x35; high bytes 0x12, 0x12.
    let png = png_with(2, 1, 16, 0, 0, &[1, 0x12, 0x34, 0x00, 0x01], &[]);
    let d = dec(&png, &l).unwrap();
    let g: Vec<u8> = d
        .first_image()
        .unwrap()
        .rgba
        .chunks(4)
        .map(|p| p[0])
        .collect();
    assert_eq!(g, vec![0x12, 0x12]);
    // Up: row0 = 10, row1 = 5 + 10 = 15.
    let png = png_with(1, 2, 8, 0, 0, &[0, 10, 2, 5], &[]);
    assert_eq!(dec(&png, &l).unwrap().first_image().unwrap().rgba[4], 15);
    // Average: 5 + floor((0 + 10) / 2) = 10.
    let png = png_with(1, 2, 8, 0, 0, &[0, 10, 3, 5], &[]);
    assert_eq!(dec(&png, &l).unwrap().first_image().unwrap().rgba[4], 10);
    // Paeth with a=0, b=10, c=0 predicts b: 5 + 10 = 15.
    let png = png_with(1, 2, 8, 0, 0, &[0, 10, 4, 5], &[]);
    assert_eq!(dec(&png, &l).unwrap().first_image().unwrap().rgba[4], 15);
}

#[test]
fn palette_index_out_of_range_is_an_error() {
    let plte = png_chunk(b"PLTE", &[1, 2, 3]);
    let png = png_with(1, 1, 8, 3, 0, &[0, 5], &[plte]);
    assert!(dec(&png, &Limits::default()).is_err());
}

#[test]
fn bad_bit_depth_colour_combination_is_rejected() {
    assert!(
        dec(
            &png_with(1, 1, 16, 3, 0, &[0, 0, 0], &[]),
            &Limits::default()
        )
        .is_err()
    );
    assert!(
        dec(
            &png_with(1, 1, 4, 2, 0, &[0, 0, 0], &[]),
            &Limits::default()
        )
        .is_err()
    );
}

#[test]
fn corrupted_crc_is_rejected() {
    let mut png = png_with(1, 1, 8, 0, 0, &[0, 9], &[]);
    let n = png.len();
    png[n - 18] ^= 1; // inside IDAT's body
    assert!(dec(&png, &Limits::default()).is_err());
}

#[test]
fn huge_dimensions_rejected_before_allocating() {
    for (w, h) in [
        (u32::MAX, u32::MAX),
        (100_000, 100_000),
        (0, 5),
        (5, 0),
        (1 << 25, 2),
    ] {
        let png = png_with(w, h, 8, 6, 0, &[0], &[]);
        assert!(dec(&png, &Limits::default()).is_err(), "{w}x{h}");
    }
}

#[test]
fn four_gib_decode_is_refused_by_the_budget_not_the_allocator() {
    // 32768 x 32768 RGBA needs 4 GiB; the pixel cap allows it, the byte cap must not.
    let l = Limits {
        max_pixels: 1 << 30,
        max_frames: 8,
        max_total_bytes: 1 << 20,
    };
    let png = png_with(32768, 32768, 8, 6, 0, &[0], &[]);
    assert_eq!(dec(&png, &l), Err(DecodeError::LimitExceeded("bytes")));
}

#[test]
fn decompression_bomb_is_bounded() {
    // 1 MiB of zeros in the stream, but the header claims a 4x4 image.
    let z = zlib_stored(&vec![0u8; 1 << 20]);
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    v.extend(png_chunk(b"IHDR", &[0, 0, 0, 4, 0, 0, 0, 4, 8, 0, 0, 0, 0]));
    v.extend(png_chunk(b"IDAT", &z));
    v.extend(png_chunk(b"IEND", &[]));
    assert!(dec(&v, &Limits::default()).is_err());
}

#[test]
fn data_longer_than_the_header_claims_is_rejected() {
    let mut raw = vec![0u8; 18];
    raw.extend_from_slice(&[1, 2, 3]);
    assert!(dec(&png_with(4, 4, 8, 0, 0, &raw, &[]), &Limits::default()).is_err());
}

#[test]
fn unknown_critical_chunk_rejected_unknown_ancillary_skipped() {
    let crit = png_chunk(b"ABCD", &[1]);
    let png = png_with(1, 1, 8, 0, 0, &[0, 9], &[crit]);
    assert_eq!(
        dec(&png, &Limits::default()),
        Err(DecodeError::Unsupported("unknown critical chunk"))
    );
    let anc = png_chunk(b"abcd", &[1]);
    assert!(
        dec(
            &png_with(1, 1, 8, 0, 0, &[0, 9], &[anc]),
            &Limits::default()
        )
        .is_ok()
    );
}

#[test]
fn apng_frame_count_is_limited_before_anything_is_allocated() {
    let actl = png_chunk(b"acTL", &[0xFF, 0xFF, 0xFF, 0xFF, 0, 0, 0, 0]);
    let png = png_with(1, 1, 8, 0, 0, &[0, 9], &[actl]);
    assert!(matches!(
        dec(&png, &Limits::default()),
        Err(DecodeError::LimitExceeded("frames"))
    ));
}

#[test]
fn hostile_truncation_and_corruption_on_every_fixture() {
    for (e, bytes) in fixtures() {
        hostile_battery(&e.name, &bytes, &dec);
    }
}

#[test]
fn random_bytes_never_panic() {
    let sig = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    random_bytes_battery(&sig, 0x1234_5678_9abc_def1, &dec);
    random_bytes_battery(&[], 0xdead_beef_1234_5678, &dec);
}

#[test]
fn deterministic() {
    for (_, bytes) in fixtures().iter().take(10) {
        assert_eq!(
            dec(bytes, &Limits::default()),
            dec(bytes, &Limits::default())
        );
    }
}
