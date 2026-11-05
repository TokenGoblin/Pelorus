//! Column width resolution: the pure arithmetic of CSS 2.1 section 17.5.2.
//!
//! No DOM and no styles in here, only `Au` and integers, so the algorithm can be
//! read and tested on its own. Percentages are carried as hundredths of a percent
//! (`5000` is 50%), so there is no float anywhere.

use app_units::Au;

/// What the cells and `<col>` elements say about one grid column.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Col {
    /// Min-content width (border box of the widest cell).
    pub min: Au,
    /// Max-content width.
    pub max: Au,
    /// A stated length width (border box), if any cell or `<col>` has one.
    pub fixed: Option<Au>,
    /// A stated percentage, in hundredths of a percent.
    pub pct: i64,
}

/// A cell (or `<col>`) spanning several columns.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Span {
    pub start: usize,
    pub count: usize,
    pub min: Au,
    pub max: Au,
    pub fixed: Option<Au>,
    pub pct: i64,
}

/// Split `total` in proportion to `weights`, exactly (the shares sum to `total`).
///
/// Cumulative rounding: share `i` is `floor(cum_i * total / W) - floor(cum_(i-1) *
/// total / W)`, so no unit is lost or invented and the result does not depend on
/// anything but the inputs.
pub(crate) fn split(total: Au, weights: &[i64]) -> Vec<Au> {
    let sum: i64 = weights.iter().map(|w| (*w).max(0)).sum();
    let t = i64::from(total.0);
    let mut out = Vec::with_capacity(weights.len());
    if sum <= 0 {
        // Equal shares.
        let n = weights.len() as i64;
        let mut prev = 0i64;
        for i in 1..=weights.len() {
            let cum = t * i as i64 / n.max(1);
            out.push(Au(i32::try_from(cum - prev).unwrap_or(0)));
            prev = cum;
        }
        return out;
    }
    let (mut cum_w, mut prev) = (0i64, 0i64);
    for w in weights {
        cum_w += (*w).max(0);
        let cum = t * cum_w / sum;
        out.push(Au(i32::try_from(cum - prev).unwrap_or(0)));
        prev = cum;
    }
    out
}

/// Fold the spanning cells into the columns (the single-column pass has already
/// been done by the caller). A span's requirement beyond what its columns already
/// provide is spread over them in proportion to their max-content widths.
pub(crate) fn apply_spans(cols: &mut [Col], spans: &[Span], spacing: Au) {
    for s in spans {
        let end = (s.start + s.count).min(cols.len());
        if s.start >= end {
            continue;
        }
        let inner = spacing * (end - s.start - 1) as i32;
        let range = s.start..end;
        // A span's shortfall goes to the columns that have no stated width of
        // their own (all of them, if every one has).
        let free: Vec<usize> = {
            let f: Vec<usize> = range.clone().filter(|i| cols[*i].fixed.is_none()).collect();
            if f.is_empty() {
                range.clone().collect()
            } else {
                f
            }
        };
        let weights: Vec<i64> = free
            .iter()
            .map(|i| i64::from(cols[*i].max.0.max(1)))
            .collect();

        let have: Au = cols[range.clone()].iter().map(|c| c.min).sum();
        let need = s.min - inner;
        if need > have {
            for (i, d) in free.iter().zip(split(need - have, &weights)) {
                let c = &mut cols[*i];
                c.min += d;
                if c.max < c.min {
                    c.max = c.min;
                }
            }
        }
        let have: Au = cols[range.clone()].iter().map(|c| c.max).sum();
        let need = s.max - inner;
        if need > have {
            for (i, d) in free.iter().zip(split(need - have, &weights)) {
                cols[*i].max += d;
            }
        }
        if let Some(f) = s.fixed {
            let have: Au = cols[range.clone()]
                .iter()
                .map(|c| c.fixed.unwrap_or(c.min))
                .sum();
            let need = f - inner;
            if need > have {
                let free: Vec<usize> = range.clone().filter(|i| cols[*i].fixed.is_none()).collect();
                if !free.is_empty() {
                    let w: Vec<i64> = free
                        .iter()
                        .map(|i| i64::from(cols[*i].max.0.max(1)))
                        .collect();
                    for (i, d) in free.iter().zip(split(need - have, &w)) {
                        let c = &mut cols[*i];
                        c.fixed = Some(c.min + d);
                        c.max = c.max.max(c.min + d);
                    }
                }
            }
        }
        if s.pct > 0 {
            let have: i64 = cols[range.clone()].iter().map(|c| c.pct).sum();
            if s.pct > have {
                let free: Vec<usize> = range.clone().filter(|i| cols[*i].pct == 0).collect();
                if !free.is_empty() {
                    let w: Vec<i64> = free
                        .iter()
                        .map(|i| i64::from(cols[*i].max.0.max(1)))
                        .collect();
                    let total = i32::try_from(s.pct - have).unwrap_or(0);
                    for (i, d) in free.iter().zip(split(Au(total), &w)) {
                        cols[*i].pct = i64::from(d.0);
                    }
                }
            }
        }
    }
}

/// The table's own min-content and max-content widths from its columns: the sum
/// of the columns (plus `extra`, the spacing and the table's edges), where a
/// percentage column makes the table at least wide enough that the percentage of
/// it covers the column's max-content.
pub(crate) fn table_intrinsic(cols: &[Col], extra: Au) -> (Au, Au) {
    let min: Au = cols.iter().map(|c| c.min).sum();
    let mut max: Au = cols.iter().map(|c| c.max).sum();
    let pct_total: i64 = cols.iter().map(|c| c.pct).sum::<i64>().min(10_000);
    for c in cols {
        if c.pct > 0 {
            let needed = i64::from(c.max.0) * 10_000 / c.pct;
            max = max.max(Au(i32::try_from(needed.min(1 << 29)).unwrap_or(0)));
        }
    }
    if pct_total > 0 && pct_total < 10_000 {
        let non_pct: i64 = cols
            .iter()
            .filter(|c| c.pct == 0)
            .map(|c| i64::from(c.max.0))
            .sum();
        let needed = non_pct * 10_000 / (10_000 - pct_total);
        max = max.max(Au(i32::try_from(needed.min(1 << 29)).unwrap_or(0)));
    }
    (min + extra, max + extra)
}

/// Distribute `total` (the width available to the columns, spacing excluded) over
/// the columns of an auto-layout table. Returns the column widths; their sum is
/// `max(total, sum of the minimums)`.
pub(crate) fn distribute(cols: &[Col], total: Au) -> Vec<Au> {
    let n = cols.len();
    let mut w: Vec<Au> = cols.iter().map(|c| c.min).collect();
    let min_sum: Au = w.iter().copied().sum();
    if n == 0 || total <= min_sum {
        return w;
    }

    // 1. Percentage columns take their share of the table.
    let pct_sum: i64 = cols.iter().map(|c| c.pct).sum();
    for (i, c) in cols.iter().enumerate() {
        if c.pct > 0 {
            let scaled = if pct_sum > 10_000 {
                c.pct * 10_000 / pct_sum
            } else {
                c.pct
            };
            let target = Au(i32::try_from(i64::from(total.0) * scaled / 10_000).unwrap_or(0));
            w[i] = target.max(c.min);
        }
    }
    let mut used: Au = w.iter().copied().sum();
    if used > total {
        // The percentages ask for more than there is: take it back from the
        // percentage columns' excess over their minimums.
        let over = used - total;
        let slack: Vec<i64> = cols
            .iter()
            .zip(&w)
            .map(|(c, w)| {
                if c.pct > 0 {
                    i64::from((*w - c.min).0.max(0))
                } else {
                    0
                }
            })
            .collect();
        let total_slack: i64 = slack.iter().sum();
        if total_slack > 0 {
            let take = split(
                over.min(Au(i32::try_from(total_slack).unwrap_or(0))),
                &slack,
            );
            for (x, d) in w.iter_mut().zip(take) {
                *x -= d;
            }
        }
        used = w.iter().copied().sum();
    }
    let mut remaining = total - used;
    if remaining <= Au(0) {
        return w;
    }

    // 2. Columns with a stated length grow to it, then auto columns to their
    //    max-content width.
    for pass in 0..2 {
        let idx: Vec<usize> = (0..n)
            .filter(|i| {
                cols[*i].pct == 0
                    && if pass == 0 {
                        cols[*i].fixed.is_some()
                    } else {
                        cols[*i].fixed.is_none()
                    }
            })
            .collect();
        let want: Vec<i64> = idx
            .iter()
            .map(|i| {
                let c = &cols[*i];
                let target = if pass == 0 {
                    c.fixed.unwrap_or(c.max).max(c.min)
                } else {
                    c.max
                };
                i64::from((target - w[*i]).0.max(0))
            })
            .collect();
        let total_want: i64 = want.iter().sum();
        if total_want == 0 {
            continue;
        }
        let give = remaining.min(Au(i32::try_from(total_want).unwrap_or(i32::MAX)));
        for (i, d) in idx.iter().zip(split(give, &want)) {
            w[*i] += d;
        }
        remaining -= give;
        if remaining <= Au(0) {
            return w;
        }
    }

    // 3. Still room: auto columns share it by max-content width, else columns with
    //    a stated length do, else percentage columns.
    for pass in 0..3 {
        let idx: Vec<usize> = (0..n)
            .filter(|i| match pass {
                0 => cols[*i].pct == 0 && cols[*i].fixed.is_none(),
                1 => cols[*i].pct == 0,
                _ => true,
            })
            .collect();
        if idx.is_empty() {
            continue;
        }
        let weights: Vec<i64> = idx.iter().map(|i| i64::from(w[*i].0)).collect();
        for (i, d) in idx.iter().zip(split(remaining, &weights)) {
            w[*i] += d;
        }
        return w;
    }
    w
}

/// Fixed layout (17.5.2.1): the widths come from the `<col>` elements and the
/// first row only, and the cells' contents are never measured.
///
/// `known[i]` is a column's stated width, `None` when nothing in the first row or
/// on a `<col>` states one. Returns the widths and the width they add up to.
pub(crate) fn fixed_layout(known: &[Option<Au>], total: Au) -> Vec<Au> {
    let sum_known: Au = known.iter().flatten().copied().sum();
    let unknown: Vec<usize> = (0..known.len()).filter(|i| known[*i].is_none()).collect();
    let mut w: Vec<Au> = known.iter().map(|k| k.unwrap_or(Au(0))).collect();
    let left = total - sum_known;
    if left <= Au(0) {
        return w;
    }
    if !unknown.is_empty() {
        let ones = vec![1i64; unknown.len()];
        for (i, d) in unknown.iter().zip(split(left, &ones)) {
            w[*i] = d;
        }
    } else {
        let weights: Vec<i64> = w.iter().map(|x| i64::from(x.0)).collect();
        for (x, d) in w.iter_mut().zip(split(left, &weights)) {
            *x += d;
        }
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    fn col(min: i32, max: i32) -> Col {
        Col {
            min: px(min),
            max: px(max),
            ..Col::default()
        }
    }

    #[test]
    fn table_columns_split_is_exact() {
        let s = split(Au(100), &[1, 1, 1]);
        assert_eq!(s.iter().copied().sum::<Au>(), Au(100));
    }

    #[test]
    fn table_columns_narrow_total_gives_minimums() {
        let w = distribute(&[col(50, 100), col(30, 60)], px(10));
        assert_eq!(w, vec![px(50), px(30)]);
    }

    #[test]
    fn table_columns_room_up_to_max_then_proportional() {
        let w = distribute(&[col(10, 100), col(10, 300)], px(400));
        assert_eq!(w, vec![px(100), px(300)]);
        let w = distribute(&[col(10, 100), col(10, 300)], px(800));
        assert_eq!(w.iter().copied().sum::<Au>(), px(800));
        assert_eq!(w, vec![px(200), px(600)]);
    }

    #[test]
    fn table_columns_percentages_take_their_share() {
        let mut a = col(10, 50);
        a.pct = 5000;
        let w = distribute(&[a, col(10, 50)], px(400));
        assert_eq!(w[0], px(200));
        assert_eq!(w.iter().copied().sum::<Au>(), px(400));
    }

    #[test]
    fn table_columns_fixed_layout_splits_the_unknown_equally() {
        let w = fixed_layout(&[Some(px(100)), None, None], px(400));
        assert_eq!(w, vec![px(100), px(150), px(150)]);
    }

    #[test]
    fn table_columns_span_requirement_is_spread() {
        let mut cols = [col(10, 20), col(10, 20)];
        apply_spans(
            &mut cols,
            &[Span {
                start: 0,
                count: 2,
                min: px(100),
                max: px(100),
                fixed: None,
                pct: 0,
            }],
            Au(0),
        );
        assert_eq!(cols[0].min + cols[1].min, px(100));
    }
}
