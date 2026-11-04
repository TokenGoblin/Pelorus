//! What the tree builder reads besides the DOM: computed style and layout, as
//! plain data.
//!
//! The builder depends on `px-dom` and nothing else. Style and geometry arrive
//! through two small traits so that this crate does not compile the style
//! engine and so that a test, a fuzz target or a future out-of-process caller
//! can supply them without one. The glue from `px-css` and `px-layout` (a
//! `StyleRoot` and a `FragmentTree`) is a dozen lines that belong in a crate
//! which already depends on both; `tests/layout.rs` contains a working
//! adapter and is the reference for it.

use std::collections::HashMap;

use px_dom::NodeId;

/// The two facts about computed style that decide whether a node is exposed.
///
/// `display: none` removes a subtree from the tree. `visibility: hidden`
/// removes only the node itself (a descendant may set `visibility: visible`
/// again), so it is carried separately. Everything else in the style system is
/// irrelevant to accessibility and deliberately not carried.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StyleHints {
    /// The element generates no box (`display: none`).
    pub display_none: bool,
    /// The computed `visibility` is `hidden` or `collapse`.
    pub visibility_hidden: bool,
}

/// A source of [`StyleHints`] keyed by DOM node.
pub trait StyleSource {
    /// The hints for `node`, or `None` when the node has no computed style
    /// (not an element, or style was never resolved for it). `None` is
    /// treated as "visible": the builder does not guess.
    fn hints(&self, node: NodeId) -> Option<StyleHints>;
}

impl StyleSource for HashMap<NodeId, StyleHints> {
    fn hints(&self, node: NodeId) -> Option<StyleHints> {
        self.get(&node).copied()
    }
}

/// A border box in CSS pixels, rounded to whole pixels, in the coordinate
/// space of the page (the fragment tree's origin).
///
/// Integers, not app units: a platform bridge wants pixels, and `Eq` is what
/// the diff needs to decide whether bounds changed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// A source of [`Bounds`] keyed by DOM node.
pub trait BoundsSource {
    /// The page-space border box of `node`, or `None` when it has no box.
    fn bounds(&self, node: NodeId) -> Option<Bounds>;
}

impl BoundsSource for HashMap<NodeId, Bounds> {
    fn bounds(&self, node: NodeId) -> Option<Bounds> {
        self.get(&node).copied()
    }
}

/// Everything optional that a build can use.
#[derive(Clone, Copy, Default)]
pub struct BuildInputs<'a> {
    /// Computed style. `None` means "no style information": `hidden`,
    /// `aria-hidden` and the user-agent never-rendered elements still apply,
    /// but `display: none` from a stylesheet cannot be known.
    pub style: Option<&'a dyn StyleSource>,
    /// Layout geometry. `None` leaves every node's bounds `None`.
    pub bounds: Option<&'a dyn BoundsSource>,
}

/// Limits for one build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuildOptions {
    /// The most nodes the tree will hold. Past it the build stops and the tree
    /// reports [`crate::AccessibilityTree::is_truncated`].
    pub max_nodes: usize,
    /// The most DOM nodes the name and description computation may visit in
    /// total across one build. Past it names come out empty; without it a
    /// chain of nested buttons costs the square of its depth.
    pub max_name_visits: usize,
}

/// The default node cap. A page with more exposed nodes than this is not
/// usefully navigable by a screen reader and is a memory hazard for the
/// process that receives the tree.
pub const MAX_NODES: usize = 65_536;

/// The default total work allowance for name computation in one build.
pub const MAX_NAME_VISITS: usize = 4_000_000;

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            max_nodes: MAX_NODES,
            max_name_visits: MAX_NAME_VISITS,
        }
    }
}
