//! Reading elements and attribute values, and the string hygiene every
//! exposed string passes through.

use px_dom::{Arena, NodeData, NodeId};

use crate::role::is_ascii_ws;

const HTML_NS: &str = "http://www.w3.org/1999/xhtml";
const MATHML_NS: &str = "http://www.w3.org/1998/Math/MathML";

/// The longest string the tree will hold for any single name, description or
/// value, in bytes. Longer text is cut at a character boundary.
pub const MAX_STRING: usize = 4096;

/// A borrowed view of one element.
#[derive(Clone, Copy)]
pub(crate) struct El<'a> {
    pub id: NodeId,
    pub local: &'a str,
    html: bool,
    mathml: bool,
    node: &'a px_dom::Node,
}

impl<'a> El<'a> {
    /// The element behind `id`, or `None` for a stale handle or a non-element.
    pub fn of(arena: &'a Arena, id: NodeId) -> Option<El<'a>> {
        let node = arena.get(id)?;
        let name = node.element_name()?;
        node.attrs()?;
        let ns: &str = &name.ns;
        Some(El {
            id,
            local: &name.local,
            html: ns == HTML_NS,
            mathml: ns == MATHML_NS,
            node,
        })
    }

    pub fn is_html(&self) -> bool {
        self.html
    }

    pub fn is_mathml(&self) -> bool {
        self.mathml
    }

    /// An HTML element with this local name.
    pub fn is(&self, local: &str) -> bool {
        self.html && self.local == local
    }

    /// The first attribute with this name (ASCII case-insensitive, no
    /// namespace), if present.
    pub fn attr(&self, name: &str) -> Option<&'a str> {
        self.node
            .attrs()?
            .iter()
            .find(|a| {
                a.name.ns.is_empty()
                    && AsRef::<str>::as_ref(&a.name.local).eq_ignore_ascii_case(name)
            })
            .map(|a| &*a.value)
    }

    pub fn has(&self, name: &str) -> bool {
        self.attr(name).is_some()
    }

    /// Whether any attribute is a global ARIA attribute other than
    /// `aria-hidden` (used to decide whether a presentational role is
    /// ignored).
    pub fn has_global_aria(&self) -> bool {
        self.node.attrs().is_some_and(|attrs| {
            attrs.iter().any(|a| {
                let n: &str = &a.name.local;
                n.len() > 5
                    && n.get(..5).is_some_and(|p| p.eq_ignore_ascii_case("aria-"))
                    && !n.eq_ignore_ascii_case("aria-hidden")
            })
        })
    }
}

/// The text of a text node.
pub(crate) fn text_of(arena: &Arena, id: NodeId) -> Option<&str> {
    match arena.get(id)?.data() {
        NodeData::Text { contents } => Some(&**contents),
        _ => None,
    }
}

pub(crate) fn trim_ws(s: &str) -> &str {
    s.trim_matches(is_ascii_ws)
}

/// An ARIA true/false value. `None` for anything else, which ARIA treats as
/// the attribute's default.
pub(crate) fn aria_bool(value: Option<&str>) -> Option<bool> {
    let v = trim_ws(value?);
    if v.eq_ignore_ascii_case("true") {
        Some(true)
    } else if v.eq_ignore_ascii_case("false") {
        Some(false)
    } else {
        None
    }
}

/// An ARIA tri-state value.
pub(crate) fn aria_tristate(value: Option<&str>) -> Option<crate::Tri> {
    let v = trim_ws(value?);
    if v.eq_ignore_ascii_case("mixed") {
        Some(crate::Tri::Mixed)
    } else {
        aria_bool(Some(v)).map(|b| {
            if b {
                crate::Tri::True
            } else {
                crate::Tri::False
            }
        })
    }
}

/// HTML's "rules for parsing integers": optional sign, then digits, ignoring
/// whatever follows. Saturates rather than failing on a huge number.
pub(crate) fn parse_int(value: &str) -> Option<i64> {
    let s = trim_ws(value);
    let (negative, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let mut n: i64 = 0;
    let mut any = false;
    for c in digits.chars() {
        let Some(d) = c.to_digit(10) else { break };
        any = true;
        n = n.saturating_mul(10).saturating_add(i64::from(d));
    }
    any.then(|| if negative { -n } else { n })
}

/// A positive integer attribute (`aria-level`, `aria-posinset`, ...), clamped
/// to `u32`.
pub(crate) fn parse_positive(value: Option<&str>) -> Option<u32> {
    let n = parse_int(value?)?;
    if n >= 1 {
        Some(u32::try_from(n).unwrap_or(u32::MAX))
    } else {
        None
    }
}

/// A finite number, or `None`. Long inputs are refused without parsing.
pub(crate) fn parse_number(value: Option<&str>) -> Option<f64> {
    let s = trim_ws(value?);
    if s.is_empty() || s.len() > 64 {
        return None;
    }
    s.parse::<f64>().ok().filter(|n| n.is_finite())
}

/// Collapse every run of ASCII whitespace to one space and trim the ends: the
/// accname "flat string".
pub(crate) fn flatten(s: &str) -> String {
    let mut out = String::with_capacity(s.len().min(MAX_STRING));
    push_flat(&mut out, s, usize::MAX);
    clean(&out)
}

/// Append `s` to `acc` with whitespace runs collapsed (also across the
/// boundary with what `acc` already ends in), stopping once `acc` reaches
/// `cap` bytes. Leading and trailing spaces are kept so that adjacent inline
/// pieces still join with the spacing the page gave them.
pub(crate) fn push_flat(acc: &mut String, s: &str, cap: usize) {
    let mut last_space = acc.ends_with(' ');
    for c in s.chars() {
        if acc.len() >= cap {
            return;
        }
        if is_ascii_ws(c) {
            if !last_space {
                acc.push(' ');
                last_space = true;
            }
        } else {
            acc.push(c);
            last_space = false;
        }
    }
}

/// Append a single separating space unless the text is empty or already ends
/// in one.
pub(crate) fn push_sep(acc: &mut String, cap: usize) {
    if !acc.is_empty() && !acc.ends_with(' ') && acc.len() < cap {
        acc.push(' ');
    }
}

/// Make a string safe to hand to a platform bridge: trimmed, NUL replaced
/// (a NUL truncates strings in C-based accessibility APIs and cannot appear in
/// a D-Bus string at all), and cut to [`MAX_STRING`] at a character boundary.
pub(crate) fn clean(s: &str) -> String {
    let trimmed = s.trim_matches(' ');
    let mut out = String::with_capacity(trimmed.len().min(MAX_STRING));
    for c in trimmed.chars() {
        let c = if c == '\0' { '\u{fffd}' } else { c };
        if out.len() + c.len_utf8() > MAX_STRING {
            break;
        }
        out.push(c);
    }
    out
}
