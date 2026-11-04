//! Grid item placement: css-grid-1 section 8.
//!
//! Pure: it takes the placement properties as plain values and returns line
//! numbers, so the algorithm can be read, tested and bounded without a DOM.
//!
//! # Axes
//!
//! The algorithm is written for `grid-auto-flow: row`. For `column` the spec says
//! to swap every mention of rows and columns, so this module speaks of a *major*
//! axis (the one the cursor advances along when a line is full: rows for `row`
//! flow) and a *minor* axis (columns for `row` flow). The caller transposes.
//!
//! # Line numbers
//!
//! 1-based, as in the spec. The explicit grid has lines `1..=explicit + 1`. Items
//! may land on lines `<= 0` (implicit tracks before the explicit grid) or past
//! `explicit + 1`. Every line is clamped to `+-`[`MAX_LINES`], which is the bound
//! browsers apply (10,000) and what makes `grid-column: 99999999` allocate nothing.

use std::collections::BTreeMap;

use super::num::Fuel;

/// The largest line number magnitude. Spans are clamped to the same figure.
pub(crate) const MAX_LINES: i32 = 10_000;

/// One `<grid-line>`, already stripped of stylo's representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Line {
    Auto,
    /// `span N` with an optional name to count.
    Span(i32, Option<String>),
    /// `N` with an optional name; `N` may be negative.
    Num(i32, Option<String>),
    /// A bare identifier: the edge of a named area, or the first such line.
    Ident(String),
}

/// Line names to the line numbers that carry them, ascending.
pub(crate) type Names = BTreeMap<String, Vec<i32>>;

/// A placement in one axis after §8.3's conflict handling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Resolved {
    Definite { start: i32, span: i32 },
    Auto { span: i32 },
}

fn clamp_line(value: i64) -> i32 {
    let bound = i64::from(MAX_LINES);
    i32::try_from(value.clamp(-bound, bound)).unwrap_or(0)
}

fn named<'a>(names: &'a Names, name: &str) -> &'a [i32] {
    names.get(name).map_or(&[], Vec::as_slice)
}

/// The `n`th line (negative: from the end), optionally among lines with `name`.
///
/// When fewer than `|n|` lines carry the name, "all implicit grid lines are
/// assumed to have that name": the lines past the explicit grid's end for a
/// positive count, those before its start for a negative one (§8.3).
fn nth_line(names: &Names, name: Option<&str>, n: i32, explicit: i32) -> i32 {
    let last = i64::from(explicit) + 1;
    let n = i64::from(if n == 0 { 1 } else { n });
    let Some(name) = name else {
        return clamp_line(if n > 0 { n } else { last + 1 + n });
    };
    let lines = named(names, name);
    let len = i64::try_from(lines.len()).unwrap_or(i64::MAX);
    if n > 0 {
        if n <= len {
            let index = usize::try_from(n - 1).unwrap_or(0);
            return lines.get(index).copied().unwrap_or(1);
        }
        clamp_line(last + (n - len))
    } else {
        let m = -n;
        if m <= len {
            let index = usize::try_from(len - m).unwrap_or(0);
            return lines.get(index).copied().unwrap_or(1);
        }
        clamp_line(1 - (m - len))
    }
}

/// The line an edge names, for the forms that name one.
fn line_of(line: &Line, is_start: bool, explicit: i32, names: &Names) -> Option<i32> {
    match line {
        Line::Num(n, name) => Some(nth_line(names, name.as_deref(), *n, explicit)),
        Line::Ident(ident) => {
            // §8.3: an identifier first tries `ident-start` / `ident-end` (the
            // edges of a named area), then falls back to `1 ident`.
            let area_edge = format!("{ident}-{}", if is_start { "start" } else { "end" });
            match named(names, &area_edge).first() {
                Some(line) => Some(*line),
                None => Some(nth_line(names, Some(ident), 1, explicit)),
            }
        }
        Line::Auto | Line::Span(..) => None,
    }
}

/// `span n [name]` forward from line `from` to the line it ends on.
fn span_forward(from: i32, n: i32, name: Option<&str>, explicit: i32, names: &Names) -> i32 {
    let n = n.max(1);
    let Some(name) = name else {
        return clamp_line(i64::from(from) + i64::from(n));
    };
    let after: Vec<i32> = named(names, name)
        .iter()
        .copied()
        .filter(|l| *l > from)
        .collect();
    let have = i32::try_from(after.len()).unwrap_or(i32::MAX);
    if n <= have {
        let index = usize::try_from(n - 1).unwrap_or(0);
        return after.get(index).copied().unwrap_or(from + 1);
    }
    let base = i64::from(from).max(i64::from(explicit) + 1);
    clamp_line(base + i64::from(n - have))
}

/// `span n [name]` backward from line `from` to the line it starts on.
fn span_backward(from: i32, n: i32, name: Option<&str>, names: &Names) -> i32 {
    let n = n.max(1);
    let Some(name) = name else {
        return clamp_line(i64::from(from) - i64::from(n));
    };
    let before: Vec<i32> = named(names, name)
        .iter()
        .rev()
        .copied()
        .filter(|l| *l < from)
        .collect();
    let have = i32::try_from(before.len()).unwrap_or(i32::MAX);
    if n <= have {
        let index = usize::try_from(n - 1).unwrap_or(0);
        return before.get(index).copied().unwrap_or(from - 1);
    }
    let base = i64::from(from).min(1);
    clamp_line(base - i64::from(n - have))
}

/// Resolve one axis of a grid item's placement properties (§8.3).
pub(crate) fn resolve_axis(start: &Line, end: &Line, explicit: i32, names: &Names) -> Resolved {
    // "If the placement contains two spans, remove the one contributed by the end
    // grid-placement property."
    let end = if matches!(start, Line::Span(..)) && matches!(end, Line::Span(..)) {
        &Line::Auto
    } else {
        end
    };
    let s = line_of(start, true, explicit, names);
    let e = line_of(end, false, explicit, names);
    let span_of = |line: &Line| match line {
        // "If the placement contains only a span for a named line, replace it with
        // a span of 1."
        Line::Span(n, None) => (*n).clamp(1, MAX_LINES),
        _ => 1,
    };

    match (s, e) {
        (Some(s), Some(e)) => {
            // Start after end: swap. Equal: drop the end line.
            let (s, e) = if s > e { (e, s) } else { (s, e) };
            let e = if e == s { s + 1 } else { e };
            Resolved::Definite {
                start: s,
                span: (e - s).min(MAX_LINES),
            }
        }
        (Some(s), None) => {
            let e = match end {
                Line::Span(n, name) => span_forward(s, *n, name.as_deref(), explicit, names),
                _ => s + 1,
            };
            Resolved::Definite {
                start: s,
                span: (e - s).clamp(1, MAX_LINES),
            }
        }
        (None, Some(e)) => {
            let s = match start {
                Line::Span(n, name) => span_backward(e, *n, name.as_deref(), names),
                _ => e - 1,
            };
            Resolved::Definite {
                start: s,
                span: (e - s).clamp(1, MAX_LINES),
            }
        }
        (None, None) => Resolved::Auto {
            span: if matches!(start, Line::Span(..)) {
                span_of(start)
            } else {
                span_of(end)
            },
        },
    }
}

/// One item's placement in both axes, major first.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ItemPlace {
    pub major: Resolved,
    pub minor: Resolved,
}

/// An item's final lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Placed {
    pub major_start: i32,
    pub major_span: i32,
    pub minor_start: i32,
    pub minor_span: i32,
}

/// The result: every item's area and the extent of the implicit grid.
#[derive(Debug)]
pub(crate) struct Placement {
    pub placed: Vec<Placed>,
    /// First and one-past-last line of the implicit grid, major axis.
    pub major_lines: (i32, i32),
    /// The same for the minor axis.
    pub minor_lines: (i32, i32),
}

/// Which cells are taken: for each major line, the occupied minor intervals.
///
/// Sparse on purpose. A dense matrix of 10,000 x 10,000 cells is 100 MB for a page
/// that wrote `grid-row: 1 / span 10000`; a map of intervals costs one entry per
/// row an item crosses, and the work is charged to the budget.
#[derive(Default)]
struct Occupancy {
    rows: BTreeMap<i32, Vec<(i32, i32)>>,
}

impl Occupancy {
    fn overlaps(
        &self,
        major: i32,
        major_span: i32,
        minor: i32,
        minor_span: i32,
        fuel: &mut Fuel,
    ) -> bool {
        let end = minor.saturating_add(minor_span);
        for row in major..major.saturating_add(major_span) {
            if !fuel.spend(1) {
                return false;
            }
            if let Some(list) = self.rows.get(&row) {
                if !fuel.spend(list.len()) {
                    return false;
                }
                if list.iter().any(|(a, b)| *a < end && minor < *b) {
                    return true;
                }
            }
        }
        false
    }

    fn insert(&mut self, p: &Placed, fuel: &mut Fuel) {
        let end = p.minor_start.saturating_add(p.minor_span);
        for row in p.major_start..p.major_start.saturating_add(p.major_span) {
            if !fuel.spend(1) {
                return;
            }
            self.rows.entry(row).or_default().push((p.minor_start, end));
        }
    }
}

/// Run the placement algorithm (§8.5) over items in order-modified document order.
pub(crate) fn place(
    items: &[ItemPlace],
    explicit_major: i32,
    explicit_minor: i32,
    dense: bool,
    fuel: &mut Fuel,
) -> Placement {
    let mut out: Vec<Option<Placed>> = vec![None; items.len()];
    let mut occupied = Occupancy::default();

    // Step 1: anything not auto-positioned.
    for (item, slot) in items.iter().zip(out.iter_mut()) {
        if let (
            Resolved::Definite {
                start: ms,
                span: mspan,
            },
            Resolved::Definite {
                start: ns,
                span: nspan,
            },
        ) = (item.major, item.minor)
        {
            let p = Placed {
                major_start: ms,
                major_span: mspan,
                minor_start: ns,
                minor_span: nspan,
            };
            occupied.insert(&p, fuel);
            *slot = Some(p);
        }
    }

    // Step 2: items locked to a major line (a row, for row flow).
    let mut last_in_line: BTreeMap<i32, i32> = BTreeMap::new();
    for (item, slot) in items.iter().zip(out.iter_mut()) {
        let (
            Resolved::Definite {
                start: ms,
                span: mspan,
            },
            Resolved::Auto { span: nspan },
        ) = (item.major, item.minor)
        else {
            continue;
        };
        let mut c = 1;
        if !dense {
            for row in ms..ms.saturating_add(mspan) {
                if let Some(end) = last_in_line.get(&row) {
                    c = c.max(*end);
                }
            }
        }
        while c < MAX_LINES && occupied.overlaps(ms, mspan, c, nspan, fuel) {
            c += 1;
        }
        let p = Placed {
            major_start: ms,
            major_span: mspan,
            minor_start: c,
            minor_span: nspan,
        };
        occupied.insert(&p, fuel);
        if !dense {
            for row in ms..ms.saturating_add(mspan) {
                last_in_line.insert(row, c.saturating_add(nspan));
            }
        }
        *slot = Some(p);
    }

    // Step 3: the minor axis of the implicit grid.
    let mut minor_lo = 1;
    let mut minor_hi = explicit_minor + 1;
    let mut major_lo = 1;
    for (item, slot) in items.iter().zip(out.iter()) {
        if let Some(p) = slot {
            minor_lo = minor_lo.min(p.minor_start);
            minor_hi = minor_hi.max(p.minor_start.saturating_add(p.minor_span));
            major_lo = major_lo.min(p.major_start);
        } else {
            if let Resolved::Definite { start, span } = item.minor {
                minor_lo = minor_lo.min(start);
                minor_hi = minor_hi.max(start.saturating_add(span));
            }
            if let Resolved::Definite { start, .. } = item.major {
                major_lo = major_lo.min(start);
            }
        }
    }
    let widest = items
        .iter()
        .zip(out.iter())
        .filter(|(_, p)| p.is_none())
        .filter_map(|(i, _)| match i.minor {
            Resolved::Auto { span } => Some(span),
            Resolved::Definite { .. } => None,
        })
        .max()
        .unwrap_or(0);
    if minor_hi - minor_lo < widest {
        minor_hi = minor_lo + widest;
    }

    // Step 4: everything else, with the auto-placement cursor.
    let (mut cur_major, mut cur_minor) = (major_lo, minor_lo);
    for (item, slot) in items.iter().zip(out.iter_mut()) {
        if slot.is_some() {
            continue;
        }
        let Resolved::Auto { span: mspan } = item.major else {
            continue;
        };
        let p = match item.minor {
            Resolved::Definite { start, span } => {
                if dense {
                    cur_major = major_lo;
                } else if start < cur_minor {
                    cur_major += 1;
                }
                cur_minor = start;
                while cur_major < MAX_LINES
                    && occupied.overlaps(cur_major, mspan, start, span, fuel)
                {
                    cur_major += 1;
                }
                Placed {
                    major_start: cur_major,
                    major_span: mspan,
                    minor_start: start,
                    minor_span: span,
                }
            }
            Resolved::Auto { span } => {
                if dense {
                    cur_major = major_lo;
                    cur_minor = minor_lo;
                }
                loop {
                    let mut found = None;
                    let mut c = cur_minor;
                    while c.saturating_add(span) <= minor_hi {
                        if !occupied.overlaps(cur_major, mspan, c, span, fuel) {
                            found = Some(c);
                            break;
                        }
                        c += 1;
                    }
                    if let Some(c) = found {
                        cur_minor = c;
                        break;
                    }
                    cur_major += 1;
                    cur_minor = minor_lo;
                    if cur_major >= MAX_LINES {
                        break;
                    }
                }
                Placed {
                    major_start: cur_major,
                    major_span: mspan,
                    minor_start: cur_minor,
                    minor_span: span,
                }
            }
        };
        occupied.insert(&p, fuel);
        *slot = Some(p);
    }

    let placed: Vec<Placed> = out
        .into_iter()
        .map(|p| {
            p.unwrap_or(Placed {
                major_start: 1,
                major_span: 1,
                minor_start: 1,
                minor_span: 1,
            })
        })
        .collect();

    let mut major_lines = (1, explicit_major + 1);
    let mut minor_lines = (1, explicit_minor + 1);
    for p in &placed {
        major_lines.0 = major_lines.0.min(p.major_start);
        major_lines.1 = major_lines
            .1
            .max(p.major_start.saturating_add(p.major_span));
        minor_lines.0 = minor_lines.0.min(p.minor_start);
        minor_lines.1 = minor_lines
            .1
            .max(p.minor_start.saturating_add(p.minor_span));
    }
    Placement {
        placed,
        major_lines,
        minor_lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(pairs: &[(&str, &[i32])]) -> Names {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.to_vec()))
            .collect()
    }

    /// The §8.2 example: eight columns, lines named A B C A B C A B C.
    #[test]
    fn place_spec_example_named_lines() {
        let n = names(&[("A", &[1, 4, 7]), ("B", &[2, 5, 8]), ("C", &[3, 6, 9])]);
        let ident = |s: &str| Line::Ident(s.to_string());
        let r = |a: Line, b: Line| resolve_axis(&a, &b, 8, &n);
        let def = |start, span| Resolved::Definite { start, span };

        assert_eq!(r(Line::Num(4, None), Line::Auto), def(4, 1));
        assert_eq!(r(Line::Auto, Line::Num(6, None)), def(5, 1));
        assert_eq!(
            r(
                Line::Num(1, Some("C".into())),
                Line::Num(-1, Some("C".into()))
            ),
            def(3, 6)
        );
        assert_eq!(r(ident("C"), Line::Span(1, Some("C".into()))), def(3, 3));
        assert_eq!(
            r(
                Line::Span(1, Some("C".into())),
                Line::Num(-1, Some("C".into()))
            ),
            def(6, 3)
        );
        assert_eq!(
            r(
                Line::Span(1, Some("C".into())),
                Line::Span(1, Some("C".into()))
            ),
            Resolved::Auto { span: 1 }
        );
        assert_eq!(
            r(Line::Num(5, None), Line::Num(-1, Some("C".into()))),
            def(5, 4)
        );
        assert_eq!(
            r(Line::Num(5, None), Line::Span(1, Some("C".into()))),
            def(5, 1)
        );
        assert_eq!(r(Line::Num(8, None), Line::Num(8, None)), def(8, 1));
        assert_eq!(
            r(Line::Num(2, Some("B".into())), Line::Span(1, None)),
            def(5, 1)
        );
    }

    #[test]
    fn place_huge_numbers_are_clamped() {
        let n = Names::new();
        let r = resolve_axis(
            &Line::Num(i32::MAX, None),
            &Line::Span(i32::MAX, None),
            3,
            &n,
        );
        assert_eq!(
            r,
            Resolved::Definite {
                start: MAX_LINES,
                span: 1
            }
        );
    }
}
