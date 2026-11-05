//! The slice of computed style that painting reads, as plain data.
//!
//! Paint needs ten things from a box's computed style: whether it is visible,
//! its text colour, its background colour and clip, its four borders and whether
//! it clips overflow. Reading them needs stylo's types (`AbsoluteColor`,
//! `BorderStyle`, the `background-clip` list), and `px-layout` forbids floats while
//! `px-paint` should not name stylo, so the conversion lives here, once, and
//! hands out [`PaintStyle`]: no stylo type crosses this boundary.
//!
//! Colours are resolved: `currentcolor` is replaced by the element's `color`,
//! the value is converted to sRGB, and each channel is rounded to 8 bits.
//! Out-of-gamut and wide-gamut colours are clamped by that conversion; a later
//! unit can carry wider colour through if a rasterizer wants it.

use app_units::Au;
use px_dom::NodeId;
use style::computed_values::float::T as Float;
use style::computed_values::position::T as Position;
use style::properties::ComputedValues;
use style::properties::longhands::visibility::computed_value::T as Visibility;
use style::values::computed::{BorderStyle, Overflow, ZIndex};

use crate::view::Dom;

/// Non-premultiplied sRGB, 8 bits per channel: `[r, g, b, a]`.
pub type Rgba = [u8; 4];

/// Which box `background-color` is painted to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundClip {
    /// The default.
    BorderBox,
    PaddingBox,
    ContentBox,
}

/// One border side as painting sees it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintBorder {
    /// Used width: zero when the style is `none` or `hidden` (CSS 2.1 section 8.5.1).
    pub width: Au,
    /// Whether a line is drawn: every style except `none` and `hidden`. The
    /// particular style (dashed, double, ...) is not carried yet.
    pub drawn: bool,
    pub color: Rgba,
}

/// One radius component as painting sees it: `abs + pct * basis`, where `basis`
/// is the border-box width (horizontal radius) or height (vertical radius).
///
/// A `<length-percentage>` is linear in its basis (`calc()` with `min`/`max` is
/// not, and is approximated by its value at the two sample points). Storing the
/// two coefficients keeps the conversion here, where floats are allowed, and the
/// resolution against the box in `px-paint`, where they are not.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RadiusLen {
    /// The length part.
    pub abs: Au,
    /// The percentage part in 1/2^20: `basis * pct / 2^20` Au.
    pub pct: i32,
}

/// The shape of one corner: `corner-*-shape` as the exponent of its
/// superellipse. See `px_paint::display_list::CornerShape`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CornerKind {
    /// `superellipse(K)` for `K >= 0` (`round` is 1, `squircle` 2, `bevel` 0),
    /// as `n = 2^K` in 1/16, saturated. `square` (K = infinity) is `u16::MAX`.
    Convex(u16),
    /// `K < 0` (`scoop` is -1), as `n = 2^-K` in 1/16. `notch` is `u16::MAX`.
    Concave(u16),
}

impl CornerKind {
    /// `round`, the initial value.
    pub const ROUND: CornerKind = CornerKind::Convex(32);
}

/// One corner: elliptical radii and shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintCorner {
    /// Horizontal radius.
    pub x: RadiusLen,
    /// Vertical radius.
    pub y: RadiusLen,
    /// `corner-*-shape`.
    pub kind: CornerKind,
}

/// What painting reads from one element's computed style.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PaintStyle {
    /// `visibility: visible`. `hidden` and `collapse` paint nothing of this box
    /// but do not hide visible descendants.
    pub visible: bool,
    /// The computed `color`: the text colour.
    pub color: Rgba,
    /// `text-decoration-color`, `currentcolor` resolved: what an underline,
    /// overline or line-through this element declares is drawn in.
    pub decoration_color: Rgba,
    /// `background-color`, `currentcolor` resolved.
    pub background: Rgba,
    /// The clip of the last background layer, which is the one the colour layer
    /// belongs to (css-backgrounds-3 section 2.2).
    pub background_clip: BackgroundClip,
    /// Borders in CSS order: top, right, bottom, left.
    pub borders: [PaintBorder; 4],
    /// `overflow-x` or `overflow-y` is not `visible`. `clip` also counts: it
    /// clips, it merely cannot scroll.
    pub clips_overflow: bool,
    /// `position` is anything but `static` (relative, absolute, fixed, sticky).
    pub positioned: bool,
    /// `position` is `absolute` or `fixed`: the box is out of flow and its
    /// containing block, not its parent, decides which `overflow` clips it.
    pub position_absolute: bool,
    /// The computed `z-index`: `None` for `auto`.
    pub z_index: Option<i32>,
    /// `float` is not `none` (and the box is not absolutely positioned, which
    /// computes `float` to `none` by CSS 2.1 section 9.7).
    pub floated: bool,
    /// `border-radius` and `corner-shape`, corners in the order top-left,
    /// top-right, bottom-right, bottom-left.
    pub corners: [PaintCorner; 4],
}

/// Linear coefficients of a radius component.
fn radius_len(lp: &style::values::computed::NonNegativeLengthPercentage) -> RadiusLen {
    use style::values::computed::Length;
    let lp = &lp.0;
    let abs = lp.resolve(Length::new(0.0)).to_i32_au();
    // Au per 2^20 Au of basis, which is the percentage in 1/2^20.
    let slope = i64::from(lp.resolve(Length::from(Au(1 << 20))).to_i32_au()) - i64::from(abs);
    RadiusLen {
        abs: Au(abs.max(0)),
        pct: i32::try_from(slope.clamp(0, 1 << 28)).unwrap_or(0),
    }
}

/// `n = 2^|K|` in 1/16, rounded half up and saturated at `u16::MAX` (which means
/// "infinite": a square or a notch). Uses `exp2`, which is a float operation, in
/// the one crate allowed floats; what crosses into `px-paint` is an integer.
fn exponent16(k: f32) -> u16 {
    if k.is_nan() {
        return 32;
    }
    let k = k.abs();
    if !k.is_finite() || k >= 12.0 {
        return u16::MAX;
    }
    let n = (k.exp2() * 16.0).round();
    if n >= f32::from(u16::MAX) {
        u16::MAX
    } else {
        n as u16
    }
}

fn corner_kind(shape: style::values::computed::corner_shape::CornerShape) -> CornerKind {
    if shape.k.is_nan() {
        CornerKind::ROUND
    } else if shape.k >= 0.0 {
        CornerKind::Convex(exponent16(shape.k))
    } else {
        CornerKind::Concave(exponent16(shape.k))
    }
}

/// Convert a resolved colour to 8-bit sRGB.
fn rgba(color: style::color::AbsoluteColor) -> Rgba {
    let c = color.into_srgb_legacy();
    let channel = |v: f32| -> u8 {
        if v.is_nan() {
            return 0;
        }
        // Clamped first, so the cast cannot saturate on its own.
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    [
        channel(c.components.0),
        channel(c.components.1),
        channel(c.components.2),
        channel(c.alpha),
    ]
}

fn border(
    style: &ComputedValues,
    width: Au,
    s: BorderStyle,
    color: &style::values::computed::Color,
) -> PaintBorder {
    let drawn = !matches!(s, BorderStyle::None | BorderStyle::Hidden);
    PaintBorder {
        width: if drawn { width } else { Au(0) },
        drawn,
        color: rgba(style.resolve_color(color)),
    }
}

/// Whether this box's own `border-*` properties are not painted by the box itself.
///
/// CSS 2.1 section 17.6: borders do not apply to rows, row groups, columns and
/// column groups in the separated model, and in the collapsing model they apply to
/// nothing but are resolved per grid edge by table layout, which emits one filled
/// rectangle per resolved segment. Painting the computed borders as well would
/// draw every edge twice, once wrongly.
fn table_borders_not_drawn_by_box(style: &ComputedValues) -> bool {
    use style::values::specified::box_::DisplayInside as I;
    let inside = style.clone_display().inside();
    let collapsed = style.get_inherited_table().clone_border_collapse()
        == style::computed_values::border_collapse::T::Collapse;
    match inside {
        I::TableRow | I::TableRowGroup | I::TableHeaderGroup | I::TableFooterGroup => true,
        I::TableColumn | I::TableColumnGroup => true,
        I::Table | I::TableCell => collapsed,
        _ => false,
    }
}

/// One border side as the collapsing border model's conflict resolution sees it
/// (CSS 2.1 section 17.6.2.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollapsedSide {
    /// Used width: zero when the style is `none` or `hidden`.
    pub width: Au,
    /// Style rank: 0 for `none`, then inset, groove, outset, ridge, dotted,
    /// dashed, solid, double in ascending precedence.
    pub style: u8,
    /// `border-style: hidden`, which suppresses every other border on the edge.
    pub hidden: bool,
    pub color: Rgba,
}

/// The four borders of `style` for the collapsing model, in order top, right,
/// bottom, left. Unlike [`paint_style_of`] this is not zeroed for table boxes.
#[must_use]
pub fn collapsed_sides(style: &ComputedValues) -> [CollapsedSide; 4] {
    let b = style.get_border();
    let side = |w: Au, s: BorderStyle, c: &style::values::computed::Color| {
        let rank = match s {
            BorderStyle::None | BorderStyle::Hidden => 0,
            BorderStyle::Inset => 1,
            BorderStyle::Groove => 2,
            BorderStyle::Outset => 3,
            BorderStyle::Ridge => 4,
            BorderStyle::Dotted => 5,
            BorderStyle::Dashed => 6,
            BorderStyle::Solid => 7,
            BorderStyle::Double => 8,
        };
        CollapsedSide {
            width: if rank == 0 { Au(0) } else { w },
            style: rank,
            hidden: matches!(s, BorderStyle::Hidden),
            color: rgba(style.resolve_color(c)),
        }
    };
    [
        side(b.border_top_width.0, b.border_top_style, &b.border_top_color),
        side(
            b.border_right_width.0,
            b.border_right_style,
            &b.border_right_color,
        ),
        side(
            b.border_bottom_width.0,
            b.border_bottom_style,
            &b.border_bottom_color,
        ),
        side(b.border_left_width.0, b.border_left_style, &b.border_left_color),
    ]
}

/// Read the paint-relevant parts of `style`.
#[must_use]
pub fn paint_style_of(style: &ComputedValues) -> PaintStyle {
    let b = style.get_border();
    let clip = {
        use style::properties::longhands::background_clip::single_value::computed_value::T as Clip;
        match style.get_background().background_clip.0.last() {
            Some(Clip::PaddingBox) => BackgroundClip::PaddingBox,
            Some(Clip::ContentBox) => BackgroundClip::ContentBox,
            _ => BackgroundClip::BorderBox,
        }
    };
    let corner = |r: &style::values::computed::BorderCornerRadius,
                  shape: style::values::computed::corner_shape::CornerShape| {
        PaintCorner {
            x: radius_len(&r.0.width),
            y: radius_len(&r.0.height),
            kind: corner_kind(shape),
        }
    };
    let mut borders = [
        border(
            style,
            b.border_top_width.0,
            b.border_top_style,
            &b.border_top_color,
        ),
        border(
            style,
            b.border_right_width.0,
            b.border_right_style,
            &b.border_right_color,
        ),
        border(
            style,
            b.border_bottom_width.0,
            b.border_bottom_style,
            &b.border_bottom_color,
        ),
        border(
            style,
            b.border_left_width.0,
            b.border_left_style,
            &b.border_left_color,
        ),
    ];
    if table_borders_not_drawn_by_box(style) {
        for side in &mut borders {
            side.width = Au(0);
            side.drawn = false;
        }
    }
    PaintStyle {
        corners: [
            corner(&b.border_top_left_radius, b.clone_corner_top_left_shape()),
            corner(&b.border_top_right_radius, b.clone_corner_top_right_shape()),
            corner(
                &b.border_bottom_right_radius,
                b.clone_corner_bottom_right_shape(),
            ),
            corner(
                &b.border_bottom_left_radius,
                b.clone_corner_bottom_left_shape(),
            ),
        ],
        visible: style.get_inherited_box().clone_visibility() == Visibility::Visible,
        color: rgba(style.get_inherited_text().clone_color()),
        decoration_color: rgba(
            style.resolve_color(&style.get_text().clone_text_decoration_color()),
        ),
        background: rgba(style.resolve_color(&style.get_background().clone_background_color())),
        background_clip: clip,
        borders,
        clips_overflow: style.clone_overflow_x() != Overflow::Visible
            || style.clone_overflow_y() != Overflow::Visible,
        positioned: !matches!(style.clone_position(), Position::Static),
        position_absolute: matches!(style.clone_position(), Position::Absolute | Position::Fixed),
        z_index: match style.clone_z_index() {
            ZIndex::Integer(n) => Some(n),
            ZIndex::Auto => None,
        },
        floated: !matches!(style.clone_float(), Float::None)
            && !matches!(style.clone_position(), Position::Absolute | Position::Fixed),
    }
}

/// The paint style of `node`, if it is a styled element.
#[must_use]
pub fn paint_style(dom: Dom<'_>, node: NodeId) -> Option<PaintStyle> {
    let view = dom.node(node)?;
    let element = crate::dom::StyleElement::new(view)?;
    let data = style::dom::TElement::borrow_data(&element)?;
    Some(paint_style_of(data.styles.primary()))
}
