//! The box tree.
//!
//! Built once from the DOM, then laid out in place. Boxes are stored flat in a
//! `Vec` and refer to each other by index, so building, walking and dropping
//! never recurse over tree depth (the Phase 6 gate's structural half), and the
//! order is a pure function of the input (the other half: identical box trees
//! on repeat runs).

use px_dom::{Arena, NodeId};

use crate::au::Au;
use crate::style::{BoxStyle, Display, Edges};

/// Index of a box. Only meaningful for the tree that issued it; every lookup
/// returns `Option`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BoxId(u32);

/// Resolved geometry, in CSS pixels' app units, relative to the root's origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Geometry {
    /// Border-box origin and size.
    pub x: Au,
    pub y: Au,
    pub width: Au,
    pub height: Au,
    pub margin: Edges<Au>,
    pub padding: Edges<Au>,
    pub border: Edges<Au>,
}

impl Geometry {
    pub fn content_box(&self) -> (Au, Au, Au, Au) {
        let left = self.border.left + self.padding.left;
        let top = self.border.top + self.padding.top;
        let w = self.width - left - self.border.right - self.padding.right;
        let h = self.height - top - self.border.bottom - self.padding.bottom;
        (
            self.x + left,
            self.y + top,
            w.non_negative(),
            h.non_negative(),
        )
    }
}

#[derive(Debug)]
pub struct LayoutBox {
    pub node: Option<NodeId>,
    pub style: BoxStyle,
    pub(crate) parent: Option<BoxId>,
    pub(crate) children: Vec<BoxId>,
    pub geometry: Geometry,
}

#[derive(Debug, Default)]
pub struct LayoutTree {
    boxes: Vec<LayoutBox>,
    root: Option<BoxId>,
}

impl LayoutTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Boxes in creation order, which is document order for a tree built from
    /// the DOM.
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    pub fn root(&self) -> Option<BoxId> {
        self.root
    }

    pub fn get(&self, id: BoxId) -> Option<&LayoutBox> {
        self.boxes.get(id.0 as usize)
    }

    pub(crate) fn get_mut(&mut self, id: BoxId) -> Option<&mut LayoutBox> {
        self.boxes.get_mut(id.0 as usize)
    }

    pub fn children(&self, id: BoxId) -> &[BoxId] {
        self.get(id).map(|b| b.children.as_slice()).unwrap_or(&[])
    }

    pub fn parent(&self, id: BoxId) -> Option<BoxId> {
        self.get(id).and_then(|b| b.parent)
    }

    /// Add a box. `parent == None` makes it the root, which may happen once.
    /// Returns `None` if the parent is unknown, a second root is attempted, or
    /// the tree is full.
    pub fn push(
        &mut self,
        parent: Option<BoxId>,
        node: Option<NodeId>,
        style: BoxStyle,
    ) -> Option<BoxId> {
        let index = u32::try_from(self.boxes.len()).ok()?;
        let id = BoxId(index);
        match parent {
            Some(p) => self.get_mut(p)?.children.push(id),
            None if self.root.is_some() => return None,
            None => self.root = Some(id),
        }
        self.boxes.push(LayoutBox {
            node,
            style,
            parent,
            children: Vec::new(),
            geometry: Geometry::default(),
        });
        Some(id)
    }

    /// Build the box tree for the subtree under the document's root element.
    ///
    /// `style_of` supplies each element's style. Elements it returns `None`
    /// for, `display: none` elements, and inline-level elements generate no
    /// box (and neither do their descendants); non-element nodes generate none
    /// until inline layout exists. Iterative.
    pub fn build(arena: &Arena, mut style_of: impl FnMut(NodeId) -> Option<BoxStyle>) -> Self {
        let mut tree = LayoutTree::new();
        // (dom node, box parent). Pushed in reverse so pops are document order.
        let mut stack: Vec<(NodeId, Option<BoxId>)> = Vec::new();
        for child in arena
            .child_ids(arena.document())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
        {
            stack.push((child, None));
        }
        while let Some((node, parent)) = stack.pop() {
            let Some(n) = arena.get(node) else { continue };
            if n.element_name().is_none() {
                continue;
            }
            let Some(style) = style_of(node) else {
                continue;
            };
            if style.display != Display::Block {
                continue;
            }
            let Some(id) = tree.push(parent, Some(node), style) else {
                continue;
            };
            for child in arena.child_ids(node).collect::<Vec<_>>().into_iter().rev() {
                stack.push((child, Some(id)));
            }
        }
        tree
    }
}
