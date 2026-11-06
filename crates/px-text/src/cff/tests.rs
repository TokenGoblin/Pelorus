//! Unit tests for the CFF reader and the Type 2 interpreter, on bare `CFF ` tables
//! assembled here (no sfnt wrapper), so each operator is checked in isolation.

use super::*;

// ---- a tiny assembler -------------------------------------------------------

#[derive(Clone, Copy)]
enum I {
    /// An integer operand.
    N(i32),
    /// A 16.16 operand (the `255` form).
    F(i32),
    Op(u8),
    Esc(u8),
    Raw(&'static [u8]),
}

use I::{Esc, F, N, Op, Raw};

const HSTEM: u8 = 1;
const VSTEM: u8 = 3;
const VMOVETO: u8 = 4;
const RLINETO: u8 = 5;
const HLINETO: u8 = 6;
const VLINETO: u8 = 7;
const RRCURVETO: u8 = 8;
const CALLSUBR: u8 = 10;
const RETURN: u8 = 11;
const ENDCHAR: u8 = 14;
const HSTEMHM: u8 = 18;
const HINTMASK: u8 = 19;
const CNTRMASK: u8 = 20;
const RMOVETO: u8 = 21;
const HMOVETO: u8 = 22;
const RCURVELINE: u8 = 24;
const RLINECURVE: u8 = 25;
const VVCURVETO: u8 = 26;
const HHCURVETO: u8 = 27;
const CALLGSUBR: u8 = 29;
const VHCURVETO: u8 = 30;
const HVCURVETO: u8 = 31;

fn num(v: i32, out: &mut Vec<u8>) {
    match v {
        -107..=107 => out.push((v + 139) as u8),
        108..=1131 => {
            let w = v - 108;
            out.push((w / 256 + 247) as u8);
            out.push((w % 256) as u8);
        }
        -1131..=-108 => {
            let w = -v - 108;
            out.push((w / 256 + 251) as u8);
            out.push((w % 256) as u8);
        }
        _ => {
            out.push(28);
            out.extend_from_slice(&(v as i16).to_be_bytes());
        }
    }
}

fn code(items: &[I]) -> Vec<u8> {
    let mut out = Vec::new();
    for it in items {
        match *it {
            N(v) => num(v, &mut out),
            F(v) => {
                out.push(255);
                out.extend_from_slice(&v.to_be_bytes());
            }
            Op(o) => out.push(o),
            Esc(o) => out.extend_from_slice(&[12, o]),
            Raw(b) => out.extend_from_slice(b),
        }
    }
    out
}

fn nums(values: &[i32]) -> Vec<I> {
    values.iter().map(|&v| N(v)).collect()
}

/// `values` then `op`, as a charstring fragment.
fn with(values: &[i32], op: u8) -> Vec<I> {
    let mut v = nums(values);
    v.push(Op(op));
    v
}

fn cat(parts: &[Vec<I>]) -> Vec<I> {
    parts.iter().flatten().copied().collect()
}

fn index(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(objects.len() as u16).to_be_bytes());
    if objects.is_empty() {
        return out;
    }
    out.push(2);
    let mut at = 1u16;
    out.extend_from_slice(&at.to_be_bytes());
    for o in objects {
        at += o.len() as u16;
        out.extend_from_slice(&at.to_be_bytes());
    }
    for o in objects {
        out.extend_from_slice(o);
    }
    out
}

fn num5(v: i32) -> Vec<u8> {
    let mut o = vec![29];
    o.extend_from_slice(&v.to_be_bytes());
    o
}

#[derive(Default, Clone)]
struct Spec {
    charstrings: Vec<Vec<u8>>,
    gsubrs: Vec<Vec<u8>>,
    lsubrs: Vec<Vec<u8>>,
    default_width: i32,
    nominal_width: i32,
}

fn private_dict(default_width: i32, nominal_width: i32, subrs: bool) -> Vec<u8> {
    let mut p = num5(default_width);
    p.push(20);
    p.extend(num5(nominal_width));
    p.push(21);
    if subrs {
        // The local subrs INDEX follows the Private DICT directly.
        let len = p.len() + 6;
        p.extend(num5(len as i32));
        p.push(19);
    }
    p
}

/// A complete name-keyed CFF table.
fn build(spec: &Spec) -> Vec<u8> {
    let head = [1u8, 0, 4, 2];
    let name = index(&[b"T".to_vec()]);
    let strings = index(&[]);
    let gs = index(&spec.gsubrs);
    let top_len = index(&[vec![0; 17]]).len();
    let cs = index(&spec.charstrings);
    let cs_off = head.len() + name.len() + top_len + strings.len() + gs.len();
    let private = private_dict(
        spec.default_width,
        spec.nominal_width,
        !spec.lsubrs.is_empty(),
    );
    let priv_off = cs_off + cs.len();
    let mut top = num5(cs_off as i32);
    top.push(17);
    top.extend(num5(private.len() as i32));
    top.extend(num5(priv_off as i32));
    top.push(18);
    let top = index(&[top]);
    assert_eq!(top.len(), top_len);
    let mut out = head.to_vec();
    for part in [&name, &top, &strings, &gs, &cs, &private] {
        out.extend_from_slice(part);
    }
    if !spec.lsubrs.is_empty() {
        out.extend(index(&spec.lsubrs));
    }
    out
}

/// A CID-keyed table: one Private DICT (default width) per font dict, and an
/// FDSelect of format 0 or 3 over the glyphs.
fn build_cid(
    charstrings: &[Vec<u8>],
    fds: &[(i32, i32, Vec<Vec<u8>>)],
    select: &[u8],
    format3: bool,
) -> Vec<u8> {
    let head = [1u8, 0, 4, 2];
    let name = index(&[b"T".to_vec()]);
    let strings = index(&[]);
    let gs = index(&[]);
    let top_len = index(&[vec![0; 37]]).len();
    let cs = index(charstrings);
    let mut fdsel = Vec::new();
    if format3 {
        let mut ranges: Vec<(u16, u8)> = Vec::new();
        for (i, &fd) in select.iter().enumerate() {
            if ranges.last().is_none_or(|r| r.1 != fd) {
                ranges.push((i as u16, fd));
            }
        }
        fdsel.push(3);
        fdsel.extend_from_slice(&(ranges.len() as u16).to_be_bytes());
        for (first, fd) in &ranges {
            fdsel.extend_from_slice(&first.to_be_bytes());
            fdsel.push(*fd);
        }
        fdsel.extend_from_slice(&(select.len() as u16).to_be_bytes());
    } else {
        fdsel.push(0);
        fdsel.extend_from_slice(select);
    }
    let cs_off = head.len() + name.len() + top_len + strings.len() + gs.len();
    let fdsel_off = cs_off + cs.len();
    let mut blobs = Vec::new();
    let mut fd_dicts = Vec::new();
    let mut at = fdsel_off + fdsel.len();
    for (def, nom, subrs) in fds {
        let p = private_dict(*def, *nom, !subrs.is_empty());
        let mut blob = p.clone();
        if !subrs.is_empty() {
            blob.extend(index(subrs));
        }
        let mut d = num5(p.len() as i32);
        d.extend(num5(at as i32));
        d.push(18);
        fd_dicts.push(d);
        at += blob.len();
        blobs.push(blob);
    }
    let fda = index(&fd_dicts);
    let mut top = Vec::new();
    for v in [391, 391, 0] {
        top.extend(num5(v));
    }
    top.extend_from_slice(&[12, 30]);
    top.extend(num5(at as i32));
    top.extend_from_slice(&[12, 36]);
    top.extend(num5(fdsel_off as i32));
    top.extend_from_slice(&[12, 37]);
    top.extend(num5(cs_off as i32));
    top.push(17);
    let top = index(&[top]);
    assert_eq!(top.len(), top_len);
    let mut out = head.to_vec();
    for part in [&name, &top, &strings, &gs, &cs, &fdsel] {
        out.extend_from_slice(part);
    }
    for b in &blobs {
        out.extend_from_slice(b);
    }
    out.extend(fda);
    out
}

fn parse(table: &[u8]) -> Result<Cff, FontError> {
    Cff::parse(
        table,
        Span {
            offset: 0,
            len: table.len(),
        },
    )
}

fn glyph_of(table: &[u8], g: usize) -> Result<(Outline, i32), FontError> {
    parse(table)?.glyph(table, g)
}

/// Run one charstring with no subroutines.
fn run(items: &[I]) -> Result<(Outline, i32), FontError> {
    let table = build(&Spec {
        charstrings: vec![code(items)],
        default_width: 500,
        nominal_width: 600,
        ..Spec::default()
    });
    glyph_of(&table, 0)
}

fn ok(items: &[I]) -> Outline {
    run(items).unwrap().0
}

/// The endpoint of every command except `Close`.
fn ends(o: &Outline) -> Vec<(i32, i32)> {
    o.commands
        .iter()
        .filter_map(|c| match *c {
            PathCmd::MoveTo(p) | PathCmd::LineTo(p) | PathCmd::QuadTo(_, p) => Some((p.x, p.y)),
            PathCmd::Close => None,
        })
        .collect()
}

/// Like [`ok`], with a unit line after the items so a lone move is not dropped.
fn moved(items: &[I]) -> Outline {
    ok(&cat(&[items.to_vec(), with(&[1], HLINETO)]))
}

fn last(o: &Outline) -> (i32, i32) {
    *ends(o).last().unwrap()
}

fn start() -> Vec<I> {
    with(&[0, 0], RMOVETO)
}

// ---- path operators ----------------------------------------------------------

#[test]
fn lines_and_moves() {
    let o = ok(&cat(&[
        with(&[10, 20], RMOVETO),
        with(&[30, 40, 50, 60], RLINETO),
        vec![Op(ENDCHAR)],
    ]));
    assert_eq!(ends(&o), [(10, 20), (40, 60), (90, 120)]);
    assert_eq!(o.commands.last(), Some(&PathCmd::Close));

    let o = ok(&cat(&[start(), with(&[10, 20, 30], HLINETO)]));
    assert_eq!(ends(&o), [(0, 0), (10, 0), (10, 20), (40, 20)]);
    let o = ok(&cat(&[start(), with(&[10, 20, 30], VLINETO)]));
    assert_eq!(ends(&o), [(0, 0), (0, 10), (20, 10), (20, 40)]);

    let o = ok(&cat(&[with(&[7], HMOVETO), with(&[5], HLINETO)]));
    assert_eq!(ends(&o), [(7, 0), (12, 0)]);
    let o = ok(&cat(&[with(&[7], VMOVETO), with(&[5], VLINETO)]));
    assert_eq!(ends(&o), [(0, 7), (0, 12)]);
}

#[test]
fn every_contour_is_closed_and_an_empty_one_vanishes() {
    let o = ok(&cat(&[
        with(&[0, 0], RMOVETO),
        with(&[10, 0, 0, 10], RLINETO),
        with(&[50, 50], RMOVETO), // a move to nowhere, then another
        with(&[100, 0], RMOVETO),
        with(&[10, 0, 0, 10], RLINETO),
    ]));
    let closes = o.commands.iter().filter(|c| **c == PathCmd::Close).count();
    let moves = o
        .commands
        .iter()
        .filter(|c| matches!(c, PathCmd::MoveTo(_)))
        .count();
    assert_eq!((moves, closes), (2, 2));
    assert_eq!(
        o.commands.first(),
        Some(&PathCmd::MoveTo(Pt { x: 0, y: 0 }))
    );
}

#[test]
fn a_path_operator_before_any_move_starts_at_the_origin() {
    let o = ok(&with(&[5, 5], RLINETO));
    assert_eq!(ends(&o), [(0, 0), (5, 5)]);
}

#[test]
fn rrcurveto_chains() {
    let o = ok(&cat(&[
        start(),
        with(&[10, 0, 10, 10, 0, 10, 5, 0, 5, 5, 0, 5], RRCURVETO),
    ]));
    let e = ends(&o);
    assert!(e.contains(&(20, 20)), "end of the first curve: {e:?}");
    assert_eq!(last(&o), (30, 30));
}

#[test]
fn hvcurveto_and_vhcurveto_alternate() {
    // dx1 dx2 dy2 dy3
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40], HVCURVETO)]))),
        (30, 70)
    );
    // ... with the optional final dx3
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40, 5], HVCURVETO)]))),
        (35, 70)
    );
    // two curves: the second starts vertical (dy1 dx2 dy2 dx3)
    let o = ok(&cat(&[
        start(),
        with(&[10, 20, 30, 40, 50, 60, 70, 80], HVCURVETO),
    ]));
    assert!(ends(&o).contains(&(30, 70)));
    assert_eq!(last(&o), (170, 190));
    // ... and a trailing dy3 on the second
    let o = ok(&cat(&[
        start(),
        with(&[10, 20, 30, 40, 50, 60, 70, 80, 9], HVCURVETO),
    ]));
    assert_eq!(last(&o), (170, 199));
    // vhcurveto: dy1 dx2 dy2 dx3 [dy3]
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40], VHCURVETO)]))),
        (60, 40)
    );
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40, 5], VHCURVETO)]))),
        (60, 45)
    );
    // Starting tangents are real: hv leaves horizontally, vh vertically. The first
    // quadratic's control point lies on the starting axis (within rounding).
    let hv = ok(&cat(&[start(), with(&[100, 100, 100, 100], HVCURVETO)]));
    let vh = ok(&cat(&[start(), with(&[100, 100, 100, 100], VHCURVETO)]));
    let first_ctrl = |o: &Outline| {
        o.commands.iter().find_map(|c| match c {
            PathCmd::QuadTo(c, _) => Some(*c),
            _ => None,
        })
    };
    let (h, v) = (first_ctrl(&hv).unwrap(), first_ctrl(&vh).unwrap());
    assert!(h.x > h.y * 2 && v.y > v.x * 2, "{h:?} {v:?}");
}

#[test]
fn hhcurveto_and_vvcurveto() {
    // dxa dxb dyb dxc
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40], HHCURVETO)]))),
        (70, 30)
    );
    // dy1 first when the count is odd
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[5, 10, 20, 30, 40], HHCURVETO)]))),
        (70, 35)
    );
    // dya dxb dyb dyc
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[10, 20, 30, 40], VVCURVETO)]))),
        (20, 80)
    );
    assert_eq!(
        last(&ok(&cat(&[start(), with(&[5, 10, 20, 30, 40], VVCURVETO)]))),
        (25, 80)
    );
    // two curves
    let o = ok(&cat(&[
        start(),
        with(&[10, 20, 30, 40, 1, 2, 3, 4], HHCURVETO),
    ]));
    assert_eq!(last(&o), (70 + 1 + 2 + 4, 30 + 3));
}

#[test]
fn rcurveline_and_rlinecurve() {
    // one curve then a line
    let o = ok(&cat(&[
        start(),
        with(&[10, 0, 10, 10, 0, 10, 7, 8], RCURVELINE),
    ]));
    let e = ends(&o);
    assert!(e.contains(&(20, 20)));
    assert_eq!(last(&o), (27, 28));
    // two curves then a line
    let o = ok(&cat(&[
        start(),
        with(&[1, 0, 1, 1, 0, 1, 1, 0, 1, 1, 0, 1, 3, 4], RCURVELINE),
    ]));
    assert_eq!(last(&o), (2 + 2 + 3, 2 + 2 + 4));
    // two lines then a curve
    let o = ok(&cat(&[
        start(),
        with(&[3, 4, 5, 6, 10, 0, 10, 10, 0, 10], RLINECURVE),
    ]));
    let e = ends(&o);
    assert!(e.contains(&(8, 10)));
    assert_eq!(last(&o), (28, 30));
    // operand counts that fit neither shape are refused
    assert!(run(&cat(&[start(), with(&[1, 2, 3, 4, 5, 6, 7], RCURVELINE)])).is_err());
    assert!(run(&cat(&[start(), with(&[1, 2, 3, 4, 5, 6, 7], RLINECURVE)])).is_err());
}

#[test]
fn flex_operators() {
    // hflex: dx1 dx2 dy2 dx3 dx4 dx5 dx6
    let o = ok(&cat(&[
        start(),
        vec![N(10), N(20), N(5), N(30), N(40), N(50), N(60), Esc(34)],
    ]));
    assert!(ends(&o).contains(&(60, 5)));
    assert_eq!(last(&o), (210, 0));
    // flex: 12 deltas and the depth
    let o = ok(&cat(&[
        start(),
        vec![
            N(1),
            N(2),
            N(3),
            N(4),
            N(5),
            N(6),
            N(7),
            N(8),
            N(9),
            N(10),
            N(11),
            N(12),
            N(50),
            Esc(35),
        ],
    ]));
    assert!(ends(&o).contains(&(9, 12)));
    assert_eq!(last(&o), (9 + 27, 12 + 30));
    // hflex1: dx1 dy1 dx2 dy2 dx3 dx4 dx5 dy5 dx6
    let o = ok(&cat(&[
        start(),
        vec![
            N(10),
            N(5),
            N(20),
            N(6),
            N(30),
            N(40),
            N(50),
            N(7),
            N(60),
            Esc(36),
        ],
    ]));
    assert!(ends(&o).contains(&(60, 11)));
    assert_eq!(last(&o), (210, 0), "hflex1 returns to the starting y");
    // flex1 where the x extent is larger: x takes d6, y returns to the start
    let o = ok(&cat(&[
        start(),
        vec![
            N(10),
            N(5),
            N(20),
            N(5),
            N(30),
            N(5),
            N(40),
            N(5),
            N(50),
            N(5),
            N(7),
            Esc(37),
        ],
    ]));
    assert_eq!(last(&o), (157, 0));
    // ... and where the y extent is larger: y takes d6, x returns
    let o = ok(&cat(&[
        start(),
        vec![
            N(5),
            N(10),
            N(5),
            N(20),
            N(5),
            N(30),
            N(5),
            N(40),
            N(5),
            N(50),
            N(7),
            Esc(37),
        ],
    ]));
    assert_eq!(last(&o), (0, 157));
    // a wrong operand count
    assert!(run(&cat(&[start(), vec![N(1), N(2), Esc(34)]])).is_err());
}

#[test]
fn fixed_point_operands_round_half_up() {
    // 1.5 -> 2, -1.5 -> -1, 0.25 -> 0.
    let o = moved(&cat(&[vec![F(0x0001_8000), F(0x0000_4000), Op(RMOVETO)]]));
    assert_eq!(ends(&o)[0], (2, 0));
    let o = moved(&cat(&[vec![F(-0x0001_8000), N(0), Op(RMOVETO)]]));
    assert_eq!(ends(&o)[0], (-1, 0));
}

#[test]
fn the_deprecated_arithmetic_operators() {
    // 10 5 sub -> 5; 3 4 add -> 7; 6 7 mul -> 42; 7 2 div -> 3.5 -> 4; neg; abs.
    let o = moved(&cat(&[vec![
        N(10),
        N(5),
        Esc(11),
        N(3),
        N(4),
        Esc(10),
        Op(RMOVETO),
    ]]));
    assert_eq!(ends(&o)[0], (5, 7));
    let o = moved(&cat(&[vec![
        N(6),
        N(7),
        Esc(24),
        N(7),
        N(2),
        Esc(12),
        Op(RMOVETO),
    ]]));
    assert_eq!(ends(&o)[0], (42, 4));
    let o = moved(&cat(&[vec![N(-9), Esc(9), N(4), Esc(14), Op(RMOVETO)]]));
    assert_eq!(ends(&o)[0], (9, -4));
    // dup, exch, drop: 1 2 exch -> 2 1; dup -> 2 1 1; drop -> 2 1
    let o = moved(&cat(&[vec![
        N(1),
        N(2),
        Esc(28),
        Esc(27),
        Esc(18),
        Op(RMOVETO),
    ]]));
    assert_eq!(ends(&o)[0], (2, 1));
    assert!(run(&[N(1), N(0), Esc(12), N(0), Op(RMOVETO)]).is_err());
    // the storage and conditional operators are named, not guessed
    assert_eq!(
        run(&[N(1), N(2), Esc(20)]).unwrap_err(),
        FontError::Unsupported("Type 2 storage or conditional operator")
    );
}

// ---- width, stems and masks ---------------------------------------------------

#[test]
fn width_from_each_first_operator() {
    // No width: the Private DICT's default (500).
    assert_eq!(run(&cat(&[start()])).unwrap().1, 500);
    assert_eq!(run(&[Op(ENDCHAR)]).unwrap().1, 500);
    // rmoveto with a third operand: nominal 600 + width.
    assert_eq!(run(&cat(&[with(&[-20, 1, 2], RMOVETO)])).unwrap().1, 580);
    // hmoveto / vmoveto with two operands.
    assert_eq!(run(&with(&[30, 4], HMOVETO)).unwrap().1, 630);
    assert_eq!(run(&with(&[40, 4], VMOVETO)).unwrap().1, 640);
    // endchar with one operand.
    assert_eq!(run(&with(&[50], ENDCHAR)).unwrap().1, 650);
    // An odd stem operand count.
    assert_eq!(run(&with(&[15, 0, 70], HSTEM)).unwrap().1, 615);
    assert_eq!(run(&with(&[0, 70], VSTEM)).unwrap().1, 500);
    // Taken once: later operands are never a width.
    let (o, w) = run(&cat(&[
        with(&[0, 70], HSTEMHM),
        with(&[1, 2], RMOVETO),
        with(&[5], HLINETO),
    ]))
    .unwrap();
    assert_eq!(w, 500);
    assert_eq!(ends(&o), [(1, 2), (6, 2)]);
}

#[test]
fn hintmask_skips_its_mask_bytes_and_counts_implicit_vstems() {
    // Two stems: one mask byte, whose value 255 would start a 16.16 number.
    let o = ok(&cat(&[
        with(&[0, 10, 20, 10], HSTEMHM),
        vec![Op(HINTMASK), Raw(&[255])],
        with(&[3, 4], RMOVETO),
        with(&[1], HLINETO),
    ]));
    assert_eq!(ends(&o), [(3, 4), (4, 4)]);
    // Nine stems (18 operands): two mask bytes. The hintmask itself carries four
    // more operands (an implicit vstemhm): 11 stems, still two bytes.
    let nine: Vec<i32> = (0..18).collect();
    let o = ok(&cat(&[
        with(&nine, HSTEMHM),
        with(&[1, 2, 3, 4], HINTMASK),
        vec![Raw(&[255, 255])],
        with(&[3, 4], RMOVETO),
    ]));
    assert!(o.is_empty(), "a lone move draws nothing");
    // Seventeen stems need three bytes.
    let seventeen: Vec<i32> = (0..34).collect();
    let o = ok(&cat(&[
        with(&seventeen, HSTEMHM),
        vec![Op(CNTRMASK), Raw(&[255, 255, 255])],
        with(&[3, 4], RMOVETO),
        with(&[1], HLINETO),
    ]));
    assert_eq!(ends(&o), [(3, 4), (4, 4)]);
    // A mask that runs off the end of the charstring.
    assert_eq!(
        run(&cat(&[with(&[0, 10], HSTEMHM), vec![Op(HINTMASK)]])).unwrap_err(),
        FontError::Truncated
    );
}

// ---- subroutines --------------------------------------------------------------

#[test]
fn local_and_global_subroutines_with_bias() {
    let body = |items: &[I]| code(items);
    let table = build(&Spec {
        charstrings: vec![code(&cat(&[
            start(),
            with(&[-107], CALLSUBR),  // local 0
            with(&[-106], CALLGSUBR), // global 1
            with(&[-107], CALLGSUBR), // global 0
        ]))],
        gsubrs: vec![
            body(&cat(&[with(&[100], HLINETO), vec![Op(RETURN)]])),
            body(&cat(&[with(&[0, 5], RLINETO), vec![Op(RETURN)]])),
        ],
        lsubrs: vec![body(&cat(&[with(&[1], VLINETO), vec![Op(RETURN)]]))],
        default_width: 500,
        nominal_width: 600,
    });
    let o = glyph_of(&table, 0).unwrap().0;
    assert_eq!(ends(&o), [(0, 0), (0, 1), (0, 6), (100, 6)]);
}

#[test]
fn a_large_subroutine_set_uses_the_1131_bias() {
    let mut lsubrs: Vec<Vec<u8>> = (0..1240).map(|_| code(&[Op(RETURN)])).collect();
    lsubrs[0] = code(&cat(&[with(&[11], HLINETO), vec![Op(RETURN)]]));
    lsubrs[1024] = code(&cat(&[with(&[22], VLINETO), vec![Op(RETURN)]]));
    let table = build(&Spec {
        charstrings: vec![code(&cat(&[
            start(),
            with(&[-1131], CALLSUBR),
            with(&[-107], CALLSUBR),
        ]))],
        lsubrs,
        ..Spec::default()
    });
    let o = glyph_of(&table, 0).unwrap().0;
    assert_eq!(ends(&o), [(0, 0), (11, 0), (11, 22)]);
}

#[test]
fn bias_boundaries() {
    let idx = |count| Index {
        count,
        ..Index::default()
    };
    assert_eq!(idx(0).bias(), 107);
    assert_eq!(idx(1239).bias(), 107);
    assert_eq!(idx(1240).bias(), 1131);
    assert_eq!(idx(33899).bias(), 1131);
    assert_eq!(idx(33900).bias(), 32768);
}

fn chain(depth: usize) -> Spec {
    // Subroutine i calls subroutine i + 1; the last one draws a line.
    let mut lsubrs = Vec::new();
    for i in 0..depth {
        if i + 1 < depth {
            lsubrs.push(code(&[N(i as i32 + 1 - 107), Op(CALLSUBR), Op(RETURN)]));
        } else {
            lsubrs.push(code(&cat(&[with(&[9], HLINETO), vec![Op(RETURN)]])));
        }
    }
    Spec {
        charstrings: vec![code(&cat(&[start(), with(&[-107], CALLSUBR)]))],
        lsubrs,
        ..Spec::default()
    }
}

#[test]
fn nesting_up_to_ten_deep_works_and_eleven_does_not() {
    let o = glyph_of(&build(&chain(MAX_SUBR_DEPTH)), 0).unwrap().0;
    assert_eq!(last(&o), (9, 0));
    assert_eq!(
        glyph_of(&build(&chain(MAX_SUBR_DEPTH + 1)), 0).unwrap_err(),
        FontError::TooComplex
    );
}

#[test]
fn a_subroutine_that_calls_itself_is_stopped() {
    let table = build(&Spec {
        charstrings: vec![code(&[N(-107), Op(CALLSUBR)])],
        lsubrs: vec![code(&[N(-107), Op(CALLSUBR), Op(RETURN)])],
        ..Spec::default()
    });
    assert_eq!(glyph_of(&table, 0).unwrap_err(), FontError::TooComplex);
}

#[test]
fn an_exponential_call_tree_hits_the_operation_cap() {
    // Each level calls the next one ten times: 10^10 operations if unchecked.
    let mut lsubrs = Vec::new();
    for i in 0..9 {
        let mut c = Vec::new();
        for _ in 0..10 {
            num(i + 1 - 107, &mut c);
            c.push(CALLSUBR);
        }
        c.push(RETURN);
        lsubrs.push(c);
    }
    lsubrs.push(code(&[Op(RETURN)]));
    let table = build(&Spec {
        charstrings: vec![code(&[N(-107), Op(CALLSUBR)])],
        lsubrs,
        ..Spec::default()
    });
    assert_eq!(glyph_of(&table, 0).unwrap_err(), FontError::TooComplex);
}

#[test]
fn the_point_cap_stops_a_huge_path() {
    // A subroutine of 200 lines, called repeatedly from a loop of subroutines.
    let mut lsubrs = vec![];
    let mut c = Vec::new();
    for _ in 0..48 {
        num(1, &mut c);
        num(1, &mut c);
    }
    c.push(RLINETO);
    c.push(RETURN);
    lsubrs.push(c);
    let mut main = code(&start());
    for _ in 0..2000 {
        num(-107, &mut main);
        main.push(CALLSUBR);
    }
    let table = build(&Spec {
        charstrings: vec![main],
        lsubrs,
        ..Spec::default()
    });
    assert_eq!(glyph_of(&table, 0).unwrap_err(), FontError::TooComplex);
}

// ---- errors in the charstring ---------------------------------------------------

#[test]
fn malformed_charstrings_are_errors_not_panics() {
    // Operand stack overflow.
    let many: Vec<I> = (0..=MAX_STACK).map(|_| N(1)).collect();
    assert!(run(&many).is_err());
    // Underflow.
    assert!(run(&[Op(RMOVETO)]).is_err());
    assert!(run(&[Op(CALLSUBR)]).is_err());
    // Truncated numbers.
    assert_eq!(run(&[Raw(&[28, 1])]).unwrap_err(), FontError::Truncated);
    assert_eq!(run(&[Raw(&[255, 1, 2])]).unwrap_err(), FontError::Truncated);
    assert_eq!(run(&[Raw(&[247])]).unwrap_err(), FontError::Truncated);
    // Reserved operators.
    for op in [0u8, 2, 9, 13, 15, 16, 17] {
        assert!(run(&[Op(op)]).is_err(), "operator {op}");
    }
    // callsubr with no local subroutines, and with a number out of range.
    assert!(run(&[N(0), Op(CALLSUBR)]).is_err());
    assert!(run(&[N(0), Op(CALLGSUBR)]).is_err());
    // A negative subroutine number (below the bias).
    let table = build(&Spec {
        charstrings: vec![code(&[N(-300), Op(CALLSUBR)])],
        lsubrs: vec![code(&[Op(RETURN)])],
        ..Spec::default()
    });
    assert!(glyph_of(&table, 0).is_err());
    // The accent form of endchar.
    assert_eq!(
        run(&with(&[1, 2, 65, 66], ENDCHAR)).unwrap_err(),
        FontError::Unsupported("endchar accent composition")
    );
    // hlineto and friends with no operands.
    assert!(run(&cat(&[start(), vec![Op(HLINETO)]])).is_err());
    assert!(run(&cat(&[start(), vec![Op(RRCURVETO)]])).is_err());
    assert!(run(&cat(&[start(), with(&[1, 2, 3], HHCURVETO)])).is_err());
    assert!(run(&cat(&[start(), with(&[1, 2, 3, 4, 5, 6], HVCURVETO)])).is_err());
    // A glyph past the CharStrings INDEX.
    let table = build(&Spec {
        charstrings: vec![code(&[Op(ENDCHAR)])],
        ..Spec::default()
    });
    assert_eq!(glyph_of(&table, 1).unwrap_err(), FontError::NoSuchGlyph);
}

#[test]
fn falling_off_the_end_without_endchar_still_closes_the_path() {
    let o = ok(&cat(&[start(), with(&[5, 5], RLINETO)]));
    assert_eq!(o.commands.last(), Some(&PathCmd::Close));
}

#[test]
fn coordinates_are_clamped_not_wrapped() {
    let o = ok(&cat(&[
        with(&[32767, 32767], RMOVETO),
        with(&[32767, 32767], RLINETO),
        with(&[32767, 32767], RLINETO),
    ]));
    assert!(ends(&o).iter().all(|&(x, y)| x == 32767 && y == 32767));
}

// ---- cubic flattening -----------------------------------------------------------

fn cubic_at(p: [(i64, i64); 4], t: f64) -> (f64, f64) {
    let u = 1.0 - t;
    let f = |a: i64, b: i64, c: i64, d: i64| {
        u * u * u * a as f64
            + 3.0 * u * u * t * b as f64
            + 3.0 * u * t * t * c as f64
            + t * t * t * d as f64
    };
    (
        f(p[0].0, p[1].0, p[2].0, p[3].0),
        f(p[0].1, p[1].1, p[2].1, p[3].1),
    )
}

#[test]
fn quads_follow_the_cubic_within_a_unit_and_end_on_it() {
    let shapes: [[(i64, i64); 4]; 5] = [
        [(0, 0), (0, 276), (224, 500), (500, 500)],
        [(0, 0), (800, 1000), (-300, 1000), (500, 0)],
        [(0, 0), (10, 0), (10, 10), (0, 10)],
        [(-2000, 300), (5000, -4000), (-5000, 4000), (2000, -300)],
        [(7, 7), (7, 7), (7, 7), (7, 7)],
    ];
    for s in shapes {
        let pt = |p: (i64, i64)| Pt {
            x: p.0 as i32,
            y: p.1 as i32,
        };
        let mut out = Vec::new();
        cubic_to_quads(pt(s[0]), pt(s[1]), pt(s[2]), pt(s[3]), &mut out);
        let k = out.len();
        assert!((1..=MAX_SPLIT as usize).contains(&k));
        let mut cur = (s[0].0 as f64, s[0].1 as f64);
        for (i, c) in out.iter().enumerate() {
            let PathCmd::QuadTo(ctrl, end) = *c else {
                panic!("only quadratics are emitted");
            };
            // The end point sits on the cubic at t = (i + 1) / k.
            let want = cubic_at(s, (i + 1) as f64 / k as f64);
            assert!(
                (end.x as f64 - want.0).abs() <= 1.0 && (end.y as f64 - want.1).abs() <= 1.0,
                "{s:?} piece {i}: {end:?} vs {want:?}"
            );
            // The quadratic's midpoint is within a unit of the cubic's.
            let (c0, c1) = ((cur.0, cur.1), (f64::from(end.x), f64::from(end.y)));
            let mid = (
                0.25 * c0.0 + 0.5 * f64::from(ctrl.x) + 0.25 * c1.0,
                0.25 * c0.1 + 0.5 * f64::from(ctrl.y) + 0.25 * c1.1,
            );
            let want_mid = cubic_at(s, (i as f64 + 0.5) / k as f64);
            assert!(
                (mid.0 - want_mid.0).abs() <= 1.5 && (mid.1 - want_mid.1).abs() <= 1.5,
                "{s:?} piece {i}: mid {mid:?} vs {want_mid:?}"
            );
            cur = c1;
        }
        assert_eq!(cur, (s[3].0 as f64, s[3].1 as f64), "ends exactly on p3");
    }
}

#[test]
fn flattening_is_deterministic_and_straight_cubics_stay_one_piece() {
    let p = |x, y| Pt { x, y };
    let mut a = Vec::new();
    let mut b = Vec::new();
    cubic_to_quads(p(0, 0), p(33, 33), p(66, 66), p(99, 99), &mut a);
    cubic_to_quads(p(0, 0), p(33, 33), p(66, 66), p(99, 99), &mut b);
    assert_eq!(a, b);
    assert_eq!(a.len(), 1);
}

// ---- the table: INDEX, DICT, Private, CID ----------------------------------------

#[test]
fn dict_numbers_of_every_form() {
    let mut seen = Vec::new();
    // 139 -> 0; 28 -> i16; 29 -> i32; 247/251 forms; operator 17; escape 12 7.
    let dict = [
        139, 28, 0xff, 0xfe, 29, 0, 1, 0, 0, 247, 0, 251, 0, 17, 32, 12, 7,
    ];
    read_dict(&dict, |op, a| seen.push((op, a.to_vec()))).unwrap();
    assert_eq!(
        seen,
        [(17, vec![0, -2, 65536, 108, -108]), (0x0c07, vec![-107]),]
    );
    // A real: -2.5e3 packs as e 2 a 5 b 3 f, whose integer part is 2.
    let mut seen = Vec::new();
    read_dict(&[30, 0xe2, 0xa5, 0xb3, 0xff, 20], |op, a| {
        seen.push((op, a.to_vec()));
    })
    .unwrap();
    assert_eq!(seen, [(20, vec![-2])]);
    // Unterminated real, truncated operands, bad byte.
    assert!(read_dict(&[30, 0x12], |_, _| {}).is_err());
    assert!(read_dict(&[28, 1], |_, _| {}).is_err());
    assert!(read_dict(&[29, 1, 2, 3], |_, _| {}).is_err());
    assert!(read_dict(&[255], |_, _| {}).is_err());
    // More operands than any operator takes.
    let long = vec![139u8; MAX_DICT_OPERANDS + 1];
    assert!(read_dict(&long, |_, _| {}).is_err());
}

#[test]
fn a_cff_table_validates_its_structure() {
    let good = build(&Spec {
        charstrings: vec![code(&[Op(ENDCHAR)])],
        lsubrs: vec![code(&[Op(RETURN)])],
        ..Spec::default()
    });
    assert!(parse(&good).is_ok());
    // CFF2 is named, other majors are malformed.
    let mut v2 = good.clone();
    v2[0] = 2;
    assert_eq!(
        parse(&v2).unwrap_err(),
        FontError::Unsupported("CFF2 outlines")
    );
    let mut v3 = good.clone();
    v3[0] = 3;
    assert!(parse(&v3).is_err());
    // Header size and offSize.
    let mut h = good.clone();
    h[2] = 2;
    assert!(parse(&h).is_err());
    let mut h = good.clone();
    h[3] = 0;
    assert!(parse(&h).is_err());
    // Every truncation is an error, never a panic.
    for n in 0..good.len() {
        let _ = parse(&good[..n]).and_then(|c| c.glyph(&good[..n], 0));
    }
    // Every byte replaced by every interesting value.
    for i in 0..good.len() {
        for v in [0u8, 1, 0x7f, 0x80, 0xff] {
            let mut m = good.clone();
            m[i] = v;
            let _ = parse(&m).and_then(|c| c.glyph(&m, 0));
        }
    }
}

#[test]
fn index_offsets_are_checked() {
    let mut t = vec![0, 2, 1, 1, 3, 9, b'a', b'b']; // second offset is past the data
    assert!(Index::parse(&t, 0).is_err());
    t = vec![0, 2, 1, 1, 3, 2, b'a', b'b']; // 3 passes the end, then decreases
    let idx = Index::parse(&t, 0).unwrap();
    assert!(idx.get(&t, 0).is_err());
    assert!(idx.get(&t, 1).is_err());
    assert!(idx.get(&t, 2).is_err());
    t = vec![0, 2, 1, 1, 2, 3, b'a', b'b'];
    let idx = Index::parse(&t, 0).unwrap();
    assert_eq!(idx.get(&t, 0).unwrap(), b"a");
    assert_eq!(idx.get(&t, 1).unwrap(), b"b");
    // offSize 0 and 5.
    assert!(Index::parse(&[0, 1, 0, 1, 1, 1], 0).is_err());
    assert!(Index::parse(&[0, 1, 5, 1, 1, 1], 0).is_err());
    // A count whose offset array alone is larger than the table.
    assert!(Index::parse(&[0xff, 0xff, 4, 0, 0], 0).is_err());
    // First offset zero: the object cannot be read.
    let z = [0, 1, 1, 0, 1, 7];
    assert!(Index::parse(&z, 0).unwrap().get(&z, 0).is_err());
    // An empty INDEX is two bytes.
    let e = Index::parse(&[0, 0, 9], 0).unwrap();
    assert_eq!((e.count, e.end), (0, 2));
}

#[test]
fn missing_or_unsupported_top_dict_entries() {
    // No CharStrings.
    let mut t = build(&Spec {
        charstrings: vec![code(&[Op(ENDCHAR)])],
        ..Spec::default()
    });
    // Op 17 sits right after the first operand of the Top DICT: rewrite it to 99
    // (unknown, ignored) so CharStrings is absent.
    let at = t.iter().position(|&b| b == 17).unwrap();
    t[at] = 21; // nominalWidthX: CharStrings is gone
    assert!(parse(&t).is_err());
    // An empty CharStrings INDEX.
    let t = build(&Spec::default());
    assert!(parse(&t).is_err());
}

#[test]
fn cid_keyed_fonts_use_a_private_dict_per_font_dict() {
    // Three glyphs, each ending with endchar and a width where the FD says so.
    let glyphs: Vec<Vec<u8>> = (0..3).map(|_| code(&with(&[7], ENDCHAR))).collect();
    let fds = vec![(500, 100, vec![]), (800, 200, vec![code(&[Op(RETURN)])])];
    for format3 in [false, true] {
        let table = build_cid(&glyphs, &fds, &[0, 1, 1], format3);
        let c = parse(&table).unwrap();
        // 7 + nominal of the glyph's font dict.
        let widths: Vec<i32> = (0..3).map(|g| c.glyph(&table, g).unwrap().1).collect();
        assert_eq!(widths, [107, 207, 207], "format3 {format3}");
        // Glyphs with no width take the font dict's default.
        let bare: Vec<Vec<u8>> = (0..3).map(|_| code(&[Op(ENDCHAR)])).collect();
        let table = build_cid(&bare, &fds, &[1, 0, 1], format3);
        let c = parse(&table).unwrap();
        let widths: Vec<i32> = (0..3).map(|g| c.glyph(&table, g).unwrap().1).collect();
        assert_eq!(widths, [800, 500, 800], "format3 {format3}");
        assert!(c.glyph(&table, 3).is_err());
    }
    // An FDSelect naming a font dict that does not exist.
    let table = build_cid(&glyphs, &fds, &[0, 5, 0], false);
    let c = parse(&table).unwrap();
    assert!(c.glyph(&table, 1).is_err());
    assert!(c.glyph(&table, 0).is_ok());
    // Format 3 with a hole: glyph 2 is outside every range.
    let mut t3 = build_cid(&glyphs, &fds, &[0, 0, 1], true);
    let c = parse(&t3).unwrap();
    let sel = c.fd_select.unwrap().pos;
    // Shrink the sentinel so the last glyph falls outside.
    t3[sel + 3 + 2 * 3] = 0;
    t3[sel + 3 + 2 * 3 + 1] = 2;
    assert!(c.glyph(&t3, 2).is_err());
}
