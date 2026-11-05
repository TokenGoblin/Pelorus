//! Deterministic image scaling for [`crate::display_list::Item::Image`].
//!
//! Integer arithmetic only, so the same image scaled to the same destination
//! gives the same bytes on every platform.
//!
//! The destination is a snapped pixel rectangle `dw` by `dh` (see
//! [`crate::raster`]); the source is `sw` by `sh`. Destination pixel `i` has its
//! centre at source coordinate `(2i + 1) * sw / (2 * dw)`.
//!
//! - [`ImageFilter::Nearest`]: the source pixel containing that centre,
//!   `floor((2i + 1) * sw / (2 * dw))`. Crisp; what `image-rendering: pixelated`
//!   means and what the pixel-art fixtures use.
//! - [`ImageFilter::Smooth`]: per axis, when the destination is at least as large
//!   as the source, linear interpolation between the two source pixels around the
//!   centre, weights in 1/256 (an exact copy at 1:1); when it is smaller, a box
//!   filter: every source pixel overlapping the destination pixel's footprint,
//!   weighted by the overlap length (exact integer overlaps, so the weights of
//!   one destination pixel sum to `sw`). The two axes are independent, so an
//!   image can be magnified in one and minified in the other.
//!
//! Colour is averaged weighted by alpha (so a transparent pixel contributes no
//! colour), as `sum(w * a * c) / sum(w * a)` rounded half up; alpha is the plain
//! weighted mean, rounded half up. The result is non-premultiplied RGBA, which
//! the rasterizer then composites source-over like any other colour.

use crate::display_list::{Color, ImageData, ImageFilter};

/// One source pixel's contribution to a destination pixel along one axis.
#[derive(Clone, Copy, Debug)]
struct Tap {
    src: u32,
    weight: u64,
}

/// The taps of every destination index in `from..to`, and the weight total each
/// destination pixel's taps sum to.
struct Axis {
    taps: Vec<Vec<Tap>>,
    total: u64,
}

fn clamp_src(i: i64, n: u32) -> u32 {
    i.clamp(0, i64::from(n) - 1) as u32
}

/// Taps for the axis. `n_src` and `n_dst` are at least 1.
fn axis(n_src: u32, n_dst: u32, from: u32, to: u32, filter: ImageFilter) -> Axis {
    let (s, d) = (i64::from(n_src), i64::from(n_dst));
    let mut taps = Vec::with_capacity((to - from) as usize);
    match filter {
        ImageFilter::Nearest => {
            for i in from..to {
                let src = ((2 * i64::from(i) + 1) * s) / (2 * d);
                taps.push(vec![Tap {
                    src: clamp_src(src, n_src),
                    weight: 1,
                }]);
            }
            Axis { taps, total: 1 }
        }
        ImageFilter::Smooth if n_dst >= n_src => {
            for i in from..to {
                let fx = ((2 * i64::from(i) + 1) * s * 128).div_euclid(d) - 128;
                let i0 = fx.div_euclid(256);
                let frac = fx.rem_euclid(256) as u64;
                let mut v = Vec::with_capacity(2);
                if frac < 256 {
                    v.push(Tap {
                        src: clamp_src(i0, n_src),
                        weight: 256 - frac,
                    });
                }
                if frac > 0 {
                    v.push(Tap {
                        src: clamp_src(i0 + 1, n_src),
                        weight: frac,
                    });
                }
                taps.push(v);
            }
            Axis { taps, total: 256 }
        }
        ImageFilter::Smooth => {
            for i in from..to {
                // The footprint in units of 1/d source pixels: [i*s, (i+1)*s).
                let (a, b) = (i64::from(i) * s, (i64::from(i) + 1) * s);
                let k0 = a / d;
                let k1 = (b - 1) / d;
                let mut v = Vec::new();
                for k in k0..=k1.min(s - 1) {
                    let lo = a.max(k * d);
                    let hi = b.min((k + 1) * d);
                    if hi > lo {
                        v.push(Tap {
                            src: k as u32,
                            weight: (hi - lo) as u64,
                        });
                    }
                }
                taps.push(v);
            }
            Axis {
                taps,
                total: n_src as u64,
            }
        }
    }
}

/// Scaled samples of `img` for destination pixels `x0..x1` by `y0..y1` of a
/// `dw` by `dh` destination (indices relative to its top-left), row-major.
/// Empty when anything is degenerate.
#[must_use]
pub fn scale(
    img: &ImageData,
    dw: u32,
    dh: u32,
    (x0, x1): (u32, u32),
    (y0, y1): (u32, u32),
    filter: ImageFilter,
) -> Vec<Color> {
    let (sw, sh) = (img.width, img.height);
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 || x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    if img.rgba.len() < (sw as usize) * (sh as usize) * 4 {
        return Vec::new();
    }
    let ax = axis(sw, dw, x0, x1, filter);
    let ay = axis(sh, dh, y0, y1, filter);
    let total = ax.total * ay.total;
    let px = |x: u32, y: u32| -> [u64; 4] {
        let i = (y as usize * sw as usize + x as usize) * 4;
        match img.rgba.get(i..i + 4) {
            Some(&[r, g, b, a]) => [u64::from(r), u64::from(g), u64::from(b), u64::from(a)],
            _ => [0; 4],
        }
    };
    let mut out = Vec::with_capacity(((x1 - x0) as usize) * ((y1 - y0) as usize));
    for ty in &ay.taps {
        for tx in &ax.taps {
            // Single tap each way: a straight copy (the Nearest case and 1:1).
            if let ([a], [b]) = (ty.as_slice(), tx.as_slice())
                && a.weight == ay.total
                && b.weight == ax.total
            {
                let p = px(b.src, a.src);
                out.push(Color {
                    r: p[0] as u8,
                    g: p[1] as u8,
                    b: p[2] as u8,
                    a: p[3] as u8,
                });
                continue;
            }
            let (mut sa, mut sr, mut sg, mut sb) = (0u64, 0u64, 0u64, 0u64);
            for wy in ty {
                for wx in tx {
                    let w = wy.weight * wx.weight;
                    let p = px(wx.src, wy.src);
                    let wa = w * p[3];
                    sa += wa;
                    sr += wa * p[0];
                    sg += wa * p[1];
                    sb += wa * p[2];
                }
            }
            if sa == 0 {
                out.push(Color::TRANSPARENT);
                continue;
            }
            let ch = |s: u64| ((s + sa / 2) / sa).min(255) as u8;
            out.push(Color {
                r: ch(sr),
                g: ch(sg),
                b: ch(sb),
                a: ((sa + total / 2) / total).min(255) as u8,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> ImageData {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&f(x, y));
            }
        }
        ImageData {
            width: w,
            height: h,
            rgba,
        }
    }

    fn c(p: [u8; 4]) -> Color {
        Color {
            r: p[0],
            g: p[1],
            b: p[2],
            a: p[3],
        }
    }

    #[test]
    fn nearest_doubles_each_pixel() {
        let i = img(2, 1, |x, _| [x as u8 * 100, 0, 0, 255]);
        let out = scale(&i, 4, 2, (0, 4), (0, 2), ImageFilter::Nearest);
        let want: Vec<Color> = [0u8, 0, 100, 100, 0, 0, 100, 100]
            .iter()
            .map(|&r| c([r, 0, 0, 255]))
            .collect();
        assert_eq!(out, want);
    }

    #[test]
    fn nearest_minifies_by_centre_sampling() {
        // 4 -> 2: centres at 1 and 3 (of 4).
        let i = img(4, 1, |x, _| [x as u8, 0, 0, 255]);
        let out = scale(&i, 2, 1, (0, 2), (0, 1), ImageFilter::Nearest);
        assert_eq!(out, vec![c([1, 0, 0, 255]), c([3, 0, 0, 255])]);
    }

    #[test]
    fn smooth_at_one_to_one_is_a_copy() {
        let i = img(3, 2, |x, y| [x as u8 * 50, y as u8 * 90, 7, 255]);
        let out = scale(&i, 3, 2, (0, 3), (0, 2), ImageFilter::Smooth);
        let want: Vec<Color> = (0..2)
            .flat_map(|y| (0..3).map(move |x| c([x as u8 * 50, y as u8 * 90, 7, 255])))
            .collect();
        assert_eq!(out, want);
    }

    #[test]
    fn bilinear_blends_neighbours_and_clamps_at_the_edges() {
        // 2 -> 4: centres at source 0.25, 0.75, 1.25, 1.75 (minus the half pixel).
        let i = img(2, 1, |x, _| [x as u8 * 200, 0, 0, 255]);
        let out = scale(&i, 4, 1, (0, 4), (0, 1), ImageFilter::Smooth);
        let reds: Vec<u8> = out.iter().map(|p| p.r).collect();
        // fx = ((2i+1)*2*128)/4 - 128 = 64*(2i+1) - 128 = -64, 64, 192, 320:
        // clamp(-1)=0 -> 0; 0.25 -> 50; 0.75 -> 150; 1.25 clamps to 1 -> 200.
        assert_eq!(reds, vec![0, 50, 150, 200]);
    }

    #[test]
    fn box_filter_averages_the_footprint_exactly() {
        let i = img(4, 1, |x, _| [x as u8 * 10, 0, 0, 255]);
        let out = scale(&i, 2, 1, (0, 2), (0, 1), ImageFilter::Smooth);
        // (0+10)/2 = 5, (20+30)/2 = 25.
        assert_eq!(out, vec![c([5, 0, 0, 255]), c([25, 0, 0, 255])]);
        // 3 -> 2 weights overlap lengths 2:1 then 1:2.
        let j = img(3, 1, |x, _| [x as u8 * 30, 0, 0, 255]);
        let out = scale(&j, 2, 1, (0, 2), (0, 1), ImageFilter::Smooth);
        // (2*0 + 1*30)/3 = 10; (1*30 + 2*60)/3 = 50.
        assert_eq!(out, vec![c([10, 0, 0, 255]), c([50, 0, 0, 255])]);
    }

    #[test]
    fn transparent_pixels_carry_no_colour_into_a_blend() {
        let i = img(2, 1, |x, _| {
            if x == 0 {
                [255, 0, 0, 0]
            } else {
                [0, 0, 255, 255]
            }
        });
        let out = scale(&i, 1, 1, (0, 1), (0, 1), ImageFilter::Smooth);
        assert_eq!(out, vec![c([0, 0, 255, 128])]);
    }

    #[test]
    fn a_window_of_the_destination_equals_the_same_pixels_of_the_whole() {
        let i = img(5, 7, |x, y| {
            [(x * 40) as u8, (y * 30) as u8, (x * y) as u8, 255]
        });
        for filter in [ImageFilter::Nearest, ImageFilter::Smooth] {
            let whole = scale(&i, 13, 4, (0, 13), (0, 4), filter);
            let part = scale(&i, 13, 4, (3, 9), (1, 3), filter);
            for (j, p) in part.iter().enumerate() {
                let (x, y) = (3 + j % 6, 1 + j / 6);
                assert_eq!(*p, whole[y * 13 + x], "{filter:?} {x},{y}");
            }
        }
    }

    #[test]
    fn degenerate_inputs_give_nothing() {
        let i = img(2, 2, |_, _| [1, 2, 3, 4]);
        assert!(scale(&i, 0, 2, (0, 0), (0, 2), ImageFilter::Nearest).is_empty());
        let bad = ImageData {
            width: 4,
            height: 4,
            rgba: vec![0; 3],
        };
        assert!(scale(&bad, 2, 2, (0, 2), (0, 2), ImageFilter::Smooth).is_empty());
    }
}
