//! The fragment tree: what layout produces.
//!
//! A *box* is what the box tree says should exist; a *fragment* is one piece of
//! it with geometry. They differ when a box is split — across lines, and later
//! across pages and columns — so the two are separate ideas even though Phase 6
//! produces exactly one fragment per box everywhere except inline content.
//!
//! # Flat, and not for tidiness
//!
//! Fragments live in one `Vec` and refer to each other by index. No fragment owns
//! another: a `Vec<Fragment>` *inside* a `Fragment` would give the compiler a
//! recursive `Drop`, and dropping a hundred-thousand-deep tree then overflows the
//! stack in a function nobody wrote. `px-dom` reached the same arrangement for
//! the same reason, and Phase 4's gate item exists because it is the failure that
//! is invisible until it is a crash report.
//!
//! **That property is enforced by `Fragment: Copy`, not by a source scan.** A type
//! owning a `Vec` cannot be `Copy`, so the structure simply does not compile.
//! `cargo xtask layout` carries a note about the two scans that tried to catch this
//! by grepping and both fired on correct code — the second on `FragmentTree`'s own
//! `Vec<Fragment>`, which is the arena the scan existed to encourage.
//!
//! Children are a contiguous range rather than a linked list. Layout builds a
//! parent's children together and reads them together, so a range is one bound
//! check instead of a pointer chase per sibling — and, more usefully here, it
//! makes "the children of this fragment, in layout order" a slice, which is
//! exactly what ADR 029's geometry comparison walks.

use app_units::Au;

use crate::geom::LogicalSize;

/// A handle to a fragment in a [`FragmentTree`].
///
/// A plain index, not a generational handle. §4.1's rule is about `px-dom`, where
/// handles outlive the operations that invalidate them; a fragment tree is built
/// in one pass, read, and dropped whole, so there is no window in which an index
/// can go stale. The accessors still return `Option`, because a bounds check that
/// is already being performed may as well be visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FragmentId(u32);

impl FragmentId {
    /// The index this handle names.
    #[must_use]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// What kind of box produced this fragment.
///
/// Kept small and closed. Phase 6 generates three kinds; flex and grid items in
/// Phase 7 are still boxes, and the distinction that matters for layout is the
/// *formatting context* a box establishes, not the element it came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FragmentKind {
    /// A block-level box participating in a block formatting context.
    Block,
    /// An anonymous block box, generated to hold inline content that had to be
    /// separated from block siblings.
    ///
    /// Distinguished from [`FragmentKind::Block`] because the reftest comparison
    /// in ADR 029 relies on anonymous boxes lining up with the explicit ones a
    /// reference document spells out — and because a bug that fails to generate
    /// one produces a tree that is *shorter*, not wrong-looking, which is hard to
    /// see without naming the case.
    AnonymousBlock,
    /// A line box inside an inline formatting context.
    Line,
}

/// One laid-out box.
///
/// The rectangle is the border box, in the containing block's coordinate space,
/// resolved to absolute coordinates once layout completes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fragment {
    /// What produced this fragment.
    pub kind: FragmentKind,
    /// Offset from the fragment tree's origin, along the inline axis.
    pub inline_offset: Au,
    /// Offset from the fragment tree's origin, along the block axis.
    pub block_offset: Au,
    /// The border-box size.
    pub size: LogicalSize,
    /// The DOM node whose box this fragment is, or `None` for fragments with no
    /// element of their own: the initial containing block, anonymous blocks and
    /// line boxes.
    ///
    /// This is the link paint needs to find a fragment's computed style. It is
    /// `Option<NodeId>` and `NodeId` is `Copy`, so `Fragment` stays `Copy` and
    /// `Drop` still cannot recurse. Layout itself never reads it.
    pub node: Option<px_dom::NodeId>,
    /// This fragment's children, as a range into the tree's flat storage.
    first_child: u32,
    child_count: u32,
    /// This fragment's text runs, as a range into the tree's text-run arena
    /// (see [`FragmentTree::text_runs`]). Only line fragments have any.
    first_text_run: u32,
    text_run_count: u32,
}

impl Fragment {
    /// A fragment with no children, at the origin.
    #[must_use]
    pub fn new(kind: FragmentKind, size: LogicalSize) -> Self {
        Self {
            kind,
            inline_offset: Au(0),
            block_offset: Au(0),
            size,
            node: None,
            first_child: 0,
            child_count: 0,
            first_text_run: 0,
            text_run_count: 0,
        }
    }

    /// How many children this fragment has.
    #[must_use]
    pub fn child_count(self) -> usize {
        self.child_count as usize
    }
}

/// One shaped glyph, placed. Plain integers, no font reference: the run that
/// owns it names the font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GlyphPos {
    /// The glyph index in the run's font.
    pub glyph_id: u32,
    /// The pen position of the glyph origin from the **start of its run**, to
    /// the right, including the shaper's x offset (mark attachment).
    pub x: Au,
    /// The vertical offset of the glyph origin from the baseline, **downward**
    /// positive (the shaper's y offset negated).
    pub y: Au,
    /// The horizontal advance.
    pub advance: Au,
}

/// A run of glyphs drawn in one font, one direction, one size, on one line.
///
/// Everything unit 4's display-list builder needs except the text colour, which
/// is `node`'s computed `color` (the element whose style the run was measured
/// with). Glyphs live in the tree's glyph arena and are reached with
/// [`FragmentTree::glyphs`]; the run keeps only an index range, so it stays
/// `Copy` like [`Fragment`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextRun {
    /// The font in the process-wide registry `crate::text` resolves from.
    pub font: px_text::FontId,
    /// The font size the glyph positions were computed for.
    pub font_size: Au,
    /// Drawn right to left (an odd bidi level). Glyphs are already in visual
    /// order, left to right, whichever way this is set.
    pub rtl: bool,
    /// The element whose computed style gives the text colour.
    pub node: Option<px_dom::NodeId>,
    /// The run's start, from the line fragment's inline start edge.
    pub inline_offset: Au,
    /// The baseline, from the line fragment's block start edge.
    pub baseline: Au,
    /// The run's total advance.
    pub width: Au,
    glyph_start: u32,
    glyph_len: u32,
}

impl TextRun {
    /// A run with no glyphs yet; [`FragmentTree::add_text_run`] attaches them.
    #[must_use]
    pub fn new(
        font: px_text::FontId,
        font_size: Au,
        rtl: bool,
        node: Option<px_dom::NodeId>,
        inline_offset: Au,
        baseline: Au,
        width: Au,
    ) -> Self {
        Self {
            font,
            font_size,
            rtl,
            node,
            inline_offset,
            baseline,
            width,
            glyph_start: 0,
            glyph_len: 0,
        }
    }
}

/// A whole laid-out document.
///
/// Built once, read many times, dropped whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FragmentTree {
    fragments: Vec<Fragment>,
    /// Child handles, indexed by each fragment's `first_child..+child_count`.
    ///
    /// A second flat array rather than children stored inside the fragment,
    /// because a fragment is `Copy` and fixed-size — which is what lets the whole
    /// tree be a `Vec` with no indirection and no recursive drop.
    children: Vec<FragmentId>,
    /// The side arena of text runs, indexed by each line fragment's
    /// `first_text_run..+text_run_count`.
    text_runs: Vec<TextRun>,
    /// The side arena of glyphs, indexed by each run's `glyph_start..+glyph_len`.
    ///
    /// Shaped text lives here and not in [`Fragment`] for the same reason the
    /// children do: a fragment that owned a `Vec` could not be `Copy`.
    glyphs: Vec<GlyphPos>,
    root: Option<FragmentId>,
}

impl FragmentTree {
    /// An empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// How many fragments the tree holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.fragments.len()
    }

    /// Whether the tree holds no fragments.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fragments.is_empty()
    }

    /// The root fragment, if layout produced one.
    #[must_use]
    pub fn root(&self) -> Option<FragmentId> {
        self.root
    }

    /// Read a fragment.
    #[must_use]
    pub fn get(&self, id: FragmentId) -> Option<&Fragment> {
        self.fragments.get(id.index())
    }

    /// Modify a fragment.
    pub fn get_mut(&mut self, id: FragmentId) -> Option<&mut Fragment> {
        self.fragments.get_mut(id.index())
    }

    /// The children of a fragment, in layout order.
    #[must_use]
    pub fn children(&self, id: FragmentId) -> &[FragmentId] {
        let Some(fragment) = self.get(id) else {
            return &[];
        };
        let start = fragment.first_child as usize;
        let end = start + fragment.child_count as usize;
        self.children.get(start..end).unwrap_or(&[])
    }

    /// Add a fragment with no children, returning its handle.
    pub fn push(&mut self, fragment: Fragment) -> FragmentId {
        let id = FragmentId(
            u32::try_from(self.fragments.len()).expect("a document cannot hold 4G fragments"),
        );
        self.fragments.push(fragment);
        id
    }

    /// Name the fragment the tree is rooted at.
    ///
    /// Explicit, because it used to be implicit: `push` made the first fragment
    /// pushed the root, which was correct only because `layout_document` happens
    /// to push the initial containing block first. `layout_lines` called on a
    /// fresh tree rooted it at a `Line`, and any future bottom-up construction
    /// would have rooted it at a leaf — silently, since every accessor would keep
    /// working and only the traversal would be wrong.
    pub fn set_root(&mut self, id: FragmentId) {
        self.root = Some(id);
    }

    /// Attach `kids` to `parent`, replacing any it already had.
    ///
    /// Appends to the shared child array rather than editing in place, so a
    /// fragment whose children are set twice leaks the first range. Layout sets
    /// them once; the alternative is a compaction pass that would exist only to
    /// tidy something that does not happen.
    pub fn set_children(&mut self, parent: FragmentId, kids: &[FragmentId]) {
        let start =
            u32::try_from(self.children.len()).expect("a document cannot hold 4G child links");
        self.children.extend_from_slice(kids);
        if let Some(fragment) = self.fragments.get_mut(parent.index()) {
            fragment.first_child = start;
            fragment.child_count =
                u32::try_from(kids.len()).expect("a fragment cannot have 4G children");
        }
    }

    /// Append a text run to `line`, with its glyphs.
    ///
    /// A line's runs must be added together, in visual order, before any other
    /// line's: the fragment keeps one contiguous range. `layout_lines` is the
    /// only caller and does exactly that.
    pub fn add_text_run(&mut self, line: FragmentId, mut run: TextRun, glyphs: &[GlyphPos]) {
        run.glyph_start = u32::try_from(self.glyphs.len()).unwrap_or(u32::MAX);
        run.glyph_len = u32::try_from(glyphs.len()).unwrap_or(u32::MAX);
        self.glyphs.extend_from_slice(glyphs);
        let at = u32::try_from(self.text_runs.len()).unwrap_or(u32::MAX);
        self.text_runs.push(run);
        if let Some(f) = self.fragments.get_mut(line.index()) {
            if f.text_run_count == 0 {
                f.first_text_run = at;
            }
            f.text_run_count += 1;
        }
    }

    /// A fragment's text runs, left to right. Empty for fragments that are not
    /// lines of text.
    ///
    /// **Unit 4 reads this.** For each line fragment the display-list builder
    /// takes the line's absolute origin, adds each run's `inline_offset` and
    /// `baseline`, and draws [`FragmentTree::glyphs`]`(run)` with the run's font,
    /// size and the `color` of `run.node`'s computed style. Glyph `x` is from the
    /// run's start; `y` is from the baseline, downward.
    #[must_use]
    pub fn text_runs(&self, id: FragmentId) -> &[TextRun] {
        let Some(f) = self.get(id) else { return &[] };
        let start = f.first_text_run as usize;
        let end = start + f.text_run_count as usize;
        self.text_runs.get(start..end).unwrap_or(&[])
    }

    /// The glyphs of a run, left to right.
    #[must_use]
    pub fn glyphs(&self, run: &TextRun) -> &[GlyphPos] {
        let start = run.glyph_start as usize;
        let end = start + run.glyph_len as usize;
        self.glyphs.get(start..end).unwrap_or(&[])
    }

    /// Every fragment in layout order, as `(depth, id)`.
    ///
    /// **Iterative.** This is the walk ADR 029's comparison uses and the one the
    /// deep-nesting test exercises; a recursive version is the crash gate item 3
    /// exists to prevent, and it is the natural way to write this function, which
    /// is why it is written the other way here where it can be seen.
    #[must_use]
    pub fn in_layout_order(&self) -> Vec<(usize, FragmentId)> {
        let mut out = Vec::with_capacity(self.fragments.len());
        let Some(root) = self.root else {
            return out;
        };
        // Explicit stack, deepest-last so children come off in order.
        let mut stack = vec![(0usize, root)];
        while let Some((depth, id)) = stack.pop() {
            out.push((depth, id));
            let kids = self.children(id);
            for kid in kids.iter().rev() {
                stack.push((depth + 1, *kid));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::px;

    fn block(inline: i32, block_size: i32) -> Fragment {
        Fragment::new(
            FragmentKind::Block,
            LogicalSize::new(px(inline), px(block_size)),
        )
    }

    #[test]
    fn fragment_children_are_a_slice_in_layout_order() {
        let mut tree = FragmentTree::new();
        let root = tree.push(block(100, 100));
        tree.set_root(root);
        let a = tree.push(block(10, 10));
        let b = tree.push(block(20, 20));
        tree.set_children(root, &[a, b]);

        assert_eq!(tree.children(root), &[a, b]);
        assert_eq!(tree.children(a), &[]);
        assert_eq!(tree.root(), Some(root));
        assert_eq!(tree.len(), 3);
    }

    #[test]
    fn fragment_layout_order_is_preorder_and_depth_tagged() {
        let mut tree = FragmentTree::new();
        let root = tree.push(block(100, 100));
        tree.set_root(root);
        let a = tree.push(block(10, 10));
        let b = tree.push(block(20, 20));
        let a1 = tree.push(block(5, 5));
        tree.set_children(a, &[a1]);
        tree.set_children(root, &[a, b]);

        let order = tree.in_layout_order();
        assert_eq!(
            order,
            vec![(0, root), (1, a), (2, a1), (1, b)],
            "children must come out before later siblings, tagged with depth"
        );
    }

    /// A tree deeper than any stack would survive, walked without recursion.
    ///
    /// Not gate item 3 — that one lays out a real document — but the same property
    /// at the level of the data structure, and it fails in seconds rather than
    /// after a layout engine exists.
    #[test]
    fn fragment_deep_tree_walks_without_overflowing() {
        const DEPTH: usize = 100_000;
        let mut tree = FragmentTree::new();

        let mut chain = Vec::with_capacity(DEPTH);
        for _ in 0..DEPTH {
            chain.push(tree.push(block(10, 10)));
        }
        if let Some(first) = chain.first() {
            tree.set_root(*first);
        }
        for window in chain.windows(2) {
            tree.set_children(window[0], &[window[1]]);
        }

        let order = tree.in_layout_order();
        assert_eq!(order.len(), DEPTH);
        assert_eq!(order.last().map(|(depth, _)| *depth), Some(DEPTH - 1));
    }

    /// A `Fragment` is `Copy`, which is what makes recursive `Drop` impossible.
    ///
    /// Not a style preference. A fragment that owned its children — a
    /// `Vec<Fragment>` field — could not be `Copy`, so this assertion is the
    /// compiler refusing the structure that produces a recursive drop. Two
    /// attempts at catching that with a source scan in `cargo xtask layout` both
    /// fired on correct code; this cannot, because it is not a heuristic.
    #[test]
    fn fragment_is_copy_so_drop_cannot_recurse() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Fragment>();
        assert_copy::<FragmentId>();
    }

    /// Dropping a deep tree must not overflow either.
    ///
    /// The failure the gate's structural scan exists to prevent, asserted here as
    /// well: if a `Fragment` ever owned its children, this test would crash in a
    /// `drop` nobody wrote.
    #[test]
    fn fragment_deep_tree_drops_without_overflowing() {
        let mut tree = FragmentTree::new();
        let mut previous: Option<FragmentId> = None;
        for _ in 0..100_000 {
            let id = tree.push(block(10, 10));
            if let Some(parent) = previous {
                tree.set_children(parent, &[id]);
            }
            previous = Some(id);
        }
        drop(tree);
    }
}
