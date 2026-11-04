//! `glyf` outlines: simple and composite glyphs as integer font-unit paths.
//!
//! # Rules
//!
//! * Coordinates are `i32` font units, y up, exactly as stored. No scaling here.
//! * TrueType contours are quadratic with implied on-curve points. Where two
//!   off-curve points are adjacent the implied on-curve point is their midpoint,
//!   computed as `(a + b) >> 1` (floor): the only rounding in this module, at most
//!   half a font unit.
//! * Composite components apply the 2x2 F2Dot14 matrix to each point with
//!   `(x*a + y*c + 0x2000) >> 14` in `i64` (round to nearest, halves up), then add
//!   the integer offset. Components positioned by point matching (rather than by
//!   x/y offset) are refused with [`FontError::Unsupported`].
//! * Caps: nesting depth [`MAX_DEPTH`], components [`MAX_COMPONENTS`] per glyph
//!   (summed over the whole tree), points [`MAX_POINTS`]. Exceeding any gives
//!   [`FontError::TooComplex`]. Hostile bytes can therefore cost bounded work.

use crate::sfnt::{FontError, i16_at, u8_at, u16_at};

/// Deepest composite nesting.
pub const MAX_DEPTH: usize = 6;
/// Most components visited while building one glyph.
pub const MAX_COMPONENTS: usize = 128;
/// Most points in one glyph.
pub const MAX_POINTS: usize = 20_000;

/// A point in font units.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Pt {
    pub x: i32,
    pub y: i32,
}

/// One path command. Contours always end with [`PathCmd::Close`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathCmd {
    MoveTo(Pt),
    LineTo(Pt),
    /// Control point, end point.
    QuadTo(Pt, Pt),
    Close,
}

/// A glyph outline in font units.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Outline {
    pub commands: Vec<PathCmd>,
}

impl Outline {
    /// `true` for glyphs with no contours (a space).
    pub fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    /// The bounding box of every point named by a command (control points
    /// included), as `(x_min, y_min, x_max, y_max)`; `None` when empty.
    pub fn bounds(&self) -> Option<(i32, i32, i32, i32)> {
        let mut b: Option<(i32, i32, i32, i32)> = None;
        let mut add = |p: Pt| {
            b = Some(match b {
                None => (p.x, p.y, p.x, p.y),
                Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
            });
        };
        for c in &self.commands {
            match *c {
                PathCmd::MoveTo(p) | PathCmd::LineTo(p) => add(p),
                PathCmd::QuadTo(a, p) => {
                    add(a);
                    add(p);
                }
                PathCmd::Close => {}
            }
        }
        b
    }
}

#[derive(Clone, Copy)]
struct GPoint {
    p: Pt,
    on: bool,
}

#[derive(Default)]
struct Points {
    pts: Vec<GPoint>,
    /// Exclusive end index of each contour in `pts`.
    ends: Vec<usize>,
    components: usize,
}

/// The `glyf` and `loca` data of a font, borrowed for one outline extraction.
pub(crate) struct GlyfTables<'a> {
    pub glyf: &'a [u8],
    pub loca: &'a [u8],
    pub long_loca: bool,
    pub num_glyphs: u16,
}

impl GlyfTables<'_> {
    /// The byte range of `glyph` within `glyf`, empty when the glyph has no outline.
    fn glyph_data(&self, glyph: u16) -> Result<&[u8], FontError> {
        if glyph >= self.num_glyphs {
            return Err(FontError::NoSuchGlyph);
        }
        let i = usize::from(glyph);
        let (start, end) = if self.long_loca {
            let a = crate::sfnt::u32_at(self.loca, i * 4).ok_or(FontError::Truncated)?;
            let b = crate::sfnt::u32_at(self.loca, i * 4 + 4).ok_or(FontError::Truncated)?;
            (a as usize, b as usize)
        } else {
            let a = u16_at(self.loca, i * 2).ok_or(FontError::Truncated)?;
            let b = u16_at(self.loca, i * 2 + 2).ok_or(FontError::Truncated)?;
            (usize::from(a) * 2, usize::from(b) * 2)
        };
        if end < start {
            return Err(FontError::Malformed("loca not monotonic"));
        }
        self.glyf.get(start..end).ok_or(FontError::Truncated)
    }

    /// The outline of `glyph`.
    pub(crate) fn outline(&self, glyph: u16) -> Result<Outline, FontError> {
        let mut acc = Points::default();
        self.load(glyph, 0, &mut acc, None)?;
        Ok(to_outline(&acc))
    }

    fn load(
        &self,
        glyph: u16,
        depth: usize,
        acc: &mut Points,
        matrix: Option<Transform>,
    ) -> Result<(), FontError> {
        if depth > MAX_DEPTH {
            return Err(FontError::TooComplex);
        }
        let data = self.glyph_data(glyph)?;
        if data.is_empty() {
            return Ok(());
        }
        let contours = i16_at(data, 0).ok_or(FontError::Truncated)?;
        if contours >= 0 {
            self.simple(data, usize::try_from(contours).unwrap_or(0), acc, matrix)
        } else {
            self.composite(data, depth, acc, matrix)
        }
    }

    fn simple(
        &self,
        data: &[u8],
        contours: usize,
        acc: &mut Points,
        matrix: Option<Transform>,
    ) -> Result<(), FontError> {
        let mut pos = 10;
        let mut ends = Vec::with_capacity(contours.min(1024));
        let mut last_end: Option<usize> = None;
        for _ in 0..contours {
            let e = usize::from(u16_at(data, pos).ok_or(FontError::Truncated)?);
            pos += 2;
            if last_end.is_some_and(|l| e <= l) {
                return Err(FontError::Malformed("contour ends not increasing"));
            }
            last_end = Some(e);
            ends.push(e + 1);
        }
        let n = ends.last().copied().unwrap_or(0);
        if acc.pts.len().saturating_add(n) > MAX_POINTS {
            return Err(FontError::TooComplex);
        }
        let instr = usize::from(u16_at(data, pos).ok_or(FontError::Truncated)?);
        pos = pos.checked_add(2 + instr).ok_or(FontError::Truncated)?;
        // Flags, with repeat.
        let mut flags: Vec<u8> = Vec::with_capacity(n);
        while flags.len() < n {
            let f = u8_at(data, pos).ok_or(FontError::Truncated)?;
            pos += 1;
            flags.push(f);
            if f & 8 != 0 {
                let r = usize::from(u8_at(data, pos).ok_or(FontError::Truncated)?);
                pos += 1;
                for _ in 0..r {
                    if flags.len() >= n {
                        return Err(FontError::Malformed("flag repeat overruns"));
                    }
                    flags.push(f);
                }
            }
        }
        let mut xs: Vec<i32> = Vec::with_capacity(n);
        let mut v: i32 = 0;
        for &f in &flags {
            v = v.saturating_add(read_delta(data, &mut pos, f, 2)?);
            xs.push(v);
        }
        let mut ys: Vec<i32> = Vec::with_capacity(n);
        v = 0;
        for &f in &flags {
            v = v.saturating_add(read_delta(data, &mut pos, f, 4)?);
            ys.push(v);
        }
        let base = acc.pts.len();
        for ((&f, &x), &y) in flags.iter().zip(&xs).zip(&ys) {
            let p = Pt { x, y };
            let p = matrix.map_or(p, |m| m.apply(p));
            acc.pts.push(GPoint { p, on: f & 1 != 0 });
        }
        acc.ends.extend(ends.into_iter().map(|e| base + e));
        Ok(())
    }

    fn composite(
        &self,
        data: &[u8],
        depth: usize,
        acc: &mut Points,
        matrix: Option<Transform>,
    ) -> Result<(), FontError> {
        let mut pos = 10;
        loop {
            acc.components += 1;
            if acc.components > MAX_COMPONENTS {
                return Err(FontError::TooComplex);
            }
            let flags = u16_at(data, pos).ok_or(FontError::Truncated)?;
            let child = u16_at(data, pos + 2).ok_or(FontError::Truncated)?;
            pos += 4;
            let (dx, dy) = if flags & 1 != 0 {
                let a = i16_at(data, pos).ok_or(FontError::Truncated)?;
                let b = i16_at(data, pos + 2).ok_or(FontError::Truncated)?;
                pos += 4;
                (i32::from(a), i32::from(b))
            } else {
                let a = u8_at(data, pos).ok_or(FontError::Truncated)? as i8;
                let b = u8_at(data, pos + 1).ok_or(FontError::Truncated)? as i8;
                pos += 2;
                (i32::from(a), i32::from(b))
            };
            if flags & 2 == 0 {
                return Err(FontError::Unsupported("point-matched component"));
            }
            let (mut xx, mut xy, mut yx, mut yy) = (0x4000i32, 0, 0, 0x4000i32);
            if flags & 0x0008 != 0 {
                let s = i32::from(i16_at(data, pos).ok_or(FontError::Truncated)?);
                pos += 2;
                xx = s;
                yy = s;
            } else if flags & 0x0040 != 0 {
                xx = i32::from(i16_at(data, pos).ok_or(FontError::Truncated)?);
                yy = i32::from(i16_at(data, pos + 2).ok_or(FontError::Truncated)?);
                pos += 4;
            } else if flags & 0x0080 != 0 {
                xx = i32::from(i16_at(data, pos).ok_or(FontError::Truncated)?);
                xy = i32::from(i16_at(data, pos + 2).ok_or(FontError::Truncated)?);
                yx = i32::from(i16_at(data, pos + 4).ok_or(FontError::Truncated)?);
                yy = i32::from(i16_at(data, pos + 6).ok_or(FontError::Truncated)?);
                pos += 8;
            }
            let local = Transform {
                xx,
                xy,
                yx,
                yy,
                dx,
                dy,
            };
            // child point = local(child); then the parent's matrix applies on top.
            let combined = Transform::then(local, matrix);
            self.load(child, depth + 1, acc, Some(combined))?;
            if flags & 0x0020 == 0 {
                return Ok(());
            }
        }
    }
}

/// Read one coordinate delta. `short_bit` is the axis's "short vector" flag (x:
/// 0x02, y: 0x04); its "same / positive" flag is `short_bit << 3` (0x10, 0x20).
fn read_delta(data: &[u8], pos: &mut usize, flag: u8, short_bit: u8) -> Result<i32, FontError> {
    let same = flag & (short_bit << 3) != 0;
    if flag & short_bit != 0 {
        let v = i32::from(u8_at(data, *pos).ok_or(FontError::Truncated)?);
        *pos += 1;
        Ok(if same { v } else { -v })
    } else if same {
        Ok(0)
    } else {
        let v = i32::from(i16_at(data, *pos).ok_or(FontError::Truncated)?);
        *pos += 2;
        Ok(v)
    }
}

/// `p' = (round((xx*x + yx*y) / 2^14) + dx, round((xy*x + yy*y) / 2^14) + dy)`.
#[derive(Clone, Copy)]
struct Transform {
    xx: i32,
    xy: i32,
    yx: i32,
    yy: i32,
    dx: i32,
    dy: i32,
}

impl Transform {
    fn apply(self, p: Pt) -> Pt {
        let (x, y) = (i64::from(p.x), i64::from(p.y));
        let nx = (x * i64::from(self.xx) + y * i64::from(self.yx) + 0x2000) >> 14;
        let ny = (x * i64::from(self.xy) + y * i64::from(self.yy) + 0x2000) >> 14;
        Pt {
            x: clamp32(nx + i64::from(self.dx)),
            y: clamp32(ny + i64::from(self.dy)),
        }
    }

    /// `inner` applied first, then `outer`, as one matrix and offset. Each
    /// product is rounded once with the F2Dot14 rule (round to nearest, halves up).
    fn then(inner: Transform, outer: Option<Transform>) -> Transform {
        let Some(o) = outer else { return inner };
        let m = |a: i32, b: i32| i64::from(a) * i64::from(b);
        let r = |v: i64| clamp32((v + 0x2000) >> 14);
        let i = inner;
        Transform {
            xx: r(m(i.xx, o.xx) + m(i.xy, o.yx)),
            xy: r(m(i.xx, o.xy) + m(i.xy, o.yy)),
            yx: r(m(i.yx, o.xx) + m(i.yy, o.yx)),
            yy: r(m(i.yx, o.xy) + m(i.yy, o.yy)),
            dx: clamp32(
                ((i64::from(i.dx) * i64::from(o.xx) + i64::from(i.dy) * i64::from(o.yx) + 0x2000)
                    >> 14)
                    + i64::from(o.dx),
            ),
            dy: clamp32(
                ((i64::from(i.dx) * i64::from(o.xy) + i64::from(i.dy) * i64::from(o.yy) + 0x2000)
                    >> 14)
                    + i64::from(o.dy),
            ),
        }
    }
}

fn clamp32(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

fn mid(a: Pt, b: Pt) -> Pt {
    Pt {
        x: ((i64::from(a.x) + i64::from(b.x)) >> 1) as i32,
        y: ((i64::from(a.y) + i64::from(b.y)) >> 1) as i32,
    }
}

fn to_outline(acc: &Points) -> Outline {
    let mut out = Outline::default();
    let mut begin = 0usize;
    for &end in &acc.ends {
        let end = end.min(acc.pts.len());
        let Some(pts) = acc.pts.get(begin..end) else {
            break;
        };
        begin = end;
        contour(pts, &mut out.commands);
    }
    out
}

fn contour(pts: &[GPoint], cmds: &mut Vec<PathCmd>) {
    let (Some(first), Some(last)) = (pts.first(), pts.last()) else {
        return;
    };
    // Where the contour starts, and which points remain to walk.
    let (start, rest): (Pt, &[GPoint]) = if first.on {
        (first.p, pts.get(1..).unwrap_or(&[]))
    } else if last.on {
        (last.p, pts.get(..pts.len() - 1).unwrap_or(&[]))
    } else {
        (mid(last.p, first.p), pts)
    };
    cmds.push(PathCmd::MoveTo(start));
    let mut pending: Option<Pt> = None;
    for g in rest {
        if g.on {
            match pending.take() {
                Some(c) => cmds.push(PathCmd::QuadTo(c, g.p)),
                None => cmds.push(PathCmd::LineTo(g.p)),
            }
        } else {
            if let Some(c) = pending {
                cmds.push(PathCmd::QuadTo(c, mid(c, g.p)));
            }
            pending = Some(g.p);
        }
    }
    if let Some(c) = pending {
        cmds.push(PathCmd::QuadTo(c, start));
    }
    cmds.push(PathCmd::Close);
}
