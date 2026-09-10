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

/// A `NodeId` packed into a non-zero, pointer-sized value.
///
/// This is what stylo's `OpaqueNode(pub usize)` and
/// `OpaqueElement(NonNull<()>)` will hold. See [`NodeId::to_opaque`].
pub type OpaqueNodeId = core::num::NonZeroUsize;

// The packing puts the index in the high 32 bits, so `usize` has to be at
// least 64 bits wide. Both targets this project builds for are, and a build
// for one that is not should stop here rather than silently truncate every
// node identity to its generation.
const _: () = assert!(
    core::mem::size_of::<usize>() >= 8,
    "px-dom packs a 32-bit index and a 32-bit generation into a usize; on a \
     target with a narrower usize this would truncate, and two different nodes \
     would get the same opaque identity"
);

impl NodeId {
    /// Pack into the non-zero, pointer-sized value stylo uses as an identity
    /// key.
    ///
    /// # Why this is a Phase 4 concern
    ///
    /// `docs/research/stylo-requirements.md` §3.5. Stylo keys its snapshot map
    /// and its traversal-root comparison on `OpaqueNode`, which in Servo is a
    /// **pointer**. Pointer-derived identity is stale-unsafe across free and
    /// reuse: a removed element's snapshot can be matched to a different
    /// element later allocated at the same address.
    ///
    /// Packing the generational handle instead makes that structurally
    /// impossible. A reused slot has a different generation, so it packs to a
    /// different value, so a stale snapshot simply misses — §4.1's guarantee
    /// extending into stylo's own data structures for free.
    ///
    /// # The trap this avoids
    ///
    /// `OpaqueElement` is `NonNull<()>`, and stylo reaches it through
    /// `NonNull::new_unchecked`, so **a zero value is undefined behaviour with
    /// no diagnostic**. A node at index 0 with generation 0 would pack to zero.
    ///
    /// It cannot happen here, and the reason is worth stating because it is
    /// load-bearing rather than incidental: `NodeId::generation` is
    /// `NonZeroU32`, so the low half is never zero, so the packed word is never
    /// zero. The niche that makes `Option<NodeId>` cost nothing (ADR 018) is
    /// the same property that makes this safe.
    ///
    /// The research note puts the cost of getting it wrong at "one line in
    /// Phase 4, an afternoon of debugging in Phase 5".
    pub fn to_opaque(self) -> OpaqueNodeId {
        let packed = ((self.index as usize) << 32) | (self.generation.get() as usize);
        // Non-zero because `generation` is non-zero and occupies the low half.
        // `NonZeroUsize::new` rather than the unchecked form: this crate is
        // `forbid(unsafe_code)`, and the fallback is unreachable rather than
        // wrong.
        OpaqueNodeId::new(packed).unwrap_or(OpaqueNodeId::MIN)
    }

    /// Recover a handle from its packed form.
    ///
    /// `None` if the value did not come from [`NodeId::to_opaque`] — a zero
    /// generation is the only way that can show, and it is exactly the shape a
    /// pointer misread as a packed handle would have.
    pub fn from_opaque(value: OpaqueNodeId) -> Option<Self> {
        let packed = value.get();
        let generation = core::num::NonZeroU32::new((packed & 0xFFFF_FFFF) as u32)?;
        let index = u32::try_from(packed >> 32).ok()?;
        Some(Self { index, generation })
    }
}
