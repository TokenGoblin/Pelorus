//! Path data: parsing, shape-to-path conversion and flattening to polylines.

use super::fixed::{Q, TWO_PI, atan2, deg_to_rad, sincos};
use super::geom::{P, p};
use super::values::{number, skip_sep};
use crate::DecodeError;

/// Most segments read from one path (the rest of the data is ignored).
pub const MAX_SEGS: usize = 100_000;

/// An elliptical arc in centre form (SVG 1.1 appendix F.6).
#[derive(Clone, Copy, Debug)]
pub struct Arc {
    pub cx: Q,
    pub cy: Q,
    pub rx: Q,
    pub ry: Q,
    pub cos: Q,
    pub sin: Q,
    /// Start angle and signed sweep, Q30 radians.
    pub t1: i64,
    pub dt: i64,
    pub end: P,
}

#[derive(Clone, Copy, Debug)]
pub enum Seg {
    M(P),
    L(P),
    C(P, P, P),
    A(Arc),
    Z,
}

/// A flattened subpath.
#[derive(Clone, Debug, Default)]
pub struct Poly {
    pub pts: Vec<P>,
    pub closed: bool,
}

/// Arc steps for a full turn of a circle of radius `r_px` device pixels.
pub fn steps_per_turn(r_px: Q) -> i64 {
    let r = r_px.round().clamp(0, 1 << 20) as u64;
    (8 + (6 * r.isqrt())).clamp(8, 360) as i64
}

fn make_arc(p0: P, rx: Q, ry: Q, rot: Q, large: bool, sweep: bool, p1: P) -> Seg {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if p0 == p1 || rx.0 == 0 || ry.0 == 0 {
        return Seg::L(p1);
    }
    let (s, c) = sincos(deg_to_rad(rot));
    let dx2 = (p0.x - p1.x).half();
    let dy2 = (p0.y - p1.y).half();
    let x1p = (c * dx2) + (s * dy2);
    let y1p = (c * dy2) - (s * dx2);
    let mut u = x1p / rx;
    let mut v = y1p / ry;
    let lam = (u * u) + (v * v);
    if lam > Q::ONE {
        let k = lam.sqrt();
        rx = rx * k;
        ry = ry * k;
        u = u / k;
        v = v / k;
    }
    let uv = (u * u) + (v * v);
    let coef2 = if uv.0 == 0 {
        Q::ZERO
    } else {
        let r = (Q::ONE - uv) / uv;
        if r.is_neg() { Q::ZERO } else { r }
    };
    let mut coef = coef2.sqrt();
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * rx * v;
    let cyp = -(coef * ry * u);
    let cx = (c * cxp) - (s * cyp) + (p0.x + p1.x).half();
    let cy = (s * cxp) + (c * cyp) + (p0.y + p1.y).half();
    let (ax, ay) = (u - (cxp / rx), v - (cyp / ry));
    let (bx, by) = (-u - (cxp / rx), -v - (cyp / ry));
    let t1 = atan2(ay, ax);
    let mut dt = atan2(by, bx) - t1;
    if !sweep && dt > 0 {
        dt -= TWO_PI;
    } else if sweep && dt < 0 {
        dt += TWO_PI;
    }
    Seg::A(Arc {
        cx,
        cy,
        rx,
        ry,
        cos: c,
        sin: s,
        t1,
        dt,
        end: p1,
    })
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn num(&mut self) -> Option<Q> {
        self.i = skip_sep(self.b, self.i);
        let (v, j) = number(self.b, self.i)?;
        self.i = j;
        Some(v)
    }
    fn flag(&mut self) -> Option<bool> {
        self.i = skip_sep(self.b, self.i);
        let f = match self.b.get(self.i)? {
            b'0' => false,
            b'1' => true,
            _ => return None,
        };
        self.i += 1;
        Some(f)
    }
}

/// Parse path data. On an error the segments read so far are returned, as the
/// SVG error-handling rules require.
pub fn parse_path(d: &str) -> Vec<Seg> {
    let mut segs: Vec<Seg> = Vec::new();
    let mut r = Reader {
        b: d.as_bytes(),
        i: 0,
    };
    let mut cur = P::default();
    let mut start = P::default();
    let mut cmd = 0u8;
    // The previous control point and whether it was a cubic's.
    let mut ctrl: Option<(P, bool)> = None;
    loop {
        r.i = skip_sep(r.b, r.i);
        let Some(&c) = r.b.get(r.i) else { break };
        if c.is_ascii_alphabetic() {
            cmd = c;
            r.i += 1;
            if segs.is_empty() && !matches!(cmd, b'M' | b'm') {
                break;
            }
            if matches!(cmd, b'Z' | b'z') {
                segs.push(Seg::Z);
                cur = start;
                ctrl = None;
                if segs.len() >= MAX_SEGS {
                    break;
                }
                continue;
            }
        } else if cmd == 0 || matches!(cmd, b'Z' | b'z') {
            break;
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { cur } else { P::default() };
        let ok = (|| -> Option<()> {
            match cmd.to_ascii_uppercase() {
                b'M' => {
                    let pt = p(r.num()?, r.num()?).add(base);
                    segs.push(Seg::M(pt));
                    cur = pt;
                    start = pt;
                    cmd = if rel { b'l' } else { b'L' };
                    ctrl = None;
                }
                b'L' => {
                    let pt = p(r.num()?, r.num()?).add(base);
                    segs.push(Seg::L(pt));
                    cur = pt;
                    ctrl = None;
                }
                b'H' => {
                    let x = r.num()?;
                    cur = p(if rel { cur.x + x } else { x }, cur.y);
                    segs.push(Seg::L(cur));
                    ctrl = None;
                }
                b'V' => {
                    let y = r.num()?;
                    cur = p(cur.x, if rel { cur.y + y } else { y });
                    segs.push(Seg::L(cur));
                    ctrl = None;
                }
                b'C' => {
                    let c1 = p(r.num()?, r.num()?).add(base);
                    let c2 = p(r.num()?, r.num()?).add(base);
                    let e = p(r.num()?, r.num()?).add(base);
                    segs.push(Seg::C(c1, c2, e));
                    cur = e;
                    ctrl = Some((c2, true));
                }
                b'S' => {
                    let c2 = p(r.num()?, r.num()?).add(base);
                    let e = p(r.num()?, r.num()?).add(base);
                    let c1 = match ctrl {
                        Some((q, true)) => cur.add(cur.sub(q)),
                        _ => cur,
                    };
                    segs.push(Seg::C(c1, c2, e));
                    cur = e;
                    ctrl = Some((c2, true));
                }
                b'Q' => {
                    let q = p(r.num()?, r.num()?).add(base);
                    let e = p(r.num()?, r.num()?).add(base);
                    segs.push(quad(cur, q, e));
                    cur = e;
                    ctrl = Some((q, false));
                }
                b'T' => {
                    let e = p(r.num()?, r.num()?).add(base);
                    let q = match ctrl {
                        Some((q, false)) => cur.add(cur.sub(q)),
                        _ => cur,
                    };
                    segs.push(quad(cur, q, e));
                    cur = e;
                    ctrl = Some((q, false));
                }
                b'A' => {
                    let rx = r.num()?;
                    let ry = r.num()?;
                    let rot = r.num()?;
                    let large = r.flag()?;
                    let sweep = r.flag()?;
                    let e = p(r.num()?, r.num()?).add(base);
                    segs.push(make_arc(cur, rx, ry, rot, large, sweep, e));
                    cur = e;
                    ctrl = None;
                }
                _ => return None,
            }
            Some(())
        })();
        if ok.is_none() || segs.len() >= MAX_SEGS {
            break;
        }
    }
    segs
}

fn quad(p0: P, q: P, e: P) -> Seg {
    let two_thirds = Q::int(2).scale(1, 3);
    Seg::C(
        p0.add(q.sub(p0).scale(two_thirds)),
        e.add(q.sub(e).scale(two_thirds)),
        e,
    )
}

/// Bezier control-point offset for a quarter ellipse (0.5522847).
const KAPPA: Q = Q(36_195);

/// A rectangle, rounded when `rx` and `ry` are positive.
pub fn rect_segs(x: Q, y: Q, w: Q, h: Q, rx: Q, ry: Q) -> Vec<Seg> {
    let (x2, y2) = (x + w, y + h);
    if rx.0 <= 0 || ry.0 <= 0 {
        return vec![
            Seg::M(p(x, y)),
            Seg::L(p(x2, y)),
            Seg::L(p(x2, y2)),
            Seg::L(p(x, y2)),
            Seg::Z,
        ];
    }
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Seg::M(p(x + rx, y)),
        Seg::L(p(x2 - rx, y)),
        Seg::C(p(x2 - rx + kx, y), p(x2, y + ry - ky), p(x2, y + ry)),
        Seg::L(p(x2, y2 - ry)),
        Seg::C(p(x2, y2 - ry + ky), p(x2 - rx + kx, y2), p(x2 - rx, y2)),
        Seg::L(p(x + rx, y2)),
        Seg::C(p(x + rx - kx, y2), p(x, y2 - ry + ky), p(x, y2 - ry)),
        Seg::L(p(x, y + ry)),
        Seg::C(p(x, y + ry - ky), p(x + rx - kx, y), p(x + rx, y)),
        Seg::Z,
    ]
}

pub fn ellipse_segs(cx: Q, cy: Q, rx: Q, ry: Q) -> Vec<Seg> {
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    vec![
        Seg::M(p(cx + rx, cy)),
        Seg::C(p(cx + rx, cy + ky), p(cx + kx, cy + ry), p(cx, cy + ry)),
        Seg::C(p(cx - kx, cy + ry), p(cx - rx, cy + ky), p(cx - rx, cy)),
        Seg::C(p(cx - rx, cy - ky), p(cx - kx, cy - ry), p(cx, cy - ry)),
        Seg::C(p(cx + kx, cy - ry), p(cx + rx, cy - ky), p(cx + rx, cy)),
        Seg::Z,
    ]
}

/// Flatten to polylines. `scale` is device pixels per user unit (an upper
/// bound is fine); it only chooses how finely curves are cut. At most
/// `max_points` points in all, else a limit error.
pub fn flatten(segs: &[Seg], scale: Q, max_points: usize) -> Result<Vec<Poly>, DecodeError> {
    let mut out: Vec<Poly> = Vec::new();
    let mut total = 0usize;
    let mut cur = P::default();
    let mut start = P::default();
    let mut open = false;
    let over = || DecodeError::LimitExceeded("svg path points");
    for seg in segs {
        if let Seg::M(pt) = seg {
            out.push(Poly {
                pts: vec![*pt],
                closed: false,
            });
            total += 1;
            cur = *pt;
            start = *pt;
            open = true;
            continue;
        }
        if let Seg::Z = seg {
            if let Some(last) = out.last_mut().filter(|_| open) {
                last.closed = true;
            }
            cur = start;
            open = false;
            continue;
        }
        if !open {
            out.push(Poly {
                pts: vec![cur],
                closed: false,
            });
            total += 1;
            start = cur;
            open = true;
        }
        let Some(poly) = out.last_mut() else { continue };
        match seg {
            Seg::L(pt) => {
                poly.pts.push(*pt);
                total += 1;
                cur = *pt;
            }
            Seg::C(c1, c2, e) => {
                let l = c1.sub(cur).len() + c2.sub(*c1).len() + e.sub(*c2).len();
                let lpx = (l * scale).round().clamp(0, 1 << 24) as u64;
                let n = (2 + (lpx * 8).isqrt()).min(200) as i64;
                for k in 1..n {
                    let t = Q::ratio(k, n);
                    let mt = Q::ONE - t;
                    let w0 = mt * mt * mt;
                    let w1 = Q::int(3) * mt * mt * t;
                    let w2 = Q::int(3) * mt * t * t;
                    let w3 = t * t * t;
                    poly.pts.push(p(
                        (w0 * cur.x) + (w1 * c1.x) + (w2 * c2.x) + (w3 * e.x),
                        (w0 * cur.y) + (w1 * c1.y) + (w2 * c2.y) + (w3 * e.y),
                    ));
                }
                poly.pts.push(*e);
                total += n as usize;
                cur = *e;
            }
            Seg::A(a) => {
                let r = if a.rx > a.ry { a.rx } else { a.ry };
                let per = steps_per_turn(r * scale);
                let n = ((a.dt.abs() * per) + TWO_PI - 1) / TWO_PI;
                let n = n.clamp(1, 400);
                for k in 1..n {
                    let th = a.t1 + ((a.dt * k) / n);
                    let (s, c) = sincos(th);
                    let (ex, ey) = (a.rx * c, a.ry * s);
                    poly.pts.push(p(
                        a.cx + (a.cos * ex) - (a.sin * ey),
                        a.cy + (a.sin * ex) + (a.cos * ey),
                    ));
                }
                poly.pts.push(a.end);
                total += n as usize;
                cur = a.end;
            }
            Seg::M(_) | Seg::Z => {}
        }
        if total > max_points {
            return Err(over());
        }
    }
    if total > max_points {
        return Err(over());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pts(d: &str) -> Vec<Poly> {
        flatten(&parse_path(d), Q::ONE, 1_000_000).unwrap()
    }

    #[test]
    fn lines_and_close() {
        let v = pts("M10 10 h20 v20 H10 z");
        assert_eq!(v.len(), 1);
        assert!(v[0].closed);
        assert_eq!(v[0].pts.len(), 4);
        assert_eq!(v[0].pts[2], p(Q::int(30), Q::int(30)));
        // Implicit lineto after moveto, relative.
        let v = pts("m1 1 2 2 3 3");
        assert_eq!(v[0].pts.last().copied(), Some(p(Q::int(6), Q::int(6))));
    }

    #[test]
    fn stops_at_the_first_error_keeping_what_came_before() {
        let v = pts("M0 0 L10 10 L oops L50 50");
        assert_eq!(v[0].pts.len(), 2);
        assert!(parse_path("L1 1").is_empty());
        assert!(parse_path("").is_empty());
        let v = pts("M0 0 z 5 5");
        assert_eq!(v.len(), 1);
    }

    #[test]
    fn arc_flags_may_be_packed() {
        let v = pts("M0 0 a5 5 0 1110 0");
        assert_eq!(v[0].pts.last().copied(), Some(p(Q::int(10), Q::ZERO)));
    }

    #[test]
    fn half_circle_arc_passes_through_the_apex() {
        let v = pts("M0 0 A5 5 0 0 1 10 0");
        let top = v[0].pts.iter().map(|q| q.y.0).min().unwrap();
        // Sweep flag 1 goes through negative y with this winding; radius 5.
        let bot = v[0].pts.iter().map(|q| q.y.0).max().unwrap();
        assert!(top.abs().max(bot.abs()) >= 5 * 65536 - 2000, "{top} {bot}");
        // Undersized radii are scaled up, never fail.
        let v = pts("M0 0 A1 1 0 0 0 10 0");
        assert!(v[0].pts.len() > 4);
    }

    #[test]
    fn curves_end_where_they_say() {
        let v = pts("M0 0 C 10 0 10 10 0 10 S -10 20 0 20 Q 5 25 0 30 T 0 40");
        assert_eq!(v[0].pts.last().copied(), Some(p(Q::ZERO, Q::int(40))));
        assert!(v[0].pts.len() > 8);
    }

    #[test]
    fn point_limit() {
        let d = "M0 0 ".to_string() + &"L1 1 ".repeat(5000);
        assert!(flatten(&parse_path(&d), Q::ONE, 1000).is_err());
        let huge = "M0 0 ".to_string() + &"L1 1 ".repeat(MAX_SEGS * 2);
        assert!(parse_path(&huge).len() <= MAX_SEGS);
    }

    #[test]
    fn shapes() {
        let r = flatten(
            &rect_segs(
                Q::ZERO,
                Q::ZERO,
                Q::int(10),
                Q::int(10),
                Q::int(2),
                Q::int(2),
            ),
            Q::ONE,
            10_000,
        )
        .unwrap();
        assert!(r[0].closed && r[0].pts.len() > 8);
        let e = flatten(
            &ellipse_segs(Q::ZERO, Q::ZERO, Q::int(5), Q::int(5)),
            Q::ONE,
            10_000,
        )
        .unwrap();
        assert!(e[0].pts.iter().all(|q| q.len().0 <= 5 * 65536 + 400));
    }
}
