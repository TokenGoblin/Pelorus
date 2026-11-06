//! `CFF ` outlines: a bounded Type 2 charstring interpreter (Adobe TN #5176 for the
//! table, TN #5177 for the charstrings).
//!
//! # Scope
//!
//! Supported: the CFF 1.0 header and INDEX structures, the Top DICT, Private DICTs,
//! local and global subroutines (with the spec's bias and a depth limit), CID-keyed
//! fonts (`FDArray`, `FDSelect` formats 0 and 3), and every Type 2 path operator:
//! `rmoveto hmoveto vmoveto rlineto hlineto vlineto rrcurveto hhcurveto hvcurveto
//! rcurveline rlinecurve vhcurveto vvcurveto hflex flex hflex1 flex1`, stems and
//! `hintmask`/`cntrmask` (skipped, hints are never executed), width handling,
//! `callsubr`, `callgsubr`, `return`, `endchar`, and the deprecated arithmetic
//! operators a few old fonts still use (`abs add sub div neg mul drop dup exch`).
//!
//! Not supported, and refused with a named error rather than misdrawn: CFF2
//! (variable fonts), the `endchar` accent form (`seac`), and the deprecated
//! storage and conditional operators. The Top DICT `FontMatrix` is ignored (the
//! `head` table's unitsPerEm governs, which is what every real font means).
//!
//! # Numbers
//!
//! Operands are 16.16 fixed point in `i32`; the current point is 16.16 in `i64`.
//! Nothing is floating point. Each emitted point is rounded half up, `(v + 0x8000)
//! >> 16`, and clamped to the `i16` range (the range `glyf` can express).
//!
//! # From cubic to what the rasterizer draws
//!
//! The rasterizer's path vocabulary ([`PathCmd`]) is lines and quadratics. Each
//! cubic is split at `t = i/k` into `k` pieces and each piece replaced by the
//! quadratic with the same end points whose control point is
//! `(3 (c1 + c2) - (p0 + p3)) / 4`. `k` is the smallest value in `1..=16` with
//! `|p0 - 3 c1 + 3 c2 - p3|_max <= 4 k^3` (the quadratic's error is about
//! `0.048 d / k^3` font units, so under a quarter of a unit), the sub-cubic control points
//! come from integer de Casteljau blossoms scaled by `k^3`, and every division
//! rounds half up. All of it is integer arithmetic, so the output is a pure
//! function of the font bytes on every platform.
//!
//! # Hostile input
//!
//! Every offset and size is checked against the table before it is used; an INDEX
//! is validated when parsed (its last offset must lie inside the table) and each
//! entry again when read. Work per glyph is capped: [`MAX_OPS`] operators and
//! operands in total (across all subroutines, so a subroutine loop cannot spin),
//! subroutine nesting [`MAX_SUBR_DEPTH`], operand stack [`MAX_STACK`], and
//! [`MAX_POINTS`] output points. Exceeding a cap is [`FontError::TooComplex`].
//! Nothing allocates in proportion to a number read from the file except the
//! output, which the point cap bounds.

use crate::glyf::{MAX_POINTS, Outline, PathCmd, Pt};
use crate::sfnt::{FontError, Span, u8_at, u16_at};

/// Deepest subroutine nesting (the spec's limit).
pub const MAX_SUBR_DEPTH: usize = 10;
/// Operand stack size. The Type 2 limit is 48; a little slack costs nothing.
pub const MAX_STACK: usize = 96;
/// Most operators plus operands executed for one glyph.
pub const MAX_OPS: usize = 200_000;
/// Most font dictionaries in a CID-keyed font.
const MAX_FDS: usize = 256;
/// Most DICT operands kept while reading a DICT.
const MAX_DICT_OPERANDS: usize = 64;
/// Most pieces one cubic is split into.
const MAX_SPLIT: i64 = 16;

fn bad(why: &'static str) -> FontError {
    FontError::Malformed(why)
}

// ---------------------------------------------------------------------------
// INDEX
// ---------------------------------------------------------------------------

/// A parsed INDEX header. Positions are offsets into the CFF table.
#[derive(Clone, Copy, Debug, Default)]
struct Index {
    count: usize,
    off_size: usize,
    /// Position of the offset array.
    offsets: usize,
    /// Position of the first data byte (object 0 starts here).
    data: usize,
    /// Position just past the INDEX.
    end: usize,
}

impl Index {
    fn parse(table: &[u8], pos: usize) -> Result<Index, FontError> {
        let count = usize::from(u16_at(table, pos).ok_or(FontError::Truncated)?);
        let after_count = pos.checked_add(2).ok_or(FontError::Truncated)?;
        if count == 0 {
            return Ok(Index {
                count: 0,
                off_size: 1,
                offsets: after_count,
                data: after_count,
                end: after_count,
            });
        }
        let off_size = usize::from(u8_at(table, after_count).ok_or(FontError::Truncated)?);
        if !(1..=4).contains(&off_size) {
            return Err(bad("CFF INDEX offSize"));
        }
        let offsets = after_count.checked_add(1).ok_or(FontError::Truncated)?;
        let array_len = count
            .checked_add(1)
            .and_then(|n| n.checked_mul(off_size))
            .ok_or(FontError::Truncated)?;
        // Offsets are 1-based: object 0 starts at `offsets + array_len`.
        let data = offsets.checked_add(array_len).ok_or(FontError::Truncated)?;
        let idx = Index {
            count,
            off_size,
            offsets,
            data,
            end: data,
        };
        let last = idx.offset(table, count)?;
        if last == 0 {
            return Err(bad("CFF INDEX offset zero"));
        }
        let end = data.checked_add(last - 1).ok_or(FontError::Truncated)?;
        if end > table.len() {
            return Err(FontError::Truncated);
        }
        Ok(Index { end, ..idx })
    }

    /// Offset-array entry `i` (`0..=count`).
    fn offset(&self, table: &[u8], i: usize) -> Result<usize, FontError> {
        let at = i
            .checked_mul(self.off_size)
            .and_then(|o| o.checked_add(self.offsets))
            .ok_or(FontError::Truncated)?;
        let end = at.checked_add(self.off_size).ok_or(FontError::Truncated)?;
        let bytes = table.get(at..end).ok_or(FontError::Truncated)?;
        Ok(bytes.iter().fold(0usize, |v, &b| (v << 8) | usize::from(b)))
    }

    /// Object `i`, or an error for a bad index or a corrupt offset pair.
    fn get<'t>(&self, table: &'t [u8], i: usize) -> Result<&'t [u8], FontError> {
        if i >= self.count {
            return Err(FontError::NoSuchGlyph);
        }
        let a = self.offset(table, i)?;
        let b = self.offset(table, i + 1)?;
        if a == 0 || b < a {
            return Err(bad("CFF INDEX offsets not monotonic"));
        }
        let start = self.data.checked_add(a - 1).ok_or(FontError::Truncated)?;
        let end = self.data.checked_add(b - 1).ok_or(FontError::Truncated)?;
        if end > self.end {
            return Err(FontError::Truncated);
        }
        table.get(start..end).ok_or(FontError::Truncated)
    }

    /// The subroutine bias for this many subroutines (TN #5177 section 4.7).
    fn bias(&self) -> i32 {
        match self.count {
            0..=1239 => 107,
            1240..=33899 => 1131,
            _ => 32768,
        }
    }
}

// ---------------------------------------------------------------------------
// DICT
// ---------------------------------------------------------------------------

/// Read a DICT, calling `f(operator, operands)` for each operator. Real numbers
/// are truncated to their integer part (no field this reader needs is fractional
/// in a way that matters). Operator `12 x` is reported as `0x0c00 | x`.
fn read_dict(dict: &[u8], mut f: impl FnMut(u16, &[i32])) -> Result<(), FontError> {
    let mut operands: Vec<i32> = Vec::new();
    let mut pos = 0usize;
    while pos < dict.len() {
        let b0 = u8_at(dict, pos).ok_or(FontError::Truncated)?;
        pos += 1;
        let value: Option<i32> = match b0 {
            0..=21 => {
                let op = if b0 == 12 {
                    let b1 = u8_at(dict, pos).ok_or(FontError::Truncated)?;
                    pos += 1;
                    0x0c00 | u16::from(b1)
                } else {
                    u16::from(b0)
                };
                f(op, &operands);
                operands.clear();
                None
            }
            28 => {
                let v = u16_at(dict, pos).ok_or(FontError::Truncated)?;
                pos += 2;
                Some(i32::from(v as i16))
            }
            29 => {
                let end = pos.checked_add(4).ok_or(FontError::Truncated)?;
                let b: [u8; 4] = dict
                    .get(pos..end)
                    .and_then(|s| s.try_into().ok())
                    .ok_or(FontError::Truncated)?;
                pos = end;
                Some(i32::from_be_bytes(b))
            }
            30 => {
                let (v, used) = real_to_int(dict.get(pos..).unwrap_or(&[]))?;
                pos += used;
                Some(v)
            }
            32..=246 => Some(i32::from(b0) - 139),
            247..=250 => {
                let b1 = i32::from(u8_at(dict, pos).ok_or(FontError::Truncated)?);
                pos += 1;
                Some((i32::from(b0) - 247) * 256 + b1 + 108)
            }
            251..=254 => {
                let b1 = i32::from(u8_at(dict, pos).ok_or(FontError::Truncated)?);
                pos += 1;
                Some(-(i32::from(b0) - 251) * 256 - b1 - 108)
            }
            _ => return Err(bad("CFF DICT byte")),
        };
        if let Some(v) = value {
            if operands.len() >= MAX_DICT_OPERANDS {
                return Err(bad("CFF DICT operand stack"));
            }
            operands.push(v);
        }
    }
    Ok(())
}

/// The integer part of a packed-BCD real (TN #5176 table 5), and how many bytes
/// it occupied. The nibbles must end with `f` inside `data`.
fn real_to_int(data: &[u8]) -> Result<(i32, usize), FontError> {
    let mut value: i64 = 0;
    let mut negative = false;
    let mut in_fraction = false;
    let mut in_exponent = false;
    for (i, &byte) in data.iter().enumerate() {
        for nibble in [byte >> 4, byte & 15] {
            match nibble {
                0..=9 => {
                    if !in_fraction && !in_exponent {
                        value = value.saturating_mul(10).saturating_add(i64::from(nibble));
                        value = value.min(i64::from(i32::MAX));
                    }
                }
                0xa => in_fraction = true,
                0xb | 0xc => in_exponent = true,
                0xe => negative = true,
                0xf => {
                    let v = if negative { -value } else { value };
                    return Ok((i32::try_from(v).unwrap_or(0), i + 1));
                }
                _ => {}
            }
        }
    }
    Err(FontError::Truncated)
}

// ---------------------------------------------------------------------------
// The table
// ---------------------------------------------------------------------------

/// Per-font-dict data: the local subroutines and the two width constants.
#[derive(Clone, Copy, Debug, Default)]
struct Private {
    subrs: Option<Index>,
    default_width: i32,
    nominal_width: i32,
}

/// Where an `FDSelect` lives.
#[derive(Clone, Copy, Debug)]
struct FdSelect {
    pos: usize,
    format: u8,
}

/// A validated `CFF ` table. Holds positions only; the bytes stay in the font.
#[derive(Clone, Debug)]
pub(crate) struct Cff {
    span: Span,
    charstrings: Index,
    gsubrs: Index,
    privates: Vec<Private>,
    fd_select: Option<FdSelect>,
}

#[derive(Default)]
struct TopDict {
    charstrings: Option<i32>,
    private: Option<(i32, i32)>,
    fd_array: Option<i32>,
    fd_select: Option<i32>,
    charstring_type: Option<i32>,
}

fn nonneg(v: i32) -> Result<usize, FontError> {
    usize::try_from(v).map_err(|_| bad("CFF negative offset"))
}

impl Cff {
    /// Validate the `CFF ` table at `span` in `data`.
    pub(crate) fn parse(data: &[u8], span: Span) -> Result<Cff, FontError> {
        let table = span.slice(data);
        let major = u8_at(table, 0).ok_or(FontError::Truncated)?;
        if major == 2 {
            return Err(FontError::Unsupported("CFF2 outlines"));
        }
        if major != 1 {
            return Err(bad("CFF version"));
        }
        let hdr_size = usize::from(u8_at(table, 2).ok_or(FontError::Truncated)?);
        let off_size = u8_at(table, 3).ok_or(FontError::Truncated)?;
        if hdr_size < 4 || !(1..=4).contains(&off_size) {
            return Err(bad("CFF header"));
        }
        let names = Index::parse(table, hdr_size)?;
        let tops = Index::parse(table, names.end)?;
        let strings = Index::parse(table, tops.end)?;
        let gsubrs = Index::parse(table, strings.end)?;

        let mut top = TopDict::default();
        read_dict(tops.get(table, 0)?, |op, a| match (op, a) {
            (17, [v, ..]) => top.charstrings = Some(*v),
            (18, [size, offset, ..]) => top.private = Some((*size, *offset)),
            (0x0c24, [v, ..]) => top.fd_array = Some(*v),
            (0x0c25, [v, ..]) => top.fd_select = Some(*v),
            (0x0c06, [v, ..]) => top.charstring_type = Some(*v),
            _ => {}
        })?;
        if top.charstring_type.is_some_and(|t| t != 2) {
            return Err(FontError::Unsupported("Type 1 charstrings"));
        }
        let charstrings = Index::parse(
            table,
            nonneg(top.charstrings.ok_or(bad("CFF without CharStrings"))?)?,
        )?;
        if charstrings.count == 0 {
            return Err(bad("CFF without glyphs"));
        }

        let mut privates = Vec::new();
        let mut fd_select = None;
        if let Some(fda) = top.fd_array {
            let fds = Index::parse(table, nonneg(fda)?)?;
            if fds.count == 0 || fds.count > MAX_FDS {
                return Err(bad("CFF FDArray size"));
            }
            for i in 0..fds.count {
                let mut private = None;
                read_dict(fds.get(table, i)?, |op, a| {
                    if let (18, [size, offset, ..]) = (op, a) {
                        private = Some((*size, *offset));
                    }
                })?;
                privates.push(match private {
                    Some(p) => read_private(table, p)?,
                    None => Private::default(),
                });
            }
            let pos = nonneg(top.fd_select.ok_or(bad("CID font without FDSelect"))?)?;
            let format = u8_at(table, pos).ok_or(FontError::Truncated)?;
            match format {
                0 => {
                    let need = pos
                        .checked_add(1)
                        .and_then(|p| p.checked_add(charstrings.count))
                        .ok_or(FontError::Truncated)?;
                    if need > table.len() {
                        return Err(FontError::Truncated);
                    }
                }
                3 => {}
                _ => return Err(bad("FDSelect format")),
            }
            fd_select = Some(FdSelect { pos, format });
        } else {
            let p = match top.private {
                Some(p) => read_private(table, p)?,
                None => Private::default(),
            };
            privates.push(p);
        }
        Ok(Cff {
            span,
            charstrings,
            gsubrs,
            privates,
            fd_select,
        })
    }

    /// How many glyphs the CharStrings INDEX holds.
    pub(crate) fn num_glyphs(&self) -> usize {
        self.charstrings.count
    }

    fn fd_of(&self, table: &[u8], glyph: usize) -> Result<usize, FontError> {
        let Some(sel) = self.fd_select else {
            return Ok(0);
        };
        let fd = match sel.format {
            0 => u8_at(table, sel.pos + 1 + glyph).ok_or(FontError::Truncated)?,
            _ => {
                let n = usize::from(u16_at(table, sel.pos + 1).ok_or(FontError::Truncated)?);
                let mut found = None;
                let mut first = u16_at(table, sel.pos + 3).ok_or(FontError::Truncated)?;
                for i in 0..n {
                    let rec = sel.pos + 3 + i * 3;
                    let fd = u8_at(table, rec + 2).ok_or(FontError::Truncated)?;
                    let next = u16_at(table, rec + 3).ok_or(FontError::Truncated)?;
                    if usize::from(first) <= glyph && glyph < usize::from(next) {
                        found = Some(fd);
                        break;
                    }
                    first = next;
                }
                found.ok_or(bad("FDSelect range"))?
            }
        };
        let fd = usize::from(fd);
        if fd >= self.privates.len() {
            return Err(bad("FDSelect names a missing font dict"));
        }
        Ok(fd)
    }

    /// The outline of `glyph` in font units and its charstring width (the advance
    /// the charstring declares; `hmtx` is what layout uses).
    pub(crate) fn glyph(&self, data: &[u8], glyph: usize) -> Result<(Outline, i32), FontError> {
        let table = self.span.slice(data);
        let code = self.charstrings.get(table, glyph)?;
        let private = *self
            .privates
            .get(self.fd_of(table, glyph)?)
            .ok_or(bad("font dict"))?;
        let mut it = Interp {
            table,
            gsubrs: self.gsubrs,
            lsubrs: private.subrs,
            stack: Vec::new(),
            x: 0,
            y: 0,
            open: false,
            stems: 0,
            width: None,
            default_width: private.default_width,
            nominal_width: private.nominal_width,
            ops: 0,
            points: 0,
            done: false,
            out: Vec::new(),
        };
        it.run(code, 0)?;
        it.close_contour();
        let width = it.width.unwrap_or(private.default_width);
        Ok((Outline { commands: it.out }, width))
    }
}

fn read_private(table: &[u8], (size, offset): (i32, i32)) -> Result<Private, FontError> {
    let (size, offset) = (nonneg(size)?, nonneg(offset)?);
    let end = offset.checked_add(size).ok_or(FontError::Truncated)?;
    let dict = table.get(offset..end).ok_or(FontError::Truncated)?;
    let mut p = Private::default();
    let mut subrs_at: Option<i32> = None;
    read_dict(dict, |op, a| match (op, a) {
        (19, [v, ..]) => subrs_at = Some(*v),
        (20, [v, ..]) => p.default_width = *v,
        (21, [v, ..]) => p.nominal_width = *v,
        _ => {}
    })?;
    if let Some(rel) = subrs_at {
        let pos = offset
            .checked_add(nonneg(rel)?)
            .ok_or(FontError::Truncated)?;
        p.subrs = Some(Index::parse(table, pos)?);
    }
    Ok(p)
}

// ---------------------------------------------------------------------------
// The Type 2 interpreter
// ---------------------------------------------------------------------------

struct Interp<'a> {
    table: &'a [u8],
    gsubrs: Index,
    lsubrs: Option<Index>,
    /// Operands, 16.16 fixed.
    stack: Vec<i32>,
    /// Current point, 16.16 fixed.
    x: i64,
    y: i64,
    open: bool,
    stems: usize,
    width: Option<i32>,
    default_width: i32,
    nominal_width: i32,
    ops: usize,
    points: usize,
    done: bool,
    out: Vec<PathCmd>,
}

/// Round a 16.16 value to an integer, half up, within the `i16` range.
fn round_fixed(v: i64) -> i32 {
    let r = v.saturating_add(0x8000) >> 16;
    r.clamp(-32768, 32767) as i32
}

/// Keep the current point finite: ten bits above the largest operand sum.
const COORD_LIMIT: i64 = 1 << 40;

fn arg(a: &[i32], i: usize) -> Result<i64, FontError> {
    a.get(i)
        .map(|&v| i64::from(v))
        .ok_or(bad("Type 2 operand underflow"))
}

/// `n / d` rounded half up, for `d > 0`.
fn div_round(n: i64, d: i64) -> i64 {
    n.saturating_mul(2).saturating_add(d).div_euclid(2 * d)
}

impl Interp<'_> {
    fn pt(&self) -> Pt {
        Pt {
            x: round_fixed(self.x),
            y: round_fixed(self.y),
        }
    }

    fn bump(&mut self, by: usize) -> Result<(), FontError> {
        self.ops = self.ops.saturating_add(by);
        if self.ops > MAX_OPS {
            return Err(FontError::TooComplex);
        }
        Ok(())
    }

    fn add_points(&mut self, n: usize) -> Result<(), FontError> {
        self.points = self.points.saturating_add(n);
        if self.points > MAX_POINTS {
            return Err(FontError::TooComplex);
        }
        Ok(())
    }

    fn advance(&mut self, dx: i64, dy: i64) {
        self.x = self.x.saturating_add(dx).clamp(-COORD_LIMIT, COORD_LIMIT);
        self.y = self.y.saturating_add(dy).clamp(-COORD_LIMIT, COORD_LIMIT);
    }

    /// End the open contour. A contour that is only a move to nowhere vanishes.
    fn close_contour(&mut self) {
        if !self.open {
            return;
        }
        self.open = false;
        if matches!(self.out.last(), Some(PathCmd::MoveTo(_))) {
            self.out.pop();
        } else {
            self.out.push(PathCmd::Close);
        }
    }

    fn move_to(&mut self, dx: i64, dy: i64) -> Result<(), FontError> {
        self.close_contour();
        self.advance(dx, dy);
        self.add_points(1)?;
        self.out.push(PathCmd::MoveTo(self.pt()));
        self.open = true;
        Ok(())
    }

    /// Start a contour at the current point if a path op comes before any move.
    fn ensure_open(&mut self) -> Result<(), FontError> {
        if !self.open {
            self.add_points(1)?;
            self.out.push(PathCmd::MoveTo(self.pt()));
            self.open = true;
        }
        Ok(())
    }

    fn line_rel(&mut self, dx: i64, dy: i64) -> Result<(), FontError> {
        self.ensure_open()?;
        self.advance(dx, dy);
        self.add_points(1)?;
        self.out.push(PathCmd::LineTo(self.pt()));
        Ok(())
    }

    /// A cubic with all three control vectors relative to the previous point.
    fn curve_rel(&mut self, d: [i64; 6]) -> Result<(), FontError> {
        self.ensure_open()?;
        let p0 = self.pt();
        self.advance(d[0], d[1]);
        let p1 = self.pt();
        self.advance(d[2], d[3]);
        let p2 = self.pt();
        self.advance(d[4], d[5]);
        let p3 = self.pt();
        let before = self.out.len();
        cubic_to_quads(p0, p1, p2, p3, &mut self.out);
        let added = self.out.len().saturating_sub(before);
        self.add_points(added.saturating_mul(2))
    }

    /// The width, if it has not been taken yet: an extra leading operand when the
    /// operand count is `expected` plus one (or, for stems, odd).
    fn take_width(&mut self, has_extra: bool) {
        if self.width.is_some() {
            return;
        }
        if has_extra && !self.stack.is_empty() {
            let w = self.stack.remove(0);
            self.width = Some(self.nominal_width.saturating_add(round_fixed(i64::from(w))));
        } else {
            self.width = Some(self.default_width);
        }
    }

    fn push(&mut self, v: i32) -> Result<(), FontError> {
        if self.stack.len() >= MAX_STACK {
            return Err(bad("Type 2 operand stack overflow"));
        }
        self.stack.push(v);
        Ok(())
    }

    fn pop(&mut self) -> Result<i32, FontError> {
        self.stack.pop().ok_or(bad("Type 2 operand underflow"))
    }

    /// Run `code`; returns at `return`, at the end of the code, or after `endchar`.
    fn run(&mut self, code: &[u8], depth: usize) -> Result<(), FontError> {
        let mut pos = 0usize;
        while pos < code.len() && !self.done {
            let b0 = u8_at(code, pos).ok_or(FontError::Truncated)?;
            pos += 1;
            self.bump(1)?;
            match b0 {
                28 => {
                    let v = u16_at(code, pos).ok_or(FontError::Truncated)?;
                    pos += 2;
                    self.push(i32::from(v as i16) << 16)?;
                }
                32..=246 => self.push((i32::from(b0) - 139) << 16)?,
                247..=250 => {
                    let b1 = i32::from(u8_at(code, pos).ok_or(FontError::Truncated)?);
                    pos += 1;
                    self.push(((i32::from(b0) - 247) * 256 + b1 + 108) << 16)?;
                }
                251..=254 => {
                    let b1 = i32::from(u8_at(code, pos).ok_or(FontError::Truncated)?);
                    pos += 1;
                    self.push((-(i32::from(b0) - 251) * 256 - b1 - 108) << 16)?;
                }
                255 => {
                    let end = pos.checked_add(4).ok_or(FontError::Truncated)?;
                    let b: [u8; 4] = code
                        .get(pos..end)
                        .and_then(|s| s.try_into().ok())
                        .ok_or(FontError::Truncated)?;
                    pos = end;
                    self.push(i32::from_be_bytes(b))?;
                }
                1 | 3 | 18 | 23 => {
                    self.take_width(self.stack.len() % 2 == 1);
                    self.stems = self.stems.saturating_add(self.stack.len() / 2);
                    self.stack.clear();
                }
                19 | 20 => {
                    // Operands before the mask are an implicit vstemhm.
                    self.take_width(self.stack.len() % 2 == 1);
                    self.stems = self.stems.saturating_add(self.stack.len() / 2);
                    self.stack.clear();
                    let mask = self.stems.div_ceil(8);
                    pos = pos.checked_add(mask).ok_or(FontError::Truncated)?;
                    if pos > code.len() {
                        return Err(FontError::Truncated);
                    }
                }
                21 => {
                    self.take_width(self.stack.len() > 2);
                    let a = std::mem::take(&mut self.stack);
                    let (dx, dy) = (arg(&a, 0)?, arg(&a, 1)?);
                    self.move_to(dx, dy)?;
                }
                22 => {
                    self.take_width(self.stack.len() > 1);
                    let a = std::mem::take(&mut self.stack);
                    self.move_to(arg(&a, 0)?, 0)?;
                }
                4 => {
                    self.take_width(self.stack.len() > 1);
                    let a = std::mem::take(&mut self.stack);
                    self.move_to(0, arg(&a, 0)?)?;
                }
                5 | 6 | 7 | 8 | 24 | 25 | 26 | 27 | 30 | 31 => {
                    let a = std::mem::take(&mut self.stack);
                    self.path_op(b0, &a)?;
                }
                10 | 29 => {
                    let n = self.pop()?;
                    let subrs = if b0 == 10 {
                        self.lsubrs
                    } else {
                        Some(self.gsubrs)
                    }
                    .ok_or(bad("callsubr without local subroutines"))?;
                    if depth >= MAX_SUBR_DEPTH {
                        return Err(FontError::TooComplex);
                    }
                    let index = i64::from(n >> 16) + i64::from(subrs.bias());
                    let index = usize::try_from(index).map_err(|_| bad("subroutine number"))?;
                    let body = subrs.get(self.table, index)?;
                    self.run(body, depth + 1)?;
                }
                11 => return Ok(()),
                14 => {
                    if self.stack.len() >= 4 {
                        return Err(FontError::Unsupported("endchar accent composition"));
                    }
                    self.take_width(!self.stack.is_empty());
                    self.stack.clear();
                    self.close_contour();
                    self.done = true;
                }
                12 => {
                    let b1 = u8_at(code, pos).ok_or(FontError::Truncated)?;
                    pos += 1;
                    self.escape(b1)?;
                }
                _ => return Err(bad("reserved Type 2 operator")),
            }
        }
        Ok(())
    }

    fn escape(&mut self, op: u8) -> Result<(), FontError> {
        match op {
            // hflex, flex, hflex1, flex1.
            34..=37 => {
                let a = std::mem::take(&mut self.stack);
                self.flex_op(op, &a)
            }
            // abs, add, sub, div, neg, mul.
            9 => {
                let v = self.pop()?;
                self.push(v.saturating_abs())
            }
            10 | 11 | 24 | 12 => {
                let b = i64::from(self.pop()?);
                let a = i64::from(self.pop()?);
                let r = match op {
                    10 => a.saturating_add(b),
                    11 => a.saturating_sub(b),
                    24 => a.saturating_mul(b) >> 16,
                    _ => {
                        if b == 0 {
                            return Err(bad("Type 2 division by zero"));
                        }
                        (a << 16) / b
                    }
                };
                self.push(r.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32)
            }
            14 => {
                let v = self.pop()?;
                self.push(v.saturating_neg())
            }
            18 => self.pop().map(|_| ()),
            27 => {
                let v = self.pop()?;
                self.push(v)?;
                self.push(v)
            }
            28 => {
                let b = self.pop()?;
                let a = self.pop()?;
                self.push(b)?;
                self.push(a)
            }
            _ => Err(FontError::Unsupported(
                "Type 2 storage or conditional operator",
            )),
        }
    }

    fn path_op(&mut self, op: u8, a: &[i32]) -> Result<(), FontError> {
        let n = a.len();
        match op {
            // rlineto
            5 => {
                if n == 0 || !n.is_multiple_of(2) {
                    return Err(bad("rlineto operands"));
                }
                for i in (0..n).step_by(2) {
                    self.line_rel(arg(a, i)?, arg(a, i + 1)?)?;
                }
            }
            // hlineto, vlineto: alternate, starting horizontal for 6.
            6 | 7 => {
                if n == 0 {
                    return Err(bad("hlineto operands"));
                }
                for i in 0..n {
                    let v = arg(a, i)?;
                    if (i % 2 == 0) == (op == 6) {
                        self.line_rel(v, 0)?;
                    } else {
                        self.line_rel(0, v)?;
                    }
                }
            }
            // rrcurveto
            8 => {
                if n == 0 || !n.is_multiple_of(6) {
                    return Err(bad("rrcurveto operands"));
                }
                for i in (0..n).step_by(6) {
                    self.curve_rel([
                        arg(a, i)?,
                        arg(a, i + 1)?,
                        arg(a, i + 2)?,
                        arg(a, i + 3)?,
                        arg(a, i + 4)?,
                        arg(a, i + 5)?,
                    ])?;
                }
            }
            // rcurveline: curves, then one line.
            24 => {
                if n < 8 || !(n - 2).is_multiple_of(6) {
                    return Err(bad("rcurveline operands"));
                }
                let curves = (n - 2) / 6 * 6;
                for i in (0..curves).step_by(6) {
                    self.curve_rel([
                        arg(a, i)?,
                        arg(a, i + 1)?,
                        arg(a, i + 2)?,
                        arg(a, i + 3)?,
                        arg(a, i + 4)?,
                        arg(a, i + 5)?,
                    ])?;
                }
                self.line_rel(arg(a, curves)?, arg(a, curves + 1)?)?;
            }
            // rlinecurve: lines, then one curve.
            25 => {
                if n < 8 || !(n - 6).is_multiple_of(2) {
                    return Err(bad("rlinecurve operands"));
                }
                let lines = n - 6;
                for i in (0..lines).step_by(2) {
                    self.line_rel(arg(a, i)?, arg(a, i + 1)?)?;
                }
                self.curve_rel([
                    arg(a, lines)?,
                    arg(a, lines + 1)?,
                    arg(a, lines + 2)?,
                    arg(a, lines + 3)?,
                    arg(a, lines + 4)?,
                    arg(a, lines + 5)?,
                ])?;
            }
            // vvcurveto: [dx1] {dya dxb dyb dyc}+
            26 => {
                if n < 4 || !matches!(n % 4, 0 | 1) {
                    return Err(bad("vvcurveto operands"));
                }
                let mut i = n % 4;
                let mut dx1 = if n % 4 == 1 { arg(a, 0)? } else { 0 };
                while i + 4 <= n {
                    self.curve_rel([
                        dx1,
                        arg(a, i)?,
                        arg(a, i + 1)?,
                        arg(a, i + 2)?,
                        0,
                        arg(a, i + 3)?,
                    ])?;
                    dx1 = 0;
                    i += 4;
                }
            }
            // hhcurveto: [dy1] {dxa dxb dyb dxc}+
            27 => {
                if n < 4 || !matches!(n % 4, 0 | 1) {
                    return Err(bad("hhcurveto operands"));
                }
                let mut i = n % 4;
                let mut dy1 = if n % 4 == 1 { arg(a, 0)? } else { 0 };
                while i + 4 <= n {
                    self.curve_rel([
                        arg(a, i)?,
                        dy1,
                        arg(a, i + 1)?,
                        arg(a, i + 2)?,
                        arg(a, i + 3)?,
                        0,
                    ])?;
                    dy1 = 0;
                    i += 4;
                }
            }
            // vhcurveto (30) and hvcurveto (31): alternating tangents.
            _ => {
                if n < 4 || !matches!(n % 4, 0 | 1) {
                    return Err(bad("hvcurveto operands"));
                }
                let mut horizontal = op == 31;
                let mut i = 0usize;
                while i + 4 <= n {
                    let last = if n - i == 5 { arg(a, i + 4)? } else { 0 };
                    let (p, q, r, s) = (arg(a, i)?, arg(a, i + 1)?, arg(a, i + 2)?, arg(a, i + 3)?);
                    if horizontal {
                        // dx1 dx2 dy2 dy3 [dx3]
                        self.curve_rel([p, 0, q, r, last, s])?;
                    } else {
                        // dy1 dx2 dy2 dx3 [dy3]
                        self.curve_rel([0, p, q, r, s, last])?;
                    }
                    horizontal = !horizontal;
                    i += 4;
                }
            }
        }
        Ok(())
    }

    fn flex_op(&mut self, op: u8, a: &[i32]) -> Result<(), FontError> {
        let v = |i: usize| arg(a, i);
        match op {
            // hflex: dx1 dx2 dy2 dx3 dx4 dx5 dx6
            34 => {
                if a.len() != 7 {
                    return Err(bad("hflex operands"));
                }
                self.curve_rel([v(0)?, 0, v(1)?, v(2)?, v(3)?, 0])?;
                self.curve_rel([v(4)?, 0, v(5)?, -v(2)?, v(6)?, 0])?;
            }
            // flex: twelve deltas, then the flex depth.
            35 => {
                if a.len() != 13 {
                    return Err(bad("flex operands"));
                }
                self.curve_rel([v(0)?, v(1)?, v(2)?, v(3)?, v(4)?, v(5)?])?;
                self.curve_rel([v(6)?, v(7)?, v(8)?, v(9)?, v(10)?, v(11)?])?;
            }
            // hflex1: dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6
            36 => {
                if a.len() != 9 {
                    return Err(bad("hflex1 operands"));
                }
                self.curve_rel([v(0)?, v(1)?, v(2)?, v(3)?, v(4)?, 0])?;
                let back = -(v(1)? + v(3)? + v(7)?);
                self.curve_rel([v(5)?, 0, v(6)?, v(7)?, v(8)?, back])?;
            }
            // flex1: five deltas, five more, then d6 for the last axis.
            _ => {
                if a.len() != 11 {
                    return Err(bad("flex1 operands"));
                }
                let dx = v(0)? + v(2)? + v(4)? + v(6)? + v(8)?;
                let dy = v(1)? + v(3)? + v(5)? + v(7)? + v(9)?;
                self.curve_rel([v(0)?, v(1)?, v(2)?, v(3)?, v(4)?, v(5)?])?;
                let (ex, ey) = if dx.abs() > dy.abs() {
                    (v(10)?, -dy)
                } else {
                    (-dx, v(10)?)
                };
                self.curve_rel([v(6)?, v(7)?, v(8)?, v(9)?, ex, ey])?;
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Cubic to quadratic
// ---------------------------------------------------------------------------

/// Append quadratics that follow the cubic `p0 p1 p2 p3` (see the module docs).
/// The path is already at `p0`; the last command ends exactly at `p3`.
pub(crate) fn cubic_to_quads(p0: Pt, p1: Pt, p2: Pt, p3: Pt, out: &mut Vec<PathCmd>) {
    let (x0, y0, x1, y1) = (
        i64::from(p0.x),
        i64::from(p0.y),
        i64::from(p1.x),
        i64::from(p1.y),
    );
    let (x2, y2, x3, y3) = (
        i64::from(p2.x),
        i64::from(p2.y),
        i64::from(p3.x),
        i64::from(p3.y),
    );
    let dx = (x0 - 3 * x1 + 3 * x2 - x3).abs();
    let dy = (y0 - 3 * y1 + 3 * y2 - y3).abs();
    let d = dx.max(dy);
    let mut k = 1i64;
    while k < MAX_SPLIT && d > 4 * k * k * k {
        k += 1;
    }
    let k3 = k * k * k;
    // Control points of the sub-cubic from blossom parameters (u, v, w) in 0..=k,
    // scaled by k^3.
    let blossom = |u: i64, v: i64, w: i64, c: [i64; 4]| -> i64 {
        let [a, b, cc, e] = c;
        let l1 = [
            (k - u) * a + u * b,
            (k - u) * b + u * cc,
            (k - u) * cc + u * e,
        ];
        let l2 = [(k - v) * l1[0] + v * l1[1], (k - v) * l1[1] + v * l1[2]];
        (k - w) * l2[0] + w * l2[1]
    };
    let xs = [x0, x1, x2, x3];
    let ys = [y0, y1, y2, y3];
    for i in 0..k {
        let (a, b) = (i, i + 1);
        let at = |u: i64, v: i64, w: i64| (blossom(u, v, w, xs), blossom(u, v, w, ys));
        let (q0x, q0y) = at(a, a, a);
        let (q1x, q1y) = at(a, a, b);
        let (q2x, q2y) = at(a, b, b);
        let (q3x, q3y) = at(b, b, b);
        let cx = div_round(3 * (q1x + q2x) - (q0x + q3x), 4 * k3);
        let cy = div_round(3 * (q1y + q2y) - (q0y + q3y), 4 * k3);
        let ex = div_round(q3x, k3);
        let ey = div_round(q3y, k3);
        let clamp = |v: i64| v.clamp(-32768, 32767) as i32;
        out.push(PathCmd::QuadTo(
            Pt {
                x: clamp(cx),
                y: clamp(cy),
            },
            Pt {
                x: clamp(ex),
                y: clamp(ey),
            },
        ));
    }
}

#[cfg(test)]
mod tests;
