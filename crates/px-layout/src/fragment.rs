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
//! `ci/gate-layout.sh` carries a note about the two scans that tried to catch this
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
    /// This fragment's children, as a range into the tree's flat storage.
    first_child: u32,
    child_count: u32,
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
            first_child: 0,
            child_count: 0,
        }
    }

    /// How many children this fragment has.
    #[must_use]
    pub fn child_count(self) -> usize {
        self.child_count as usize
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
        if self.root.is_none() {
            self.root = Some(id);
        }
        id
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
    /// attempts at catching that with a source scan in `ci/gate-layout.sh` both
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
