//! The element arena an SVG document is parsed into, and the builder that
//! fills it (from the XML parser, or from an HTML DOM's inline `<svg>`).

use std::collections::HashMap;

use crate::DecodeError;

/// The SVG namespace.
pub const SVG_NS: &str = "http://www.w3.org/2000/svg";

/// Most elements in one document.
pub const MAX_ELEMENTS: usize = 50_000;
/// Deepest element nesting.
pub const MAX_DEPTH: usize = 64;
/// Most attributes kept on one element.
pub const MAX_ATTRS: usize = 128;
/// Most bytes of `<style>` text kept in total.
pub const MAX_STYLE_BYTES: usize = 256 * 1024;

/// One element. `name` is the local name; an element in another namespace, or
/// with a prefix, is named `ns:...` and is never rendered.
#[derive(Debug)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    /// Concatenated character data (kept for `<style>` only).
    pub text: String,
    pub kids: Vec<u32>,
    pub parent: Option<u32>,
}

impl El {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A parsed SVG document: elements by index (the root is 0) and an id table.
#[derive(Debug)]
pub struct Doc {
    pub els: Vec<El>,
    pub ids: HashMap<String, u32>,
    pub style_bytes: usize,
    /// The HTML ancestors of an inline `<svg>`, innermost first. They exist
    /// only so page selectors such as `.icon path` can look past the root;
    /// they are never rendered and have the ids `OUTER_BASE + k`.
    pub outer: Vec<El>,
}

/// The first id of a context ancestor (see [`Doc::outer`]).
pub const OUTER_BASE: u32 = 1 << 30;
/// Most context ancestors kept.
pub const MAX_OUTER: usize = 32;

impl Doc {
    pub fn get(&self, i: u32) -> Option<&El> {
        match i.checked_sub(OUTER_BASE) {
            Some(k) => self.outer.get(k as usize),
            None => self.els.get(i as usize),
        }
    }
    /// The parent of `i`, the root's being the innermost context ancestor.
    pub fn parent_of(&self, i: u32) -> Option<u32> {
        if i == 0 {
            return (!self.outer.is_empty()).then_some(OUTER_BASE);
        }
        self.get(i).and_then(|e| e.parent)
    }
    /// Set the context ancestors, innermost first, as `(tag, id, class)`.
    pub fn set_outer(&mut self, chain: &[(String, String, String)]) {
        self.outer.clear();
        for (k, (tag, id, class)) in chain.iter().take(MAX_OUTER).enumerate() {
            let mut attrs = Vec::new();
            if !id.is_empty() {
                attrs.push(("id".to_string(), id.clone()));
            }
            if !class.is_empty() {
                attrs.push(("class".to_string(), class.clone()));
            }
            let next = k + 1;
            self.outer.push(El {
                name: tag.clone(),
                attrs,
                text: String::new(),
                kids: Vec::new(),
                parent: (next < chain.len().min(MAX_OUTER)).then(|| OUTER_BASE + next as u32),
            });
        }
    }
    pub fn by_id(&self, id: &str) -> Option<u32> {
        self.ids.get(id).copied()
    }
}

/// Builds a [`Doc`] from open/text/close events, enforcing the limits.
#[derive(Debug, Default)]
pub struct Builder {
    els: Vec<El>,
    stack: Vec<u32>,
    ids: HashMap<String, u32>,
    style_bytes: usize,
    closed_root: bool,
}

impl Builder {
    pub fn new() -> Builder {
        Builder::default()
    }

    pub fn depth(&self) -> usize {
        self.stack.len()
    }

    /// Open an element. `name` is its local name; `qualified` is true when it
    /// had a namespace prefix or a foreign default namespace (it is kept in
    /// the tree, never rendered). The first element must be `svg`.
    pub fn open(
        &mut self,
        name: &str,
        qualified: bool,
        attrs: Vec<(String, String)>,
    ) -> Result<(), DecodeError> {
        if self.closed_root {
            return Err(DecodeError::Malformed("content after the root element"));
        }
        if self.els.len() >= MAX_ELEMENTS {
            return Err(DecodeError::LimitExceeded("svg elements"));
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(DecodeError::LimitExceeded("svg depth"));
        }
        let name = if qualified {
            format!("ns:{name}")
        } else {
            name.to_string()
        };
        if self.els.is_empty() && name != "svg" {
            return Err(DecodeError::Malformed("root element is not svg"));
        }
        let idx = u32::try_from(self.els.len())
            .map_err(|_| DecodeError::LimitExceeded("svg elements"))?;
        let parent = self.stack.last().copied();
        let mut kept: Vec<(String, String)> = Vec::new();
        for (n, v) in attrs.into_iter().take(MAX_ATTRS) {
            if !kept.iter().any(|(k, _)| *k == n) {
                kept.push((n, v));
            }
        }
        if let Some((_, id)) = kept.iter().find(|(n, _)| n == "id") {
            self.ids.entry(id.clone()).or_insert(idx);
        }
        self.els.push(El {
            name,
            attrs: kept,
            text: String::new(),
            kids: Vec::new(),
            parent,
        });
        if let Some(p) = parent
            && let Some(pe) = self.els.get_mut(p as usize)
        {
            pe.kids.push(idx);
        }
        self.stack.push(idx);
        Ok(())
    }

    /// Character data inside the current element.
    pub fn text(&mut self, s: &str) {
        let Some(&top) = self.stack.last() else {
            return;
        };
        let Some(el) = self.els.get_mut(top as usize) else {
            return;
        };
        if el.name == "style" && self.style_bytes < MAX_STYLE_BYTES {
            let room = MAX_STYLE_BYTES - self.style_bytes;
            let take = s.len().min(room);
            if let Some(part) = s.get(..take) {
                el.text.push_str(part);
                self.style_bytes += part.len();
            }
        }
    }

    pub fn close(&mut self) {
        self.stack.pop();
        if self.stack.is_empty() && !self.els.is_empty() {
            self.closed_root = true;
        }
    }

    pub fn finish(self) -> Result<Doc, DecodeError> {
        if self.els.is_empty() {
            return Err(DecodeError::Malformed("no svg element"));
        }
        if !self.stack.is_empty() {
            return Err(DecodeError::Truncated);
        }
        Ok(Doc {
            els: self.els,
            ids: self.ids,
            style_bytes: self.style_bytes,
            outer: Vec::new(),
        })
    }
}
