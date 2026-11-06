//! Stroking: turning flattened subpaths into polygons whose non-zero union is
//! the stroke, and dashing.
//!
//! Each segment is a quad, each join and cap a small convex polygon. They are
//! emitted separately and wound the same way, so filling them non-zero gives
//! their union. This is simple and robust (no offset-curve self intersection
//! to repair) at the price of more edges than a single outline.

use super::fixed::{Q, TWO_PI, sincos};
use super::geom::{P, p};
use super::path::{Poly, steps_per_turn};
use crate::DecodeError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

#[derive(Clone, Copy, Debug)]
pub struct StrokeStyle {
    pub width: Q,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: Q,
}

/// Most dash pieces produced for one shape.
pub const MAX_DASHES: usize = 100_000;

/// Emit the polygons of the stroke of `polys`. `scale` is device pixels per
/// user unit (it picks how finely round joins are cut).
pub fn stroke(
    polys: &[Poly],
    st: &StrokeStyle,
    scale: Q,
    emit: &mut dyn FnMut(&[P]) -> Result<(), DecodeError>,
) -> Result<(), DecodeError> {
    let hw = st.width.half();
    if hw.0 <= 0 {
        return Ok(());
    }
    let n = steps_per_turn(hw * scale);
    let circle: Vec<P> = (0..n)
        .map(|k| {
            let (s, c) = sincos((TWO_PI * k) / n);
            p(c * hw, s * hw)
        })
        .collect();
    let mut buf: Vec<P> = Vec::with_capacity(circle.len());
    for poly in polys {
        stroke_one(poly, st, hw, &circle, &mut buf, emit)?;
    }
    Ok(())
}

fn disc(
    at: P,
    circle: &[P],
    buf: &mut Vec<P>,
    emit: &mut dyn FnMut(&[P]) -> Result<(), DecodeError>,
) -> Result<(), DecodeError> {
    buf.clear();
    buf.extend(circle.iter().map(|o| at.add(*o)));
    emit(buf)
}

fn stroke_one(
    poly: &Poly,
    st: &StrokeStyle,
    hw: Q,
    circle: &[P],
    buf: &mut Vec<P>,
    emit: &mut dyn FnMut(&[P]) -> Result<(), DecodeError>,
) -> Result<(), DecodeError> {
    let mut pts: Vec<P> = Vec::with_capacity(poly.pts.len());
    for q in &poly.pts {
        if pts.last() != Some(q) {
            pts.push(*q);
        }
    }
    if poly.closed && pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    let n = pts.len();
    let at = |i: usize| pts.get(i % n.max(1)).copied().unwrap_or_default();
    if n == 1 {
        // A zero-length subpath draws a dot with round or square caps, but
        // only when it was a real (if empty) segment, not a lone moveto.
        if poly.pts.len() >= 2 || poly.closed {
            let c = at(0);
            match st.cap {
                Cap::Round => disc(c, circle, buf, emit)?,
                Cap::Square => emit(&[
                    p(c.x - hw, c.y - hw),
                    p(c.x + hw, c.y - hw),
                    p(c.x + hw, c.y + hw),
                    p(c.x - hw, c.y + hw),
                ])?,
                Cap::Butt => {}
            }
        }
        return Ok(());
    }
    if n < 2 {
        return Ok(());
    }
    let segs = if poly.closed { n } else { n - 1 };
    let dir = |i: usize| at(i + 1).sub(at(i)).unit();
    let normal = |d: P| p(-d.y, d.x);
    for i in 0..segs {
        let (a, b) = (at(i), at(i + 1));
        let nm = normal(dir(i)).scale(hw);
        emit(&[a.add(nm), b.add(nm), b.sub(nm), a.sub(nm)])?;
    }
    // Joins.
    let join_range = if poly.closed { 0..n } else { 1..n - 1 };
    for i in join_range {
        let prev = if i == 0 { n - 1 } else { i - 1 };
        let (d1, d2) = (dir(prev), dir(i));
        let v = at(i);
        let cross = d1.cross(d2);
        let dot = d1.dot(d2);
        if cross.0 == 0 && dot.0 > 0 {
            continue;
        }
        match st.join {
            Join::Round => disc(v, circle, buf, emit)?,
            Join::Bevel | Join::Miter => {
                // The outer side is opposite the turn.
                let s = if cross.0 > 0 { -Q::ONE } else { Q::ONE };
                let o1 = normal(d1).scale(hw * s);
                let o2 = normal(d2).scale(hw * s);
                let denom = Q::ONE + dot;
                let ratio_sq = Q::int(2) / denom;
                if st.join == Join::Miter
                    && denom.0 > 64
                    && ratio_sq <= st.miter_limit * st.miter_limit
                {
                    let tip = o1.add(o2).scale(Q::ONE / denom);
                    emit(&[v, v.add(o1), v.add(tip), v.add(o2)])?;
                } else {
                    emit(&[v, v.add(o1), v.add(o2)])?;
                }
            }
        }
    }
    // Caps.
    if !poly.closed {
        for (end, d) in [(at(0), dir(0).scale(-Q::ONE)), (at(n - 1), dir(n - 2))] {
            match st.cap {
                Cap::Butt => {}
                Cap::Round => disc(end, circle, buf, emit)?,
                Cap::Square => {
                    let nm = normal(d).scale(hw);
                    let ext = d.scale(hw);
                    emit(&[
                        end.add(nm),
                        end.add(nm).add(ext),
                        end.sub(nm).add(ext),
                        end.sub(nm),
                    ])?;
                }
            }
        }
    }
    Ok(())
}

/// Split `polys` into dashes. `pattern` has been validated by the caller to
/// hold non-negative lengths with a positive sum.
pub fn dash(polys: &[Poly], pattern: &[Q], offset: Q) -> Result<Vec<Poly>, DecodeError> {
    let total: Q = pattern.iter().fold(Q::ZERO, |a, b| a + *b);
    if total.0 <= 0 || pattern.is_empty() {
        return Ok(polys.to_vec());
    }
    let mut out: Vec<Poly> = Vec::new();
    for poly in polys {
        let mut pts = poly.pts.clone();
        if poly.closed
            && let Some(f) = pts.first().copied()
        {
            pts.push(f);
        }
        // Dash state: index into the pattern, remaining length of that entry.
        let mut off = Q(offset.0.rem_euclid(total.0));
        let mut idx = 0usize;
        for _ in 0..pattern.len() {
            let l = pattern.get(idx).copied().unwrap_or(Q::ONE);
            if off < l {
                break;
            }
            off = off - l;
            idx = (idx + 1) % pattern.len();
        }
        let mut left = pattern.get(idx).copied().unwrap_or(Q::ZERO) - off;
        let mut on = idx.is_multiple_of(2);
        let mut cur: Option<Poly> = None;
        for w in pts.windows(2) {
            let (a, b) = match w {
                [a, b] => (*a, *b),
                _ => continue,
            };
            let seg_len = b.sub(a).len();
            if seg_len.0 == 0 {
                continue;
            }
            let mut pos = Q::ZERO;
            if on && cur.is_none() {
                cur = Some(Poly {
                    pts: vec![a],
                    closed: false,
                });
            }
            while seg_len - pos > left {
                pos = pos + left;
                let at_pt = a.lerp(b, pos, seg_len);
                if on {
                    if let Some(mut c) = cur.take() {
                        c.pts.push(at_pt);
                        out.push(c);
                    }
                } else {
                    cur = Some(Poly {
                        pts: vec![at_pt],
                        closed: false,
                    });
                }
                on = !on;
                idx = (idx + 1) % pattern.len();
                left = pattern.get(idx).copied().unwrap_or(Q::ONE);
                if out.len() > MAX_DASHES {
                    return Err(DecodeError::LimitExceeded("svg dashes"));
                }
                if left.0 == 0 && pattern.iter().all(|q| q.0 == 0) {
                    break;
                }
            }
            left = left - (seg_len - pos);
            if let Some(c) = cur.as_mut().filter(|_| on) {
                c.pts.push(b);
            }
        }
        if let Some(c) = cur.take().filter(|c| c.pts.len() > 1) {
            out.push(c);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(x1: i64) -> Poly {
        Poly {
            pts: vec![p(Q::ZERO, Q::ZERO), p(Q::int(x1), Q::ZERO)],
            closed: false,
        }
    }

    #[test]
    fn dash_splits_a_line() {
        let d = dash(&[line(10)], &[Q::int(2), Q::int(3)], Q::ZERO).unwrap();
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].pts.last().unwrap().x, Q::int(2));
        assert_eq!(d[1].pts.first().unwrap().x, Q::int(5));
        assert_eq!(d[1].pts.last().unwrap().x, Q::int(7));
        let d = dash(&[line(10)], &[Q::int(2), Q::int(3)], Q::int(2)).unwrap();
        assert_eq!(d[0].pts.first().unwrap().x, Q::int(3));
    }

    #[test]
    fn dash_bomb_is_bounded() {
        let r = dash(&[line(1_000_000)], &[Q(2), Q(2)], Q::ZERO);
        assert!(r.is_err());
        assert!(dash(&[line(10)], &[Q::ZERO, Q::ZERO], Q::ZERO).is_ok());
    }

    #[test]
    fn stroke_emits_quads_joins_and_caps() {
        let poly = Poly {
            pts: vec![
                p(Q::ZERO, Q::ZERO),
                p(Q::int(10), Q::ZERO),
                p(Q::int(10), Q::int(10)),
            ],
            closed: false,
        };
        let mut count = 0;
        let st = StrokeStyle {
            width: Q::int(2),
            cap: Cap::Square,
            join: Join::Miter,
            miter_limit: Q::int(4),
        };
        stroke(&[poly], &st, Q::ONE, &mut |pts| {
            assert!(pts.len() >= 3);
            count += 1;
            Ok(())
        })
        .unwrap();
        // 2 segments + 1 miter join + 2 square caps.
        assert_eq!(count, 5);
    }
}
