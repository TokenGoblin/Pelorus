//! Attribute value grammars: numbers, lengths, lists, colours, transforms.
//! Integer arithmetic only.

use super::fixed::Q;
use super::geom::Mat;

/// Most numbers read from one list attribute.
pub const MAX_LIST: usize = 200_000;

fn is_sep(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b',')
}

pub fn skip_sep(s: &[u8], mut i: usize) -> usize {
    while s.get(i).is_some_and(|b| is_sep(*b)) {
        i += 1;
    }
    i
}

/// Read one number at `i`: `[+-] digits [. digits] [e[+-]digits]`. Returns the
/// value and the index after it.
pub fn number(s: &[u8], start: usize) -> Option<(Q, usize)> {
    let mut i = start;
    let mut neg = false;
    match s.get(i) {
        Some(b'-') => {
            neg = true;
            i += 1;
        }
        Some(b'+') => i += 1,
        _ => {}
    }
    let mut mant: u64 = 0;
    let mut digits = 0usize;
    let mut exp: i64 = 0;
    let mut any = false;
    while let Some(d) = s.get(i).filter(|b| b.is_ascii_digit()) {
        any = true;
        if digits < 18 {
            mant = (mant * 10) + u64::from(d - b'0');
            if mant != 0 {
                digits += 1;
            }
        } else {
            exp += 1;
        }
        i += 1;
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        while let Some(d) = s.get(i).filter(|b| b.is_ascii_digit()) {
            any = true;
            if digits < 18 {
                mant = (mant * 10) + u64::from(d - b'0');
                if mant != 0 {
                    digits += 1;
                }
                exp -= 1;
            }
            i += 1;
        }
    }
    if !any {
        return None;
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        let mut j = i + 1;
        let mut eneg = false;
        match s.get(j) {
            Some(b'-') => {
                eneg = true;
                j += 1;
            }
            Some(b'+') => j += 1,
            _ => {}
        }
        if s.get(j).is_some_and(u8::is_ascii_digit) {
            let mut e: i64 = 0;
            while let Some(d) = s.get(j).filter(|b| b.is_ascii_digit()) {
                e = (e * 10 + i64::from(d - b'0')).min(1000);
                j += 1;
            }
            exp += if eneg { -e } else { e };
            i = j;
        }
    }
    let m = i128::from(mant) << 16;
    let v = if mant == 0 {
        0
    } else if exp >= 0 {
        if exp > 14 {
            i128::MAX >> 8
        } else {
            m.saturating_mul(10i128.pow(exp as u32))
        }
    } else if exp < -30 {
        0
    } else {
        m / 10i128.pow((-exp) as u32)
    };
    let q = Q::sat(v);
    Some((if neg { -q } else { q }, i))
}

/// A whole-string number (surrounding whitespace allowed).
pub fn parse_number(s: &str) -> Option<Q> {
    let b = s.trim().as_bytes();
    let (v, end) = number(b, 0)?;
    (end == b.len()).then_some(v)
}

/// A number or a percentage as a fraction (`50%` is 0.5), for opacities.
pub fn parse_fraction(s: &str) -> Option<Q> {
    let t = s.trim();
    if let Some(p) = t.strip_suffix('%') {
        return parse_number(p).map(|v| v.scale(1, 100));
    }
    parse_number(t)
}

/// A comma/space separated number list; stops at the first bad token.
pub fn number_list(s: &str) -> Vec<Q> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = skip_sep(b, 0);
    while i < b.len() && out.len() < MAX_LIST {
        let Some((v, j)) = number(b, i) else { break };
        out.push(v);
        i = skip_sep(b, j);
    }
    out
}

/// A length in user units, or a percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Len {
    pub v: Q,
    pub pct: bool,
}

impl Len {
    /// Resolve against `basis` for a percentage.
    pub fn resolve(self, basis: Q) -> Q {
        if self.pct {
            (self.v * basis).scale(1, 100)
        } else {
            self.v
        }
    }
}

/// `number [unit]`. Absolute units use 96 px per inch; `em` is 16 px, `ex` 8.
pub fn parse_length(s: &str) -> Option<Len> {
    let t = s.trim();
    let b = t.as_bytes();
    let (v, end) = number(b, 0)?;
    let unit = t.get(end..)?.trim();
    let (v, pct) = match unit {
        "" | "px" => (v, false),
        "%" => (v, true),
        "pt" => (v.scale(4, 3), false),
        "pc" => (v.scale(16, 1), false),
        "in" => (v.scale(96, 1), false),
        "cm" => (v.scale(9600, 254), false),
        "mm" => (v.scale(960, 254), false),
        "q" | "Q" => (v.scale(240, 254), false),
        "em" | "rem" => (v.scale(16, 1), false),
        "ex" => (v.scale(8, 1), false),
        _ => return None,
    };
    Some(Len { v, pct })
}

/// A parsed colour value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    Rgba([u8; 4]),
    Current,
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

fn clamp8(v: Q) -> u8 {
    v.round().clamp(0, 255) as u8
}

fn func_args(inner: &str) -> Vec<(Q, bool)> {
    inner
        .split([',', ' ', '/'])
        .filter(|t| !t.is_empty())
        .filter_map(|t| match t.strip_suffix('%') {
            Some(p) => parse_number(p).map(|v| (v, true)),
            None => parse_number(t).map(|v| (v, false)),
        })
        .collect()
}

fn hsl(h: Q, s: Q, l: Q) -> [u8; 3] {
    // h in degrees, s and l in 0..1.
    let h = Q(h.0.rem_euclid(360 << 16));
    let c = (Q::ONE - (Q::int(2) * l - Q::ONE).abs()) * s;
    let hp = h.scale(1, 60);
    let x = c * (Q::ONE - ((Q(hp.0.rem_euclid(2 << 16))) - Q::ONE).abs());
    let (r, g, b) = match hp.floor() {
        0 => (c, x, Q::ZERO),
        1 => (x, c, Q::ZERO),
        2 => (Q::ZERO, c, x),
        3 => (Q::ZERO, x, c),
        4 => (x, Q::ZERO, c),
        _ => (c, Q::ZERO, x),
    };
    let m = l - c.half();
    let to8 = |v: Q| clamp8((v + m) * Q::int(255));
    [to8(r), to8(g), to8(b)]
}

pub fn parse_color(s: &str) -> Option<Color> {
    let t = s.trim();
    if t.eq_ignore_ascii_case("currentcolor") {
        return Some(Color::Current);
    }
    if let Some(h) = t.strip_prefix('#') {
        let d: Option<Vec<u8>> = h.bytes().map(hex).collect();
        let d = d?;
        return match d.as_slice() {
            [r, g, b] => Some(Color::Rgba([r * 17, g * 17, b * 17, 255])),
            [r, g, b, a] => Some(Color::Rgba([r * 17, g * 17, b * 17, a * 17])),
            [r1, r2, g1, g2, b1, b2] => {
                Some(Color::Rgba([r1 * 16 + r2, g1 * 16 + g2, b1 * 16 + b2, 255]))
            }
            [r1, r2, g1, g2, b1, b2, a1, a2] => Some(Color::Rgba([
                r1 * 16 + r2,
                g1 * 16 + g2,
                b1 * 16 + b2,
                a1 * 16 + a2,
            ])),
            _ => None,
        };
    }
    let lower = t.to_ascii_lowercase();
    if let Some(open) = lower.find('(') {
        let inner = lower.get(open + 1..)?.strip_suffix(')')?;
        let name = lower.get(..open)?.trim();
        let a = func_args(inner);
        let alpha = |v: Option<&(Q, bool)>| match v {
            None => 255,
            Some((q, true)) => clamp8(q.scale(255, 100)),
            Some((q, false)) => clamp8(*q * Q::int(255)),
        };
        return match (name, a.as_slice()) {
            ("rgb" | "rgba", [r, g, b, rest @ ..]) => {
                let ch = |(q, p): &(Q, bool)| {
                    if *p {
                        clamp8(q.scale(255, 100))
                    } else {
                        clamp8(*q)
                    }
                };
                Some(Color::Rgba([ch(r), ch(g), ch(b), alpha(rest.first())]))
            }
            ("hsl" | "hsla", [h, s, l, rest @ ..]) => {
                let [r, g, b] = hsl(h.0, s.0.scale(1, 100), l.0.scale(1, 100));
                Some(Color::Rgba([r, g, b, alpha(rest.first())]))
            }
            _ => None,
        };
    }
    if lower == "transparent" {
        return Some(Color::Rgba([0, 0, 0, 0]));
    }
    NAMED
        .binary_search_by(|(n, _)| (*n).cmp(lower.as_str()))
        .ok()
        .and_then(|i| NAMED.get(i))
        .map(|(_, rgb)| Color::Rgba([(rgb >> 16) as u8, (rgb >> 8) as u8, *rgb as u8, 255]))
}

/// The CSS named colours, sorted for binary search.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xf0f8ff),
    ("antiquewhite", 0xfaebd7),
    ("aqua", 0x00ffff),
    ("aquamarine", 0x7fffd4),
    ("azure", 0xf0ffff),
    ("beige", 0xf5f5dc),
    ("bisque", 0xffe4c4),
    ("black", 0x000000),
    ("blanchedalmond", 0xffebcd),
    ("blue", 0x0000ff),
    ("blueviolet", 0x8a2be2),
    ("brown", 0xa52a2a),
    ("burlywood", 0xdeb887),
    ("cadetblue", 0x5f9ea0),
    ("chartreuse", 0x7fff00),
    ("chocolate", 0xd2691e),
    ("coral", 0xff7f50),
    ("cornflowerblue", 0x6495ed),
    ("cornsilk", 0xfff8dc),
    ("crimson", 0xdc143c),
    ("cyan", 0x00ffff),
    ("darkblue", 0x00008b),
    ("darkcyan", 0x008b8b),
    ("darkgoldenrod", 0xb8860b),
    ("darkgray", 0xa9a9a9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xa9a9a9),
    ("darkkhaki", 0xbdb76b),
    ("darkmagenta", 0x8b008b),
    ("darkolivegreen", 0x556b2f),
    ("darkorange", 0xff8c00),
    ("darkorchid", 0x9932cc),
    ("darkred", 0x8b0000),
    ("darksalmon", 0xe9967a),
    ("darkseagreen", 0x8fbc8f),
    ("darkslateblue", 0x483d8b),
    ("darkslategray", 0x2f4f4f),
    ("darkslategrey", 0x2f4f4f),
    ("darkturquoise", 0x00ced1),
    ("darkviolet", 0x9400d3),
    ("deeppink", 0xff1493),
    ("deepskyblue", 0x00bfff),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1e90ff),
    ("firebrick", 0xb22222),
    ("floralwhite", 0xfffaf0),
    ("forestgreen", 0x228b22),
    ("fuchsia", 0xff00ff),
    ("gainsboro", 0xdcdcdc),
    ("ghostwhite", 0xf8f8ff),
    ("gold", 0xffd700),
    ("goldenrod", 0xdaa520),
    ("gray", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xadff2f),
    ("grey", 0x808080),
    ("honeydew", 0xf0fff0),
    ("hotpink", 0xff69b4),
    ("indianred", 0xcd5c5c),
    ("indigo", 0x4b0082),
    ("ivory", 0xfffff0),
    ("khaki", 0xf0e68c),
    ("lavender", 0xe6e6fa),
    ("lavenderblush", 0xfff0f5),
    ("lawngreen", 0x7cfc00),
    ("lemonchiffon", 0xfffacd),
    ("lightblue", 0xadd8e6),
    ("lightcoral", 0xf08080),
    ("lightcyan", 0xe0ffff),
    ("lightgoldenrodyellow", 0xfafad2),
    ("lightgray", 0xd3d3d3),
    ("lightgreen", 0x90ee90),
    ("lightgrey", 0xd3d3d3),
    ("lightpink", 0xffb6c1),
    ("lightsalmon", 0xffa07a),
    ("lightseagreen", 0x20b2aa),
    ("lightskyblue", 0x87cefa),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xb0c4de),
    ("lightyellow", 0xffffe0),
    ("lime", 0x00ff00),
    ("limegreen", 0x32cd32),
    ("linen", 0xfaf0e6),
    ("magenta", 0xff00ff),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66cdaa),
    ("mediumblue", 0x0000cd),
    ("mediumorchid", 0xba55d3),
    ("mediumpurple", 0x9370db),
    ("mediumseagreen", 0x3cb371),
    ("mediumslateblue", 0x7b68ee),
    ("mediumspringgreen", 0x00fa9a),
    ("mediumturquoise", 0x48d1cc),
    ("mediumvioletred", 0xc71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xf5fffa),
    ("mistyrose", 0xffe4e1),
    ("moccasin", 0xffe4b5),
    ("navajowhite", 0xffdead),
    ("navy", 0x000080),
    ("oldlace", 0xfdf5e6),
    ("olive", 0x808000),
    ("olivedrab", 0x6b8e23),
    ("orange", 0xffa500),
    ("orangered", 0xff4500),
    ("orchid", 0xda70d6),
    ("palegoldenrod", 0xeee8aa),
    ("palegreen", 0x98fb98),
    ("paleturquoise", 0xafeeee),
    ("palevioletred", 0xdb7093),
    ("papayawhip", 0xffefd5),
    ("peachpuff", 0xffdab9),
    ("peru", 0xcd853f),
    ("pink", 0xffc0cb),
    ("plum", 0xdda0dd),
    ("powderblue", 0xb0e0e6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xff0000),
    ("rosybrown", 0xbc8f8f),
    ("royalblue", 0x4169e1),
    ("saddlebrown", 0x8b4513),
    ("salmon", 0xfa8072),
    ("sandybrown", 0xf4a460),
    ("seagreen", 0x2e8b57),
    ("seashell", 0xfff5ee),
    ("sienna", 0xa0522d),
    ("silver", 0xc0c0c0),
    ("skyblue", 0x87ceeb),
    ("slateblue", 0x6a5acd),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xfffafa),
    ("springgreen", 0x00ff7f),
    ("steelblue", 0x4682b4),
    ("tan", 0xd2b48c),
    ("teal", 0x008080),
    ("thistle", 0xd8bfd8),
    ("tomato", 0xff6347),
    ("turquoise", 0x40e0d0),
    ("violet", 0xee82ee),
    ("wheat", 0xf5deb3),
    ("white", 0xffffff),
    ("whitesmoke", 0xf5f5f5),
    ("yellow", 0xffff00),
    ("yellowgreen", 0x9acd32),
];

/// Most transform functions read from one attribute.
const MAX_TRANSFORMS: usize = 64;

/// A `transform` list. `None` when it does not parse (the caller then uses
/// the identity, as the specification's error handling does for a bad
/// presentation attribute).
pub fn parse_transform(s: &str) -> Option<Mat> {
    let b = s.as_bytes();
    let mut m = Mat::IDENTITY;
    let mut i = 0;
    let mut count = 0;
    loop {
        i = skip_sep(b, i);
        if i >= b.len() {
            return Some(m);
        }
        count += 1;
        if count > MAX_TRANSFORMS {
            return None;
        }
        let name_end = (i..b.len()).find(|k| b.get(*k).is_none_or(|c| !c.is_ascii_alphabetic()))?;
        let name = s.get(i..name_end)?;
        let mut j = name_end;
        while b.get(j) == Some(&b' ') {
            j += 1;
        }
        if b.get(j) != Some(&b'(') {
            return None;
        }
        let close = (j..b.len()).find(|k| b.get(*k) == Some(&b')'))?;
        let args = number_list(s.get(j + 1..close)?);
        let a = |n: usize| args.get(n).copied();
        let t = match (name, args.len()) {
            ("matrix", 6) => Mat {
                a: a(0)?,
                b: a(1)?,
                c: a(2)?,
                d: a(3)?,
                e: a(4)?,
                f: a(5)?,
            },
            ("translate", 1) => Mat::translate(a(0)?, Q::ZERO),
            ("translate", 2) => Mat::translate(a(0)?, a(1)?),
            ("scale", 1) => Mat::scaling(a(0)?, a(0)?),
            ("scale", 2) => Mat::scaling(a(0)?, a(1)?),
            ("rotate", 1) => Mat::rotate(a(0)?),
            ("rotate", 3) => Mat::translate(a(1)?, a(2)?)
                .mul(&Mat::rotate(a(0)?))
                .mul(&Mat::translate(-a(1)?, -a(2)?)),
            ("skewX", 1) => Mat::skew_x(a(0)?),
            ("skewY", 1) => Mat::skew_y(a(0)?),
            _ => return None,
        };
        m = m.mul(&t);
        i = close + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(s: &str) -> i64 {
        parse_number(s).unwrap().0
    }

    #[test]
    fn numbers() {
        assert_eq!(q("1"), 65536);
        assert_eq!(q("-.5"), -32768);
        assert_eq!(q("1e2"), 100 * 65536);
        assert_eq!(q("2.5E-1"), 16384);
        assert_eq!(q("+3."), 3 * 65536);
        assert!(parse_number("e3").is_none());
        assert!(parse_number("1x").is_none());
        // Absurd magnitudes saturate; absurd precision truncates.
        assert_eq!(q("1e999"), 1 << 40);
        assert_eq!(q("1e-999"), 0);
        assert_eq!(q("123456789012345678901234567890"), 1 << 40);
        assert_eq!(number_list("1,2 3-4 .5.5").len(), 6);
    }

    #[test]
    fn lengths_and_colours() {
        assert_eq!(parse_length("1in").unwrap().v, Q::int(96));
        assert!(parse_length("50%").unwrap().pct);
        assert!(parse_length("3furlongs").is_none());
        assert_eq!(parse_color("#f00"), Some(Color::Rgba([255, 0, 0, 255])));
        assert_eq!(
            parse_color("#11223344"),
            Some(Color::Rgba([0x11, 0x22, 0x33, 0x44]))
        );
        assert_eq!(
            parse_color("rgb(10, 20%, 300)"),
            Some(Color::Rgba([10, 51, 255, 255]))
        );
        assert_eq!(
            parse_color("rgba(0,0,0,0.5)"),
            Some(Color::Rgba([0, 0, 0, 128]))
        );
        assert_eq!(
            parse_color("hsl(120, 100%, 25%)"),
            Some(Color::Rgba([0, 128, 0, 255]))
        );
        assert_eq!(
            parse_color("RebeccaPurple"),
            Some(Color::Rgba([0x66, 0x33, 0x99, 255]))
        );
        assert_eq!(parse_color("currentColor"), Some(Color::Current));
        assert_eq!(parse_color("nope"), None);
        assert_eq!(parse_color("#12"), None);
        let mut sorted = NAMED.to_vec();
        sorted.sort_by_key(|(n, _)| *n);
        assert_eq!(sorted, NAMED);
    }

    #[test]
    fn transforms() {
        let m = parse_transform("translate(10 20) scale(2)").unwrap();
        assert_eq!(
            m.apply(super::super::geom::p(Q::int(1), Q::int(1))).x,
            Q::int(12)
        );
        let r = parse_transform("rotate(90)").unwrap();
        let pt = r.apply(super::super::geom::p(Q::int(1), Q::ZERO));
        assert!(pt.x.0.abs() < 8 && (pt.y.0 - 65536).abs() < 8);
        assert!(parse_transform("rotate(1 2)").is_none());
        assert!(parse_transform("blah(1)").is_none());
        assert_eq!(parse_transform("  "), Some(Mat::IDENTITY));
    }
}
