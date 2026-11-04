//! The track sizing algorithm: css-grid-1 section 11.
//!
//! Pure and axis-agnostic. The caller resolves percentages, builds one [`Track`]
//! per grid track and one [`Item`] per grid item (with its contributions already
//! measured in this axis), and gets track sizes back in `Track::base`.
//!
//! Gutters are not tracks here. The spec treats them as fixed, empty tracks that
//! spanning items cross; that is equivalent to adding the gutters an item spans to
//! the tracks' sum when finding how much space it still needs, and never giving a
//! gutter any of it. `gaps[i]` is the gutter after track `i` (zero after the last,
//! and after a collapsed track).
//!
//! # Integer arithmetic
//!
//! Space is shared in whole app units. Equal shares put the remainder, one unit at
//! a time, on the earliest tracks, so nothing is lost; an `fr` is kept as the
//! fraction it is ([`Frac`]) until it has been multiplied by a flex factor, so one
//! track's size is one division and not a division of a division.

use app_units::Au;

use super::num::{Fuel, narrow, wide};

/// What a track's minimum sizing function resolved to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MinFn {
    Fixed(Au),
    Auto,
    MinContent,
    MaxContent,
}

/// What a track's maximum sizing function resolved to. `Flex` is in thousandths.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MaxFn {
    Fixed(Au),
    Auto,
    MinContent,
    MaxContent,
    FitContent(Au),
    Flex(i64),
}

/// A grid track while it is being sized.
#[derive(Clone, Debug)]
pub(crate) struct Track {
    pub min: MinFn,
    pub max: MaxFn,
    /// The base size; the used size when sizing is done.
    pub base: Au,
    /// The growth limit; `None` is infinity.
    pub limit: Option<Au>,
    growable: bool,
}

impl Track {
    #[must_use]
    pub(crate) fn new(min: MinFn, max: MaxFn) -> Self {
        Self {
            min,
            max,
            base: Au(0),
            limit: None,
            growable: false,
        }
    }

    fn flexible(&self) -> bool {
        matches!(self.max, MaxFn::Flex(_))
    }

    /// An intrinsic minimum: `auto`, `min-content` or `max-content`.
    fn intrinsic_min(&self) -> bool {
        !matches!(self.min, MinFn::Fixed(_))
    }

    /// An intrinsic maximum. `auto` and `fit-content()` count as `max-content`
    /// (section 11.3, "max track sizing function").
    fn intrinsic_max(&self) -> bool {
        matches!(
            self.max,
            MaxFn::Auto | MaxFn::MinContent | MaxFn::MaxContent | MaxFn::FitContent(_)
        )
    }

    fn fixed_max(&self) -> Option<Au> {
        match self.max {
            MaxFn::Fixed(a) | MaxFn::FitContent(a) => Some(a),
            _ => None,
        }
    }
}

/// A grid item as the sizing algorithm sees it in one axis.
///
/// All three contributions are outer sizes (margins included), already floored at
/// zero by the caller.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Item {
    pub start: usize,
    pub span: usize,
    pub min_content: Au,
    pub max_content: Au,
    /// The minimum contribution (section 11.5): the outer size when its used
    /// minimum size is taken as its preferred size.
    pub minimum: Au,
}

/// The space the tracks are sized into.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Avail {
    Definite(Au),
    MinContent,
    MaxContent,
}

/// The container's `min-*` and `max-*` in this axis, as inner sizes.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub min: Au,
    pub max: Option<Au>,
}

/// An `fr`: `num / den` app units per flex unit, with `den` in thousandths.
#[derive(Clone, Copy, Debug)]
struct Frac {
    num: i128,
    den: i128,
}

impl Frac {
    const ZERO: Self = Self { num: 0, den: 1 };

    fn of(self, flex_milli: i64) -> Au {
        if self.den <= 0 || self.num <= 0 {
            return Au(0);
        }
        narrow(self.num * i128::from(flex_milli) / (self.den * 1000))
    }

    fn greater(self, other: Self) -> bool {
        self.num * other.den > other.num * self.den
    }
}

fn zero_floor(a: Au) -> Au {
    a.max(Au(0))
}

fn sum_bases(tracks: &[Track], gaps: &[Au]) -> Au {
    let mut total = Au(0);
    for t in tracks {
        total += t.base;
    }
    for g in gaps {
        total += *g;
    }
    total
}

fn span_gaps(gaps: &[Au], start: usize, span: usize) -> Au {
    let mut total = Au(0);
    for g in gaps.iter().skip(start).take(span.saturating_sub(1)) {
        total += *g;
    }
    total
}

/// Share `space` equally among `idx`, giving each at most `room(i)` more, and
/// return what is left over. Remainder units go to the earliest tracks.
fn fill(
    mut space: i64,
    idx: &[usize],
    room: &dyn Fn(usize) -> i64,
    given: &mut [i64],
    fuel: &mut Fuel,
) -> i64 {
    let mut active: Vec<usize> = idx.to_vec();
    while space > 0 && !active.is_empty() {
        if !fuel.spend(active.len()) {
            break;
        }
        let n = i64::try_from(active.len()).unwrap_or(1);
        let share = space / n;
        let extra = space % n;
        let mut spent = 0;
        let mut still = Vec::with_capacity(active.len());
        for (k, i) in active.iter().enumerate() {
            let want = share + i64::from(i64::try_from(k).unwrap_or(0) < extra);
            let have = given.get(*i).copied().unwrap_or(0);
            let can = (room(*i) - have).max(0);
            let take = want.min(can);
            if let Some(g) = given.get_mut(*i) {
                *g += take;
            }
            spent += take;
            if take < want || can == take {
                // Reached its limit: frozen.
            } else {
                still.push(*i);
            }
        }
        space -= spent;
        if spent == 0 {
            break;
        }
        active = still;
    }
    space
}

const INF: i64 = i64::MAX / 4;

/// What a distribution adds to: base sizes or growth limits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Base,
    Limit,
}

/// Which tracks receive space once the limits are used up (section 11.5.1).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Beyond {
    /// Affected tracks with an intrinsic maximum, else all affected tracks.
    IntrinsicMax,
    /// Affected tracks with a `max-content` maximum, else all affected tracks.
    MaxContentMax,
}

struct Scratch {
    planned: Vec<i64>,
    given: Vec<i64>,
}

/// "Distribute extra space" (section 11.5.1) for `group` of items.
#[allow(clippy::too_many_arguments)]
fn distribute(
    tracks: &mut [Track],
    gaps: &[Au],
    items: &[Item],
    group: &[usize],
    contribution: &dyn Fn(usize, &Item) -> Au,
    affected: &dyn Fn(&Track) -> bool,
    target: Target,
    beyond: Beyond,
    scratch: &mut Scratch,
    fuel: &mut Fuel,
) {
    let size_of = |t: &Track| match target {
        Target::Base => t.base,
        Target::Limit => t.limit.unwrap_or(t.base),
    };
    let limit_of = |t: &Track| -> i64 {
        match target {
            Target::Base => {
                let cap = match t.max {
                    MaxFn::FitContent(a) => Some(wide(a)),
                    _ => None,
                };
                match (t.limit, cap) {
                    (Some(l), Some(c)) => wide(l).min(c),
                    (Some(l), None) => wide(l),
                    (None, Some(c)) => c,
                    (None, None) => INF,
                }
            }
            Target::Limit => match t.limit {
                Some(l) if !t.growable => wide(l),
                _ => match t.max {
                    MaxFn::FitContent(a) => wide(a),
                    _ => INF,
                },
            },
        }
    };

    let mut touched: Vec<usize> = Vec::new();
    for &index in group {
        let Some(item) = items.get(index) else {
            continue;
        };
        let end = (item.start + item.span).min(tracks.len());
        if item.start >= end || !fuel.spend(end - item.start) {
            continue;
        }
        let range: Vec<usize> = (item.start..end).collect();
        let mut sum = span_gaps(gaps, item.start, end - item.start);
        for i in &range {
            sum += tracks.get(*i).map_or(Au(0), size_of);
        }
        let space = wide(zero_floor(contribution(index, item) - sum));
        if space == 0 {
            continue;
        }
        let aff: Vec<usize> = range
            .iter()
            .copied()
            .filter(|i| tracks.get(*i).is_some_and(affected))
            .collect();
        if aff.is_empty() {
            continue;
        }
        for i in &range {
            if let Some(g) = scratch.given.get_mut(*i) {
                *g = 0;
            }
        }

        let tr: &[Track] = tracks;
        let room = |i: usize| -> i64 {
            tr.get(i)
                .map_or(0, |t| (limit_of(t) - wide(size_of(t))).max(0))
        };
        let mut left = fill(space, &aff, &room, &mut scratch.given, fuel);
        if left > 0 && aff.len() < range.len() {
            let others: Vec<usize> = range.iter().copied().filter(|i| !aff.contains(i)).collect();
            left = fill(left, &others, &room, &mut scratch.given, fuel);
        }
        if left > 0 {
            let preferred: Vec<usize> = aff
                .iter()
                .copied()
                .filter(|i| {
                    tr.get(*i).is_some_and(|t| match beyond {
                        Beyond::IntrinsicMax => t.intrinsic_max(),
                        Beyond::MaxContentMax => {
                            matches!(
                                t.max,
                                MaxFn::MaxContent | MaxFn::Auto | MaxFn::FitContent(_)
                            )
                        }
                    })
                })
                .collect();
            let set = if preferred.is_empty() {
                &aff
            } else {
                &preferred
            };
            // Beyond the limits there is no cap, except that a `fit-content()`
            // track stops at its argument (section 11.5.1).
            let beyond_room = |i: usize| -> i64 {
                tr.get(i).map_or(0, |t| match t.max {
                    MaxFn::FitContent(a) => (wide(a) - wide(size_of(t))).max(0),
                    _ => INF,
                })
            };
            let _ = fill(left, set, &beyond_room, &mut scratch.given, fuel);
        }
        for i in &range {
            let g = scratch.given.get(*i).copied().unwrap_or(0);
            if g > 0 {
                if let Some(p) = scratch.planned.get_mut(*i)
                    && g > *p
                {
                    *p = g;
                }
                touched.push(*i);
            }
        }
    }

    touched.sort_unstable();
    touched.dedup();
    for i in touched {
        let planned = scratch.planned.get(i).copied().unwrap_or(0);
        if let Some(p) = scratch.planned.get_mut(i) {
            *p = 0;
        }
        let Some(t) = tracks.get_mut(i) else { continue };
        let add = narrow(i128::from(planned));
        match target {
            Target::Base => t.base += add,
            Target::Limit => {
                if t.limit.is_none() {
                    t.growable = true;
                }
                t.limit = Some(t.limit.unwrap_or(t.base) + add);
            }
        }
    }
}

/// Everything in section 11.3 for one axis.
///
/// `stretch` is whether `justify-content`/`align-content` is `normal` or `stretch`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn size_tracks(
    tracks: &mut [Track],
    gaps: &[Au],
    items: &[Item],
    avail: Avail,
    limits: Limits,
    stretch: bool,
    fuel: &mut Fuel,
) {
    let constrained = !matches!(avail, Avail::Definite(_));

    // 11.4 Initialize track sizes.
    for t in tracks.iter_mut() {
        t.base = match t.min {
            MinFn::Fixed(a) => zero_floor(a),
            _ => Au(0),
        };
        t.limit = match t.max {
            MaxFn::Fixed(a) => Some(zero_floor(a)),
            _ => None,
        };
        t.growable = false;
        if let Some(l) = t.limit {
            t.limit = Some(l.max(t.base));
        }
    }

    // 11.5 Resolve intrinsic track sizes.
    let mut scratch = Scratch {
        planned: vec![0; tracks.len()],
        given: vec![0; tracks.len()],
    };

    // Single-span items in tracks that are intrinsic and not flexible.
    {
        let n = tracks.len();
        let mut base_cand: Vec<Option<Au>> = vec![None; n];
        let mut limit_cand: Vec<Option<Au>> = vec![None; n];
        for item in items.iter().filter(|i| i.span == 1) {
            let Some(t) = tracks.get(item.start) else {
                continue;
            };
            if t.flexible() || !(t.intrinsic_min() || t.intrinsic_max()) {
                continue;
            }
            let b = match t.min {
                MinFn::MinContent => Some(item.min_content),
                MinFn::MaxContent => Some(item.max_content),
                MinFn::Auto => Some(if constrained {
                    limited(item.min_content, item.minimum, t.fixed_max())
                } else {
                    item.minimum
                }),
                MinFn::Fixed(_) => None,
            };
            let l = match t.max {
                MaxFn::MinContent => Some(item.min_content),
                MaxFn::MaxContent | MaxFn::Auto | MaxFn::FitContent(_) => Some(item.max_content),
                _ => None,
            };
            if let (Some(b), Some(slot)) = (b, base_cand.get_mut(item.start)) {
                *slot = Some(slot.map_or(b, |o| o.max(b)));
            }
            if let (Some(l), Some(slot)) = (l, limit_cand.get_mut(item.start)) {
                *slot = Some(slot.map_or(l, |o| o.max(l)));
            }
        }
        for (i, t) in tracks.iter_mut().enumerate() {
            if let Some(Some(b)) = base_cand.get(i) {
                t.base = t.base.max(zero_floor(*b));
            }
            if let Some(Some(l)) = limit_cand.get(i) {
                let mut l = zero_floor(*l);
                if let MaxFn::FitContent(a) = t.max {
                    l = l.min(a);
                }
                t.limit = Some(l);
            }
            if let Some(l) = t.limit {
                t.limit = Some(l.max(t.base));
            }
        }
    }

    // Spanning items that do not cross a flexible track, by increasing span.
    let crosses_flex = |item: &Item| {
        tracks
            .iter()
            .skip(item.start)
            .take(item.span)
            .any(Track::flexible)
    };
    let flex_crossing: Vec<bool> = items.iter().map(crosses_flex).collect();
    let mut spanning: Vec<usize> = (0..items.len())
        .filter(|i| {
            items.get(*i).is_some_and(|it| it.span >= 2)
                && !flex_crossing.get(*i).copied().unwrap_or(false)
        })
        .collect();
    spanning.sort_by_key(|i| items.get(*i).map_or(0, |it| it.span));

    let mut at = 0;
    while let Some(&first) = spanning.get(at) {
        let span = items.get(first).map_or(0, |it| it.span);
        let mut stop = at;
        while spanning
            .get(stop)
            .is_some_and(|i| items.get(*i).is_some_and(|it| it.span == span))
        {
            stop += 1;
        }
        let group: Vec<usize> = spanning.get(at..stop).unwrap_or(&[]).to_vec();
        at = stop;

        // The upper limit for a limited contribution is the sum of the fixed
        // maximums an item spans, applied if it only spans such tracks.
        let caps: Vec<Option<Au>> = items
            .iter()
            .map(|item| {
                let mut total = Au(0);
                for t in tracks.iter().skip(item.start).take(item.span) {
                    total += t.fixed_max()?;
                }
                Some(total + span_gaps(gaps, item.start, item.span))
            })
            .collect();
        let cap_of = |index: usize| caps.get(index).copied().flatten();

        // (a) Intrinsic minimums.
        distribute(
            tracks,
            gaps,
            items,
            &group,
            &|ix, it| {
                if constrained {
                    limited(it.min_content, it.minimum, cap_of(ix))
                } else {
                    it.minimum
                }
            },
            &Track::intrinsic_min,
            Target::Base,
            Beyond::IntrinsicMax,
            &mut scratch,
            fuel,
        );
        // (b) Content-based minimums.
        distribute(
            tracks,
            gaps,
            items,
            &group,
            &|_, it| it.min_content,
            &|t| matches!(t.min, MinFn::MinContent | MinFn::MaxContent),
            Target::Base,
            Beyond::IntrinsicMax,
            &mut scratch,
            fuel,
        );
        // (c) Max-content minimums.
        if avail == Avail::MaxContent {
            distribute(
                tracks,
                gaps,
                items,
                &group,
                &|ix, it| limited(it.max_content, it.minimum, cap_of(ix)),
                &|t| matches!(t.min, MinFn::Auto | MinFn::MaxContent),
                Target::Base,
                Beyond::MaxContentMax,
                &mut scratch,
                fuel,
            );
        }
        distribute(
            tracks,
            gaps,
            items,
            &group,
            &|_, it| it.max_content,
            &|t| matches!(t.min, MinFn::MaxContent),
            Target::Base,
            Beyond::MaxContentMax,
            &mut scratch,
            fuel,
        );
        for t in tracks.iter_mut() {
            if let Some(l) = t.limit {
                t.limit = Some(l.max(t.base));
            }
        }
        // (d) Intrinsic maximums.
        distribute(
            tracks,
            gaps,
            items,
            &group,
            &|_, it| it.min_content,
            &Track::intrinsic_max,
            Target::Limit,
            Beyond::IntrinsicMax,
            &mut scratch,
            fuel,
        );
        // (e) Max-content maximums.
        distribute(
            tracks,
            gaps,
            items,
            &group,
            &|_, it| it.max_content,
            &|t| {
                matches!(
                    t.max,
                    MaxFn::MaxContent | MaxFn::Auto | MaxFn::FitContent(_)
                )
            },
            Target::Limit,
            Beyond::IntrinsicMax,
            &mut scratch,
            fuel,
        );
    }

    // Items that span a flexible track, together, space going to flexible tracks.
    distribute_flex(tracks, gaps, items, &flex_crossing, constrained, fuel);

    // Tracks still infinite (no items, or flexible): the limit is the base.
    for t in tracks.iter_mut() {
        let l = t.limit.unwrap_or(t.base);
        t.limit = Some(l.max(t.base));
    }

    // 11.6 Maximize tracks.
    maximize(tracks, gaps, avail, limits, fuel);

    // 11.7 Expand flexible tracks.
    expand_flexible(tracks, gaps, items, avail, limits);

    // 11.8 Stretch auto tracks.
    if stretch {
        let target = match avail {
            Avail::Definite(a) => Some(a),
            _ if limits.min > Au(0) => Some(limits.min),
            _ => None,
        };
        if let Some(a) = target {
            let free = wide(a - sum_bases(tracks, gaps));
            let idx: Vec<usize> = tracks
                .iter()
                .enumerate()
                .filter(|(_, t)| matches!(t.max, MaxFn::Auto))
                .map(|(i, _)| i)
                .collect();
            if free > 0 && !idx.is_empty() {
                let mut given = vec![0i64; tracks.len()];
                let _ = fill(free, &idx, &|_| INF, &mut given, fuel);
                for (t, g) in tracks.iter_mut().zip(given) {
                    t.base += narrow(i128::from(g));
                }
            }
        }
    }
}

/// A "limited" min- or max-content contribution: the contribution limited by the
/// spanned fixed maximums, and floored by the minimum contribution.
fn limited(contribution: Au, minimum: Au, cap: Option<Au>) -> Au {
    let capped = cap.map_or(contribution, |c| contribution.min(c));
    capped.max(minimum)
}

/// Section 11.5 step 4: items crossing flexible tracks.
fn distribute_flex(
    tracks: &mut [Track],
    gaps: &[Au],
    items: &[Item],
    crossing: &[bool],
    constrained: bool,
    fuel: &mut Fuel,
) {
    let mut planned: Vec<i64> = vec![0; tracks.len()];
    for (index, item) in items.iter().enumerate() {
        if !crossing.get(index).copied().unwrap_or(false) {
            continue;
        }
        let end = (item.start + item.span).min(tracks.len());
        if item.start >= end || !fuel.spend(end - item.start) {
            continue;
        }
        let contribution = if constrained {
            limited(item.min_content, item.minimum, None)
        } else {
            item.minimum
        };
        let mut sum = span_gaps(gaps, item.start, end - item.start);
        for t in tracks.iter().take(end).skip(item.start) {
            sum += t.base;
        }
        let space = wide(zero_floor(contribution - sum));
        if space == 0 {
            continue;
        }
        let flex: Vec<(usize, i64)> = (item.start..end)
            .filter_map(|i| match tracks.get(i)?.max {
                MaxFn::Flex(f) => Some((i, f)),
                _ => None,
            })
            .collect();
        let total: i64 = flex.iter().map(|(_, f)| *f).sum();
        let count = i64::try_from(flex.len()).unwrap_or(1).max(1);
        for (i, f) in &flex {
            // A sum of factors of at least one shares by ratio; below one, that
            // proportion is shared by ratio and the rest equally.
            let share = if total >= 1000 {
                share_of(space, *f, total)
            } else if total > 0 {
                let part = share_of(space, total, 1000);
                share_of(part, *f, total) + (space - part) / count
            } else {
                space / count
            };
            if let Some(p) = planned.get_mut(*i) {
                *p = (*p).max(share);
            }
        }
        // Applied after each item would make the result order-dependent; the
        // spec's "planned increase" is per round, so apply at the end of the loop.
    }
    for (t, p) in tracks.iter_mut().zip(planned) {
        t.base += narrow(i128::from(p));
        if let Some(l) = t.limit {
            t.limit = Some(l.max(t.base));
        }
    }
}

fn share_of(space: i64, part: i64, total: i64) -> i64 {
    if total <= 0 {
        return 0;
    }
    i64::try_from(i128::from(space) * i128::from(part) / i128::from(total)).unwrap_or(0)
}

/// Section 11.6.
fn maximize(tracks: &mut [Track], gaps: &[Au], avail: Avail, limits: Limits, fuel: &mut Fuel) {
    match avail {
        Avail::MinContent => {}
        Avail::MaxContent => {
            let before: Vec<Au> = tracks.iter().map(|t| t.base).collect();
            for t in tracks.iter_mut() {
                t.base = t.limit.unwrap_or(t.base).max(t.base);
            }
            if let Some(max) = limits.max
                && sum_bases(tracks, gaps) > max
            {
                for (t, b) in tracks.iter_mut().zip(before) {
                    t.base = b;
                }
                maximize_into(tracks, gaps, max, fuel);
            }
        }
        Avail::Definite(a) => maximize_into(tracks, gaps, a, fuel),
    }
}

fn maximize_into(tracks: &mut [Track], gaps: &[Au], available: Au, fuel: &mut Fuel) {
    let free = wide(zero_floor(available - sum_bases(tracks, gaps)));
    if free == 0 {
        return;
    }
    let idx: Vec<usize> = (0..tracks.len()).collect();
    let mut given = vec![0i64; tracks.len()];
    let room = |i: usize| -> i64 {
        tracks.get(i).map_or(0, |t| match t.limit {
            Some(l) => wide(l - t.base).max(0),
            None => INF,
        })
    };
    let _ = fill(free, &idx, &room, &mut given, fuel);
    for (t, g) in tracks.iter_mut().zip(given) {
        t.base += narrow(i128::from(g));
    }
}

/// "Find the size of an fr" (section 11.7.1) over `idx`, filling `space`.
fn find_fr(tracks: &[Track], idx: &[usize], gutters: Au, space: Au) -> Frac {
    let mut flexible: Vec<usize> = idx
        .iter()
        .copied()
        .filter(|i| tracks.get(*i).is_some_and(Track::flexible))
        .collect();
    loop {
        if flexible.is_empty() {
            return Frac::ZERO;
        }
        let mut leftover = wide(space - gutters);
        let mut sum: i64 = 0;
        for i in idx {
            let Some(t) = tracks.get(*i) else { continue };
            if flexible.contains(i) {
                if let MaxFn::Flex(f) = t.max {
                    sum += f;
                }
            } else {
                leftover -= wide(t.base);
            }
        }
        let sum = sum.max(1000);
        let frac = Frac {
            num: i128::from(leftover) * 1000,
            den: i128::from(sum),
        };
        let before = flexible.len();
        flexible.retain(|i| {
            let Some(t) = tracks.get(*i) else {
                return false;
            };
            let MaxFn::Flex(f) = t.max else { return false };
            // hypothetical size * factor >= base
            i128::from(leftover) * i128::from(f) >= i128::from(wide(t.base)) * i128::from(sum)
        });
        if flexible.len() == before {
            return frac;
        }
    }
}

/// Section 11.7.
fn expand_flexible(
    tracks: &mut [Track],
    gaps: &[Au],
    items: &[Item],
    avail: Avail,
    limits: Limits,
) {
    if !tracks.iter().any(Track::flexible) {
        return;
    }
    let all: Vec<usize> = (0..tracks.len()).collect();
    let mut gutters = Au(0);
    for g in gaps {
        gutters += *g;
    }

    let definite = |tracks: &[Track], space: Au| -> Frac {
        if wide(space - sum_bases(tracks, gaps)) <= 0 {
            return Frac::ZERO;
        }
        find_fr(tracks, &all, gutters, space)
    };
    let apply = |tracks: &mut [Track], frac: Frac| {
        for t in tracks.iter_mut() {
            if let MaxFn::Flex(f) = t.max {
                let product = frac.of(f);
                if product > t.base {
                    t.base = product;
                }
            }
        }
    };

    match avail {
        Avail::MinContent => {}
        Avail::Definite(a) => {
            let frac = definite(tracks, a);
            apply(tracks, frac);
        }
        Avail::MaxContent => {
            let mut frac = Frac::ZERO;
            for t in tracks.iter() {
                if let MaxFn::Flex(f) = t.max {
                    let candidate = if f > 1000 {
                        Frac {
                            num: i128::from(wide(t.base)) * 1000,
                            den: i128::from(f),
                        }
                    } else {
                        Frac {
                            num: i128::from(wide(t.base)) * 1000,
                            den: 1000,
                        }
                    };
                    if candidate.greater(frac) {
                        frac = candidate;
                    }
                }
            }
            for item in items {
                let end = (item.start + item.span).min(tracks.len());
                let idx: Vec<usize> = (item.start..end).collect();
                if !idx
                    .iter()
                    .any(|i| tracks.get(*i).is_some_and(Track::flexible))
                {
                    continue;
                }
                let inner = span_gaps(gaps, item.start, end.saturating_sub(item.start));
                let candidate = find_fr(tracks, &idx, inner, item.max_content);
                if candidate.greater(frac) {
                    frac = candidate;
                }
            }
            // The grid this would make, against the container's min and max.
            let mut total = gutters;
            for t in tracks.iter() {
                total += match t.max {
                    MaxFn::Flex(f) => t.base.max(frac.of(f)),
                    _ => t.base,
                };
            }
            if total < limits.min {
                let frac = definite(tracks, limits.min);
                apply(tracks, frac);
            } else if let Some(max) = limits.max.filter(|m| total > *m) {
                let frac = definite(tracks, max);
                apply(tracks, frac);
            } else {
                apply(tracks, frac);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    fn fixed(a: i32) -> Track {
        Track::new(MinFn::Fixed(px(a)), MaxFn::Fixed(px(a)))
    }

    fn run(tracks: &mut [Track], items: &[Item], avail: Avail) {
        let gaps = vec![Au(0); tracks.len()];
        let mut fuel = Fuel::new();
        size_tracks(
            tracks,
            &gaps,
            items,
            avail,
            Limits {
                min: Au(0),
                max: None,
            },
            true,
            &mut fuel,
        );
    }

    #[test]
    fn size_fixed_tracks_keep_their_size() {
        let mut t = vec![fixed(100), fixed(50)];
        run(&mut t, &[], Avail::Definite(px(800)));
        assert_eq!((t[0].base, t[1].base), (px(100), px(50)));
    }

    #[test]
    fn size_flex_tracks_split_in_proportion() {
        let mut t = vec![
            Track::new(MinFn::Auto, MaxFn::Flex(1000)),
            Track::new(MinFn::Auto, MaxFn::Flex(3000)),
        ];
        run(&mut t, &[], Avail::Definite(px(400)));
        assert_eq!((t[0].base, t[1].base), (px(100), px(300)));
    }

    #[test]
    fn size_auto_track_takes_the_item_then_stretches() {
        let mut t = vec![Track::new(MinFn::Auto, MaxFn::Auto)];
        let items = [Item {
            start: 0,
            span: 1,
            min_content: px(10),
            max_content: px(40),
            minimum: px(10),
        }];
        run(&mut t, &items, Avail::Definite(px(200)));
        assert_eq!(t[0].base, px(200));
        run(&mut t, &items, Avail::MaxContent);
        assert_eq!(t[0].base, px(40));
        run(&mut t, &items, Avail::MinContent);
        assert_eq!(t[0].base, px(10));
    }
}
