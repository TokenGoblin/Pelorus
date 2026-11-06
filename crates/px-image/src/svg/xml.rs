//! A small, strict XML parser for SVG documents.
//!
//! Hostile-input choices:
//! * No entity is ever defined or expanded beyond the five predefined ones and
//!   numeric references. A `<!DOCTYPE>` (with or without an internal subset)
//!   is skipped unread, so an entity-expansion bomb has nothing to expand and
//!   `&name;` for any other name is an error.
//! * No external reference is followed: there is no DTD, XInclude or
//!   `xml-stylesheet` processing. Processing instructions are skipped.
//! * Element count, depth, attribute count and style bytes are capped by
//!   [`super::doc::Builder`]; the input length is capped by the caller.
//! * Mismatched or unterminated tags are errors, never guesses.

use super::doc::{Builder, Doc, SVG_NS};
use crate::DecodeError;

fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r')
}

fn find(hay: &str, from: usize, needle: &str) -> Option<usize> {
    hay.get(from..)?.find(needle).map(|p| p + from)
}

/// Decode the five predefined entities and numeric references.
fn unescape(s: &str, attr: bool) -> Result<String, DecodeError> {
    if !s.contains('&') && !(attr && s.contains(['\n', '\t', '\r'])) {
        return Ok(s.to_string());
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find('&') {
        push_normalised(&mut out, rest.get(..p).unwrap_or(""), attr);
        let after = rest.get(p + 1..).unwrap_or("");
        let end = after
            .find(';')
            .filter(|e| *e <= 10)
            .ok_or(DecodeError::Malformed("bad entity reference"))?;
        let name = after.get(..end).unwrap_or("");
        let ch = match name {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let num = name
                    .strip_prefix("#x")
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| name.strip_prefix('#').and_then(|d| d.parse().ok()))
                    .ok_or(DecodeError::Malformed("undefined entity"))?;
                char::from_u32(num)
                    .filter(|c| *c != '\0')
                    .ok_or(DecodeError::Malformed("bad character reference"))?
            }
        };
        out.push(ch);
        rest = after.get(end + 1..).unwrap_or("");
    }
    push_normalised(&mut out, rest, attr);
    Ok(out)
}

/// Attribute-value normalisation: tab, CR and LF each become a space.
fn push_normalised(out: &mut String, s: &str, attr: bool) {
    if attr {
        out.extend(s.chars().map(|c| {
            if matches!(c, '\n' | '\t' | '\r') {
                ' '
            } else {
                c
            }
        }));
    } else {
        out.push_str(s);
    }
}

/// Skip a `<!DOCTYPE ...>` starting at `i` (which points at `<!`).
fn skip_doctype(s: &str, i: usize) -> Result<usize, DecodeError> {
    let b = s.as_bytes();
    let mut j = i + 2;
    let mut brackets = 0usize;
    let mut quote: Option<u8> = None;
    while let Some(&c) = b.get(j) {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, b'"' | b'\'') => quote = Some(c),
            (None, b'[') => brackets += 1,
            (None, b']') => brackets = brackets.saturating_sub(1),
            (None, b'>') if brackets == 0 => return Ok(j + 1),
            _ => {}
        }
        j += 1;
    }
    Err(DecodeError::Truncated)
}

struct OpenTag {
    name: String,
    attrs: Vec<(String, String)>,
    empty: bool,
    end: usize,
}

fn parse_open(s: &str, start: usize) -> Result<OpenTag, DecodeError> {
    let b = s.as_bytes();
    let mut i = start;
    let name_end = (i..b.len())
        .find(|k| {
            b.get(*k)
                .is_some_and(|c| is_ws(*c) || *c == b'/' || *c == b'>')
        })
        .ok_or(DecodeError::Truncated)?;
    let name = s.get(i..name_end).unwrap_or("").to_string();
    if name.is_empty() {
        return Err(DecodeError::Malformed("empty tag name"));
    }
    i = name_end;
    let mut attrs = Vec::new();
    loop {
        while b.get(i).is_some_and(|c| is_ws(*c)) {
            i += 1;
        }
        match b.get(i) {
            None => return Err(DecodeError::Truncated),
            Some(b'>') => {
                return Ok(OpenTag {
                    name,
                    attrs,
                    empty: false,
                    end: i + 1,
                });
            }
            Some(b'/') => {
                if b.get(i + 1) == Some(&b'>') {
                    return Ok(OpenTag {
                        name,
                        attrs,
                        empty: true,
                        end: i + 2,
                    });
                }
                return Err(DecodeError::Malformed("stray slash in tag"));
            }
            Some(_) => {}
        }
        let an_end = (i..b.len())
            .find(|k| {
                b.get(*k)
                    .is_some_and(|c| is_ws(*c) || *c == b'=' || *c == b'>' || *c == b'/')
            })
            .ok_or(DecodeError::Truncated)?;
        let an = s.get(i..an_end).unwrap_or("").to_string();
        if an.is_empty() {
            return Err(DecodeError::Malformed("empty attribute name"));
        }
        i = an_end;
        while b.get(i).is_some_and(|c| is_ws(*c)) {
            i += 1;
        }
        if b.get(i) != Some(&b'=') {
            return Err(DecodeError::Malformed("attribute without a value"));
        }
        i += 1;
        while b.get(i).is_some_and(|c| is_ws(*c)) {
            i += 1;
        }
        let q = match b.get(i) {
            Some(q @ (b'"' | b'\'')) => *q,
            _ => return Err(DecodeError::Malformed("unquoted attribute value")),
        };
        let vstart = i + 1;
        let vend = (vstart..b.len())
            .find(|k| b.get(*k) == Some(&q))
            .ok_or(DecodeError::Truncated)?;
        let raw = s.get(vstart..vend).unwrap_or("");
        if raw.contains('<') {
            return Err(DecodeError::Malformed("'<' in attribute value"));
        }
        attrs.push((an, unescape(raw, true)?));
        i = vend + 1;
        // Attributes must be separated from what follows.
        if !b
            .get(i)
            .is_some_and(|c| is_ws(*c) || *c == b'/' || *c == b'>')
        {
            return Err(DecodeError::Malformed("attributes not separated"));
        }
    }
}

/// Parse `text` into a document.
pub fn parse(text: &str) -> Result<Doc, DecodeError> {
    let b = text.as_bytes();
    let mut bld = Builder::new();
    let mut names: Vec<String> = Vec::new();
    let mut i = 0usize;
    // Namespace of each open element: true when it is the SVG namespace.
    let mut ns_stack: Vec<bool> = Vec::new();
    loop {
        let lt = find(text, i, "<");
        let chunk = text.get(i..lt.unwrap_or(b.len())).unwrap_or("");
        if bld.depth() > 0 {
            bld.text(&unescape(chunk, false)?);
        } else if chunk.bytes().any(|c| !is_ws(c)) && chunk != "\u{feff}" {
            return Err(DecodeError::Malformed("text outside the root element"));
        }
        let Some(lt) = lt else { break };
        i = lt;
        let rest = text.get(i..).unwrap_or("");
        if rest.starts_with("<!--") {
            let end = find(text, i + 4, "-->").ok_or(DecodeError::Truncated)?;
            i = end + 3;
        } else if rest.starts_with("<![CDATA[") {
            let end = find(text, i + 9, "]]>").ok_or(DecodeError::Truncated)?;
            if bld.depth() > 0 {
                bld.text(text.get(i + 9..end).unwrap_or(""));
            }
            i = end + 3;
        } else if rest.starts_with("<?") {
            let end = find(text, i + 2, "?>").ok_or(DecodeError::Truncated)?;
            i = end + 2;
        } else if rest.starts_with("<!") {
            i = skip_doctype(text, i)?;
        } else if rest.starts_with("</") {
            let end = find(text, i + 2, ">").ok_or(DecodeError::Truncated)?;
            let name = text.get(i + 2..end).unwrap_or("").trim();
            if names.pop().as_deref() != Some(name) {
                return Err(DecodeError::Malformed("mismatched closing tag"));
            }
            ns_stack.pop();
            bld.close();
            i = end + 1;
        } else {
            let tag = parse_open(text, i + 1)?;
            let (prefix, local) = match tag.name.split_once(':') {
                Some((p, l)) => (Some(p), l),
                None => (None, tag.name.as_str()),
            };
            let parent_svg = ns_stack.last().copied().unwrap_or(true);
            let mut in_svg = parent_svg;
            let mut attrs = Vec::new();
            for (n, v) in &tag.attrs {
                if n == "xmlns" {
                    in_svg = v == SVG_NS;
                } else if n.starts_with("xmlns:") {
                    // Prefix declarations are not interpreted: a prefixed
                    // element is simply never rendered.
                } else if let Some(l) = n.strip_prefix("xlink:") {
                    attrs.push((l.to_string(), v.clone()));
                } else {
                    attrs.push((n.clone(), v.clone()));
                }
            }
            if bld.depth() == 0 && prefix.is_none() && local == "svg" && !in_svg {
                return Err(DecodeError::Malformed("not an SVG document"));
            }
            bld.open(local, prefix.is_some() || !in_svg, attrs)?;
            if tag.empty {
                bld.close();
            } else {
                names.push(tag.name);
                ns_stack.push(in_svg);
            }
            i = tag.end;
        }
    }
    bld.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(d: &Doc) -> Vec<&str> {
        d.els.iter().map(|e| e.name.as_str()).collect()
    }

    #[test]
    fn parses_nesting_attrs_and_entities() {
        let d = parse(
            "<?xml version='1.0'?><!-- c --><svg xmlns=\"http://www.w3.org/2000/svg\" id='a&amp;b'>\
             <g><rect width=\"1&#x32;\"/></g><style><![CDATA[a>b{}]]></style></svg>",
        )
        .unwrap();
        assert_eq!(names(&d), ["svg", "g", "rect", "style"]);
        assert_eq!(d.by_id("a&b"), Some(0));
        assert_eq!(d.els.get(2).unwrap().attr("width"), Some("12"));
        assert_eq!(d.els.get(3).unwrap().text, "a>b{}");
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "<svg>",
            "<svg></g>",
            "<svg/><svg/>",
            "<svg a=1/>",
            "<svg>&bogus;</svg>",
            "<svg>&#0;</svg>",
            "<g/>",
            "x<svg/>",
            "<svg xmlns='http://example.com/'/>",
            "<svg a='<'/>",
            "<svg a='1'b='2'/>",
            "<svg><!-- unterminated</svg>",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn entity_bomb_is_inert() {
        let bomb = "<?xml version='1.0'?><!DOCTYPE svg [\
            <!ENTITY a 'aaaaaaaaaa'><!ENTITY b '&a;&a;&a;&a;&a;&a;&a;&a;&a;&a;'>\
            <!ENTITY c '&b;&b;&b;&b;&b;&b;&b;&b;&b;&b;'>]>\
            <svg xmlns='http://www.w3.org/2000/svg'><text>&c;</text></svg>";
        assert!(parse(bomb).is_err());
        let doctype_only = "<!DOCTYPE svg [<!ENTITY a 'x'>]><svg/>";
        assert!(parse(doctype_only).is_ok());
    }

    #[test]
    fn limits_hold() {
        let deep = format!("{}{}", "<g>".repeat(200), "</g>".repeat(200));
        assert!(parse(&format!("<svg>{deep}</svg>")).is_err());
        let wide = format!("<svg>{}</svg>", "<g/>".repeat(60_000));
        assert!(parse(&wide).is_err());
    }

    #[test]
    fn foreign_elements_are_marked() {
        let d = parse("<svg><foreignObject><p xmlns='http://www.w3.org/1999/xhtml'/></foreignObject><x:y/></svg>").unwrap();
        assert_eq!(names(&d), ["svg", "foreignObject", "ns:p", "ns:y"]);
    }
}
