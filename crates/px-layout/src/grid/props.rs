//! Computed style to the plain values the grid algorithm works on.
//!
//! Stylo's grid types are generic, boxed and carry atoms and floats. Everything
//! here converts them once, at the edge, into small owned types with no floating
//! point: an `fr` becomes an integer count of thousandths, a line name a `String`.

use std::collections::BTreeMap;

use app_units::Au;
use style::properties::ComputedValues;
use style::values::computed::LengthPercentage;
use style::values::generics::grid::{
    GridTemplateComponent, RepeatCount, TrackBreadth, TrackListValue, TrackSize,
};
use style::values::specified::align::AlignFlags;

use super::place::{Line, MAX_LINES, Names};

/// The most tracks one axis may have, explicit or implicit.
pub(crate) const MAX_TRACKS: usize = 10_000;

/// A `fr` count in thousandths, from the number stylo computed.
///
/// Goes through the number's decimal text rather than through arithmetic on it:
/// this crate does not do floating-point maths, and the text of a float is exactly
/// reproducible. Three decimal places, truncated; saturating at a million `fr`.
fn flex_milli<T: core::fmt::Display>(value: T) -> i64 {
    const CAP: i64 = 1_000_000_000;
    let text = value.to_string();
    let text = text.trim();
    let mut parts = text.splitn(2, '.');
    let whole = parts.next().unwrap_or("0");
    let frac = parts.next().unwrap_or("");
    if whole.starts_with('-') || whole.chars().any(|c| !c.is_ascii_digit()) {
        // Negative, NaN or infinite.
        return if whole.starts_with("inf") { CAP } else { 0 };
    }
    let mut milli: i64 = 0;
    for c in whole.chars() {
        milli = (milli * 10 + i64::from(c.to_digit(10).unwrap_or(0))).min(CAP / 1000);
    }
    milli *= 1000;
    let mut scale = 100;
    for c in frac.chars().take(3) {
        milli += i64::from(c.to_digit(10).unwrap_or(0)) * scale;
        scale /= 10;
    }
    milli.min(CAP)
}

/// One side of a track sizing function, before percentages are resolved.
#[derive(Clone, Debug)]
pub(crate) enum Breadth {
    Len(LengthPercentage),
    Flex(i64),
    Auto,
    MinContent,
    MaxContent,
}

/// A `<track-size>`: its minimum and maximum sizing functions.
#[derive(Clone, Debug)]
pub(crate) struct TrackSpec {
    pub min: Breadth,
    pub max: Breadth,
    /// The argument of `fit-content()`, if that is what this is.
    pub fit_content: Option<LengthPercentage>,
}

impl TrackSpec {
    pub(crate) fn auto() -> Self {
        Self {
            min: Breadth::Auto,
            max: Breadth::Auto,
            fit_content: None,
        }
    }
}

fn breadth(b: &TrackBreadth<LengthPercentage>) -> Breadth {
    match b {
        TrackBreadth::Breadth(l) => Breadth::Len(l.clone()),
        TrackBreadth::Flex(f) => Breadth::Flex(flex_milli(f.0)),
        TrackBreadth::Auto => Breadth::Auto,
        TrackBreadth::MinContent => Breadth::MinContent,
        TrackBreadth::MaxContent => Breadth::MaxContent,
    }
}

fn track(size: &TrackSize<LengthPercentage>) -> TrackSpec {
    match size {
        TrackSize::Breadth(b) => {
            let b = breadth(b);
            // "If the track was sized with a <flex> value ... auto" is the minimum.
            let min = if matches!(b, Breadth::Flex(_)) {
                Breadth::Auto
            } else {
                b.clone()
            };
            TrackSpec {
                min,
                max: b,
                fit_content: None,
            }
        }
        TrackSize::Minmax(min, max) => {
            let min = breadth(min);
            let min = if matches!(min, Breadth::Flex(_)) {
                Breadth::Auto
            } else {
                min
            };
            TrackSpec {
                min,
                max: breadth(max),
                fit_content: None,
            }
        }
        TrackSize::FitContent(b) => TrackSpec {
            min: Breadth::Auto,
            max: Breadth::Auto,
            fit_content: match b {
                TrackBreadth::Breadth(l) => Some(l.clone()),
                _ => None,
            },
        },
    }
}

/// An explicit track list after `repeat()` has been expanded.
#[derive(Clone, Debug, Default)]
pub(crate) struct Explicit {
    pub tracks: Vec<TrackSpec>,
    /// `tracks.len() + 1` lists of names, one per line.
    pub names: Vec<Vec<String>>,
    /// The tracks an `auto-fill`/`auto-fit` repeat produced, and whether it was
    /// `auto-fit` (its empty tracks collapse).
    pub auto_range: Option<(usize, usize, bool)>,
}

/// A computed track list, kept until the repeat count is known.
pub(crate) type RawList = Option<Box<style::values::computed::TrackList>>;

/// The unit an `auto-fill`/`auto-fit` repeat repeats, and the tracks around it.
pub(crate) struct AutoRepeat {
    pub unit: Vec<TrackSpec>,
    pub others: Vec<TrackSpec>,
}

/// The tracks of `list` outside and inside its auto-repeat, for counting repeats.
pub(crate) fn auto_repeat_of(list: &RawList) -> Option<AutoRepeat> {
    let list = list.as_ref()?;
    if !list.has_auto_repeat() {
        return None;
    }
    let mut unit = Vec::new();
    let mut others = Vec::new();
    for (i, v) in list.values.iter().enumerate() {
        match v {
            TrackListValue::TrackSize(s) => others.push(track(s)),
            TrackListValue::TrackRepeat(r) => {
                if i == list.auto_repeat_index {
                    unit.extend(r.track_sizes.iter().map(track));
                } else {
                    let times = match r.count {
                        RepeatCount::Number(n) => usize::try_from(n.max(1)).unwrap_or(1),
                        _ => 1,
                    };
                    for _ in 0..times.min(MAX_TRACKS) {
                        others.extend(r.track_sizes.iter().map(track));
                        if others.len() > MAX_TRACKS {
                            break;
                        }
                    }
                }
            }
        }
    }
    Some(AutoRepeat { unit, others })
}

fn names_of(set: &[style::values::CustomIdent]) -> Vec<String> {
    set.iter().map(|n| n.0.to_string()).collect()
}

/// Expand `list`, repeating the auto-repeat `auto_count` times.
pub(crate) fn expand(list: &RawList, auto_count: usize) -> Explicit {
    let mut out = Explicit::default();
    let Some(list) = list else {
        out.names.push(Vec::new());
        return out;
    };
    let get = |i: usize| {
        list.line_names
            .get(i)
            .map_or_else(Vec::new, |s| names_of(s))
    };
    let mut cur = get(0);
    for (i, v) in list.values.iter().enumerate() {
        if out.tracks.len() >= MAX_TRACKS {
            break;
        }
        match v {
            TrackListValue::TrackSize(s) => {
                out.names.push(std::mem::take(&mut cur));
                out.tracks.push(track(s));
                cur = get(i + 1);
            }
            TrackListValue::TrackRepeat(r) => {
                let auto = i == list.auto_repeat_index && list.has_auto_repeat();
                let times = match r.count {
                    RepeatCount::Number(n) => usize::try_from(n.max(1)).unwrap_or(1),
                    RepeatCount::AutoFill | RepeatCount::AutoFit => auto_count.max(1),
                };
                let start = out.tracks.len();
                let inner = r.track_sizes.len();
                let tail = r
                    .line_names
                    .get(inner)
                    .map_or_else(Vec::new, |s| names_of(s));
                'repeat: for _ in 0..times {
                    for (j, size) in r.track_sizes.iter().enumerate() {
                        if out.tracks.len() >= MAX_TRACKS {
                            break 'repeat;
                        }
                        let head = r.line_names.get(j).map_or_else(Vec::new, |s| names_of(s));
                        cur.extend(head);
                        out.names.push(std::mem::take(&mut cur));
                        out.tracks.push(track(size));
                    }
                    cur = tail.clone();
                }
                if auto {
                    out.auto_range = Some((
                        start,
                        out.tracks.len(),
                        matches!(r.count, RepeatCount::AutoFit),
                    ));
                }
                cur.extend(get(i + 1));
            }
        }
    }
    out.names.push(cur);
    out
}

/// `grid-auto-rows` / `grid-auto-columns`: the pattern for implicit tracks.
fn implicit(tracks: &[TrackSize<LengthPercentage>]) -> Vec<TrackSpec> {
    let mut out: Vec<TrackSpec> = tracks.iter().take(MAX_TRACKS).map(track).collect();
    if out.is_empty() {
        out.push(TrackSpec::auto());
    }
    out
}

/// A named area from `grid-template-areas`, in 1-based line numbers.
#[derive(Clone, Debug)]
pub(crate) struct Area {
    pub name: String,
    pub rows: (i32, i32),
    pub columns: (i32, i32),
}

/// Distribution of free space among tracks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Content {
    Normal,
    Stretch,
    Start,
    End,
    Center,
    Between,
    Around,
    Evenly,
}

/// A self-alignment value, reduced to what grid layout distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    Normal,
    Stretch,
    Start,
    End,
    Center,
}

/// An alignment with its `safe` flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Aligned<T> {
    pub value: T,
    pub safe: bool,
}

/// `auto` has no meaning here: the caller substitutes the container's default.
fn self_align(flags: AlignFlags) -> Option<Aligned<Align>> {
    let safe = flags.contains(AlignFlags::SAFE);
    let value = match flags.value() {
        AlignFlags::AUTO => return None,
        AlignFlags::NORMAL => Align::Normal,
        AlignFlags::STRETCH => Align::Stretch,
        AlignFlags::CENTER => Align::Center,
        AlignFlags::END
        | AlignFlags::FLEX_END
        | AlignFlags::SELF_END
        | AlignFlags::RIGHT
        | AlignFlags::LAST_BASELINE => Align::End,
        // Baseline alignment needs baselines, which need fonts (Phase 9). Its
        // fallback alignment is `start`.
        _ => Align::Start,
    };
    Some(Aligned { value, safe })
}

fn content(flags: AlignFlags) -> Aligned<Content> {
    let safe = flags.contains(AlignFlags::SAFE);
    let value = match flags.value() {
        AlignFlags::NORMAL | AlignFlags::AUTO => Content::Normal,
        AlignFlags::STRETCH => Content::Stretch,
        AlignFlags::CENTER => Content::Center,
        AlignFlags::END | AlignFlags::FLEX_END | AlignFlags::RIGHT | AlignFlags::LAST_BASELINE => {
            Content::End
        }
        AlignFlags::SPACE_BETWEEN => Content::Between,
        AlignFlags::SPACE_AROUND => Content::Around,
        AlignFlags::SPACE_EVENLY => Content::Evenly,
        _ => Content::Start,
    };
    Aligned { value, safe }
}

/// The grid container's own properties.
#[derive(Clone, Debug)]
pub(crate) struct ContainerStyle {
    pub columns: RawList,
    pub rows: RawList,
    pub auto_columns: Vec<TrackSpec>,
    pub auto_rows: Vec<TrackSpec>,
    pub areas: Vec<Area>,
    pub area_columns: i32,
    pub area_rows: i32,
    pub column_flow: bool,
    pub dense: bool,
    pub column_gap: Option<LengthPercentage>,
    pub row_gap: Option<LengthPercentage>,
    pub justify_content: Aligned<Content>,
    pub align_content: Aligned<Content>,
    pub justify_items: Aligned<Align>,
    pub align_items: Aligned<Align>,
}

fn raw_list(component: GridTemplateComponent<LengthPercentage, i32>) -> RawList {
    match component {
        GridTemplateComponent::TrackList(list) => Some(list),
        // `subgrid` is css-grid-2 and `masonry` is out of scope (ADR 031): both
        // lay out as `none`.
        _ => None,
    }
}

fn gap(
    value: style::values::computed::length::NonNegativeLengthPercentageOrNormal,
) -> Option<LengthPercentage> {
    use style::values::generics::length::GenericLengthPercentageOrNormal as G;
    match value {
        G::LengthPercentage(l) => Some(l.0),
        G::Normal => None,
    }
}

impl ContainerStyle {
    pub(crate) fn new(style: &ComputedValues) -> Self {
        use style::values::computed::GridTemplateAreas;
        let mut areas = Vec::new();
        let (mut area_columns, mut area_rows) = (0, 0);
        if let GridTemplateAreas::Areas(arc) = style.clone_grid_template_areas() {
            let t = &arc.0;
            area_columns = i32::try_from(t.width).unwrap_or(0).min(MAX_LINES);
            area_rows = i32::try_from(t.strings.len()).unwrap_or(0).min(MAX_LINES);
            for a in t.areas.iter() {
                let to = |v: u32| i32::try_from(v).unwrap_or(MAX_LINES).min(MAX_LINES);
                areas.push(Area {
                    name: a.name.to_string(),
                    rows: (to(a.rows.start), to(a.rows.end)),
                    columns: (to(a.columns.start), to(a.columns.end)),
                });
            }
        }
        let flow = style.clone_grid_auto_flow();
        let items = |a: AlignFlags| {
            self_align(a).unwrap_or(Aligned {
                value: Align::Normal,
                safe: false,
            })
        };
        Self {
            columns: raw_list(style.clone_grid_template_columns()),
            rows: raw_list(style.clone_grid_template_rows()),
            auto_columns: implicit(&style.clone_grid_auto_columns().0),
            auto_rows: implicit(&style.clone_grid_auto_rows().0),
            areas,
            area_columns,
            area_rows,
            column_flow: flow.contains(style::values::specified::GridAutoFlow::COLUMN),
            dense: flow.contains(style::values::specified::GridAutoFlow::DENSE),
            column_gap: gap(style.clone_column_gap()),
            row_gap: gap(style.clone_row_gap()),
            justify_content: content(style.clone_justify_content().primary()),
            align_content: content(style.clone_align_content().primary()),
            justify_items: items(style.clone_justify_items().computed.0.0),
            align_items: items(style.clone_align_items().0),
        }
    }

    /// Line names for one axis: the explicit lines' names, plus the
    /// `<area>-start` / `<area>-end` names the template areas imply (section 7.3).
    pub(crate) fn names(&self, explicit: &Explicit, rows: bool) -> Names {
        let mut out: BTreeMap<String, Vec<i32>> = BTreeMap::new();
        for (i, set) in explicit.names.iter().enumerate() {
            let line = i32::try_from(i + 1).unwrap_or(MAX_LINES);
            for name in set {
                out.entry(name.clone()).or_default().push(line);
            }
        }
        for a in &self.areas {
            let (s, e) = if rows { a.rows } else { a.columns };
            out.entry(format!("{}-start", a.name)).or_default().push(s);
            out.entry(format!("{}-end", a.name)).or_default().push(e);
        }
        for lines in out.values_mut() {
            lines.sort_unstable();
        }
        out
    }
}

/// A `width`/`height`/`min-*`/`max-*` value.
#[derive(Clone, Debug)]
pub(crate) enum SizeKind {
    /// `auto`, `stretch`, and anything this phase cannot resolve.
    Auto,
    Len(LengthPercentage),
    MinContent,
    MaxContent,
    FitContent(Option<LengthPercentage>),
    /// `max-*: none`.
    None,
}

fn size_kind(size: &style::values::computed::Size) -> SizeKind {
    use style::values::computed::Size;
    match size {
        Size::LengthPercentage(l) => SizeKind::Len(l.0.clone()),
        Size::MinContent => SizeKind::MinContent,
        Size::MaxContent => SizeKind::MaxContent,
        Size::FitContent => SizeKind::FitContent(None),
        Size::FitContentFunction(l) => SizeKind::FitContent(Some(l.0.clone())),
        _ => SizeKind::Auto,
    }
}

fn max_kind(size: &style::values::computed::MaxSize) -> SizeKind {
    use style::values::computed::MaxSize;
    match size {
        MaxSize::LengthPercentage(l) => SizeKind::Len(l.0.clone()),
        MaxSize::MinContent => SizeKind::MinContent,
        MaxSize::MaxContent => SizeKind::MaxContent,
        MaxSize::FitContent => SizeKind::FitContent(None),
        MaxSize::FitContentFunction(l) => SizeKind::FitContent(Some(l.0.clone())),
        _ => SizeKind::None,
    }
}

/// A grid item's properties.
#[derive(Clone, Debug)]
pub(crate) struct ItemStyle {
    pub columns: (Line, Line),
    pub rows: (Line, Line),
    pub order: i32,
    pub justify: Aligned<Align>,
    pub align: Aligned<Align>,
    pub width: SizeKind,
    pub height: SizeKind,
    pub min_width: SizeKind,
    pub min_height: SizeKind,
    pub max_width: SizeKind,
    pub max_height: SizeKind,
    pub border_box: bool,
    /// A scroll container has an automatic minimum size of zero (section 6.6).
    pub scrolls: bool,
    pub margin_auto_inline: (bool, bool),
    pub margin_auto_block: (bool, bool),
}

fn line(l: &style::values::computed::GridLine) -> Line {
    let name = l.ident.0.to_string();
    let name = if name.is_empty() { None } else { Some(name) };
    if l.is_span {
        Line::Span(l.line_num.max(1), name)
    } else if l.line_num != 0 {
        Line::Num(l.line_num, name)
    } else {
        match name {
            Some(n) => Line::Ident(n),
            None => Line::Auto,
        }
    }
}

impl ItemStyle {
    /// An anonymous item, or an item whose style is unavailable: everything `auto`.
    pub(crate) fn anonymous(container: &ContainerStyle) -> Self {
        Self {
            columns: (Line::Auto, Line::Auto),
            rows: (Line::Auto, Line::Auto),
            order: 0,
            justify: container.justify_items,
            align: container.align_items,
            width: SizeKind::Auto,
            height: SizeKind::Auto,
            min_width: SizeKind::Auto,
            min_height: SizeKind::Auto,
            max_width: SizeKind::None,
            max_height: SizeKind::None,
            border_box: false,
            scrolls: false,
            margin_auto_inline: (false, false),
            margin_auto_block: (false, false),
        }
    }

    pub(crate) fn new(style: &ComputedValues, container: &ContainerStyle) -> Self {
        use style::computed_values::box_sizing::T as BoxSizing;
        use style::values::computed::Overflow;
        use style::values::generics::length::GenericMargin;
        let margin = style.get_margin();
        let auto = |m: &style::values::computed::Margin| matches!(m, GenericMargin::Auto);
        Self {
            columns: (
                line(&style.clone_grid_column_start()),
                line(&style.clone_grid_column_end()),
            ),
            rows: (
                line(&style.clone_grid_row_start()),
                line(&style.clone_grid_row_end()),
            ),
            order: style.clone_order(),
            justify: self_align(style.clone_justify_self().0).unwrap_or(container.justify_items),
            align: self_align(style.clone_align_self().0).unwrap_or(container.align_items),
            width: size_kind(&style.clone_width()),
            height: size_kind(&style.clone_height()),
            min_width: size_kind(&style.clone_min_width()),
            min_height: size_kind(&style.clone_min_height()),
            max_width: max_kind(&style.clone_max_width()),
            max_height: max_kind(&style.clone_max_height()),
            border_box: style.clone_box_sizing() == BoxSizing::BorderBox,
            scrolls: style.clone_overflow_x() != Overflow::Visible
                || style.clone_overflow_y() != Overflow::Visible,
            margin_auto_inline: (auto(&margin.margin_left), auto(&margin.margin_right)),
            margin_auto_block: (auto(&margin.margin_top), auto(&margin.margin_bottom)),
        }
    }
}

/// Resolve a length-percentage against `basis`, if there is one.
pub(crate) fn resolve(l: &LengthPercentage, basis: Option<Au>) -> Option<Au> {
    l.maybe_to_used_value(basis)
}
