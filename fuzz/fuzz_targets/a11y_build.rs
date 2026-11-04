#![no_main]

//! Untrusted markup through the accessibility tree builder (Phase 16).
//!
//! Input: the bytes, lossily decoded, are a page. Whatever the page, building
//! must not panic or overflow, stay inside its documented caps, produce the
//! same tree twice, and yield only updates the receiver's `validate` accepts.
//! A second build from the same bytes with one attribute value perturbed
//! drives the differ and the delta encoder.

use libfuzzer_sys::fuzz_target;
use px_a11y::{A11yUpdate, AccessibilityTree, BuildInputs, MAX_STRING, diff};

fuzz_target!(|data: &[u8]| {
    let html = String::from_utf8_lossy(data);
    let dom = px_dom::parse(&html);
    let tree = AccessibilityTree::build(&dom.arena, BuildInputs::default());

    let again = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert_eq!(tree.dump(), again.dump());
    assert!(diff(&tree, &again).is_empty());

    assert!(tree.len() <= px_a11y::MAX_NODES);
    for id in tree.all() {
        if let Some(node) = tree.node(id) {
            assert!(node.name().len() <= MAX_STRING);
            assert!(node.description().len() <= MAX_STRING);
        }
    }
    for part in A11yUpdate::snapshot(&tree) {
        assert!(part.validate().is_ok());
    }
    let _ = tree.tab_order();

    // Perturbed variant: flip one byte, rebuild, diff.
    let mut other = data.to_vec();
    if let Some(first) = other.first_mut() {
        *first ^= 0x20;
    }
    let dom2 = px_dom::parse(&String::from_utf8_lossy(&other));
    let tree2 = AccessibilityTree::build(&dom2.arena, BuildInputs::default());
    let _ = diff(&tree, &tree2);
    for part in A11yUpdate::delta(&tree, &tree2) {
        assert!(part.validate().is_ok());
    }
});
