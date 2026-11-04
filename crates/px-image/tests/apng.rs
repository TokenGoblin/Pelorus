//! Hand-built APNG streams with hand-derived expectations (dispose and blend
//! ops, hidden default image, sequence numbers).

#![cfg(test)]
mod common;
use common::*;
use px_image::{DecodeError, Decoded, Limits, png};

fn dec(d: &[u8], l: &Limits) -> Result<Decoded, DecodeError> {
    png::decode(d, l)
}

struct F {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    dispose: u8,
    blend: u8,
    rgba: Vec<u8>,
}

fn raw_rows(w: u32, rgba: &[u8]) -> Vec<u8> {
    let mut raw = Vec::new();
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    raw
}

fn fctl(seq: u32, f: &F, delay_num: u16, delay_den: u16) -> Vec<u8> {
    let mut b = seq.to_be_bytes().to_vec();
    for v in [f.w, f.h, f.x, f.y] {
        b.extend_from_slice(&v.to_be_bytes());
    }
    b.extend_from_slice(&delay_num.to_be_bytes());
    b.extend_from_slice(&delay_den.to_be_bytes());
    b.push(f.dispose);
    b.push(f.blend);
    png_chunk(b"fcTL", &b)
}

/// `hidden`: the default image has no fcTL and is not part of the animation.
fn apng(w: u32, h: u32, hidden: bool, frames: &[F], plays: u32) -> Vec<u8> {
    let mut v = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut ihdr = w.to_be_bytes().to_vec();
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    v.extend(png_chunk(b"IHDR", &ihdr));
    let mut actl = (frames.len() as u32).to_be_bytes().to_vec();
    actl.extend_from_slice(&plays.to_be_bytes());
    v.extend(png_chunk(b"acTL", &actl));
    let mut seq = 0;
    let mut rest = frames.iter();
    if hidden {
        let def = raw_rows(w, &vec![99u8; (w * h * 4) as usize]);
        v.extend(png_chunk(b"IDAT", &zlib_stored(&def)));
    } else if let Some(f0) = rest.next() {
        v.extend(fctl(seq, f0, 1, 10));
        seq += 1;
        v.extend(png_chunk(b"IDAT", &zlib_stored(&raw_rows(f0.w, &f0.rgba))));
    }
    for f in rest {
        v.extend(fctl(seq, f, 1, 4));
        seq += 1;
        let mut body = seq.to_be_bytes().to_vec();
        seq += 1;
        body.extend(zlib_stored(&raw_rows(f.w, &f.rgba)));
        v.extend(png_chunk(b"fdAT", &body));
    }
    v.extend(png_chunk(b"IEND", &[]));
    v
}

fn frames_of(d: &Decoded) -> Vec<Vec<u8>> {
    match d {
        Decoded::Animated(a) => a.frames.iter().map(|f| f.image.rgba.clone()).collect(),
        Decoded::Still(i) => vec![i.rgba.clone()],
    }
}

fn f(x: u32, y: u32, w: u32, h: u32, dispose: u8, blend: u8, rgba: Vec<u8>) -> F {
    F {
        x,
        y,
        w,
        h,
        dispose,
        blend,
        rgba,
    }
}

#[test]
fn dispose_background_clears_only_the_frame_region() {
    let a = [10, 0, 0, 255];
    let b = [20, 0, 0, 255];
    let c = [0, 0, 30, 255];
    let png = apng(
        2,
        1,
        false,
        &[
            f(0, 0, 2, 1, 1, 0, [a, b].concat()),
            f(1, 0, 1, 1, 0, 0, c.to_vec()),
        ],
        2,
    );
    let d = dec(&png, &Limits::default()).unwrap();
    let Decoded::Animated(an) = &d else {
        panic!("not animated")
    };
    assert_eq!(an.loop_count, 2);
    assert_eq!(an.frames[0].delay_ms, 100); // 1/10 s
    assert_eq!(an.frames[1].delay_ms, 250); // 1/4 s
    assert_eq!(
        frames_of(&d),
        vec![[a, b].concat(), [[0, 0, 0, 0], c].concat()]
    );
}

#[test]
fn dispose_previous_restores() {
    let a = [10, 0, 0, 255];
    let b = [20, 0, 0, 255];
    let png = apng(
        2,
        1,
        false,
        &[
            f(0, 0, 2, 1, 0, 0, [a, b].concat()),
            f(0, 0, 1, 1, 2, 0, vec![1, 1, 1, 255]),
            f(1, 0, 1, 1, 0, 1, vec![255, 0, 0, 0]),
        ],
        0,
    );
    let d = dec(&png, &Limits::default()).unwrap();
    // Frame 1 draws (1,1,1) at x=0 then is undone; frame 2 is fully transparent
    // blended over, so it shows the restored [a, b].
    assert_eq!(
        frames_of(&d),
        vec![
            [a, b].concat(),
            [[1, 1, 1, 255], b].concat(),
            [a, b].concat()
        ]
    );
}

#[test]
fn blend_over_with_partial_alpha_uses_the_documented_integer_formula() {
    // Opaque blue, then 128/255 red blended over it: [128, 0, 127, 255] (derived in
    // png::tests::blend_over_is_exact_at_extremes_and_hand_derived_midpoint).
    let png = apng(
        1,
        1,
        false,
        &[
            f(0, 0, 1, 1, 0, 0, vec![0, 0, 255, 255]),
            f(0, 0, 1, 1, 0, 1, vec![255, 0, 0, 128]),
        ],
        1,
    );
    let d = dec(&png, &Limits::default()).unwrap();
    assert_eq!(frames_of(&d)[1], vec![128, 0, 127, 255]);
}

#[test]
fn hidden_default_image_is_not_a_frame() {
    let p = [5, 6, 7, 255];
    let q = [8, 9, 10, 255];
    let png = apng(
        1,
        1,
        true,
        &[
            f(0, 0, 1, 1, 0, 0, p.to_vec()),
            f(0, 0, 1, 1, 0, 0, q.to_vec()),
        ],
        1,
    );
    let d = dec(&png, &Limits::default()).unwrap();
    assert_eq!(frames_of(&d), vec![p.to_vec(), q.to_vec()]);
}

#[test]
fn frame_outside_canvas_or_bad_sequence_is_rejected() {
    let g = |x| f(x, 0, 2, 1, 0, 0, vec![0; 8]);
    assert!(dec(&apng(2, 1, false, &[g(0), g(1)], 1), &Limits::default()).is_err());
    let mut png = apng(2, 1, false, &[g(0), g(0)], 1);
    let at = png.windows(4).position(|w| w == b"fcTL").unwrap() + 7;
    png[at] = 9; // first fcTL sequence number
    assert!(dec(&png, &Limits::default()).is_err());
}

#[test]
fn frame_count_limit_applies() {
    let g = || f(0, 0, 1, 1, 0, 0, vec![0; 4]);
    let png = apng(1, 1, false, &[g(), g(), g()], 1);
    let l = Limits {
        max_frames: 2,
        ..Limits::default()
    };
    assert!(dec(&png, &l).is_err());
    assert!(dec(&png, &Limits::default()).is_ok());
}

#[test]
fn total_byte_budget_applies_to_frames() {
    // 64x64 canvas = 16 KiB per frame; 3 frames plus two canvases need > 64 KiB.
    let g = || f(0, 0, 64, 64, 0, 0, vec![0; 64 * 64 * 4]);
    let png = apng(64, 64, false, &[g(), g(), g()], 1);
    let l = Limits {
        max_total_bytes: 64 << 10,
        ..Limits::default()
    };
    assert!(dec(&png, &l).is_err());
}
