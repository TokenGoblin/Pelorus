//! The size stylo requires of `TElement`, asserted.
//!
//! ADR 027. stylo's style-sharing cache lives in a thread-local typed with the
//! element erased to `usize`, and `transmute`s it back to the real type. A
//! constructor assertion keeps that honest:
//!
//! ```text
//! assert_eq!(size_of::<SharingCache<E>>(), size_of::<TypelessSharingCache>());
//! ```
//!
//! So `E` must be exactly pointer-sized. **Nothing in `TElement`'s declared
//! bounds says so** — it is enforced at runtime, in a dependency, on the first
//! style pass, as a panic reading `left: 10256, right: 9488`.
//!
//! That is why this file exists. The requirement is invisible to the compiler, so
//! a field added to a view would be caught by a panic during a style traversal
//! rather than by a type error at the edit. Here it is caught by `cargo test`.

use px_css::dom::StyleElement;
use px_css::view::StyleNode;

/// `NodeId` is pointer-sized, which is what makes ADR 027's fix possible at all.
///
/// ADR 018 chose 32/32 for reasons that had nothing to do with this — measured
/// memory per node — and it happens to land on exactly the width stylo's sharing
/// cache requires. Worth asserting, because if `NodeId` ever widened, the fix in
/// ADR 027 would stop working and the reason would not be obvious.
#[test]
fn computed_node_id_is_exactly_one_word() {
    assert_eq!(
        size_of::<px_dom::NodeId>(),
        size_of::<usize>(),
        "ADR 027's fix depends on a handle fitting in the word stylo's sharing \
         cache reserves for an element"
    );
}

/// The views are currently 32 bytes, and stylo needs 8.
///
/// Asserted at the wrong value on purpose: this records the measured state that
/// ADR 027 is about, so the number in the ADR and the number in the build cannot
/// drift apart while the fix is outstanding. When the views shrink, this test
/// fails and is replaced by the one below it.
#[test]
fn computed_view_size_is_the_one_adr_027_recorded() {
    assert_eq!(size_of::<StyleNode<'static>>(), 32);
    assert_eq!(size_of::<StyleElement<'static>>(), 32);
}

/// What ADR 027 has to achieve.
///
/// `#[ignore]`d rather than deleted: it is the acceptance test for the fix, and
/// running it is how you know the fix is done. `cargo test -- --ignored` is the
/// whole verification.
#[test]
#[ignore = "ADR 027: the views are 32 bytes and stylo requires 8; this is the fix's acceptance test"]
fn computed_views_fit_the_word_stylo_reserves() {
    assert_eq!(size_of::<StyleElement<'static>>(), size_of::<usize>());
    assert_eq!(size_of::<StyleNode<'static>>(), size_of::<usize>());
}
