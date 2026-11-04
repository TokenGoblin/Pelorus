//! JPEG tests. Fixtures under tests/fixtures/jpeg (see PROVENANCE.txt).
//!
//! Oracle: Pillow 12.3.0 (libjpeg-turbo). Each fixture has `<name>.ref.png`,
//! Pillow's decoded RGB, and the manifest holds the SHA-256 of Pillow's RGBA
//! plus the SHA-256 of OUR output (the golden: any change to our bytes fails).
//!
//! Tolerance against Pillow: at most 2 per channel (IDCT implementations round
//! differently). Measured: MAX_DEVIATION_MEASURED below, for every fixture.
//! Because the IDCT is libjpeg's "islow" algorithm, fancy upsampling and the
//! same fixed-point colour tables, the deviation is expected to be 0 and the
//! bound is a guard, not a licence.
#![cfg(test)]

mod common;
use common::*;
use px_image::{DecodeError, Decoded, Image, Limits, jpeg, png};

/// Largest per-channel difference to Pillow over all fixtures, as measured
/// when this file was written (asserted exactly so a drift is noticed).
const MAX_DEVIATION_MEASURED: u8 = 0;
const TOLERANCE: u8 = 2;

fn dec(d: &[u8], l: &Limits) -> Result<Decoded, DecodeError> {
    jpeg::decode(d, l).map(Decoded::Still)
}

struct Fx {
    name: String,
    width: u32,
    height: u32,
    pil_sha: String,
    own_sha: String,
    supported: bool,
    bytes: Vec<u8>,
}

fn dir() -> String {
    format!("{}/tests/fixtures/jpeg", env!("CARGO_MANIFEST_DIR"))
}

fn fixtures() -> Vec<Fx> {
    let m = std::fs::read_to_string(format!("{}/MANIFEST", dir())).unwrap();
    m.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Fx {
                name: f[0].to_string(),
                width: f[1].parse().unwrap(),
                height: f[2].parse().unwrap(),
                pil_sha: f[3].to_string(),
                own_sha: f[4].to_string(),
                supported: f[5] == "ok",
                bytes: std::fs::read(format!("{}/{}", dir(), f[0])).unwrap(),
            }
        })
        .collect()
}

fn reference(name: &str) -> Image {
    let b = std::fs::read(format!("{}/{}", dir(), name.replace(".jpg", ".ref.png"))).unwrap();
    match png::decode(&b, &Limits::default()).unwrap() {
        Decoded::Still(i) => i,
        _ => panic!("reference not a still"),
    }
}

#[test]
fn matches_pillow_within_tolerance_and_own_golden() {
    let bless = std::env::var("PX_IMAGE_BLESS").is_ok();
    let mut max_dev = 0u8;
    let mut checked = 0;
    for fx in fixtures().iter().filter(|f| f.supported) {
        let img = jpeg::decode(&fx.bytes, &Limits::default())
            .unwrap_or_else(|e| panic!("{}: {e}", fx.name));
        assert_eq!(
            (img.width, img.height),
            (fx.width, fx.height),
            "{}",
            fx.name
        );
        assert_eq!(img.rgba.len(), (fx.width * fx.height * 4) as usize);
        assert!(img.rgba.chunks(4).all(|p| p[3] == 255));
        let own = hex(&sha256(&img.rgba));
        if bless {
            println!("BLESS {} {}", fx.name, own);
            continue;
        }
        assert_eq!(
            own, fx.own_sha,
            "{}: golden hash of our own output",
            fx.name
        );
        let r = reference(&fx.name);
        assert_eq!((r.width, r.height), (fx.width, fx.height));
        let mut dev = 0u8;
        for (a, b) in img.rgba.chunks(4).zip(r.rgba.chunks(4)) {
            for c in 0..3 {
                dev = dev.max(a[c].abs_diff(b[c]));
            }
        }
        assert!(dev <= TOLERANCE, "{}: deviation {dev} from Pillow", fx.name);
        if own == fx.pil_sha {
            assert_eq!(dev, 0, "{}", fx.name);
        }
        max_dev = max_dev.max(dev);
        checked += 1;
    }
    if !bless {
        assert!(checked >= 28, "only {checked} fixtures");
        assert_eq!(max_dev, MAX_DEVIATION_MEASURED);
    }
}

#[test]
fn unsupported_variants_are_rejected_not_misdecoded() {
    let mut n = 0;
    for fx in fixtures().iter().filter(|f| !f.supported) {
        let r = jpeg::decode(&fx.bytes, &Limits::default());
        assert!(
            matches!(r, Err(DecodeError::Unsupported(_))),
            "{}: {r:?}",
            fx.name
        );
        n += 1;
    }
    assert_eq!(n, 4); // cmyk, arithmetic, 12-bit, lossless
}

#[test]
fn exif_orientation_is_ignored() {
    let fx = fixtures()
        .into_iter()
        .find(|f| f.name == "exif_orientation6.jpg")
        .unwrap();
    let img = jpeg::decode(&fx.bytes, &Limits::default()).unwrap();
    // Unrotated: dimensions are the stored 33x25, not 25x33.
    assert_eq!((img.width, img.height), (33, 25));
}

// ---- hand-built JPEG with hand-derived pixels

fn seg(marker: u8, body: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, marker];
    v.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    v.extend_from_slice(body);
    v
}

fn tiny_gray_jpeg(width: u16, scan: &[u8]) -> Vec<u8> {
    let mut v = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend([1u8; 64]);
    v.extend(seg(0xDB, &dqt));
    let mut sof = vec![8, 0, 8];
    sof.extend_from_slice(&width.to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]);
    v.extend(seg(0xC0, &sof));
    // DC table: '0'->cat 0, '10'->cat 6, '110'->cat 2, '111'->cat 5.
    let mut dc = vec![0x00, 1, 1, 2];
    dc.extend([0u8; 13]);
    dc.extend([0, 6, 2, 5]);
    v.extend(seg(0xC4, &dc));
    // AC table: '0' -> EOB.
    let mut ac = vec![0x10, 1];
    ac.extend([0u8; 15]);
    ac.push(0);
    v.extend(seg(0xC4, &ac));
    v.extend(seg(0xDA, &[1, 1, 0x00, 0, 63, 0]));
    v.extend_from_slice(scan);
    v.extend_from_slice(&[0xFF, 0xD9]);
    v
}

#[test]
fn hand_built_two_block_gray_jpeg() {
    // Block 1: DC diff +40 (cat 6: '10' + 101000), EOB '0'.
    // Block 2: DC diff -24 (cat 5: '111' + 00111), EOB '0'. Padding with 1s.
    // Bits: 10101000 01110011 10111111 = A8 73 BF.
    // DC 40 -> pass 1: 40<<2 = 160; pass 2: (160<<13 + 2^17) >> 18 = 5 -> 133.
    // DC 16 -> 64 -> (64<<13 + 2^17) >> 18 = 2 -> 130.
    let j = tiny_gray_jpeg(16, &[0xA8, 0x73, 0xBF]);
    let img = jpeg::decode(&j, &Limits::default()).unwrap();
    assert_eq!((img.width, img.height), (16, 8));
    for row in img.rgba.chunks(16 * 4) {
        for (x, p) in row.chunks(4).enumerate() {
            let want = if x < 8 { 133 } else { 130 };
            assert_eq!(p, [want, want, want, 255], "x={x}");
        }
    }
}

#[test]
fn entropy_data_that_ends_early_is_an_error() {
    let j = tiny_gray_jpeg(16, &[0xA8]);
    assert!(jpeg::decode(&j, &Limits::default()).is_err());
}

#[test]
fn huge_claimed_dimensions_are_rejected_before_allocating() {
    let l = Limits::default();
    for (w, h) in [(65535u16, 65535u16), (65535, 1000), (0, 8), (8, 0)] {
        let mut v = vec![0xFF, 0xD8];
        let mut sof = vec![8];
        sof.extend_from_slice(&h.to_be_bytes());
        sof.extend_from_slice(&w.to_be_bytes());
        sof.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
        v.extend(seg(0xC0, &sof));
        assert!(jpeg::decode(&v, &l).is_err(), "{w}x{h}");
    }
    // Pixel cap lifted, byte cap not: coefficient planes for 65535x65535 are refused.
    let l2 = Limits {
        max_pixels: u64::MAX,
        max_frames: 1,
        max_total_bytes: 1 << 20,
    };
    let mut v = vec![0xFF, 0xD8];
    let mut sof = vec![8, 0xFF, 0xFF, 0xFF, 0xFF];
    sof.extend_from_slice(&[3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
    v.extend(seg(0xC0, &sof));
    assert_eq!(
        jpeg::decode(&v, &l2),
        Err(DecodeError::LimitExceeded("bytes"))
    );
}

#[test]
fn too_many_scans_are_refused() {
    // 200 copies of the same scan header on a progressive frame.
    let mut v = vec![0xFF, 0xD8];
    let mut dqt = vec![0u8];
    dqt.extend([1u8; 64]);
    v.extend(seg(0xDB, &dqt));
    v.extend(seg(0xC2, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]));
    let mut dc = vec![0x00, 1];
    dc.extend([0u8; 15]);
    dc.push(0);
    v.extend(seg(0xC4, &dc));
    for _ in 0..200 {
        v.extend(seg(0xDA, &[1, 1, 0x00, 0, 0, 0]));
        v.push(0x00); // one DC symbol '0' = cat 0, 7 pad bits
    }
    v.extend_from_slice(&[0xFF, 0xD9]);
    assert_eq!(
        jpeg::decode(&v, &Limits::default()),
        Err(DecodeError::LimitExceeded("scans"))
    );
}

#[test]
fn hostile_truncation_and_corruption_on_every_fixture() {
    for fx in fixtures() {
        hostile_battery(&fx.name, &fx.bytes, &dec);
    }
}

#[test]
fn random_bytes_never_panic() {
    random_bytes_battery(&[0xFF, 0xD8, 0xFF], 0x5eed_0001_dead_beef, &dec);
    let base = std::fs::read(format!("{}/base_420.jpg", dir())).unwrap();
    // Random garbage after a valid header region.
    random_bytes_battery(&base[..200.min(base.len())], 0x0102_0304_0506_0708, &dec);
}

#[test]
fn deterministic() {
    for fx in fixtures().iter().filter(|f| f.supported) {
        let a = jpeg::decode(&fx.bytes, &Limits::default());
        assert_eq!(a, jpeg::decode(&fx.bytes, &Limits::default()));
    }
}
