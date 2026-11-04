//! BMP and ICO tests. Fixtures under tests/fixtures/bmp (see PROVENANCE.txt);
//! expected hashes are Pillow's RGBA. Hand-built files carry hand-derived
//! expectations.
#![cfg(test)]

mod common;
use common::*;
use px_image::{DecodeError, Decoded, Limits, bmp, ico};

fn dec(d: &[u8], l: &Limits) -> Result<Decoded, DecodeError> {
    if d.starts_with(b"BM") {
        bmp::decode(d, l).map(Decoded::Still)
    } else {
        ico::decode(d, l).map(Decoded::Still)
    }
}

fn fixtures() -> Vec<(Entry, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/bmp");
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
    assert!(fx.len() >= 22);
    for (e, bytes) in &fx {
        let d = dec(bytes, &Limits::default()).unwrap_or_else(|err| panic!("{}: {err}", e.name));
        let (w, h, _) = dims(&d);
        assert_eq!((w, h), (e.width, e.height), "{}", e.name);
        assert_eq!(hex(&sha256(&all_rgba(&d))), e.sha, "{}", e.name);
    }
}

// ---- hand-built

fn le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// 24-bit BMP, rows given top-first as (r,g,b) triples, written bottom-up.
fn bmp24(w: u32, rows: &[Vec<[u8; 3]>], top_down: bool) -> Vec<u8> {
    let h = rows.len() as u32;
    let stride = (w * 3).div_ceil(4) * 4;
    let mut data = Vec::new();
    let order: Vec<&Vec<[u8; 3]>> = if top_down {
        rows.iter().collect()
    } else {
        rows.iter().rev().collect()
    };
    for r in order {
        let mut row = Vec::new();
        for p in r {
            row.extend_from_slice(&[p[2], p[1], p[0]]);
        }
        row.resize(stride as usize, 0);
        data.extend(row);
    }
    let mut v = b"BM".to_vec();
    v.extend(le(54 + data.len() as u32));
    v.extend([0; 4]);
    v.extend(le(54));
    v.extend(le(40));
    v.extend(le(w));
    v.extend(le(if top_down {
        (h as i32).wrapping_neg() as u32
    } else {
        h
    }));
    v.extend([1, 0, 24, 0]);
    v.extend([0; 24]);
    v.extend(data);
    v
}

#[test]
fn bottom_up_and_top_down_rows_with_padding() {
    // 1 pixel wide: each row is 3 bytes + 1 pad.
    let rows = vec![vec![[1, 2, 3]], vec![[4, 5, 6]]];
    let up = bmp::decode(&bmp24(1, &rows, false), &Limits::default()).unwrap();
    assert_eq!(up.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
    let down = bmp::decode(&bmp24(1, &rows, true), &Limits::default()).unwrap();
    assert_eq!(down.rgba, vec![1, 2, 3, 255, 4, 5, 6, 255]);
}

fn bmp8_rle(w: u32, h: u32, palette: &[[u8; 3]], rle: &[u8], comp: u32) -> Vec<u8> {
    let mut v = b"BM".to_vec();
    let off = 54 + 4 * palette.len() as u32;
    v.extend(le(off + rle.len() as u32));
    v.extend([0; 4]);
    v.extend(le(off));
    v.extend(le(40));
    v.extend(le(w));
    v.extend(le(h));
    v.extend([1, 0, if comp == 2 { 4 } else { 8 }, 0]);
    v.extend(le(comp));
    v.extend(le(rle.len() as u32));
    v.extend([0; 8]);
    v.extend(le(palette.len() as u32));
    v.extend([0; 4]);
    for p in palette {
        v.extend_from_slice(&[p[2], p[1], p[0], 0]);
    }
    v.extend_from_slice(rle);
    v
}

#[test]
fn bi_rgb_32_bit_uses_alpha_only_if_any_pixel_has_it() {
    // 1x2, 32 bpp BI_RGB (header 40): rows bottom-up (B,G,R,A).
    let build = |a0: u8, a1: u8| {
        let mut v = b"BM".to_vec();
        v.extend(le(54 + 8));
        v.extend([0; 4]);
        v.extend(le(54));
        v.extend(le(40));
        v.extend(le(1));
        v.extend(le(2));
        v.extend([1, 0, 32, 0]);
        v.extend([0; 24]);
        v.extend([3, 2, 1, a1]); // bottom row = image row 1
        v.extend([6, 5, 4, a0]); // top row = image row 0
        v
    };
    let l = Limits::default();
    // Pillow reads these as opaque RGB; we follow the browser heuristic instead.
    let some = bmp::decode(&build(128, 0), &l).unwrap();
    assert_eq!(some.rgba, vec![4, 5, 6, 128, 1, 2, 3, 0]);
    let none = bmp::decode(&build(0, 0), &l).unwrap();
    assert_eq!(none.rgba, vec![4, 5, 6, 255, 1, 2, 3, 255]);
}

#[test]
fn rle8_runs_absolute_delta_and_holes() {
    let pal = [[10, 0, 0], [0, 20, 0], [0, 0, 30], [40, 40, 40]];
    // 4x2 image. Stream row 0 (the bottom): run of 3 x idx1, then idx3 (single run),
    // EOL. Row 1 (top): delta right 1 down 0, absolute run of 3 (idx 2,0,2) + pad, EOI.
    let rle = [3, 1, 1, 3, 0, 0, 0, 2, 1, 0, 0, 3, 2, 0, 2, 0, 0, 1];
    let img = bmp::decode(&bmp8_rle(4, 2, &pal, &rle, 1), &Limits::default()).unwrap();
    let px = |i: usize| {
        let p = pal[i];
        [p[0], p[1], p[2], 255]
    };
    let hole = [0, 0, 0, 0];
    // Top row (y=0) is stream row 1: x=0 hole (skipped), x=1..3 = idx 2,0,2.
    let want_top = [hole, px(2), px(0), px(2)].concat();
    let want_bottom = [px(1), px(1), px(1), px(3)].concat();
    assert_eq!(&img.rgba[..16], &want_top[..]);
    assert_eq!(&img.rgba[16..], &want_bottom[..]);
}

#[test]
fn rle_pixels_outside_the_image_are_rejected() {
    let pal = [[0, 0, 0], [1, 1, 1]];
    // Run of 5 in a 4 wide image.
    let rle = [5, 1, 0, 1];
    assert!(bmp::decode(&bmp8_rle(4, 1, &pal, &rle, 1), &Limits::default()).is_err());
    // Delta below the image then a pixel.
    let rle = [0, 2, 0, 5, 1, 1, 0, 1];
    assert!(bmp::decode(&bmp8_rle(4, 2, &pal, &rle, 1), &Limits::default()).is_err());
}

#[test]
fn rle4_nibbles() {
    let pal = [[1, 1, 1], [2, 2, 2], [3, 3, 3]];
    // 3x1: run of 3 pixels with byte 0x12 -> idx 1,2,1.
    let rle = [3, 0x12, 0, 1];
    let img = bmp::decode(&bmp8_rle(3, 1, &pal, &rle, 2), &Limits::default()).unwrap();
    assert_eq!(img.rgba, vec![2, 2, 2, 255, 3, 3, 3, 255, 2, 2, 2, 255]);
}

#[test]
fn ico_picks_the_largest_frame_and_applies_the_and_mask() {
    // One 2x2 24-bit frame: XOR rows (bottom-up) then AND mask rows. Pixel (x=1,y=0) masked.
    let mut dib = Vec::new();
    dib.extend(le(40));
    dib.extend(le(2));
    dib.extend(le(4)); // doubled height
    dib.extend([1, 0, 24, 0]);
    dib.extend([0; 24]);
    // bottom row (y=1): (7,8,9) (1,2,3) padded to 8 bytes; top row (y=0): (4,5,6) (10,11,12)
    dib.extend([9, 8, 7, 3, 2, 1, 0, 0]);
    dib.extend([6, 5, 4, 12, 11, 10, 0, 0]);
    // AND mask rows bottom-up, 4 bytes each: y=1 none masked, y=0 x=1 masked (bit 6).
    dib.extend([0, 0, 0, 0]);
    dib.extend([0b0100_0000, 0, 0, 0]);
    let mut f = vec![0, 0, 1, 0, 2, 0];
    // A small decoy first (1x1) and the real 2x2 second.
    let off = 6 + 32;
    f.extend([1, 1, 0, 0, 1, 0, 24, 0]);
    f.extend(le(4));
    f.extend(le(off as u32 + dib.len() as u32));
    f.extend([2, 2, 0, 0, 1, 0, 24, 0]);
    f.extend(le(dib.len() as u32));
    f.extend(le(off as u32));
    f.extend(&dib);
    f.extend([0, 0, 0, 0]);
    let img = ico::decode(&f, &Limits::default()).unwrap();
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(
        img.rgba,
        vec![4, 5, 6, 255, 10, 11, 12, 0, 7, 8, 9, 255, 1, 2, 3, 255]
    );
}

#[test]
fn ico_frame_ranges_are_validated() {
    let mut f = vec![0, 0, 1, 0, 1, 0, 8, 8, 0, 0, 1, 0, 32, 0];
    f.extend(le(1000));
    f.extend(le(22));
    assert!(ico::decode(&f, &Limits::default()).is_err());
    f.extend(le(0)); // pad
    assert!(ico::decode(&[0, 0, 1, 0, 0, 0], &Limits::default()).is_err());
    assert!(ico::decode(&[0, 0, 2, 0, 1, 0], &Limits::default()).is_err());
}

#[test]
fn huge_dimensions_are_rejected_before_allocating() {
    let l = Limits::default();
    // Claimed 65536 x 65536 24-bit with no pixel data.
    let mut v = bmp24(1, &[vec![[0, 0, 0]]], false);
    v[18..22].copy_from_slice(&le(65536));
    v[22..26].copy_from_slice(&le(65536));
    assert!(bmp::decode(&v, &l).is_err());
    // Within the pixel cap but more data claimed than present: Truncated, no allocation.
    let mut v = bmp24(1, &[vec![[0, 0, 0]]], false);
    v[18..22].copy_from_slice(&le(4000));
    v[22..26].copy_from_slice(&le(4000));
    assert_eq!(bmp::decode(&v, &l), Err(DecodeError::Truncated));
    // i32::MIN height (top-down, enormous).
    let mut v = bmp24(1, &[vec![[0, 0, 0]]], false);
    v[22..26].copy_from_slice(&le(0x8000_0000));
    assert!(bmp::decode(&v, &l).is_err());
    // RLE bomb: 4G x 4G 8-bit RLE claims pixels but has 4 bytes of data; pixel cap refuses.
    let pal = [[0, 0, 0]];
    let r = bmp::decode(&bmp8_rle(0x7FFF_FFFF, 0x7FFF_FFFF, &pal, &[0, 1], 1), &l);
    assert_eq!(r, Err(DecodeError::LimitExceeded("pixels")));
    // Pixel cap lifted: the byte budget still refuses the 4 GiB output.
    let l2 = Limits {
        max_pixels: u64::MAX,
        max_frames: 1,
        max_total_bytes: 1 << 20,
    };
    let r = bmp::decode(&bmp8_rle(32768, 32768, &pal, &[0, 1], 1), &l2);
    assert_eq!(r, Err(DecodeError::LimitExceeded("bytes")));
}

#[test]
fn unsupported_embedded_compression() {
    let mut v = bmp24(1, &[vec![[0, 0, 0]]], false);
    v[30..34].copy_from_slice(&le(5)); // BI_PNG
    assert!(matches!(
        bmp::decode(&v, &Limits::default()),
        Err(DecodeError::Unsupported(_))
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
    random_bytes_battery(b"BM", 0x0a0b_0c0d_0e0f_1011, &dec);
    random_bytes_battery(&[0, 0, 1, 0, 1, 0], 0x1112_1314_1516_1718, &dec);
    let b = bmp24(2, &[vec![[1, 2, 3], [4, 5, 6]]], false);
    random_bytes_battery(&b[..40], 0x2122_2324_2526_2728, &dec);
}
