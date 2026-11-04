//! `diff(old, new)`.
//!
//! Two parses of documents with the same shape allocate the same node ids in
//! the same order, so a text or attribute edit between them is a change to
//! the *same* DOM node, which is how an in-place mutation looks to the
//! differ. Appending at the end allocates fresh ids after the common ones.

mod common;

use common::Page;
use px_a11y::{Bounds, Change, States, diff};
use std::collections::HashMap;

fn changes(old: &str, new: &str) -> (Page, Page, Vec<Change>) {
    let a = Page::new(old);
    let b = Page::new(new);
    let c = diff(&a.tree, &b.tree);
    (a, b, c)
}

#[test]
fn a11y_diff_identical_trees_have_no_changes() {
    let (_, _, c) = changes(
        "<h1>a</h1><button>b</button>",
        "<h1>a</h1><button>b</button>",
    );
    assert!(c.is_empty(), "{c:?}");
}

#[test]
fn a11y_diff_name_change() {
    let (a, _, c) = changes(
        "<button id=x>Save</button>",
        "<button id=x>Save as</button>",
    );
    let x = a.element("x").expect("x");
    // The button's name changed. Its text child is not a node (children of a
    // button are presentational), so the name is the only change.
    assert_eq!(
        c,
        vec![Change::NameChanged {
            node: x,
            old: "Save".into(),
            new: "Save as".into()
        }]
    );
}

#[test]
fn a11y_diff_text_run_change_is_a_name_change_of_the_text_node() {
    let (a, b, c) = changes("<p id=x>hello</p>", "<p id=x>goodbye</p>");
    assert_eq!(c.len(), 1, "{c:?}");
    let Change::NameChanged { node, old, new } = &c[0] else {
        unreachable!("{c:?}");
    };
    assert_eq!((old.as_str(), new.as_str()), ("hello", "goodbye"));
    // The node is the text node, a child of the paragraph in both.
    let p = a.element("x").expect("p");
    let text = a.dom.arena.get(p).and_then(|n| n.first_child());
    assert_eq!(Some(*node), text);
    assert_eq!(
        b.dom.arena.get(*node).and_then(|n| n.text()).map(|t| &**t),
        Some("goodbye")
    );
}

#[test]
fn a11y_diff_state_changes() {
    let (a, _, c) = changes(
        "<input id=x type=checkbox><button id=y>b</button>",
        "<input id=x type=checkbox checked><button id=y disabled>b</button>",
    );
    let x = a.element("x").expect("x");
    let y = a.element("y").expect("y");
    assert_eq!(c.len(), 2, "{c:?}");
    assert!(matches!(&c[0], Change::StatesChanged { node, old, new }
        if *node == x && old.checked == Some(px_a11y::Tri::False) && new.checked == Some(px_a11y::Tri::True)));
    assert!(matches!(&c[1], Change::StatesChanged { node, old, new }
        if *node == y && !old.disabled && new.disabled && old.focusable && !new.focusable));
}

#[test]
fn a11y_diff_role_description_and_props() {
    let (a, _, c) = changes(
        "<div id=x role=button title=a aria-level=1>b</div>",
        "<div id=x role=tab title=b aria-level=2>b</div>",
    );
    let x = a.element("x").expect("x");
    let kinds: Vec<&str> = c
        .iter()
        .map(|c| match c {
            Change::RoleChanged { .. } => "role",
            Change::NameChanged { .. } => "name",
            Change::DescriptionChanged { .. } => "description",
            Change::StatesChanged { .. } => "states",
            Change::PropsChanged { .. } => "props",
            _ => "other",
        })
        .collect();
    // Fixed order: role, name, description, states, props. `level` applies
    // only to level-supporting roles, so the aria-level edit is invisible;
    // the props change is the tab's computed position in its set.
    assert_eq!(kinds, ["role", "description", "props"]);
    assert!(c.iter().all(|c| c.node() == x));
}

#[test]
fn a11y_diff_added_and_removed() {
    let (a, b, c) = changes(
        "<p id=a>one</p>",
        "<p id=a>one</p><button id=n>New</button>",
    );
    let n = b.element("n").expect("n");
    let body_container = a
        .tree
        .root()
        .and_then(|r| a.tree.node(r))
        .map(|r| r.dom_node());
    // New-tree document order: the document (whose child list changed) comes
    // before the button added under it.
    assert_eq!(
        c,
        vec![
            Change::ChildrenChanged {
                node: body_container.expect("root")
            },
            Change::Added {
                node: n,
                parent: body_container
            },
        ]
    );

    let (_, _, back) = changes(
        "<p id=a>one</p><button id=n>New</button>",
        "<p id=a>one</p>",
    );
    assert!(
        matches!(back.first(), Some(Change::Removed { .. })),
        "{back:?}"
    );
    assert!(
        back.iter()
            .any(|c| matches!(c, Change::ChildrenChanged { .. }))
    );
}

#[test]
fn a11y_diff_removed_come_first_in_old_document_order() {
    let (_, _, c) = changes("<p>a</p><p>b</p><p>c</p><b>tail</b>", "<p>a</p>");
    let removed: Vec<usize> = c
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, Change::Removed { .. }))
        .map(|(i, _)| i)
        .collect();
    assert!(!removed.is_empty());
    assert_eq!(removed, (0..removed.len()).collect::<Vec<_>>(), "{c:?}");
}

#[test]
fn a11y_diff_moved_by_aria_owns() {
    let (a, _, c) = changes(
        "<ul id=l><li id=a>a</li></ul><ul id=m></ul>",
        "<ul id=l><li id=a>a</li></ul><ul id=m aria-owns=a></ul>",
    );
    let l = a.element("l").expect("l");
    let m = a.element("m").expect("m");
    let x = a.element("a").expect("a");
    assert!(
        c.contains(&Change::Moved {
            node: x,
            old_parent: Some(l),
            new_parent: Some(m)
        }),
        "{c:?}"
    );
    assert!(c.contains(&Change::ChildrenChanged { node: l }));
    assert!(c.contains(&Change::ChildrenChanged { node: m }));
}

#[test]
fn a11y_diff_becoming_hidden_is_a_removal_and_becoming_visible_an_addition() {
    let (_, _, c) = changes("<p>a</p><p id=h>b</p>", "<p>a</p><p id=h hidden>b</p>");
    assert!(
        c.iter().any(|c| matches!(c, Change::Removed { .. })),
        "{c:?}"
    );
    let (_, _, c) = changes("<p>a</p><p id=h hidden>b</p>", "<p>a</p><p id=h>b</p>");
    assert!(c.iter().any(|c| matches!(c, Change::Added { .. })), "{c:?}");
}

#[test]
fn a11y_diff_bounds_changes() {
    use px_a11y::{AccessibilityTree, BuildInputs};
    let dom = px_dom::parse("<button id=x>b</button>");
    let page = Page::new("<button id=x>b</button>");
    let x = page.element("x").expect("x");
    let mut b1 = HashMap::new();
    b1.insert(
        x,
        Bounds {
            x: 0,
            y: 0,
            width: 10,
            height: 10,
        },
    );
    let mut b2 = b1.clone();
    let t1 = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: None,
            bounds: Some(&b1),
        },
    );
    let same = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: None,
            bounds: Some(&b1),
        },
    );
    assert!(diff(&t1, &same).is_empty());
    b2.insert(
        x,
        Bounds {
            x: 5,
            y: 0,
            width: 10,
            height: 10,
        },
    );
    let t2 = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: None,
            bounds: Some(&b2),
        },
    );
    assert_eq!(
        diff(&t1, &t2),
        vec![Change::BoundsChanged {
            node: x,
            old: Some(Bounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10
            }),
            new: Some(Bounds {
                x: 5,
                y: 0,
                width: 10,
                height: 10
            }),
        }]
    );
    // Bounds appearing and disappearing.
    let none = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert_eq!(
        diff(&none, &t1),
        vec![Change::BoundsChanged {
            node: x,
            old: None,
            new: Some(Bounds {
                x: 0,
                y: 0,
                width: 10,
                height: 10
            })
        }]
    );
}

#[test]
fn a11y_diff_sibling_positions_shift_when_an_item_is_added() {
    let (_, _, c) = changes(
        "<ul><li>a</li><li>b</li></ul>",
        "<ul><li>a</li><li>b</li><li>c</li></ul>",
    );
    // The existing items' setsize changed 2 -> 3.
    let props: Vec<_> = c
        .iter()
        .filter(|c| matches!(c, Change::PropsChanged { .. }))
        .collect();
    assert_eq!(props.len(), 2, "{c:?}");
    let _ = States::default();
}

#[test]
fn a11y_diff_is_deterministic() {
    let old = "<ul><li>a</li><li>b</li></ul><button>x</button><p>t</p>";
    let new = "<ul><li>a</li></ul><button disabled>y</button>";
    let first = changes(old, new).2;
    for _ in 0..10 {
        assert_eq!(changes(old, new).2, first);
    }
}
