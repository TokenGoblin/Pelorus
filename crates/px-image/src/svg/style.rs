//! Computed style: the presentation properties the renderer understands.

use super::css::{Rule, parse_decls};
use super::doc::Doc;
use super::fixed::Q;
use super::stroke::{Cap, Join};
use super::values::{Color, Len, number_list, parse_color, parse_fraction, parse_length};

/// A `fill` or `stroke` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Paint {
    None,
    Color(Color),
    /// `url(#id)` with an optional fallback colour (`None` inside means the
    /// fallback is `none`).
    Url(String, Option<Option<Color>>),
}

#[derive(Clone, Debug)]
pub struct Style {
    // Inherited.
    pub fill: Paint,
    pub fill_opacity: Q,
    pub fill_even_odd: bool,
    pub clip_even_odd: bool,
    pub stroke: Paint,
    pub stroke_opacity: Q,
    pub stroke_width: Len,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: Q,
    pub dash: Vec<Len>,
    pub dash_offset: Len,
    pub color: [u8; 4],
    pub visible: bool,
    // Not inherited.
    pub opacity: Q,
    pub display: bool,
    pub clip_path: Option<String>,
    pub stop_color: Color,
    pub stop_opacity: Q,
}

impl Style {
    pub fn initial(color: [u8; 4]) -> Style {
        Style {
            fill: Paint::Color(Color::Rgba([0, 0, 0, 255])),
            fill_opacity: Q::ONE,
            fill_even_odd: false,
            clip_even_odd: false,
            stroke: Paint::None,
            stroke_opacity: Q::ONE,
            stroke_width: Len {
                v: Q::ONE,
                pct: false,
            },
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: Q::int(4),
            dash: Vec::new(),
            dash_offset: Len {
                v: Q::ZERO,
                pct: false,
            },
            color,
            visible: true,
            opacity: Q::ONE,
            display: true,
            clip_path: None,
            stop_color: Color::Rgba([0, 0, 0, 255]),
            stop_opacity: Q::ONE,
        }
    }

    /// The style a child starts from: inherited properties kept, the rest reset.
    pub fn child_of(&self) -> Style {
        Style {
            opacity: Q::ONE,
            display: true,
            clip_path: None,
            stop_color: Color::Rgba([0, 0, 0, 255]),
            stop_opacity: Q::ONE,
            ..self.clone()
        }
    }
}

/// Properties that may be given as presentation attributes.
const PRESENTATION: &[&str] = &[
    "fill",
    "fill-opacity",
    "fill-rule",
    "clip-rule",
    "stroke",
    "stroke-opacity",
    "stroke-width",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "color",
    "visibility",
    "opacity",
    "display",
    "clip-path",
    "stop-color",
    "stop-opacity",
];

fn parse_paint(v: &str) -> Option<Paint> {
    let t = v.trim();
    if t.eq_ignore_ascii_case("none") {
        return Some(Paint::None);
    }
    if let Some(rest) = t.strip_prefix("url(") {
        let close = rest.find(')')?;
        let target = rest.get(..close)?.trim().trim_matches(['"', '\'']);
        let tail = rest.get(close + 1..).unwrap_or("").trim();
        let fallback = if tail.is_empty() {
            None
        } else if tail.eq_ignore_ascii_case("none") {
            Some(None)
        } else {
            Some(parse_color(tail))
        };
        // Only same-document references are ever followed.
        return match target.strip_prefix('#') {
            Some(id) if !id.is_empty() => Some(Paint::Url(id.to_string(), fallback)),
            _ => Some(match fallback {
                Some(Some(c)) => Paint::Color(c),
                _ => Paint::None,
            }),
        };
    }
    parse_color(t).map(Paint::Color)
}

fn clip_ref(v: &str) -> Option<String> {
    let t = v.trim();
    let inner = t.strip_prefix("url(")?;
    let close = inner.find(')')?;
    let id = inner
        .get(..close)?
        .trim()
        .trim_matches(['"', '\''])
        .strip_prefix('#')?;
    (!id.is_empty()).then(|| id.to_string())
}

/// Apply one declaration. Unknown names and bad values change nothing.
pub fn apply(s: &mut Style, name: &str, value: &str) {
    let v = value.trim();
    if v.eq_ignore_ascii_case("inherit") || v.eq_ignore_ascii_case("initial") {
        return;
    }
    match name {
        "fill" => {
            if let Some(p) = parse_paint(v) {
                s.fill = p;
            }
        }
        "stroke" => {
            if let Some(p) = parse_paint(v) {
                s.stroke = p;
            }
        }
        "fill-opacity" => {
            if let Some(f) = parse_fraction(v) {
                s.fill_opacity = unit(f);
            }
        }
        "stroke-opacity" => {
            if let Some(f) = parse_fraction(v) {
                s.stroke_opacity = unit(f);
            }
        }
        "opacity" => {
            if let Some(f) = parse_fraction(v) {
                s.opacity = unit(f);
            }
        }
        "stop-opacity" => {
            if let Some(f) = parse_fraction(v) {
                s.stop_opacity = unit(f);
            }
        }
        "fill-rule" => s.fill_even_odd = v == "evenodd",
        "clip-rule" => s.clip_even_odd = v == "evenodd",
        "stroke-width" => {
            if let Some(l) = parse_length(v) {
                s.stroke_width = l;
            }
        }
        "stroke-linecap" => {
            s.cap = match v {
                "round" => Cap::Round,
                "square" => Cap::Square,
                _ => Cap::Butt,
            }
        }
        "stroke-linejoin" => {
            s.join = match v {
                "round" => Join::Round,
                "bevel" => Join::Bevel,
                _ => Join::Miter,
            }
        }
        "stroke-miterlimit" => {
            if let Some(f) = super::values::parse_number(v)
                && f >= Q::ONE
            {
                s.miter_limit = f;
            }
        }
        "stroke-dasharray" => {
            s.dash = if v == "none" {
                Vec::new()
            } else {
                // Lengths separated by commas or spaces.
                v.split([',', ' '])
                    .filter(|t| !t.is_empty())
                    .map_while(parse_length)
                    .take(64)
                    .collect()
            };
        }
        "stroke-dashoffset" => {
            if let Some(l) = parse_length(v) {
                s.dash_offset = l;
            }
        }
        "color" => {
            if let Some(Color::Rgba(c)) = parse_color(v) {
                s.color = c;
            }
        }
        "visibility" => s.visible = v == "visible" || v == "inherit",
        "display" => s.display = v != "none",
        "clip-path" => s.clip_path = clip_ref(v),
        "stop-color" => {
            if let Some(c) = parse_color(v) {
                s.stop_color = c;
            }
        }
        _ => {}
    }
}

fn unit(q: Q) -> Q {
    if q < Q::ZERO {
        Q::ZERO
    } else if q > Q::ONE {
        Q::ONE
    } else {
        q
    }
}

/// The style of `el` given its parent's: presentation attributes, then
/// matching stylesheet rules by specificity and order, then `style=""`.
pub fn compute(doc: &Doc, rules: &[Rule], el: u32, parent: &Style) -> Style {
    let mut s = parent.child_of();
    let Some(e) = doc.get(el) else { return s };
    for (n, v) in &e.attrs {
        if PRESENTATION.contains(&n.as_str()) {
            apply(&mut s, n, v);
        }
    }
    let mut hits: Vec<&Rule> = rules.iter().filter(|r| r.matches(doc, el)).collect();
    hits.sort_by_key(|r| (r.specificity, r.order));
    for r in hits {
        for (n, v) in &r.decls {
            apply(&mut s, n, v);
        }
    }
    if let Some(st) = e.attr("style") {
        for (n, v) in parse_decls(st) {
            apply(&mut s, &n, &v);
        }
    }
    s
}

/// A number list attribute, for `viewBox` and the like.
pub fn numbers(v: Option<&str>) -> Vec<Q> {
    v.map(number_list).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::super::css::parse_sheet;
    use super::super::xml;
    use super::*;

    #[test]
    fn cascade_order_is_attr_then_sheet_then_style_attr() {
        let doc = xml::parse(
            "<svg><style>.a{fill:green} #x{fill:blue}</style>\
             <rect id='x' class='a' fill='red' stroke='#00f' style='stroke:none'/></svg>",
        )
        .unwrap();
        let mut rules = Vec::new();
        parse_sheet(&doc.els[1].text, &mut rules);
        let root = Style::initial([1, 2, 3, 255]);
        let s = compute(&doc, &rules, 2, &root);
        assert_eq!(s.fill, Paint::Color(Color::Rgba([0, 0, 255, 255])));
        assert_eq!(s.stroke, Paint::None);
        assert_eq!(s.color, [1, 2, 3, 255]);
    }

    #[test]
    fn inheritance_and_resets() {
        let mut p = Style::initial([0, 0, 0, 255]);
        apply(&mut p, "opacity", "50%");
        apply(&mut p, "fill", "red");
        let c = p.child_of();
        assert_eq!(c.opacity, Q::ONE);
        assert_eq!(c.fill, p.fill);
    }

    #[test]
    fn paints_never_follow_external_urls() {
        assert_eq!(
            parse_paint("url(http://x/y.svg#a) red"),
            Some(Paint::Color(Color::Rgba([255, 0, 0, 255])))
        );
        assert_eq!(parse_paint("url(http://x/y.svg#a)"), Some(Paint::None));
        assert_eq!(
            parse_paint("url('#g') none"),
            Some(Paint::Url("g".into(), Some(None)))
        );
        assert_eq!(parse_paint("garbage"), None);
        assert_eq!(clip_ref("url(#c)"), Some("c".into()));
        assert_eq!(clip_ref("url(file:///etc/passwd)"), None);
    }
}
