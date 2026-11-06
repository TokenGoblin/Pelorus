//! The small subset of CSS an SVG image's own `<style>` and `style=""` use.
//!
//! Selectors: type, `*`, `.class`, `#id` compounds joined by descendant (space)
//! or child (`>`) combinators. A rule using anything else (attribute
//! selectors, pseudo-classes, sibling combinators) is dropped whole. At-rules
//! are skipped, so `@import` never fetches anything. Declarations are plain
//! `name: value` pairs; `url()` is only ever interpreted later, and only for
//! same-document `#id` references.

use super::doc::Doc;

/// Most rules kept from a document's styles.
pub const MAX_RULES: usize = 1024;
/// Most declarations kept per rule or `style` attribute.
const MAX_DECLS: usize = 128;
/// Most compounds in one selector.
const MAX_PARTS: usize = 4;

#[derive(Clone, Debug, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Comb {
    Descendant,
    Child,
}

#[derive(Clone, Debug)]
pub struct Rule {
    /// Left to right; `combs[i]` joins `parts[i]` and `parts[i + 1]`.
    parts: Vec<Compound>,
    combs: Vec<Comb>,
    pub specificity: u32,
    pub order: usize,
    pub decls: Vec<(String, String)>,
}

fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(p) = rest.find("/*") {
        out.push_str(rest.get(..p).unwrap_or(""));
        match rest.get(p + 2..).and_then(|r| r.find("*/")) {
            Some(e) => rest = rest.get(p + 2 + e + 2..).unwrap_or(""),
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Split on `sep` outside parentheses and quotes.
fn split_top(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0usize);
    let mut quote: Option<char> = None;
    for (i, c) in s.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth = (depth - 1).max(0),
            (None, c) if c == sep && depth == 0 => {
                out.push(s.get(start..i).unwrap_or(""));
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s.get(start..).unwrap_or(""));
    out
}

/// `name: value; ...` declarations (lower-case names, `!important` dropped).
pub fn parse_decls(text: &str) -> Vec<(String, String)> {
    let text = strip_comments(text);
    let mut out = Vec::new();
    for d in split_top(&text, ';') {
        let Some((n, v)) = d.split_once(':') else {
            continue;
        };
        let name = n.trim().to_ascii_lowercase();
        let mut value = v.trim();
        if let Some(i) = value.rfind('!') {
            let tail = value.get(i + 1..).unwrap_or("").trim();
            if tail.eq_ignore_ascii_case("important") {
                value = value.get(..i).unwrap_or("").trim();
            }
        }
        if !name.is_empty() && !value.is_empty() && out.len() < MAX_DECLS {
            out.push((name, value.to_string()));
        }
    }
    out
}

fn parse_compound(s: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let mut chars = s.char_indices().peekable();
    let ident_end = |from: usize| -> usize {
        s.get(from..)
            .and_then(|r| r.find(|ch: char| !(ch.is_alphanumeric() || ch == '-' || ch == '_')))
            .map_or(s.len(), |p| p + from)
    };
    while let Some((i, ch)) = chars.next() {
        match ch {
            '*' => {}
            '.' | '#' => {
                let end = ident_end(i + 1);
                let name = s.get(i + 1..end)?;
                if name.is_empty() {
                    return None;
                }
                if ch == '.' {
                    c.classes.push(name.to_string());
                } else {
                    c.id = Some(name.to_string());
                }
                while chars.peek().is_some_and(|(j, _)| *j < end) {
                    chars.next();
                }
            }
            ch if ch.is_alphabetic() && i == 0 => {
                let end = ident_end(i);
                c.tag = Some(s.get(i..end)?.to_string());
                while chars.peek().is_some_and(|(j, _)| *j < end) {
                    chars.next();
                }
            }
            _ => return None,
        }
    }
    Some(c)
}

fn parse_selector(s: &str) -> Option<(Vec<Compound>, Vec<Comb>, u32)> {
    let spaced = s.replace('>', " > ");
    let mut parts = Vec::new();
    let mut combs = Vec::new();
    let mut pending = Comb::Descendant;
    let mut spec = 0u32;
    for tok in spaced.split_whitespace() {
        if tok == ">" {
            pending = Comb::Child;
            continue;
        }
        let c = parse_compound(tok)?;
        spec += (u32::from(c.id.is_some()) * 10_000)
            + (u32::try_from(c.classes.len()).unwrap_or(0) * 100)
            + u32::from(c.tag.is_some());
        if !parts.is_empty() {
            combs.push(pending);
        }
        pending = Comb::Descendant;
        parts.push(c);
        if parts.len() > MAX_PARTS {
            return None;
        }
    }
    (!parts.is_empty()).then_some((parts, combs, spec))
}

/// Parse a stylesheet, appending its usable rules to `rules`.
pub fn parse_sheet(text: &str, rules: &mut Vec<Rule>) {
    parse_sheet_max(text, rules, MAX_RULES);
}

/// [`parse_sheet`] keeping at most `max` rules in `rules`.
pub fn parse_sheet_max(text: &str, rules: &mut Vec<Rule>, max: usize) {
    let text = strip_comments(text);
    let b = text.as_bytes();
    let mut i = 0usize;
    while i < b.len() && rules.len() < max {
        while b.get(i).is_some_and(u8::is_ascii_whitespace) {
            i += 1;
        }
        if i >= b.len() {
            break;
        }
        let semi = text.get(i..).and_then(|r| r.find(';')).map(|p| p + i);
        let open = text.get(i..).and_then(|r| r.find('{')).map(|p| p + i);
        if b.get(i) == Some(&b'@') {
            // Skip the at-rule: to its `;` or past its balanced block.
            match (semi, open) {
                (Some(s), o) if o.is_none_or(|o| s < o) => i = s + 1,
                (_, Some(o)) => i = skip_block(&text, o),
                _ => break,
            }
            continue;
        }
        let Some(open) = open else { break };
        let close = skip_block(&text, open);
        let sel_text = text.get(i..open).unwrap_or("");
        let body = text.get(open + 1..close.saturating_sub(1)).unwrap_or("");
        let decls = parse_decls(body);
        for sel in sel_text.split(',') {
            if let Some((parts, combs, specificity)) = parse_selector(sel)
                && rules.len() < max
            {
                rules.push(Rule {
                    parts,
                    combs,
                    specificity,
                    order: rules.len(),
                    decls: decls.clone(),
                });
            }
        }
        i = close;
    }
}

/// The index just past the `}` matching the `{` at `open`.
fn skip_block(text: &str, open: usize) -> usize {
    let mut depth = 0usize;
    for (k, c) in text.get(open..).unwrap_or("").char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return open + k + 1;
                }
            }
            _ => {}
        }
    }
    text.len()
}

fn compound_matches(c: &Compound, doc: &Doc, el: u32) -> bool {
    let Some(e) = doc.get(el) else { return false };
    if c.tag.as_deref().is_some_and(|t| t != e.name) {
        return false;
    }
    if c.id.as_deref().is_some_and(|i| e.attr("id") != Some(i)) {
        return false;
    }
    let classes = e.attr("class").unwrap_or("");
    c.classes
        .iter()
        .all(|want| classes.split_whitespace().any(|have| have == want))
}

impl Rule {
    /// Whether the rule's selector matches `el`.
    pub fn matches(&self, doc: &Doc, el: u32) -> bool {
        let Some(last) = self.parts.last() else {
            return false;
        };
        if !compound_matches(last, doc, el) {
            return false;
        }
        let mut cur = el;
        for k in (0..self.parts.len().saturating_sub(1)).rev() {
            let (Some(part), Some(comb)) = (self.parts.get(k), self.combs.get(k)) else {
                return false;
            };
            let mut up = doc.parent_of(cur);
            let mut found = None;
            while let Some(a) = up {
                if compound_matches(part, doc, a) {
                    found = Some(a);
                    break;
                }
                if *comb == Comb::Child {
                    break;
                }
                up = doc.parent_of(a);
            }
            match found {
                Some(a) => cur = a,
                None => return false,
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::super::xml;
    use super::*;

    #[test]
    fn declarations() {
        let d = parse_decls("fill: red; /* c */ stroke : url(#a;b) !important;;bad; x:");
        assert_eq!(
            d,
            [
                ("fill".to_string(), "red".to_string()),
                ("stroke".to_string(), "url(#a;b)".to_string())
            ]
        );
    }

    #[test]
    fn rules_match_with_specificity_and_combinators() {
        let doc =
            xml::parse("<svg><g class='a b'><rect id='r' class='c'/></g><circle/></svg>").unwrap();
        let mut rules = Vec::new();
        parse_sheet(
            "@import url(http://evil/x.css); @media print { rect { fill: blue } }\
             rect{fill:red} g > .c {x:1} svg rect#r {y:1} .a .c, circle {z:1} a:hover{w:1} [x]{v:1}",
            &mut rules,
        );
        // @-rules, :hover and [x] dropped; ".a .c, circle" is two rules.
        assert_eq!(rules.len(), 5);
        let rect = 2;
        let hits: Vec<&str> = rules
            .iter()
            .filter(|r| r.matches(&doc, rect))
            .map(|r| r.decls.first().map_or("", |d| d.0.as_str()))
            .collect();
        assert_eq!(hits, ["fill", "x", "y", "z"]);
        assert!(rules.iter().filter(|r| r.matches(&doc, 3)).count() == 1);
        assert!(rules.iter().all(|r| r.specificity > 0));
    }

    #[test]
    fn hostile_sheets_terminate() {
        let mut rules = Vec::new();
        parse_sheet("a{{{{{{ b{c:d", &mut rules);
        parse_sheet("}}}} @@@ ;;; {", &mut rules);
        parse_sheet(&"a{b:c}".repeat(5000), &mut rules);
        assert!(rules.len() <= MAX_RULES);
    }
}
