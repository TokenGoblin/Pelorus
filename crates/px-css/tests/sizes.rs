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

/// The views fit the word stylo reserves for an element.
///
/// This was ADR 027's acceptance test, `#[ignore]`d while the views were 32
/// bytes. It passes now: a `StyleNode` is a reference to one per-node record and
/// nothing else.
///
/// It stays because the requirement is invisible to the compiler. Adding a field
/// to a view would not be a type error — it would be a panic on the next style
/// pass, three modules into a dependency, reading `left: 10256, right: 9488`.
#[test]
fn computed_views_fit_the_word_stylo_reserves() {
    assert_eq!(
        size_of::<StyleElement<'static>>(),
        size_of::<usize>(),
        "ADR 027: stylo's sharing cache transmutes through a usize-sized element"
    );
    assert_eq!(size_of::<StyleNode<'static>>(), size_of::<usize>());
}
