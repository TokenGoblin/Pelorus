//! The pictures the render corpus' image pages use (`tests/render/pages/img/`),
//! generated here by the project's own encoders: `px_paint::png::encode` for the
//! PNGs, and a few lines of BMP and GIF writing below. Nothing external made them
//! (the JPEG, `photo.jpg`, is a copy of a `px-image` fixture; see
//! `tests/render/PROVENANCE`).
//!
//! `cargo test -p px-paint --test render_fixture_images` checks the committed
//! files byte for byte against what the generators below produce, and decodes
//! each with `px-image` to prove it is a picture. `PX_BLESS_FIXTURE_IMAGES=1`
//! rewrites them. Every pixel is an integer function of its coordinates, so the
//! files are the same on every platform.

use std::path::{Path, PathBuf};

type Rgba = [u8; 4];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/render/pages/img")
}

fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> Rgba) -> Vec<u8> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&f(x, y));
        }
    }
    v
}

const RED: Rgba = [220, 30, 30, 255];
const GREEN: Rgba = [30, 170, 60, 255];
const BLUE: Rgba = [40, 70, 220, 255];
const YELLOW: Rgba = [240, 200, 20, 255];
const BLACK: Rgba = [0, 0, 0, 255];
const WHITE: Rgba = [255, 255, 255, 255];

/// 8x8: four quadrants; a black pixel at the top-left corner and a white one at
/// (1, 1), so a flipped or shifted picture is visible.
fn quad8() -> Vec<u8> {
    let rgba = image(8, 8, |x, y| {
        if (x, y) == (0, 0) {
            BLACK
        } else if (x, y) == (1, 1) {
            WHITE
        } else {
            match (x / 4, y / 4) {
                (0, 0) => RED,
                (1, 0) => GREEN,
                (0, 1) => BLUE,
                _ => YELLOW,
            }
        }
    });
    px_paint::png::encode(8, 8, &rgba).unwrap()
}

/// 32x16: eight vertical stripes of four columns, a black row on top and a black
/// column on the left.
fn wide() -> Vec<u8> {
    let colors: [Rgba; 8] = [
        RED,
        [240, 130, 20, 255],
        YELLOW,
        GREEN,
        [20, 190, 190, 255],
        BLUE,
        [140, 50, 180, 255],
        [220, 40, 160, 255],
    ];
    let rgba = image(32, 16, |x, y| {
        if y == 0 || x == 0 {
            BLACK
        } else {
            colors[(x / 4) as usize]
        }
    });
    px_paint::png::encode(32, 16, &rgba).unwrap()
}

/// 16x16 with alpha: an orange fade, transparent at the top-left, opaque at the
/// bottom-right, with a fully transparent hole in the middle.
fn alpha() -> Vec<u8> {
    let rgba = image(16, 16, |x, y| {
        let hole = (6..10).contains(&x) && (6..10).contains(&y);
        let a = if hole { 0 } else { ((x + y) * 8 + 7).min(255) as u8 };
        [255, 128, 0, a]
    });
    px_paint::png::encode(16, 16, &rgba).unwrap()
}

/// 8x8 checker of 4x4 squares (teal and pale) with a red dot at (2, 2).
fn tile() -> Vec<u8> {
    let rgba = image(8, 8, |x, y| {
        if (x, y) == (2, 2) {
            RED
        } else if (x / 4 + y / 4) % 2 == 0 {
            [30, 130, 130, 255]
        } else {
            [225, 240, 240, 255]
        }
    });
    px_paint::png::encode(8, 8, &rgba).unwrap()
}

/// 4x4, sixteen different colours: nearest scaling shows each as a block.
fn pixel4() -> Vec<u8> {
    let rgba = image(4, 4, |x, y| {
        let i = y * 4 + x;
        [(i * 16) as u8, (255 - i * 16) as u8, ((i * 7) % 16 * 16) as u8, 255]
    });
    px_paint::png::encode(4, 4, &rgba).unwrap()
}

/// 16x16 24-bit BMP, bottom-up: red to blue left to right, green growing down.
fn bmp() -> Vec<u8> {
    let (w, h) = (16u32, 16u32);
    let row = (w * 3).next_multiple_of(4);
    let size = 54 + row * h;
    let mut v = Vec::new();
    v.extend_from_slice(b"BM");
    v.extend_from_slice(&size.to_le_bytes());
    v.extend_from_slice(&[0, 0, 0, 0]);
    v.extend_from_slice(&54u32.to_le_bytes());
    v.extend_from_slice(&40u32.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&24u16.to_le_bytes());
    v.extend_from_slice(&[0; 24]);
    for y in (0..h).rev() {
        let mut r = Vec::new();
        for x in 0..w {
            let red = (255 - x * 17) as u8;
            let blue = (x * 17) as u8;
            let green = (y * 17) as u8;
            r.extend_from_slice(&[blue, green, red]);
        }
        r.resize(row as usize, 0);
        v.extend_from_slice(&r);
    }
    v
}

/// 16x16 GIF, 4-colour palette, diagonal bands. LZW with a clear code after
/// every two pixels, so the code size never grows (valid and trivially
/// decodable).
fn gif() -> Vec<u8> {
    let (w, h) = (16u16, 16u16);
    let palette: [[u8; 3]; 4] = [[250, 250, 240], [220, 40, 40], [30, 30, 30], [40, 120, 220]];
    let mut v = Vec::new();
    v.extend_from_slice(b"GIF89a");
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&[0b1000_0001, 0, 0]);
    for c in palette {
        v.extend_from_slice(&c);
    }
    v.push(0x2C);
    v.extend_from_slice(&[0, 0, 0, 0]);
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.push(0);
    v.push(2); // minimum code size
    let mut codes: Vec<u8> = Vec::new();
    let pixels: Vec<u8> = (0..u32::from(h))
        .flat_map(|y| (0..u32::from(w)).map(move |x| ((x + y) / 4 % 4) as u8))
        .collect();
    for pair in pixels.chunks(2) {
        codes.push(4); // clear
        codes.extend_from_slice(pair);
    }
    codes.push(5); // end of information
    // Pack 3-bit codes LSB first.
    let mut bytes = Vec::new();
    let (mut acc, mut nbits) = (0u32, 0u32);
    for c in codes {
        acc |= u32::from(c) << nbits;
        nbits += 3;
        while nbits >= 8 {
            bytes.push((acc & 0xFF) as u8);
            acc >>= 8;
            nbits -= 8;
        }
    }
    if nbits > 0 {
        bytes.push(acc as u8);
    }
    for chunk in bytes.chunks(255) {
        v.push(chunk.len() as u8);
        v.extend_from_slice(chunk);
    }
    v.push(0);
    v.push(0x3B);
    v
}

fn photo() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../px-image/tests/fixtures/jpeg/base_420.jpg"),
    )
    .unwrap()
}

fn files() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("quad8.png", quad8()),
        ("wide.png", wide()),
        ("alpha.png", alpha()),
        ("tile.png", tile()),
        ("pixel4.png", pixel4()),
        ("pic.bmp", bmp()),
        ("pic.gif", gif()),
        ("photo.jpg", photo()),
    ]
}

#[test]
fn the_committed_pictures_are_what_the_generators_write() {
    let bless = std::env::var_os("PX_BLESS_FIXTURE_IMAGES").is_some();
    let d = dir();
    if bless {
        std::fs::create_dir_all(&d).unwrap();
    }
    for (name, bytes) in files() {
        let path = d.join(name);
        if bless {
            std::fs::write(&path, &bytes).unwrap();
        }
        let on_disk = std::fs::read(&path).unwrap_or_default();
        assert!(
            on_disk == bytes,
            "{name} differs from its generator (PX_BLESS_FIXTURE_IMAGES=1 rewrites it)"
        );
    }
}

#[test]
fn every_picture_decodes_with_px_image() {
    for (name, bytes) in files() {
        let img = px_image::decode(&bytes, &px_image::Limits::DEFAULT)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let first = img.first_image().unwrap();
        assert!(first.width >= 4 && first.height >= 4, "{name}");
    }
}

#[test]
fn the_gif_and_bmp_have_the_pixels_the_generators_mean() {
    let g = px_image::decode(&gif(), &px_image::Limits::DEFAULT).unwrap();
    let g = g.first_image().unwrap();
    assert_eq!((g.width, g.height), (16, 16));
    assert_eq!(&g.rgba[0..4], &[250, 250, 240, 255]);
    // (4, 0): band index 1 -> red.
    assert_eq!(&g.rgba[16..20], &[220, 40, 40, 255]);
    let b = px_image::decode(&bmp(), &px_image::Limits::DEFAULT).unwrap();
    let b = b.first_image().unwrap();
    // Top-left: x = 0 (red 255, blue 0), y = 0 (green 0).
    assert_eq!(&b.rgba[0..4], &[255, 0, 0, 255]);
    // Bottom-right: x = 15, y = 15.
    let last = (15 * 16 + 15) * 4;
    assert_eq!(&b.rgba[last..last + 4], &[0, 255, 255, 255]);
}
