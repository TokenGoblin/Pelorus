//! HTML presentational hints for tables, as CSS declaration text.
//!
//! HTML Standard section 15.3.8 ("Tables") maps the table elements to CSS and the
//! legacy attributes (`border`, `cellspacing`, `cellpadding`, `width`, `align`,
//! `valign`, `bgcolor`, ...) to declarations at the **presentational hint**
//! cascade level: above the user-agent sheet, below every author rule. This module
//! produces the declaration text; `data.rs` parses it once per element, with the
//! `style` attribute parser, when the style table is built, and `element.rs` hands
//! the parsed block to stylo.
//!
//! The element's `display` is in here too. It is the user-agent default for the
//! table elements, and putting it at this level (rather than in a stylesheet each
//! embedder would have to carry) means every embedder gets tables without
//! copying a table of rules around; an author's `display` still wins.

use px_dom::{Arena, NodeId};

/// The legacy-colour parse that matters in practice: `#rgb`, `#rrggbb`, a bare hex
/// string, or a named colour passed through.
fn legacy_color(value: &str) -> Option<String> {
    let v = value.trim();
    if v.is_empty() {
        return None;
    }
    let bare = v.strip_prefix('#').unwrap_or(v);
    if matches!(bare.len(), 3 | 6) && bare.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(format!("#{bare}"));
    }
    if v.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Some(v.to_owned());
    }
    None
}

/// HTML's "rules for parsing a dimension value", reduced to what pages write: a
/// non-negative integer (pixels) or an integer followed by `%`.
fn dimension(value: &str) -> Option<String> {
    let v = value.trim();
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = v[digits.len()..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    if rest.starts_with('%') {
        Some(format!("{digits}%"))
    } else {
        Some(format!("{digits}px"))
    }
}

/// HTML's "rules for parsing non-negative integers", pixels.
fn non_negative(value: &str) -> Option<u32> {
    let v = value.trim();
    let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<u32>().ok().map(|n| n.min(100_000))
}

fn attr<'a>(arena: &'a Arena, id: NodeId, name: &str) -> Option<&'a str> {
    arena
        .get(id)?
        .attrs()?
        .iter()
        .find(|a| a.name.ns == html5ever::ns!() && &*a.name.local == name)
        .map(|a| &*a.value)
}

/// The nearest `table` ancestor of a cell, looking through at most a row and a
/// row group (and anything else in between, a few levels at most).
fn table_ancestor(arena: &Arena, id: NodeId) -> Option<NodeId> {
    let mut cur = arena.get(id)?.parent();
    for _ in 0..4 {
        let n = cur?;
        let node = arena.get(n)?;
        if node
            .element_name()
            .is_some_and(|q| q.ns == html5ever::ns!(html) && &*q.local == "table")
        {
            return Some(n);
        }
        cur = node.parent();
    }
    None
}

fn align_keyword(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Some("left"),
        "right" => Some("right"),
        "center" | "middle" => Some("center"),
        "justify" => Some("justify"),
        _ => None,
    }
}

fn valign_keyword(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "top" => Some("top"),
        "middle" => Some("middle"),
        "bottom" => Some("bottom"),
        "baseline" => Some("baseline"),
        _ => None,
    }
}

/// How many `ul`, `ol` and `menu` elements enclose `id`.
fn list_depth(arena: &Arena, id: NodeId) -> usize {
    let mut depth = 0;
    let mut cur = arena.get(id).and_then(px_dom::Node::parent);
    while let Some(n) = cur {
        let Some(node) = arena.get(n) else { break };
        if node.element_name().is_some_and(|q| {
            q.ns == html5ever::ns!(html) && matches!(&*q.local, "ul" | "ol" | "menu")
        }) {
            depth += 1;
        }
        cur = node.parent();
    }
    depth
}

/// HTML's `type` attribute on a list or list item, as a `list-style-type`.
fn list_type(local: &str, value: &str) -> Option<&'static str> {
    let numbered = match value {
        "1" => Some("decimal"),
        "a" => Some("lower-alpha"),
        "A" => Some("upper-alpha"),
        "i" => Some("lower-roman"),
        "I" => Some("upper-roman"),
        _ => None,
    };
    let bullet = match value.to_ascii_lowercase().as_str() {
        "disc" => Some("disc"),
        "circle" => Some("circle"),
        "square" => Some("square"),
        _ => None,
    };
    match local {
        "ol" => numbered,
        "ul" | "menu" => bullet,
        _ => numbered.or(bullet),
    }
}

/// The declarations for element `id`, or `None` when it has none.
#[must_use]
pub fn css_for(arena: &Arena, id: NodeId) -> Option<String> {
    let node = arena.get(id)?;
    let name = node.element_name()?;
    if name.ns != html5ever::ns!(html) {
        return None;
    }
    let mut css = String::new();
    let get = |n: &str| attr(arena, id, n);
    match &*name.local {
        "table" => {
            css.push_str("display:table;border-spacing:2px;border-color:gray;");
            if let Some(n) = get("border").and_then(non_negative) {
                // `border` with a value maps to border-width and, for a non-zero
                // width, the outset style (HTML section 15.3.8).
                css.push_str(&format!("border-width:{n}px;"));
                if n > 0 {
                    css.push_str("border-style:outset;");
                }
            } else if get("border").is_some() {
                css.push_str("border-width:1px;border-style:outset;");
            }
            if let Some(n) = get("cellspacing").and_then(non_negative) {
                css.push_str(&format!("border-spacing:{n}px;"));
            }
            if let Some(d) = get("width").and_then(dimension) {
                css.push_str(&format!("width:{d};"));
            }
            if let Some(d) = get("height").and_then(dimension) {
                css.push_str(&format!("height:{d};"));
            }
            if let Some(c) = get("bgcolor").and_then(legacy_color) {
                css.push_str(&format!("background-color:{c};"));
            }
            if let Some(c) = get("bordercolor").and_then(legacy_color) {
                css.push_str(&format!("border-color:{c};"));
            }
            match get("align")
                .map(|a| a.trim().to_ascii_lowercase())
                .as_deref()
            {
                Some("left") => css.push_str("float:left;"),
                Some("right") => css.push_str("float:right;"),
                Some("center" | "middle") => css.push_str("margin-left:auto;margin-right:auto;"),
                _ => {}
            }
        }
        "caption" => {
            css.push_str("display:table-caption;text-align:center;");
            if let Some(a) = get("align").and_then(align_keyword) {
                css.push_str(&format!("text-align:{a};"));
            }
        }
        "colgroup" | "col" => {
            css.push_str(if &*name.local == "col" {
                "display:table-column;"
            } else {
                "display:table-column-group;"
            });
            if let Some(d) = get("width").and_then(dimension) {
                css.push_str(&format!("width:{d};"));
            }
        }
        "thead" | "tbody" | "tfoot" => {
            css.push_str(match &*name.local {
                "thead" => "display:table-header-group;",
                "tfoot" => "display:table-footer-group;",
                _ => "display:table-row-group;",
            });
            css.push_str("vertical-align:middle;");
            if let Some(a) = get("align").and_then(align_keyword) {
                css.push_str(&format!("text-align:{a};"));
            }
            if let Some(v) = get("valign").and_then(valign_keyword) {
                css.push_str(&format!("vertical-align:{v};"));
            }
        }
        "tr" => {
            css.push_str("display:table-row;vertical-align:inherit;");
            if let Some(a) = get("align").and_then(align_keyword) {
                css.push_str(&format!("text-align:{a};"));
            }
            if let Some(v) = get("valign").and_then(valign_keyword) {
                css.push_str(&format!("vertical-align:{v};"));
            }
            if let Some(d) = get("height").and_then(dimension) {
                css.push_str(&format!("height:{d};"));
            }
            if let Some(c) = get("bgcolor").and_then(legacy_color) {
                css.push_str(&format!("background-color:{c};"));
            }
        }
        "td" | "th" => {
            css.push_str("display:table-cell;vertical-align:inherit;");
            if &*name.local == "th" {
                css.push_str("font-weight:bold;text-align:center;");
            }
            let table = table_ancestor(arena, id);
            let mut padding = "1px".to_owned();
            if let Some(t) = table {
                if let Some(d) = attr(arena, t, "cellpadding").and_then(dimension) {
                    padding = d;
                }
                let bordered = attr(arena, t, "border")
                    .map(|b| non_negative(b).is_none_or(|n| n > 0))
                    .unwrap_or(false);
                if bordered {
                    css.push_str("border-width:1px;border-style:inset;border-color:gray;");
                }
            }
            css.push_str(&format!("padding:{padding};"));
            if let Some(d) = get("width").and_then(dimension) {
                css.push_str(&format!("width:{d};"));
            }
            if let Some(d) = get("height").and_then(dimension) {
                css.push_str(&format!("height:{d};"));
            }
            if let Some(c) = get("bgcolor").and_then(legacy_color) {
                css.push_str(&format!("background-color:{c};"));
            }
            if let Some(a) = get("align").and_then(align_keyword) {
                css.push_str(&format!("text-align:{a};"));
            }
            if let Some(v) = get("valign").and_then(valign_keyword) {
                css.push_str(&format!("vertical-align:{v};"));
            }
            if get("nowrap").is_some() {
                css.push_str("white-space:nowrap;");
            }
        }
        // HTML section 15.3.6 (Lists): the default `list-style-type` of each list
        // by nesting depth, and the legacy `type` attribute. At this level so
        // every embedder gets them without carrying a rule table (as for tables);
        // an author's `list-style` still wins.
        "ol" | "ul" | "menu" | "li" => {
            let depth = list_depth(arena, id);
            let own = match &*name.local {
                "ol" => "decimal",
                "li" => "",
                _ => match depth {
                    0 => "disc",
                    1 => "circle",
                    _ => "square",
                },
            };
            let typed = get("type").and_then(|t| list_type(&name.local, t));
            let value = typed.unwrap_or(own);
            if value.is_empty() {
                return None;
            }
            css.push_str("list-style-type:");
            css.push_str(value);
            css.push(';');
        }
        _ => return None,
    }
    Some(css)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css_of(html: &str, tag: &str) -> Option<String> {
        let dom = px_dom::parse(html);
        let arena = dom.arena;
        let doc = arena.document();
        let id = arena
            .descendants(doc)
            .find(|n| {
                arena
                    .get(*n)
                    .and_then(|x| x.element_name())
                    .is_some_and(|q| &*q.local == tag)
            })
            .expect("element");
        css_for(&arena, id)
    }

    #[test]
    fn hints_table_border_and_spacing() {
        let css = css_of("<table border=3 cellspacing=5 width=50%><tr><td>x", "table").unwrap();
        assert!(css.contains("border-width:3px;border-style:outset"));
        assert!(css.contains("border-spacing:5px"));
        assert!(css.contains("width:50%"));
    }

    #[test]
    fn hints_cellpadding_reaches_the_cell() {
        let css = css_of("<table cellpadding=7><tr><td>x", "td").unwrap();
        assert!(css.contains("padding:7px"));
    }

    #[test]
    fn hints_default_cell_padding_is_one_pixel() {
        let css = css_of("<table><tr><td>x", "td").unwrap();
        assert!(css.contains("padding:1px"));
    }

    #[test]
    fn hints_a_bordered_table_borders_its_cells() {
        let css = css_of("<table border=1><tr><td>x", "td").unwrap();
        assert!(css.contains("border-style:inset"));
    }

    #[test]
    fn hints_bgcolor_accepts_bare_hex() {
        let css = css_of("<table bgcolor=ff0000><tr><td>x", "table").unwrap();
        assert!(css.contains("background-color:#ff0000"));
    }
}
