//! Pixel buffers, compositing and gradients. Premultiplied RGBA8 throughout;
//! all arithmetic is integer.

use super::fixed::Q;
use super::geom::{Mat, p};
use crate::DecodeError;
use crate::limits::Budget;

/// `round(x / 255)` for `x` in `0..=65025`.
pub fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
}

/// A premultiplied RGBA8 canvas.
#[derive(Debug)]
pub struct Layer {
    pub w: usize,
    pub px: Vec<u8>,
}

impl Layer {
    pub fn new(w: usize, h: usize, budget: &mut Budget) -> Result<Layer, DecodeError> {
        let n = w
            .checked_mul(h)
            .and_then(|n| n.checked_mul(4))
            .ok_or(DecodeError::LimitExceeded("svg canvas"))?;
        Ok(Layer {
            w,
            px: budget.zeroed(n)?,
        })
    }

    pub fn bytes(&self) -> usize {
        self.px.len()
    }

    /// Source-over `src` (same size) at `opacity` (0..=255).
    pub fn over(&mut self, src: &Layer, opacity: u32) {
        for (d, s) in self
            .px
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(src.px.as_chunks::<4>().0)
        {
            let mut sv = [0u32; 4];
            for (o, c) in sv.iter_mut().zip(s) {
                *o = div255(u32::from(*c) * opacity);
            }
            let inv = 255 - sv[3].min(255);
            for (dc, sc) in d.iter_mut().zip(sv) {
                *dc = (sc + div255(u32::from(*dc) * inv)).min(255) as u8;
            }
        }
    }

    /// Multiply every pixel by `mask / 255`.
    pub fn mask(&mut self, mask: &[u8]) {
        for (d, m) in self.px.as_chunks_mut::<4>().0.iter_mut().zip(mask) {
            for c in d.iter_mut() {
                *c = div255(u32::from(*c) * u32::from(*m)) as u8;
            }
        }
    }

    /// Composite a straight-alpha colour over pixels `x0..` of row `y` with
    /// per-pixel `cov`erage.
    pub fn paint_solid(&mut self, y: usize, x0: usize, cov: &[u8], c: [u8; 4], alpha: u32) {
        let a1 = div255(u32::from(c[3]) * alpha);
        if a1 == 0 {
            return;
        }
        let start = (y * self.w + x0) * 4;
        let Some(row) = self.px.get_mut(start..start + cov.len() * 4) else {
            return;
        };
        for (d, cv) in row.as_chunks_mut::<4>().0.iter_mut().zip(cov) {
            let a = div255(a1 * u32::from(*cv));
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            for (k, dc) in d.iter_mut().enumerate() {
                let sc = if k == 3 {
                    a
                } else {
                    div255(u32::from(c.get(k).copied().unwrap_or(0)) * a)
                };
                *dc = (sc + div255(u32::from(*dc) * inv)).min(255) as u8;
            }
        }
    }

    pub fn paint_gradient(&mut self, y: usize, x0: usize, cov: &[u8], g: &Gradient, alpha: u32) {
        let start = (y * self.w + x0) * 4;
        let Some(row) = self.px.get_mut(start..start + cov.len() * 4) else {
            return;
        };
        for (i, (d, cv)) in row.as_chunks_mut::<4>().0.iter_mut().zip(cov).enumerate() {
            if *cv == 0 {
                continue;
            }
            let c = g.color_at((x0 + i) as i64, y as i64);
            let a = div255(div255(u32::from(c[3]) * alpha) * u32::from(*cv));
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            for (k, dc) in d.iter_mut().enumerate() {
                let sc = if k == 3 {
                    a
                } else {
                    div255(u32::from(c.get(k).copied().unwrap_or(0)) * a)
                };
                *dc = (sc + div255(u32::from(*dc) * inv)).min(255) as u8;
            }
        }
    }

    /// Straight-alpha RGBA8 bytes.
    pub fn unpremultiplied(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.px.len());
        for s in self.px.as_chunks::<4>().0 {
            let a = u32::from(s[3]);
            for c in s.iter().take(3) {
                let v = ((u32::from(*c) * 255) + (a / 2))
                    .checked_div(a)
                    .unwrap_or(0);
                out.push(v.min(255) as u8);
            }
            out.push(s[3]);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spread {
    Pad,
    Reflect,
    Repeat,
}

#[derive(Clone, Copy, Debug)]
pub enum Kind {
    Linear { x1: Q, y1: Q, x2: Q, y2: Q },
    Radial { cx: Q, cy: Q, r: Q },
}

/// A resolved gradient: `inv` maps device pixels to gradient space.
#[derive(Clone, Debug)]
pub struct Gradient {
    pub kind: Kind,
    pub stops: Vec<(Q, [u8; 4])>,
    pub spread: Spread,
    pub inv: Mat,
}

impl Gradient {
    /// The straight-alpha colour at the centre of device pixel `(x, y)`.
    pub fn color_at(&self, x: i64, y: i64) -> [u8; 4] {
        let dp = p(Q(x * 65536 + 32768), Q(y * 65536 + 32768));
        let g = self.inv.apply(dp);
        let t = match self.kind {
            Kind::Linear { x1, y1, x2, y2 } => {
                let (dx, dy) = (x2 - x1, y2 - y1);
                let len2 = (dx * dx) + (dy * dy);
                if len2.0 == 0 {
                    return self.stops.last().map_or([0; 4], |s| s.1);
                }
                (((g.x - x1) * dx) + ((g.y - y1) * dy)) / len2
            }
            Kind::Radial { cx, cy, r } => {
                if r.0 <= 0 {
                    return self.stops.last().map_or([0; 4], |s| s.1);
                }
                Q::hypot(g.x - cx, g.y - cy) / r
            }
        };
        let t = match self.spread {
            Spread::Pad => t,
            Spread::Repeat => Q(t.0.rem_euclid(65536)),
            Spread::Reflect => {
                let m = t.0.rem_euclid(131_072);
                Q(if m > 65536 { 131_072 - m } else { m })
            }
        };
        self.at(t)
    }

    fn at(&self, t: Q) -> [u8; 4] {
        let (Some(first), Some(last)) = (self.stops.first(), self.stops.last()) else {
            return [0; 4];
        };
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        for w in self.stops.windows(2) {
            let [a, b] = w else { continue };
            if t >= a.0 && t < b.0 {
                let span = b.0 - a.0;
                let f = if span.0 == 0 {
                    Q::ONE
                } else {
                    (t - a.0) / span
                };
                let mut out = [0u8; 4];
                for (k, o) in out.iter_mut().enumerate() {
                    let (ca, cb) = (
                        i64::from(a.1.get(k).copied().unwrap_or(0)),
                        i64::from(b.1.get(k).copied().unwrap_or(0)),
                    );
                    *o = (ca + (((cb - ca) * f.0 + (1 << 15)) >> 16)).clamp(0, 255) as u8;
                }
                return out;
            }
        }
        last.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;

    #[test]
    fn div255_is_exact_rounding() {
        for x in 0..=65025u32 {
            assert_eq!(div255(x), (x + 127) / 255, "{x}");
        }
    }

    #[test]
    fn solid_over_and_unpremultiply() {
        let mut b = Budget::new(&Limits::DEFAULT);
        let mut l = Layer::new(2, 1, &mut b).unwrap();
        l.paint_solid(0, 0, &[255, 128], [255, 0, 0, 255], 255);
        assert_eq!(&l.px[..4], &[255, 0, 0, 255]);
        assert_eq!(l.px[7], 128);
        let straight = l.unpremultiplied();
        assert_eq!(&straight[4..8], &[255, 0, 0, 128]);
        // Group composite at half opacity.
        let mut top = Layer::new(2, 1, &mut b).unwrap();
        top.over(&l, 128);
        assert_eq!(top.px[3], 128);
    }

    #[test]
    fn linear_gradient_interpolates() {
        let g = Gradient {
            kind: Kind::Linear {
                x1: Q::ZERO,
                y1: Q::ZERO,
                x2: Q::int(10),
                y2: Q::ZERO,
            },
            stops: vec![(Q::ZERO, [0, 0, 0, 255]), (Q::ONE, [200, 100, 0, 255])],
            spread: Spread::Pad,
            inv: Mat::IDENTITY,
        };
        assert_eq!(g.color_at(0, 0)[0], 10);
        assert_eq!(g.color_at(4, 0)[0], 90);
        assert_eq!(g.color_at(50, 0)[0], 200);
        assert_eq!(g.color_at(-5, 0)[0], 0);
        let r = Gradient {
            spread: Spread::Reflect,
            ..g.clone()
        };
        assert_eq!(r.color_at(14, 0)[0], 110);
    }
}
