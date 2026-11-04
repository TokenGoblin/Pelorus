//! The public entry point: sniffing, dispatch, and crate-wide properties.
#![cfg(test)]

mod common;
use common::*;
use px_image::{DecodeError, Decoded, Format, Limits, decode, sniff};

fn all_fixture_files() -> Vec<(String, Vec<u8>)> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
    let mut v = Vec::new();
    for sub in ["png", "gif", "jpeg", "bmp"] {
        for e in std::fs::read_dir(format!("{root}/{sub}")).unwrap() {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().to_string();
            if [".png", ".gif", ".jpg", ".bmp", ".ico"]
                .iter()
                .any(|x| name.ends_with(x))
                && !name.ends_with(".ref.png")
            {
                v.push((format!("{sub}/{name}"), std::fs::read(&p).unwrap()));
            }
        }
    }
    v.sort();
    v
}

#[test]
fn sniff_by_magic() {
    assert_eq!(
        sniff(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]),
        Some(Format::Png)
    );
    assert_eq!(sniff(b"GIF89a.."), Some(Format::Gif));
    assert_eq!(sniff(b"GIF87a"), Some(Format::Gif));
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(Format::Jpeg));
    assert_eq!(sniff(b"BM\0\0"), Some(Format::Bmp));
    assert_eq!(sniff(&[0, 0, 1, 0, 1, 0]), Some(Format::Ico));
    assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Some(Format::Webp));
    assert_eq!(sniff(b"RIFF\x10\0\0\0WAVEfmt "), None);
    assert_eq!(sniff(&[0, 0, 2, 0]), None); // cursor
    assert_eq!(sniff(b""), None);
    assert_eq!(sniff(b"GIF8"), None);
}

#[test]
fn every_fixture_decodes_through_the_front_door_or_is_a_known_unsupported() {
    let mut ok = 0;
    for (name, bytes) in all_fixture_files() {
        let r = decode(&bytes, &Limits::default());
        let expected_unsupported = name.contains("cmyk")
            || name.contains("arith")
            || name.contains("twelve")
            || name.contains("lossless");
        match r {
            Ok(_) => {
                assert!(!expected_unsupported, "{name}");
                ok += 1;
            }
            Err(DecodeError::Unsupported(_)) => assert!(expected_unsupported, "{name}"),
            Err(e) => panic!("{name}: {e}"),
        }
    }
    assert!(ok > 100, "decoded {ok}");
}

#[test]
fn webp_and_unknown_are_refused() {
    assert_eq!(
        decode(b"RIFF\x10\0\0\0WEBPVP8 ", &Limits::default()),
        Err(DecodeError::Unsupported("WebP"))
    );
    assert_eq!(
        decode(b"hello world", &Limits::default()),
        Err(DecodeError::UnknownFormat)
    );
    assert_eq!(
        decode(b"", &Limits::default()),
        Err(DecodeError::UnknownFormat)
    );
}

#[test]
fn front_door_random_bytes_with_every_magic_never_panic() {
    let prefixes: [&[u8]; 6] = [
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A],
        b"GIF89a",
        &[0xFF, 0xD8, 0xFF],
        b"BM",
        &[0, 0, 1, 0],
        b"RIFF\0\0\0\0WEBP",
    ];
    for (i, p) in prefixes.iter().enumerate() {
        random_bytes_battery(p, 0x9e37_79b9_7f4a_7c15 ^ (i as u64), &|d, l| decode(d, l));
    }
}

#[test]
fn output_buffers_are_exact_and_within_limits_for_every_fixture() {
    let l = Limits::default();
    for (name, bytes) in all_fixture_files() {
        if let Ok(d) = decode(&bytes, &l) {
            check_result(&Ok(d), &l);
            let _ = name;
        }
    }
}

#[test]
fn limits_apply_through_the_front_door() {
    let tight = Limits {
        max_pixels: 16,
        max_frames: 1,
        max_total_bytes: 1 << 20,
    };
    let mut refused = 0;
    for (name, bytes) in all_fixture_files() {
        // Every fixture is larger than 16 pixels except a handful of tiny ones.
        match decode(&bytes, &tight) {
            Ok(d) => {
                let (w, h, n) = dims(&d);
                assert!(u64::from(w) * u64::from(h) <= 16 && n <= 1, "{name}");
            }
            Err(_) => refused += 1,
        }
    }
    assert!(refused > 100);
}

/// Determinism is a design rule: no floating point in any decode path.
#[test]
fn no_floating_point_in_the_sources() {
    let sources = [
        include_str!("../src/lib.rs"),
        include_str!("../src/limits.rs"),
        include_str!("../src/inflate.rs"),
        include_str!("../src/png.rs"),
        include_str!("../src/gif.rs"),
        include_str!("../src/jpeg.rs"),
        include_str!("../src/bmp.rs"),
        include_str!("../src/ico.rs"),
    ];
    let needles = [
        concat!("f", "32"),
        concat!("f", "64"),
        concat!(".", "sqrt("),
        concat!(".", "cos("),
    ];
    for (i, s) in sources.iter().enumerate() {
        for n in needles {
            assert!(!s.contains(n), "source #{i} mentions {n}");
        }
    }
}

#[test]
fn still_and_animated_results_are_classified() {
    let gif = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/gif/anim_disposal0.gif"
    ))
    .unwrap();
    assert!(matches!(
        decode(&gif, &Limits::default()),
        Ok(Decoded::Animated(_))
    ));
    let png = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/png/rgb8.png"
    ))
    .unwrap();
    assert!(matches!(
        decode(&png, &Limits::default()),
        Ok(Decoded::Still(_))
    ));
}
