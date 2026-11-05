//! The slice of computed style that raster images read, as plain data.
//!
//! Like [`crate::paint`], this converts stylo's types once so that `px-layout`
//! and `px-paint` name none of them: `background-image` (only `url()` layers; a
//! gradient or `none` layer is an empty layer, so a later layer's
//! number still lines up), `background-size`, `-position`, `-repeat`,
//! `-origin` and `-clip`, and for replaced elements `object-fit`,
//! `object-position` and the padding that separates the content box from the
//! border box.
//!
//! Lengths that may be percentages are kept as two integers ([`LenPct`]):
//! `abs + pct * basis`, the percentage in 1/2^20. The conversion uses stylo's
//! float-based `LengthPercentage::resolve` at two sample points; what crosses
//! into `px-paint` is integer.

use app_units::Au;
use px_dom::NodeId;
use style::properties::ComputedValues;
use style::values::computed::Length;

use crate::view::Dom;

/// `abs + pct * basis / 2^20`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LenPct {
    pub abs: Au,
    /// The percentage part in 1/2^20 (50% is `1 << 19`). May be negative.
    pub pct: i32,
}

impl LenPct {
    pub const ZERO: LenPct = LenPct { abs: Au(0), pct: 0 };

    /// Resolve against `basis`, rounding half up.
    #[must_use]
    pub fn resolve(self, basis: Au) -> Au {
        let p = (i64::from(self.pct) * i64::from(basis.0) + (1 << 19)) >> 20;
        let v = i64::from(self.abs.0).saturating_add(p);
        Au(i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX }))
    }
}

fn len_pct(lp: &style::values::computed::LengthPercentage) -> LenPct {
    let abs = lp.resolve(Length::new(0.0)).to_i32_au();
    let slope = i64::from(lp.resolve(Length::from(Au(1 << 20))).to_i32_au()) - i64::from(abs);
    LenPct {
        abs: Au(abs),
        pct: i32::try_from(slope.clamp(-(1 << 28), 1 << 28)).unwrap_or(0),
    }
}

/// One `background-size` component.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeComp {
    Auto,
    Len(LenPct),
}

/// `background-size`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BgSize {
    Cover,
    Contain,
    Explicit(SizeComp, SizeComp),
}

/// One axis of `background-repeat`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    Repeat,
    Space,
    Round,
    NoRepeat,
}

/// A reference box (`background-origin`, `background-clip`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefBox {
    Border,
    Padding,
    Content,
}

/// One background layer that is a `url()` image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BgLayer {
    /// The absolute URL when the stylesheet's base made one, else the string as
    /// written.
    pub url: String,
    pub size: BgSize,
    pub pos_x: LenPct,
    pub pos_y: LenPct,
    pub repeat_x: Repeat,
    pub repeat_y: Repeat,
    pub origin: RefBox,
    pub clip: RefBox,
    /// `image-rendering` asks for no smoothing (`pixelated`, `crisp-edges`).
    pub nearest: bool,
}

/// `object-fit`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObjectFit {
    Fill,
    Contain,
    Cover,
    None,
    ScaleDown,
}

/// What a replaced element's painting needs besides its box.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplacedStyle {
    pub fit: ObjectFit,
    pub pos_x: LenPct,
    pub pos_y: LenPct,
    /// Padding `[top, right, bottom, left]`; percentages are against the
    /// containing block's width, which paint does not know, so they count as 0.
    pub padding: [Au; 4],
    /// `image-rendering` asks for no smoothing (`pixelated`, `crisp-edges`).
    pub nearest: bool,
}

fn nearest_of(style: &ComputedValues) -> bool {
    use style::values::computed::ImageRendering;
    !matches!(
        style.get_inherited_box().clone_image_rendering(),
        ImageRendering::Auto
    )
}

fn url_of(u: &style::values::computed::url::ComputedUrl) -> String {
    match u {
        style::values::computed::url::ComputedUrl::Valid(v) => v.as_str().to_string(),
        style::values::computed::url::ComputedUrl::Invalid(s) => s.to_string(),
    }
}

fn repeat_of(k: style::values::specified::background::BackgroundRepeatKeyword) -> Repeat {
    use style::values::specified::background::BackgroundRepeatKeyword as K;
    match k {
        K::Repeat => Repeat::Repeat,
        K::Space => Repeat::Space,
        K::Round => Repeat::Round,
        K::NoRepeat => Repeat::NoRepeat,
    }
}

/// The `url()` layers of `style`'s `background-image`, top layer first, each with
/// the property values the CSS Backgrounds 3 section 2 repetition rule gives it
/// (a shorter list repeats). A layer that is `none` or a gradient is omitted: it
/// paints nothing here, and a layer's neighbours keep their own values.
#[must_use]
pub fn background_layers_of(style: &ComputedValues) -> Vec<BgLayer> {
    use style::values::computed::background::BackgroundSize;
    use style::values::generics::image::GenericImage;
    use style::values::generics::length::LengthPercentageOrAuto as Lpa;

    let bg = style.get_background();
    let images = &bg.background_image.0;
    let mut out = Vec::new();
    for (i, image) in images.iter().enumerate() {
        let GenericImage::Url(u) = image else {
            continue;
        };
        let at = |n: usize| i % n.max(1);
        let size = match bg.background_size.0.get(at(bg.background_size.0.len())) {
            Some(BackgroundSize::Cover) => BgSize::Cover,
            Some(BackgroundSize::Contain) => BgSize::Contain,
            Some(BackgroundSize::ExplicitSize { width, height }) => {
                let comp = |c: &Lpa<style::values::computed::NonNegativeLengthPercentage>| match c {
                    Lpa::Auto => SizeComp::Auto,
                    Lpa::LengthPercentage(lp) => SizeComp::Len(len_pct(&lp.0)),
                };
                BgSize::Explicit(comp(width), comp(height))
            }
            None => BgSize::Explicit(SizeComp::Auto, SizeComp::Auto),
        };
        let px = bg
            .background_position_x
            .0
            .get(at(bg.background_position_x.0.len()))
            .map_or(LenPct::ZERO, len_pct);
        let py = bg
            .background_position_y
            .0
            .get(at(bg.background_position_y.0.len()))
            .map_or(LenPct::ZERO, len_pct);
        let (rx, ry) = bg
            .background_repeat
            .0
            .get(at(bg.background_repeat.0.len()))
            .map_or((Repeat::Repeat, Repeat::Repeat), |r| {
                (repeat_of(r.0), repeat_of(r.1))
            });
        let origin = {
            use style::properties::longhands::background_origin::single_value::computed_value::T as O;
            match bg.background_origin.0.get(at(bg.background_origin.0.len())) {
                Some(O::BorderBox) => RefBox::Border,
                Some(O::ContentBox) => RefBox::Content,
                _ => RefBox::Padding,
            }
        };
        let clip = {
            use style::properties::longhands::background_clip::single_value::computed_value::T as C;
            match bg.background_clip.0.get(at(bg.background_clip.0.len())) {
                Some(C::PaddingBox) => RefBox::Padding,
                Some(C::ContentBox) => RefBox::Content,
                _ => RefBox::Border,
            }
        };
        out.push(BgLayer {
            url: url_of(u),
            size,
            pos_x: px,
            pos_y: py,
            repeat_x: rx,
            repeat_y: ry,
            origin,
            clip,
            nearest: nearest_of(style),
        });
    }
    out
}

/// The replaced-element style of `style`.
#[must_use]
pub fn replaced_style_of(style: &ComputedValues) -> ReplacedStyle {
    use style::computed_values::object_fit::T as Fit;
    let pos = style.get_position();
    let fit = match pos.clone_object_fit() {
        Fit::Fill => ObjectFit::Fill,
        Fit::Contain => ObjectFit::Contain,
        Fit::Cover => ObjectFit::Cover,
        Fit::None => ObjectFit::None,
        Fit::ScaleDown => ObjectFit::ScaleDown,
    };
    let op = pos.clone_object_position();
    let p = style.get_padding();
    let abs = |lp: &style::values::computed::NonNegativeLengthPercentage| {
        Au(lp.0.resolve(Length::new(0.0)).to_i32_au().max(0))
    };
    ReplacedStyle {
        fit,
        pos_x: len_pct(&op.horizontal),
        pos_y: len_pct(&op.vertical),
        padding: [
            abs(&p.padding_top),
            abs(&p.padding_right),
            abs(&p.padding_bottom),
            abs(&p.padding_left),
        ],
        nearest: nearest_of(style),
    }
}

/// The `url()` background layers of `node`, if it is a styled element.
#[must_use]
pub fn background_layers(dom: Dom<'_>, node: NodeId) -> Vec<BgLayer> {
    let Some(view) = dom.node(node) else {
        return Vec::new();
    };
    let Some(element) = crate::dom::StyleElement::new(view) else {
        return Vec::new();
    };
    let Some(data) = style::dom::TElement::borrow_data(&element) else {
        return Vec::new();
    };
    background_layers_of(data.styles.primary())
}

/// The replaced-element style of `node`, if it is a styled element.
#[must_use]
pub fn replaced_style(dom: Dom<'_>, node: NodeId) -> Option<ReplacedStyle> {
    let view = dom.node(node)?;
    let element = crate::dom::StyleElement::new(view)?;
    let data = style::dom::TElement::borrow_data(&element)?;
    Some(replaced_style_of(data.styles.primary()))
}
