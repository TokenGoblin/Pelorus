//! The IPC-safe update types: construction, chunking, and the receiver's
//! validation.

mod common;

use common::Page;
use px_a11y::{
    A11yUpdate, MAX_STRING, MAX_UPDATE_NODES, MAX_UPDATE_REMOVED, UpdateError, UpdateKind,
    WireNode, key_of,
};

fn first(updates: Vec<A11yUpdate>) -> A11yUpdate {
    match updates.into_iter().next() {
        Some(u) => u,
        None => unreachable!("every update list has at least one part"),
    }
}

#[test]
fn a11y_wire_snapshot_of_a_small_tree_is_one_valid_part() {
    let p = Page::new("<h1 id=h>Title</h1><button id=b>Go</button>");
    let parts = A11yUpdate::snapshot(&p.tree);
    assert_eq!(parts.len(), 1);
    let u = &parts[0];
    assert_eq!(u.validate(), Ok(()));
    assert_eq!(u.kind, UpdateKind::Snapshot);
    assert_eq!((u.part, u.parts), (0, 1));
    assert_eq!(u.nodes.len(), p.tree.len());
    assert_eq!(u.root, Some(key_of(p.dom.arena.document())));
    let b = u
        .nodes
        .iter()
        .find(|n| n.key == key_of(p.element("b").expect("b")))
        .expect("button in update");
    assert_eq!(b.name, "Go");
    assert_eq!(b.role.token(), "button");
    assert_eq!(b.parent, u.root);
    assert_eq!(b.index_in_parent, 1);
    assert!(u.removed.is_empty());
}

#[test]
fn a11y_wire_large_trees_split_into_bounded_parts() {
    let n = 3000;
    let dom_html = "<p>x</p>".repeat(n);
    let p = Page::new(&dom_html);
    let parts = A11yUpdate::snapshot(&p.tree);
    let total: usize = parts.iter().map(|u| u.nodes.len()).sum();
    assert_eq!(total, p.tree.len());
    assert!(parts.len() >= p.tree.len().div_ceil(MAX_UPDATE_NODES));
    for (i, u) in parts.iter().enumerate() {
        assert_eq!(u.validate(), Ok(()));
        assert_eq!(u.part as usize, i);
        assert_eq!(u.parts as usize, parts.len());
        assert!(u.nodes.len() <= MAX_UPDATE_NODES);
        assert_eq!(u.root.is_some(), i == 0);
    }
}

#[test]
fn a11y_wire_delta_carries_changed_nodes_and_removed_keys() {
    let old = Page::new("<p id=a>one</p><button id=b>b</button><p id=c>c</p>");
    let new = Page::new("<p id=a>one</p><button id=b disabled>b</button>");
    let parts = A11yUpdate::delta(&old.tree, &new.tree);
    assert_eq!(parts.len(), 1);
    let u = &parts[0];
    assert_eq!(u.validate(), Ok(()));
    assert_eq!(u.kind, UpdateKind::Delta);
    assert!(u.root.is_none());
    let keys: Vec<_> = u.nodes.iter().map(|n| n.key).collect();
    assert!(keys.contains(&key_of(new.element("b").expect("b"))));
    // The unchanged paragraph is not resent.
    assert!(!keys.contains(&key_of(new.element("a").expect("a"))));
    // The removed paragraph and its text are listed.
    assert!(u.removed.contains(&key_of(old.element("c").expect("c"))));
    // A key is listed once even when several things about the node changed.
    let mut sorted = keys.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), keys.len());
}

#[test]
fn a11y_wire_empty_delta_is_one_empty_valid_part() {
    let p = Page::new("<p>x</p>");
    let parts = A11yUpdate::delta(&p.tree, &p.tree);
    assert_eq!(parts.len(), 1);
    assert!(parts[0].nodes.is_empty() && parts[0].removed.is_empty());
    assert_eq!(parts[0].validate(), Ok(()));
}

fn valid() -> A11yUpdate {
    first(A11yUpdate::snapshot(
        &Page::new("<button>b</button><p>p</p>").tree,
    ))
}

#[test]
fn a11y_wire_validate_refuses_oversized_updates() {
    let mut u = valid();
    let n = u.nodes.first().cloned().expect("node");
    u.nodes = (0..=MAX_UPDATE_NODES as u64)
        .map(|i| WireNode {
            key: i + 1,
            ..n.clone()
        })
        .collect();
    u.root = None;
    assert_eq!(u.validate(), Err(UpdateError::TooManyNodes));

    let mut u = valid();
    u.kind = UpdateKind::Delta;
    u.root = None;
    u.removed = (1..=MAX_UPDATE_REMOVED as u64 + 1).collect();
    assert_eq!(u.validate(), Err(UpdateError::TooManyRemoved));
}

#[test]
fn a11y_wire_validate_refuses_bad_strings_and_numbers() {
    let mut u = valid();
    if let Some(n) = u.nodes.get_mut(1) {
        n.name = "x".repeat(MAX_STRING + 1);
    }
    assert_eq!(u.validate(), Err(UpdateError::StringTooLong));

    let mut u = valid();
    if let Some(n) = u.nodes.get_mut(1) {
        n.description = "a\0b".into();
    }
    assert_eq!(u.validate(), Err(UpdateError::StringHasNul));

    let mut u = valid();
    if let Some(n) = u.nodes.get_mut(1) {
        n.props.value = Some("v\0".into());
    }
    assert_eq!(u.validate(), Err(UpdateError::StringHasNul));

    let mut u = valid();
    if let Some(n) = u.nodes.get_mut(1) {
        n.props.range = Some(px_a11y::RangeValue {
            now: Some(f64::NAN),
            min: None,
            max: None,
        });
    }
    assert_eq!(u.validate(), Err(UpdateError::NonFiniteNumber));
    if let Some(n) = u.nodes.get_mut(1) {
        n.props.range = Some(px_a11y::RangeValue {
            now: None,
            min: Some(f64::INFINITY),
            max: None,
        });
    }
    assert_eq!(u.validate(), Err(UpdateError::NonFiniteNumber));
}

#[test]
fn a11y_wire_validate_refuses_bad_keys_parts_and_roots() {
    let mut u = valid();
    if let Some(n) = u.nodes.get_mut(1) {
        n.key = 0;
    }
    assert_eq!(u.validate(), Err(UpdateError::ZeroKey));

    let mut u = valid();
    let dup = u.nodes.first().map(|n| n.key).expect("key");
    if let Some(n) = u.nodes.get_mut(1) {
        n.key = dup;
    }
    assert_eq!(u.validate(), Err(UpdateError::DuplicateKey));

    let mut u = valid();
    u.parts = 0;
    assert_eq!(u.validate(), Err(UpdateError::BadParts));
    let mut u = valid();
    u.part = u.parts;
    assert_eq!(u.validate(), Err(UpdateError::BadParts));
    let mut u = valid();
    u.parts = px_a11y::MAX_UPDATE_PARTS + 1;
    assert_eq!(u.validate(), Err(UpdateError::BadParts));

    let mut u = valid();
    u.root = Some(0xdead_beef);
    assert_eq!(u.validate(), Err(UpdateError::RootInvalid));
    let mut u = valid();
    u.part = 1;
    u.parts = 2;
    assert_eq!(u.validate(), Err(UpdateError::RootInvalid));

    let mut u = valid();
    u.removed = vec![1];
    assert_eq!(u.validate(), Err(UpdateError::SnapshotRemovesNodes));
    let mut u = valid();
    u.kind = UpdateKind::Delta;
    u.root = None;
    u.removed = vec![0];
    assert_eq!(u.validate(), Err(UpdateError::ZeroKey));
}

#[test]
fn a11y_wire_carries_no_identity_fields() {
    // Authority comes from the channel. The update has a kind, part numbers,
    // a root key, nodes and removals, and nothing that names a tab, origin
    // or sender. This is a compile-time statement: constructing the type with
    // exactly these fields must keep compiling.
    let _ = A11yUpdate {
        kind: UpdateKind::Delta,
        root: None,
        part: 0,
        parts: 1,
        nodes: Vec::new(),
        removed: Vec::new(),
    };
}
