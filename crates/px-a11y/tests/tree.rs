//! The tree: structure, navigation, generational ids, determinism,
//! `aria-owns`, caps, the dump format.

mod common;

use common::Page;
use px_a11y::{AccessibilityTree, BuildInputs, BuildOptions, DumpOptions, MAX_TREE_DEPTH, Role};

#[test]
fn a11y_tree_dump_of_a_small_page_is_readable() {
    let p = Page::new(
        "<title>Demo</title><h1>Hello</h1><p>A <a href=/x>link</a> here.</p>\
         <ul><li>one</li><li>two</li></ul><button disabled>Go</button>",
    );
    assert_eq!(
        p.tree.dump(),
        "document name=\"Demo\"\n\
         \x20 heading name=\"Hello\" level=1\n\
         \x20   text \"Hello\"\n\
         \x20 paragraph\n\
         \x20   text \"A\"\n\
         \x20   link name=\"link\" href=\"/x\" [focusable]\n\
         \x20     text \"link\"\n\
         \x20   text \"here.\"\n\
         \x20 list\n\
         \x20   listitem posinset=1 setsize=2\n\
         \x20     text \"one\"\n\
         \x20   listitem posinset=2 setsize=2\n\
         \x20     text \"two\"\n\
         \x20 button name=\"Go\" [disabled]\n"
    );
}

#[test]
fn a11y_tree_whitespace_only_text_is_not_a_node() {
    let p = Page::new("<p>a</p>   \n  <p>b</p>");
    assert_eq!(
        p.lines(),
        "paragraph\n  text \"a\"\nparagraph\n  text \"b\"\n"
    );
}

#[test]
fn a11y_tree_navigation() {
    let p = Page::new("<ul id=l><li id=a>a</li><li id=b>b</li><li id=c>c</li></ul>");
    let t = &p.tree;
    let id = |name: &str| t.find(p.element(name).expect("element")).expect("exposed");
    let (l, a, b, c) = (id("l"), id("a"), id("b"), id("c"));

    assert_eq!(t.parent(a), Some(l));
    assert_eq!(t.children(l), Some(vec![a, b, c]));
    assert_eq!(t.child_count(l), Some(3));
    assert_eq!(t.first_child(l), Some(a));
    assert_eq!(t.last_child(l), Some(c));
    assert_eq!(t.next_sibling(a), Some(b));
    assert_eq!(t.next_sibling(b), Some(c));
    assert_eq!(t.next_sibling(c), None);
    assert_eq!(t.prev_sibling(c), Some(b));
    assert_eq!(t.prev_sibling(a), None);
    assert_eq!(t.parent(l), t.root());
    assert_eq!(t.parent(t.root().expect("root")), None);
    // Document order, root first, text runs included.
    let all = t.all();
    assert_eq!(all.len(), t.len());
    assert_eq!(all.first().copied(), t.root());
    let order: Vec<_> = t.descendants(l).into_iter().take(4).collect();
    assert_eq!(
        order.get(..3),
        Some(&[l, a, t.first_child(a).expect("text")][..])
    );
    assert_eq!(t.node(a).map(|n| n.role()), Some(Role::ListItem));
    assert_eq!(t.node(a).map(|n| n.dom_node()), p.element("a"));
}

#[test]
fn a11y_tree_ids_go_stale_when_the_tree_is_rebuilt() {
    let p = Page::new("<h1 id=h>Hi</h1><p id=q>x</p>");
    let first = p.tree.find(p.element("h").expect("h")).expect("exposed");
    assert!(p.tree.node(first).is_some());

    let rebuilt = AccessibilityTree::build(&p.dom.arena, BuildInputs::default());
    assert_ne!(p.tree.generation(), rebuilt.generation());
    // The old handle resolves in no accessor of the new tree, even though
    // the same slot exists there.
    assert!(rebuilt.node(first).is_none());
    assert!(rebuilt.parent(first).is_none());
    assert!(rebuilt.children(first).is_none());
    assert!(rebuilt.first_child(first).is_none());
    assert!(rebuilt.next_sibling(first).is_none());
    assert!(rebuilt.prev_sibling(first).is_none());
    assert!(rebuilt.child_count(first).is_none());
    assert!(rebuilt.descendants(first).is_empty());
    // The slot is the same; only the build differs.
    let fresh = rebuilt.find(p.element("h").expect("h")).expect("exposed");
    assert_eq!(fresh.index(), first.index());
    assert_ne!(fresh, first);
    assert!(rebuilt.node(fresh).is_some());
}

#[test]
fn a11y_tree_empty_arena_is_a_document_only() {
    let dom = px_dom::parse("");
    let t = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert_eq!(t.len(), 1);
    assert_eq!(t.dump(), "document\n");
    assert!(!t.is_truncated());
}

#[test]
fn a11y_tree_is_deterministic() {
    let html = "<title>t</title><nav aria-label=n><a href=a>a</a><a href=b>b</a></nav>\
        <table><tr><th>h</th><td>c</td></tr></table><select><option>1</option></select>\
        <div aria-owns='o1 o2'></div><p id=o1>1</p><p id=o2>2</p>\
        <label for=i>L</label><input id=i value=v>";
    let a = Page::new(html);
    let b = Page::new(html);
    assert_eq!(a.tree.dump(), b.tree.dump());
    assert_eq!(
        a.tree.dump_with(DumpOptions { bounds: true }),
        b.tree.dump_with(DumpOptions { bounds: true })
    );
    // And the same arena built repeatedly.
    let again = AccessibilityTree::build(&a.dom.arena, BuildInputs::default());
    assert_eq!(a.tree.dump(), again.dump());
    assert!(px_a11y::diff(&a.tree, &again).is_empty());
    for _ in 0..20 {
        let t = AccessibilityTree::build(&a.dom.arena, BuildInputs::default());
        assert_eq!(t.dump(), a.tree.dump());
    }
}

// --- aria-owns ---------------------------------------------------------------

#[test]
fn a11y_tree_aria_owns_reparents_in_idref_order_after_native_children() {
    let p = Page::new(
        "<ul id=l aria-owns='c a'><li id=own>own</li></ul>\
         <ul><li id=a>a</li><li id=b>b</li></ul><div><li id=c>c</li></div>",
    );
    let list = p.tree.find(p.element("l").expect("l")).expect("l");
    let kids: Vec<_> = p
        .tree
        .children(list)
        .expect("children")
        .into_iter()
        .filter_map(|c| p.tree.node(c).map(|n| n.dom_node()))
        .collect();
    let expect: Vec<_> = ["own", "c", "a"]
        .iter()
        .map(|n| p.element(n).expect("el"))
        .collect();
    assert_eq!(kids, expect);
    // The old parent lost the child it gave away.
    let other = p
        .tree
        .parent(p.tree.find(p.element("b").expect("b")).expect("b"))
        .expect("parent");
    assert_eq!(p.tree.child_count(other), Some(1));
}

#[test]
fn a11y_tree_aria_owns_cycles_are_refused() {
    // a owns b, b owns a: a is an ancestor of b once b is re-parented.
    let p = Page::new(
        "<div id=a role=group aria-owns=b>a</div><div id=b role=group aria-owns=a>b</div>",
    );
    let a = p.tree.find(p.element("a").expect("a")).expect("a");
    let b = p.tree.find(p.element("b").expect("b")).expect("b");
    assert_eq!(p.tree.parent(b), Some(a));
    assert_eq!(p.tree.parent(a), p.tree.root());
    // Self-ownership, owning the document's ancestor chain, and a longer ring.
    let p = Page::new(
        "<div id=a role=group aria-owns='a b'><div id=b role=group aria-owns=c><div id=c role=group aria-owns=a></div></div></div>",
    );
    for id in ["a", "b", "c"] {
        let mut cursor = p.tree.find(p.element(id).expect("el"));
        let mut steps = 0;
        while let Some(c) = cursor {
            cursor = p.tree.parent(c);
            steps += 1;
            assert!(steps < 10, "a parent chain that never reaches the root");
        }
    }
    let c = p.tree.find(p.element("c").expect("c")).expect("c");
    let a = p.tree.find(p.element("a").expect("a")).expect("a");
    assert_eq!(
        p.tree.parent(a),
        p.tree.root(),
        "c owning its ancestor a is refused"
    );
    assert!(p.tree.parent(c).is_some());
}

#[test]
fn a11y_tree_aria_owns_first_owner_wins_and_unexposed_targets_are_ignored() {
    let p = Page::new(
        "<div id=o1 role=group aria-owns=t></div><div id=o2 role=group aria-owns=t></div>\
         <p id=t>t</p>\
         <div id=o3 role=group aria-owns='nope hid'></div><p id=hid hidden>h</p>",
    );
    let t = p.tree.find(p.element("t").expect("t")).expect("t");
    let o1 = p.tree.find(p.element("o1").expect("o1")).expect("o1");
    assert_eq!(p.tree.parent(t), Some(o1));
    let o2 = p.tree.find(p.element("o2").expect("o2")).expect("o2");
    assert_eq!(p.tree.child_count(o2), Some(0));
    let o3 = p.tree.find(p.element("o3").expect("o3")).expect("o3");
    assert_eq!(p.tree.child_count(o3), Some(0));
}

#[test]
fn a11y_tree_aria_owns_chain_cannot_exceed_the_depth_limit() {
    // 2,000 siblings each owning the next: a chain 2,000 deep if honoured.
    let mut html = String::new();
    for i in 0..2000 {
        html.push_str(&format!(
            "<div id=n{i} role=group aria-owns=n{}></div>",
            i + 1
        ));
    }
    html.push_str("<div id=n2000 role=group></div>");
    let p = Page::new(&html);
    let mut deepest = 0usize;
    for id in p.tree.all() {
        let mut d = 0usize;
        let mut c = Some(id);
        while let Some(x) = c {
            c = p.tree.parent(x);
            d += 1;
        }
        deepest = deepest.max(d);
    }
    assert!(deepest <= MAX_TREE_DEPTH + 1, "depth {deepest}");
    assert!(
        deepest > 100,
        "ownership was honoured up to the limit ({deepest})"
    );
}

// --- Caps ----------------------------------------------------------------------

#[test]
fn a11y_tree_node_cap_truncates_and_says_so() {
    let dom = px_dom::parse(&"<p>x</p>".repeat(100));
    let opts = BuildOptions {
        max_nodes: 10,
        ..BuildOptions::default()
    };
    let t = AccessibilityTree::build_with(&dom.arena, BuildInputs::default(), opts);
    assert_eq!(t.len(), 10);
    assert!(t.is_truncated());
    // A truncated tree is still a consistent tree.
    assert_eq!(t.all().len(), 10);
    let full = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert!(!full.is_truncated());
    assert_eq!(full.len(), 201);
}

#[test]
fn a11y_tree_default_cap_holds_for_a_huge_flat_page() {
    let dom = px_dom::parse(&"<p>x</p>".repeat(100_000));
    let t = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert!(t.is_truncated());
    assert_eq!(t.len(), px_a11y::MAX_NODES);
}

#[test]
fn a11y_tree_name_work_budget_degrades_to_empty_names() {
    let dom = px_dom::parse("<button>one</button><button>two</button>");
    let opts = BuildOptions {
        max_name_visits: 0,
        ..BuildOptions::default()
    };
    let t = AccessibilityTree::build_with(&dom.arena, BuildInputs::default(), opts);
    assert!(t.dump().contains("button [focusable]"), "{}", t.dump());
}

// --- Structure details ------------------------------------------------------------

#[test]
fn a11y_tree_visibility_hidden_promotes_children_and_display_none_does_not() {
    use px_a11y::StyleHints;
    use std::collections::HashMap;
    let dom = px_dom::parse("<ul id=u><li id=a>a</li></ul><ul id=v><li id=b>b</li></ul>");
    let find = |id: &str| {
        dom.arena
            .descendants(dom.arena.document())
            .find(|n| {
                matches!(
                    dom.arena.get(*n).map(|x| x.data()),
                    Some(px_dom::NodeData::Element { attrs, .. })
                        if attrs.iter().any(|a| &*a.name.local == "id" && &*a.value == id)
                )
            })
            .expect("element")
    };
    let mut s = HashMap::new();
    s.insert(
        find("u"),
        StyleHints {
            visibility_hidden: true,
            display_none: false,
        },
    );
    s.insert(
        find("v"),
        StyleHints {
            visibility_hidden: false,
            display_none: true,
        },
    );
    let t = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: Some(&s),
            bounds: None,
        },
    );
    // The hidden list vanishes, its visible item is promoted; display:none
    // takes the whole second list with it.
    assert_eq!(
        common::strip_root(&t.dump()),
        "listitem posinset=1 setsize=1\n  text \"a\"\n"
    );
}

#[test]
fn a11y_tree_stale_style_or_bounds_keys_are_harmless() {
    use px_a11y::{Bounds, StyleHints};
    use std::collections::HashMap;
    let other = px_dom::parse("<p>zzz</p><p>zzz</p><p>zzz</p><p>zzz</p>");
    let dom = px_dom::parse("<p>a</p>");
    let mut s: HashMap<_, _> = HashMap::new();
    let mut b: HashMap<_, _> = HashMap::new();
    for id in other.arena.descendants(other.arena.document()) {
        s.insert(
            id,
            StyleHints {
                display_none: true,
                visibility_hidden: true,
            },
        );
        b.insert(
            id,
            Bounds {
                x: 1,
                y: 2,
                width: 3,
                height: 4,
            },
        );
    }
    let t = AccessibilityTree::build(
        &dom.arena,
        BuildInputs {
            style: Some(&s),
            bounds: Some(&b),
        },
    );
    // Same shape of arena, so some ids coincide: whatever happens, no panic
    // and a consistent tree.
    assert_eq!(t.all().len(), t.len());
}

#[test]
fn a11y_tree_template_contents_are_not_in_the_tree() {
    let p = Page::new("<template><button>t</button></template><p>x</p>");
    assert_eq!(p.lines(), "paragraph\n  text \"x\"\n");
}

#[test]
fn a11y_tree_duplicate_ids_resolve_to_the_first_element() {
    let p = Page::new(
        "<span id=d>first</span><span id=d>second</span><button aria-labelledby=d id=x>c</button>",
    );
    assert_eq!(p.name("x"), "first");
}

#[test]
fn a11y_tree_dump_escapes_hostile_names() {
    let p = Page::new("<button aria-label='a\"b\\c&#10;d&#9;e&#1;f'>x</button>");
    assert_eq!(
        p.lines(),
        "button name=\"a\\\"b\\\\c d e\\u{1}f\" [focusable]\n"
    );
}

#[test]
fn a11y_tree_aria_owns_a_node_already_a_child_moves_to_the_end_once() {
    let p = Page::new("<ul id=l aria-owns=a><li id=a>a</li><li id=b>b</li></ul>");
    let l = p.tree.find(p.element("l").expect("l")).expect("l");
    let kids: Vec<_> = p
        .tree
        .children(l)
        .expect("children")
        .into_iter()
        .filter_map(|c| p.tree.node(c).map(|n| n.dom_node()))
        .collect();
    assert_eq!(
        kids,
        [p.element("b").expect("b"), p.element("a").expect("a")]
    );
    assert_eq!(p.tree.all().len(), p.tree.len(), "no node is listed twice");
}
