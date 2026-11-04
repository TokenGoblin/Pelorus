//! GIF tests. Fixtures under tests/fixtures/gif (see PROVENANCE.txt): expected
//! hashes come from Pillow's decode of every frame. Hand-built streams carry
//! hand-derived expectations.
#![cfg(test)]

mod common;
use common::*;
use px_image::{DecodeError, Decoded, Limits, gif};

fn dec(d: &[u8], l: &Limits) -> Result<Decoded, DecodeError> {
    gif::decode(d, l)
}

fn fixtures() -> Vec<(Entry, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gif");
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
    assert!(fx.len() >= 14);
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
fn the_interlace_fixtures_really_are_interlaced() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/gif");
    for (name, want) in [
        ("still4_interlace1.gif", true),
        ("still4_interlace0.gif", false),
        ("noise256_interlace1.gif", true),
        ("noise256_interlace0.gif", false),
    ] {
        let b = std::fs::read(format!("{dir}/{name}")).unwrap();
        // Image descriptor: 0x2C, left, top, width, height (these fixtures are
        // single full-screen frames), then the packed byte.
        let i = (0..b.len() - 10)
            .find(|&i| {
                b[i] == 0x2C && b[i + 1..i + 5] == [0, 0, 0, 0] && b[i + 5..i + 9] == b[6..10]
            })
            .unwrap();
        assert_eq!(b[i + 9] & 0x40 != 0, want, "{name}");
    }
}

// ---- hand-built streams

const PAL: [[u8; 3]; 4] = [[255, 0, 0], [0, 255, 0], [0, 0, 255], [9, 8, 7]];

fn pack_codes(codes: &[u32], width: u32) -> Vec<u8> {
    let (mut acc, mut n, mut out) = (0u32, 0u32, Vec::new());
    for &c in codes {
        acc |= c << n;
        n += width;
        while n >= 8 {
            out.push((acc & 0xFF) as u8);
            acc >>= 8;
            n -= 8;
        }
    }
    if n > 0 {
        out.push(acc as u8);
    }
    out
}

fn sub_blocks(data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    for c in data.chunks(255) {
        v.push(c.len() as u8);
        v.extend_from_slice(c);
    }
    v.push(0);
    v
}

/// Literal-only LZW with min code size 2: `clear a b clear c d ... eoi`.
fn lzw_literals(pixels: &[u8]) -> Vec<u8> {
    let mut codes = Vec::new();
    for pair in pixels.chunks(2) {
        codes.push(4);
        codes.extend(pair.iter().map(|&p| u32::from(p)));
    }
    codes.push(5);
    pack_codes(&codes, 3)
}

fn image(l: u16, t: u16, w: u16, h: u16, interlaced: bool, pixels: &[u8]) -> Vec<u8> {
    let mut v = vec![0x2C];
    for x in [l, t, w, h] {
        v.extend_from_slice(&x.to_le_bytes());
    }
    v.push(if interlaced { 0x40 } else { 0 });
    v.push(2);
    v.extend(sub_blocks(&lzw_literals(pixels)));
    v
}

fn gce(disposal: u8, transparent: Option<u8>, delay_cs: u16) -> Vec<u8> {
    let p = (disposal << 2) | u8::from(transparent.is_some());
    let d = delay_cs.to_le_bytes();
    vec![0x21, 0xF9, 4, p, d[0], d[1], transparent.unwrap_or(0), 0]
}

fn netscape(count: u16) -> Vec<u8> {
    let c = count.to_le_bytes();
    let mut v = vec![0x21, 0xFF, 11];
    v.extend_from_slice(b"NETSCAPE2.0");
    v.extend_from_slice(&[3, 1, c[0], c[1], 0]);
    v
}

fn gif(w: u16, h: u16, body: &[Vec<u8>]) -> Vec<u8> {
    let mut v = b"GIF89a".to_vec();
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&[0x81, 0, 0]); // global table, 4 entries
    for c in PAL {
        v.extend_from_slice(&c);
    }
    for b in body {
        v.extend_from_slice(b);
    }
    v.push(0x3B);
    v
}

fn px(i: usize) -> [u8; 4] {
    let c = PAL[i];
    [c[0], c[1], c[2], 255]
}

fn frames_of(d: &Decoded) -> Vec<Vec<u8>> {
    match d {
        Decoded::Animated(a) => a.frames.iter().map(|f| f.image.rgba.clone()).collect(),
        Decoded::Still(i) => vec![i.rgba.clone()],
    }
}

#[test]
fn plain_image_maps_indices_through_the_palette() {
    let d = dec(
        &gif(2, 2, &[image(0, 0, 2, 2, false, &[0, 1, 2, 3])]),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(frames_of(&d)[0], [px(0), px(1), px(2), px(3)].concat());
}

#[test]
fn interlaced_rows_land_in_the_right_places() {
    // 1x5: decoded order rows 0,4,2,1,3 carry indices 0,1,2,3,0.
    let d = dec(
        &gif(1, 5, &[image(0, 0, 1, 5, true, &[0, 1, 2, 3, 0])]),
        &Limits::default(),
    )
    .unwrap();
    let rows: Vec<[u8; 4]> = frames_of(&d)[0]
        .chunks(4)
        .map(|c| [c[0], c[1], c[2], c[3]])
        .collect();
    // row0=idx0, row4=idx1, row2=idx2, row1=idx3, row3=idx0
    assert_eq!(rows, vec![px(0), px(3), px(2), px(0), px(1)]);
}

#[test]
fn transparency_leaves_the_canvas_alone() {
    let d = dec(
        &gif(
            2,
            1,
            &[gce(0, Some(1), 5), image(0, 0, 2, 1, false, &[1, 2])],
        ),
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(frames_of(&d)[0], [[0, 0, 0, 0], px(2)].concat());
}

#[test]
fn disposal_modes_and_delays_and_loop() {
    // 2x1 canvas. Frame 1 fills both pixels (disposal 1: keep). Frame 2 draws
    // index 2 on the right only (disposal 2: clear its rect). Frame 3 draws
    // index 3 on the left (disposal 3: restore). Frame 4 draws index 1 on the
    // right.
    let body = [
        netscape(2),
        gce(1, None, 10),
        image(0, 0, 2, 1, false, &[0, 0]),
        gce(2, None, 20),
        image(1, 0, 1, 1, false, &[2]),
        gce(3, None, 30),
        image(0, 0, 1, 1, false, &[3]),
        gce(0, None, 40),
        image(1, 0, 1, 1, false, &[1]),
    ];
    let d = dec(&gif(2, 1, &body), &Limits::default()).unwrap();
    let Decoded::Animated(a) = &d else { panic!() };
    assert_eq!(a.loop_count, 3); // extension value 2 means 2 repeats: 3 plays
    let delays: Vec<u32> = a.frames.iter().map(|f| f.delay_ms).collect();
    assert_eq!(delays, vec![100, 200, 300, 400]);
    let f = frames_of(&d);
    assert_eq!(f[0], [px(0), px(0)].concat());
    assert_eq!(f[1], [px(0), px(2)].concat()); // after disposal 1 (keep) frame 2 drawn
    // Frame 2 is disposed to background: right pixel cleared before frame 3.
    assert_eq!(f[2], [px(3), [0, 0, 0, 0]].concat());
    // Frame 3 had disposal 3: canvas restored to before frame 3 = [p0, clear].
    assert_eq!(f[3], [px(0), px(1)].concat());
}

#[test]
fn loop_extension_zero_is_forever_and_missing_is_once() {
    let img = || image(0, 0, 1, 1, false, &[0]);
    let a = dec(&gif(1, 1, &[netscape(0), img(), img()]), &Limits::default()).unwrap();
    let Decoded::Animated(a) = a else { panic!() };
    assert_eq!(a.loop_count, 0);
    let b = dec(&gif(1, 1, &[img(), img()]), &Limits::default()).unwrap();
    let Decoded::Animated(b) = b else { panic!() };
    assert_eq!(b.loop_count, 1);
}

#[test]
fn frame_outside_the_screen_is_rejected() {
    let g = gif(2, 2, &[image(1, 1, 2, 2, false, &[0, 0, 0, 0])]);
    assert_eq!(
        dec(&g, &Limits::default()),
        Err(DecodeError::Malformed("frame outside logical screen"))
    );
}

#[test]
fn invalid_lzw_streams_are_rejected() {
    let img = |codes: &[u32]| {
        let mut v = vec![0x2C, 0, 0, 0, 0, 2, 0, 1, 0, 0, 2];
        v.extend(sub_blocks(&pack_codes(codes, 3)));
        gif(2, 1, &[v])
    };
    let l = Limits::default();
    // First code after clear is the EOI/clear range (a non-literal).
    assert!(dec(&img(&[4, 6, 0]), &l).is_err());
    // Code beyond next free entry (7 when next is 6).
    assert!(dec(&img(&[4, 0, 7]), &l).is_err());
    // EOI before the frame is full.
    assert!(dec(&img(&[4, 0, 5]), &l).is_err());
    // No data at all.
    assert!(dec(&img(&[]), &l).is_err());
    // Control: a valid 2-pixel stream works.
    assert!(dec(&img(&[4, 0, 1, 5]), &l).is_ok());
}

#[test]
fn bad_min_code_size_is_rejected() {
    for m in [0u8, 9, 12, 255] {
        let mut v = vec![0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, m];
        v.extend(sub_blocks(&[0, 0]));
        assert!(
            dec(&gif(1, 1, &[v]), &Limits::default()).is_err(),
            "min {m}"
        );
    }
}

#[test]
fn table_overflow_stream_stays_bounded() {
    // 12-bit codes forever without a clear: after the table fills, every
    // literal is still decodable and nothing grows. 60000 codes of literal 0.
    let mut codes = vec![256u32]; // clear for min code size 8
    codes.extend(std::iter::repeat_n(0u32, 20_000));
    let mut bits = Vec::new();
    let (mut acc, mut n) = (0u32, 0u32);
    // The code width grows as the table grows; emulate the encoder's width.
    let (mut next, mut width) = (258u32, 9u32);
    for (i, &c) in codes.iter().enumerate() {
        acc |= c << n;
        n += width;
        while n >= 8 {
            bits.push((acc & 0xFF) as u8);
            acc >>= 8;
            n -= 8;
        }
        if i > 0 && next < 4096 {
            next += 1;
            if next == (1 << width) && width < 12 {
                width += 1;
            }
        }
    }
    if n > 0 {
        bits.push(acc as u8);
    }
    let mut v = vec![0x2C, 0, 0, 0, 0, 100, 0, 100, 0, 0, 8];
    v.extend(sub_blocks(&bits));
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&[100, 0, 100, 0, 0x80 | 7, 0, 0]);
    g.extend(vec![0u8; 768]);
    g.extend(v);
    g.push(0x3B);
    let d = dec(&g, &Limits::default()).unwrap();
    assert!(frames_of(&d)[0].chunks(4).all(|p| p == [0, 0, 0, 255]));
}

#[test]
fn huge_canvas_is_refused_before_allocating() {
    let l = Limits::default();
    let g = gif(65535, 65535, &[image(0, 0, 1, 1, false, &[0])]);
    assert_eq!(dec(&g, &l), Err(DecodeError::LimitExceeded("pixels")));
    // Pixel cap lifted, byte cap not: a 4 GiB canvas is refused by the budget.
    let l2 = Limits {
        max_pixels: u64::MAX,
        max_frames: 4,
        max_total_bytes: 1 << 20,
    };
    assert_eq!(dec(&g, &l2), Err(DecodeError::LimitExceeded("bytes")));
    // Zero canvas.
    assert!(dec(&gif(0, 5, &[image(0, 0, 1, 1, false, &[0])]), &l).is_err());
}

#[test]
fn many_frames_hit_the_frame_limit() {
    let body: Vec<Vec<u8>> = (0..10).map(|_| image(0, 0, 1, 1, false, &[0])).collect();
    let l = Limits {
        max_frames: 5,
        ..Limits::default()
    };
    assert_eq!(
        dec(&gif(1, 1, &body), &l),
        Err(DecodeError::LimitExceeded("frames"))
    );
}

#[test]
fn missing_trailer_after_a_complete_frame_is_accepted_but_not_before() {
    let mut g = gif(1, 1, &[image(0, 0, 1, 1, false, &[0])]);
    g.pop();
    assert!(dec(&g, &Limits::default()).is_ok());
    assert!(dec(&g[..g.len() - 3], &Limits::default()).is_err());
    assert!(dec(b"GIF89a", &Limits::default()).is_err());
}

#[test]
fn hostile_truncation_and_corruption_on_every_fixture() {
    for (e, bytes) in fixtures() {
        hostile_battery(&e.name, &bytes, &dec);
    }
}

#[test]
fn random_bytes_never_panic() {
    random_bytes_battery(b"GIF89a", 0x0bad_cafe_1234_5678, &dec);
    random_bytes_battery(
        b"GIF89a\x04\x00\x04\x00\x80\x00\x00",
        0x1357_9bdf_0246_8ace,
        &dec,
    );
}
