//! `NodeId`: a generational handle into the arena.
//!
//! Layout is 32/32 by ADR 018, which measured it against the 24/8 packed
//! alternative §14.3 asks Phase 4 to consider. The measurement is
//! `tests/layout.rs` and it still runs, so the numbers in the ADR can be
//! re-derived rather than believed.

use core::num::NonZeroU32;

/// A handle to a node.
///
/// # Why every accessor that takes one returns `Option`
///
/// A `NodeId` is not a pointer and not an index. It names a *slot together
/// with a moment*: the generation says which occupant of that slot the holder
/// meant. When the slot is reused the generation moves on and every handle to
/// the previous occupant stops resolving — permanently, because generations
/// never wrap (§14.3, ADR 018).
///
/// That is the whole safety property, and it only holds if resolving a handle
/// can fail. §4.1 puts it as a rule with no exceptions: **there is no
/// infallible index API, not even a private one.** `ci/gate-dom.sh` checks the
/// source for one, because a test can show that today's accessors return
/// `Option` and cannot show that tomorrow's will.
///
/// The temptation this rule exists to resist is small and specific: somebody
/// writing a tree walk gets tired of `?` on every hop and adds a private
/// helper that unwraps. Every caller after that is one stale handle away from
/// DOM-level type confusion — in safe Rust, with no `unsafe` block for a
/// reviewer to find.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct NodeId {
    index: u32,
    /// Non-zero so that `Option<NodeId>` costs the same as `NodeId`.
    ///
    /// This is load-bearing rather than tidy. A node has five link fields; if
    /// each paid a discriminant word, a node's links would cost 60 bytes
    /// instead of 40 and ADR 018's measurement would have been comparing the
    /// wrong things. `tests/layout.rs` asserts the niche survives.
    generation: NonZeroU32,
}

impl NodeId {
    pub(crate) fn new(index: u32, generation: NonZeroU32) -> Self {
        Self { index, generation }
    }

    /// The slot this handle names.
    ///
    /// Readable, and deliberately not constructible: there is no public way to
    /// build a `NodeId` from parts, so exposing the parts forges nothing. What
    /// it buys is tests that can assert a slot was *actually reused* — without
    /// which "the stale handle did not resolve" is a claim that passes just as
    /// well when the allocator quietly stopped reusing slots at all.
    pub fn index(self) -> u32 {
        self.index
    }

    /// Which occupant of the slot this handle means.
    pub fn generation(self) -> NonZeroU32 {
        self.generation
    }
}
