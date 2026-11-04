//! Shared helpers. Each integration test file uses a different subset.
#![allow(dead_code)]

use px_a11y::{A11yNode, AccessibilityTree, BuildInputs, Role};
use px_dom::{Dom, NodeData, NodeId};

/// A parsed page and its tree.
pub struct Page {
    pub dom: Dom,
    pub tree: AccessibilityTree,
}

impl Page {
    pub fn new(html: &str) -> Page {
        let dom = px_dom::parse(html);
        let tree = AccessibilityTree::build(&dom.arena, BuildInputs::default());
        Page { dom, tree }
    }

    /// The element whose `id` attribute is `id`.
    pub fn element(&self, id: &str) -> Option<NodeId> {
        self.dom
            .arena
            .descendants(self.dom.arena.document())
            .find(|n| {
                matches!(
                    self.dom.arena.get(*n).map(|x| x.data()),
                    Some(NodeData::Element { attrs, .. })
                        if attrs.iter().any(|a| &*a.name.local == "id" && &*a.value == id)
                )
            })
    }

    /// The tree node for the element with this `id`, if it is exposed.
    pub fn node(&self, id: &str) -> Option<&A11yNode> {
        let dom = self.element(id)?;
        self.tree.node(self.tree.find(dom)?)
    }

    /// Its role token, or `"-"` when the element is not in the tree.
    pub fn role(&self, id: &str) -> &'static str {
        self.node(id).map_or("-", |n| n.role().token())
    }

    pub fn name(&self, id: &str) -> String {
        self.node(id)
            .map_or_else(|| "<absent>".into(), |n| n.name().to_string())
    }

    pub fn desc(&self, id: &str) -> String {
        self.node(id)
            .map_or_else(|| "<absent>".into(), |n| n.description().to_string())
    }

    /// The dump without the `document` line, de-indented one level.
    pub fn lines(&self) -> String {
        strip_root(&self.tree.dump())
    }
}

pub fn strip_root(dump: &str) -> String {
    let mut out = String::new();
    for line in dump.lines().skip(1) {
        out.push_str(line.strip_prefix("  ").unwrap_or(line));
        out.push('\n');
    }
    out
}

/// De-indented dump of a page.
pub fn lines(html: &str) -> String {
    Page::new(html).lines()
}

/// Role token of the element with id `x`.
pub fn role_of(html: &str) -> &'static str {
    Page::new(html).role("x")
}

pub fn name_of(html: &str) -> String {
    Page::new(html).name("x")
}

pub fn is_role(role: Role, token: &str) -> bool {
    role.token() == token
}

/// xorshift64*, deterministic.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items.get(self.below(items.len())).copied().unwrap_or("")
    }

    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % (n.max(1) as u64)).unwrap_or(0)
    }
}
