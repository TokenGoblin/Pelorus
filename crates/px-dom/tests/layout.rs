//! §14.3's measurement: two `NodeId` layouts, measured rather than argued.
//!
//! > `NodeId` doubles from 4 to 8 bytes, and DOM handles are everywhere, so
//! > this is a real memory cost at scale. Worse, **generation counters wrap**:
//! > after 2³² reuses, a stale handle becomes valid again — the exact bug the
//! > design exists to prevent.
//! >
//! > **Fix:** retire a slot permanently on generation overflow rather than
//! > wrapping. Consider a 24/8 index/generation split with slot retirement to
//! > keep handles at 4 bytes; measure both in Phase 4 and record the choice.
//!
//! ADR 018 records the choice. This file is why the numbers in it can be
//! re-derived instead of believed: every figure the ADR quotes is asserted
//! here, so a change in the tradeoff fails a test rather than quietly making
//! a document wrong.
//!
//! Both layouts are defined here rather than in `src/`. The crate ships one of
//! them; the other is a measurement, and a measurement that lives in the
//! shipping code is dead weight that eventually gets "cleaned up" along with
//! the reason anybody chose anything.

use core::mem::size_of;
use core::num::NonZeroU32;

// ---------------------------------------------------------------------------
// Layout A — 32/32, the straightforward one.
// ---------------------------------------------------------------------------

/// A `u32` index beside a `u32` generation.
///
/// The generation is `NonZeroU32` so that `Option<WideId>` gets a niche and
/// costs no more than `WideId`. Without that, every link field in a node pays
/// a discriminant word and the comparison below is not close.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct WideId {
    index: u32,
    generation: NonZeroU32,
}

impl WideId {
    /// Slots addressable. `u32::MAX` is reserved as "no slot".
    const MAX_SLOTS: u64 = u32::MAX as u64;
    /// Reuses before a slot must be retired.
    const MAX_GENERATION: u64 = u32::MAX as u64;
}

// ---------------------------------------------------------------------------
// Layout B — 24/8 packed, the one §14.3 asks to be considered.
// ---------------------------------------------------------------------------

/// 24 bits of index over 8 bits of generation, in a single `u32`.
///
/// Generations start at 1, so the whole word is never zero and `NonZeroU32`
/// holds — which is what makes `Option<PackedId>` four bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct PackedId(NonZeroU32);

impl PackedId {
    const INDEX_BITS: u32 = 24;
    const GENERATION_BITS: u32 = 8;

    /// 16,777,216 slots.
    const MAX_SLOTS: u64 = 1 << Self::INDEX_BITS;
    /// 255 reuses, then the slot is retired forever.
    const MAX_GENERATION: u64 = (1 << Self::GENERATION_BITS) - 1;

    fn new(index: u32, generation: u32) -> Option<Self> {
        if u64::from(index) >= Self::MAX_SLOTS || generation == 0 {
            return None;
        }
        if u64::from(generation) > Self::MAX_GENERATION {
            return None;
        }
        NonZeroU32::new((index << Self::GENERATION_BITS) | generation).map(Self)
    }

    fn index(self) -> u32 {
        self.0.get() >> Self::GENERATION_BITS
    }

    fn generation(self) -> u32 {
        self.0.get() & (Self::MAX_GENERATION as u32)
    }
}

// ---------------------------------------------------------------------------
// A node, shaped the way px-dom shapes one, in each layout.
//
// Five links is not a guess: parent, first and last child, previous and next
// sibling is the minimum for O(1) insertion and removal at either end plus
// upward traversal, and it is what every arena DOM converges on.
// ---------------------------------------------------------------------------

#[allow(dead_code)]
struct WideNode {
    parent: Option<WideId>,
    first_child: Option<WideId>,
    last_child: Option<WideId>,
    prev_sibling: Option<WideId>,
    next_sibling: Option<WideId>,
}

#[allow(dead_code)]
struct PackedNode {
    parent: Option<PackedId>,
    first_child: Option<PackedId>,
    last_child: Option<PackedId>,
    prev_sibling: Option<PackedId>,
    next_sibling: Option<PackedId>,
}

/// The handles are 8 bytes against 4, and the niche survives in both.
///
/// The niche is the load-bearing part. If `Option<WideId>` were 12 bytes the
/// comparison would be 60 against 20 rather than 40 against 20, and this
/// measurement would be reporting a fact about missing `NonZeroU32` rather
/// than about the layout choice.
#[test]
fn handles_are_eight_bytes_and_four() {
    assert_eq!(size_of::<WideId>(), 8);
    assert_eq!(size_of::<PackedId>(), 4);

    assert_eq!(
        size_of::<Option<WideId>>(),
        size_of::<WideId>(),
        "Option<WideId> lost its niche; the generation must stay NonZeroU32"
    );
    assert_eq!(
        size_of::<Option<PackedId>>(),
        size_of::<PackedId>(),
        "Option<PackedId> lost its niche; the packed word must never be zero"
    );
}

/// The link block of a node: 40 bytes against 20.
#[test]
fn links_cost_forty_bytes_against_twenty() {
    assert_eq!(size_of::<WideNode>(), 40);
    assert_eq!(size_of::<PackedNode>(), 20);
}

/// What the difference is worth on documents of the size the web actually has.
///
/// This test exists to put a number on "a real memory cost at scale" rather
/// than repeat the phrase. It asserts the arithmetic the ADR quotes.
#[test]
fn the_saving_at_document_scale() {
    // Node counts spanning a small page to a heavy application.
    for (nodes, expected_saving) in [
        (1_500u64, 30_000u64),      // a typical article
        (25_000, 500_000),          // a heavy application view
        (250_000, 5_000_000),       // an extreme but reachable document
    ] {
        let wide = nodes * size_of::<WideNode>() as u64;
        let packed = nodes * size_of::<PackedNode>() as u64;
        assert_eq!(wide - packed, expected_saving);
    }

    // Stated the way the ADR states it: 20 bytes per node, and a quarter of a
    // million nodes is under five megabytes of difference.
    assert_eq!(
        size_of::<WideNode>() - size_of::<PackedNode>(),
        20,
        "the whole memory case for the packed layout is this number"
    );
}

// ---------------------------------------------------------------------------
// The other half of the measurement, and the half that decides it: what
// retirement costs when something is trying to make it cost.
// ---------------------------------------------------------------------------

/// A slot model just large enough to measure retirement. Not the arena — the
/// arena is `src/`; this is the accounting the arena will have to obey.
struct Slots {
    /// Generation per live slot.
    generation: Vec<u32>,
    /// Slots whose generation is exhausted. Never reused.
    retired: u64,
    max_generation: u64,
    max_slots: u64,
}

impl Slots {
    fn new(max_generation: u64, max_slots: u64) -> Self {
        Self {
            generation: Vec::new(),
            retired: 0,
            max_generation,
            max_slots,
        }
    }

    /// One allocate-then-free cycle on the slot at `index`, returning false if
    /// the slot could not be reused because the arena is out of slots.
    ///
    /// Retirement is the whole point: on overflow the slot is dropped from the
    /// pool permanently rather than wrapping back to generation 1.
    fn churn(&mut self, index: usize) -> bool {
        if index >= self.generation.len() {
            if self.total_slots_used() >= self.max_slots {
                return false;
            }
            self.generation.push(1);
            return true;
        }
        let Some(generation) = self.generation.get_mut(index) else {
            return false;
        };
        if u64::from(*generation) >= self.max_generation {
            self.retired += 1;
            *generation = 0; // 0 marks retired; never handed out
            return true;
        }
        if *generation != 0 {
            *generation += 1;
        }
        true
    }

    fn total_slots_used(&self) -> u64 {
        self.generation.len() as u64
    }
}

/// The packed layout retires a slot every 255 reuses. The wide layout does not
/// retire within any run this test can execute, which is the point.
#[test]
fn retirement_happens_at_255_reuses_packed_and_never_wide() {
    let mut packed = Slots::new(PackedId::MAX_GENERATION, PackedId::MAX_SLOTS);
    packed.generation.push(1);
    for _ in 0..300 {
        packed.churn(0);
    }
    assert_eq!(
        packed.retired, 1,
        "a packed slot must retire once its 255 generations are spent"
    );

    let mut wide = Slots::new(WideId::MAX_GENERATION, WideId::MAX_SLOTS);
    wide.generation.push(1);
    for _ in 0..300 {
        wide.churn(0);
    }
    assert_eq!(
        wide.retired, 0,
        "the wide layout has 4 billion generations; nothing retires here"
    );
}

/// How long a hostile page needs to exhaust each layout.
///
/// This is the measurement that decides ADR 018, so it is arithmetic rather
/// than a benchmark — a benchmark would measure this machine, and the question
/// is about the shape of the bound, not this machine's speed.
///
/// A page that creates and destroys nodes in a loop burns one generation per
/// cycle. Once a slot's generations are spent it is retired and a fresh slot
/// is taken, so sustained churn consumes *slots* at
/// `churn_rate / max_generation` per second.
#[test]
fn exhaustion_time_under_sustained_churn() {
    fn seconds_to_exhaust(max_slots: u64, max_generation: u64, churn_per_second: u64) -> u64 {
        let slots_per_second = churn_per_second / max_generation;
        if slots_per_second == 0 {
            return u64::MAX;
        }
        max_slots / slots_per_second
    }

    // A page mutating a thousand nodes a second — brisk, but ordinary for an
    // animated list or a live dashboard.
    //
    // 64 days, not years. That was the first surprise here: the packed arena
    // does not comfortably outlast an ordinary long-lived tab, it merely
    // outlasts most of them. A monitoring dashboard or a chat client left open
    // for two months of continuous mutation reaches this without anybody
    // trying, and what it reaches is a tab that dies for no visible reason.
    let ordinary = seconds_to_exhaust(PackedId::MAX_SLOTS, PackedId::MAX_GENERATION, 1_000);
    assert_eq!(ordinary, 5_592_405);
    assert_eq!(ordinary / (60 * 60 * 24), 64, "64 days, and ADR 018 says so");

    // A page *trying* to exhaust it. A million node create/destroy cycles a
    // second is achievable from script on current hardware.
    //
    // 71 minutes, from script, to kill a tab permanently. Not a memory-safety
    // failure — §4.3 treats a dead content process as routine — but a denial
    // of service with no cost to the attacker and no recovery short of a
    // reload that discards the user's state.
    let hostile = seconds_to_exhaust(PackedId::MAX_SLOTS, PackedId::MAX_GENERATION, 1_000_000);
    assert_eq!(hostile, 4_278);
    assert_eq!(hostile / 60, 71, "71 minutes, and ADR 018 says so");

    // The same hostility against the wide layout. 4 billion slots at 4 billion
    // generations each is 2^64 cycles; it does not happen.
    let wide = seconds_to_exhaust(WideId::MAX_SLOTS, WideId::MAX_GENERATION, 1_000_000);
    assert_eq!(
        wide,
        u64::MAX,
        "the wide layout must not be exhaustible by churn at any rate a script \
         can reach"
    );
}

/// Packing and unpacking round-trips, and refuses what will not fit.
///
/// If the packed layout is ever chosen this becomes load-bearing; measuring a
/// layout whose encoding is untested would be measuring nothing.
#[test]
fn packed_round_trips_and_refuses_overflow() {
    for (index, generation) in [(0u32, 1u32), (1, 1), (123_456, 200), (0xFF_FFFF, 255)] {
        let id = PackedId::new(index, generation).expect("should fit");
        assert_eq!(id.index(), index);
        assert_eq!(id.generation(), generation);
    }

    assert!(
        PackedId::new(0x100_0000, 1).is_none(),
        "an index of 2^24 does not fit in 24 bits and must be refused, not truncated"
    );
    assert!(
        PackedId::new(0, 256).is_none(),
        "a generation of 256 does not fit in 8 bits and must be refused, not wrapped"
    );
    assert!(
        PackedId::new(0, 0).is_none(),
        "generation 0 would make the packed word zero and destroy the niche"
    );
}
