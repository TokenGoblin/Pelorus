//! Run the Phase 4 fuzz targets' bodies as ordinary tests.
//!
//! The campaign explores with coverage feedback and runs for a day. This runs
//! the same code in under a second, on every push, on both operating systems,
//! against a deterministic spread of inputs — so a change that breaks the
//! target's own logic fails here within a minute instead of surviving until
//! somebody reads a campaign report.
//!
//! It is also the only place these two targets execute at all on Windows
//! development machines: cargo-fuzz cannot link there without the MSVC ASAN
//! runtime, and `--sanitizer=none` leaves libFuzzer's sancov symbols
//! undefined. Without this file both targets would have been committed having
//! only ever been compiled — which is the shape of the Phase 1 campaign that
//! ran for 24 hours and tested nothing.
//!
//! Requires `--features testing`; `ci/gate-dom.sh` runs the suite that way.

#![cfg(feature = "testing")]

/// A cheap deterministic byte source. Not trying to be random — trying to be
/// reproducible and varied, and to need no dependency in a crate that has one.
fn pseudorandom(seed: u64, len: usize) -> Vec<u8> {
    let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1);
    (0..len)
        .map(|_| {
            // xorshift64*, chosen because it is four lines and its bias does
            // not matter for generating operation sequences.
            state ^= state >> 12;
            state ^= state << 25;
            state ^= state >> 27;
            (state.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33) as u8
        })
        .collect()
}

#[test]
fn dom_stale_handle_harness_holds_over_many_sequences() {
    for seed in 0..600u64 {
        // Lengths spanning "a couple of operations" to "long enough that slot
        // reuse and retirement both happen repeatedly".
        let len = 2 + (seed as usize % 400);
        px_dom::harness::stale_handles(&pseudorandom(seed, len));
    }
}

#[test]
fn dom_mutation_harness_holds_over_many_sequences() {
    for seed in 0..600u64 {
        let len = 3 + (seed as usize % 400);
        px_dom::harness::mutations(&pseudorandom(seed, len));
    }
}

/// Degenerate inputs, which a generator reaches only by accident.
#[test]
fn dom_stale_handle_harness_survives_degenerate_input() {
    for input in [
        vec![],
        vec![0],
        vec![255; 64],
        vec![0; 512],
        (0..=255u8).collect::<Vec<u8>>(),
    ] {
        px_dom::harness::stale_handles(&input);
        px_dom::harness::mutations(&input);
    }
}

/// The generation-exhaustion path is actually taken.
///
/// Operation 5 is the one that forces a slot to its last generation, and it is
/// the whole reason the `testing` feature exists. If the operation mix drifted
/// so that it never came up, both harnesses would still pass and the
/// retirement branch — the most consequential branch in the arena — would go
/// back to never executing.
#[test]
fn dom_stale_handle_harness_actually_retires_slots() {
    use px_dom::{Arena, NodeData};

    // Reproduce what the harness does on operation 5, directly, so this test
    // fails if `force_generation_to_last` or retirement stops working rather
    // than if the byte mix changed.
    let mut arena = Arena::new();
    let doc = arena.document();
    let node = arena
        .create(NodeData::Text {
            contents: "x".into(),
        })
        .expect("fresh arena");
    arena.append_child(doc, node).expect("append");

    let last = arena
        .force_generation_to_last(node)
        .expect("the node is live, so its generation can be moved");
    assert!(
        arena.get(node).is_none(),
        "the superseded handle must stop resolving"
    );
    assert!(arena.get(last).is_some());

    assert_eq!(arena.retired(), 0);
    arena.remove_subtree(last).expect("remove");
    assert_eq!(
        arena.retired(),
        1,
        "freeing a slot at its last generation must retire it, not wrap it -- \
         wrapping is the use-after-free §14.3 exists to prevent"
    );

    // And the retired slot is never handed out again.
    for _ in 0..64 {
        let fresh = arena
            .create(NodeData::Text {
                contents: "y".into(),
            })
            .expect("slots available");
        assert_ne!(
            fresh.index(),
            last.index(),
            "a retired slot was allocated again"
        );
    }
}

#[test]
fn dom_parse_harness_holds_over_many_sequences() {
    for seed in 0..400u64 {
        let len = 4 + (seed as usize % 600);
        px_dom::harness::parse_html(&pseudorandom(seed, len));
    }
}

/// Every committed corpus seed for a px-dom target replays clean.
///
/// ADR 006: *"Every crash the campaign finds gets its input committed to the
/// corpus, which is what makes 60 seconds meaningful rather than
/// decorative."* Committing them is half of that. Running them on every push
/// is the other half, and it was missing for `dom_mutation` — whose six seeds
/// are the campaign crashes from the range-constructor defect, the one the
/// harness's own 600 pseudorandom sequences never reached.
///
/// So these regressions now fail in seconds rather than in a four-hour
/// campaign, which is the difference between a corpus and a museum.
#[test]
fn every_committed_corpus_seed_replays_clean() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus");

    let mut total = 0usize;
    for (target, run) in [
        ("dom_mutation", px_dom::harness::mutations as fn(&[u8])),
        (
            "dom_stale_handle",
            px_dom::harness::stale_handles as fn(&[u8]),
        ),
        ("dom_parse", px_dom::harness::parse_html as fn(&[u8])),
    ] {
        let dir = root.join(target);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            panic!("the {target} corpus is missing at {}", dir.display());
        };
        for entry in entries.filter_map(Result::ok) {
            let Ok(bytes) = std::fs::read(entry.path()) else {
                continue;
            };
            run(&bytes);
            total += 1;
        }
    }

    assert!(
        total >= 25,
        "only {total} corpus seeds replayed; the campaign starts from these,          so a corpus that quietly shrank is a campaign that explores less --          and six of them are crashes this harness could not find on its own"
    );
}

/// The committed corpus parses cleanly, including the 100,000-level nesting
/// §4.4 names.
///
/// A corpus seed that crashes the harness is a bug the campaign would find in
/// its first second; running them here means finding it before the campaign
/// starts, and it also asserts the seeds are still readable — a corpus file
/// that got mangled by a line-ending conversion tests nothing and says
/// nothing.
#[test]
fn dom_parse_harness_handles_the_committed_corpus() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/corpus/dom_parse");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        panic!("the dom_parse corpus is missing at {}", dir.display());
    };

    let mut seen = 0usize;
    for entry in entries.filter_map(Result::ok) {
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        px_dom::harness::parse_html(&bytes);
        seen += 1;
    }
    assert!(
        seen >= 12,
        "only {seen} corpus seeds found; the campaign starts from these, so a \
         corpus that quietly shrank is a campaign that explores less"
    );
}
