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
    PaintStyle {
        visible: style.get_inherited_box().clone_visibility() == Visibility::Visible,
        color: rgba(style.get_inherited_text().clone_color()),
        decoration_color: rgba(
            style.resolve_color(&style.get_text().clone_text_decoration_color()),
        ),
        background: rgba(style.resolve_color(&style.get_background().clone_background_color())),
        background_clip: clip,
        borders: [
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
        ],
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
