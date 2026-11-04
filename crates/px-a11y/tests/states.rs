//! States and properties: exclusion, disabled/checked/pressed/selected/
//! expanded, required/invalid/readonly, level, set position, live regions,
//! focusability and tab order. Expectations follow ARIA 1.2 (the state and
//! property definitions) and the HTML-AAM state mappings.

mod common;

use common::Page;
use px_a11y::{Current, HasPopup, Invalid, Politeness, Tri};

fn dump_of(html: &str) -> String {
    Page::new(html).lines()
}

// --- Exclusion -------------------------------------------------------------

#[test]
fn a11y_state_aria_hidden_and_hidden_remove_the_subtree() {
    let p = Page::new(
        "<p id=a aria-hidden=true>no <b>no</b></p>\
         <p id=b hidden>no</p>\
         <p id=c aria-hidden=false>yes</p>\
         <p id=d aria-hidden=TRUE>no</p>\
         <p id=e aria-hidden=''>yes</p>",
    );
    assert_eq!(p.role("a"), "-");
    assert_eq!(p.role("b"), "-");
    assert_eq!(p.role("c"), "paragraph");
    assert_eq!(p.role("d"), "-");
    assert_eq!(p.role("e"), "paragraph");
    assert!(!p.tree.dump().contains("no"));
}

#[test]
fn a11y_state_never_rendered_elements_are_excluded() {
    let p = Page::new(
        "<head><title>T</title><style>p{}</style><script>var a;</script><meta><link></head>\
         <body><noscript>ns</noscript><template><p>t</p></template><p>shown</p></body>",
    );
    assert_eq!(p.lines(), "paragraph\n  text \"shown\"\n");
}

#[test]
fn a11y_state_closed_details_hides_its_content_but_not_the_summary() {
    let closed = dump_of("<details><summary>Sum</summary><p>body</p></details>");
    assert_eq!(
        closed,
        "group [collapsed]\n  button name=\"Sum\" [focusable collapsed]\n"
    );
    let open = dump_of("<details open><summary>Sum</summary><p>body</p></details>");
    assert_eq!(
        open,
        "group [expanded]\n  button name=\"Sum\" [focusable expanded]\n  paragraph\n    text \"body\"\n"
    );
}

#[test]
fn a11y_state_presentational_roles_collapse_but_keep_children() {
    assert_eq!(
        dump_of("<div role=presentation><p>x</p></div>"),
        "paragraph\n  text \"x\"\n"
    );
}

#[test]
fn a11y_state_style_hints_exclude_display_none_and_visibility_hidden() {
    use px_a11y::{AccessibilityTree, BuildInputs, StyleHints};
    use std::collections::HashMap;
    let dom = px_dom::parse(
        "<p id=a>gone</p><p id=b>kept</p><div id=c><p id=d>shown</p></div><p id=e>gone2</p>",
    );
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
    let mut styles = HashMap::new();
    styles.insert(
        find("a"),
        StyleHints {
            display_none: true,
            visibility_hidden: false,
        },
    );
    // visibility:hidden hides the box but a visible descendant stays.
    styles.insert(
        find("c"),
        StyleHints {
            display_none: false,
            visibility_hidden: true,
        },
    );
    styles.insert(
        find("e"),
        StyleHints {
            display_none: false,
            visibility_hidden: true,
        },
    );
    let inputs = BuildInputs {
        style: Some(&styles),
        bounds: None,
    };
    let tree = AccessibilityTree::build(&dom.arena, inputs);
    let dump = common::strip_root(&tree.dump());
    // `a` display:none: absent with its text. `c` visibility:hidden: absent as a
    // node, but it was a generic anyway; `d` stays. `e` is a paragraph
    // with visibility:hidden: the paragraph and its text are gone.
    assert_eq!(
        dump,
        "paragraph\n  text \"kept\"\nparagraph\n  text \"shown\"\n"
    );
}

// --- Binary and tri-state states -----------------------------------------

#[test]
fn a11y_state_disabled_native_aria_and_fieldset() {
    let p = Page::new(
        "<button id=a disabled>a</button>\
         <button id=b aria-disabled=true>b</button>\
         <button id=c aria-disabled=false>c</button>\
         <fieldset disabled><input id=d><button id=e>e</button><fieldset id=f></fieldset></fieldset>\
         <input id=g disabled>\
         <fieldset disabled><legend><input id=h></legend><legend><input id=i></legend></fieldset>",
    );
    let dis = |id: &str| p.node(id).map(|n| n.states().disabled);
    assert_eq!(dis("a"), Some(true));
    assert_eq!(dis("b"), Some(true));
    assert_eq!(dis("c"), Some(false));
    assert_eq!(dis("d"), Some(true));
    assert_eq!(dis("e"), Some(true));
    assert_eq!(dis("f"), Some(true));
    assert_eq!(dis("g"), Some(true));
    // The first legend of a disabled fieldset is exempt; a second is not.
    assert_eq!(dis("h"), Some(false));
    assert_eq!(dis("i"), Some(true));
    // Disabled controls cannot take focus.
    assert_eq!(p.node("a").map(|n| n.states().focusable), Some(false));
    assert_eq!(p.node("c").map(|n| n.states().focusable), Some(true));
}

#[test]
fn a11y_state_checked_native_and_aria() {
    let p = Page::new(
        "<input id=a type=checkbox checked>\
         <input id=b type=checkbox>\
         <input id=c type=radio checked>\
         <div id=d role=checkbox aria-checked=mixed></div>\
         <div id=e role=checkbox aria-checked=true></div>\
         <div id=f role=checkbox></div>\
         <div id=g role=switch aria-checked=false></div>\
         <div id=h role=checkbox aria-checked=bogus></div>\
         <p id=i>no state</p>",
    );
    let chk = |id: &str| p.node(id).and_then(|n| n.states().checked);
    assert_eq!(chk("a"), Some(Tri::True));
    assert_eq!(chk("b"), Some(Tri::False));
    assert_eq!(chk("c"), Some(Tri::True));
    assert_eq!(chk("d"), Some(Tri::Mixed));
    assert_eq!(chk("e"), Some(Tri::True));
    // A checkbox with no aria-checked is exposed unchecked; a bogus value too.
    assert_eq!(chk("f"), Some(Tri::False));
    assert_eq!(chk("g"), Some(Tri::False));
    assert_eq!(chk("h"), Some(Tri::False));
    // A paragraph has no checked state at all, which is not "unchecked".
    assert_eq!(chk("i"), None);
}

#[test]
fn a11y_state_pressed_selected_expanded() {
    let p = Page::new(
        "<button id=a aria-pressed=true>a</button>\
         <button id=b aria-pressed=mixed>b</button>\
         <button id=c aria-pressed=false>c</button>\
         <button id=d>d</button>\
         <select><option id=e selected>e</option><option id=f>f</option></select>\
         <div id=g role=tab aria-selected=true></div>\
         <div id=h role=tab aria-selected=false></div>\
         <div id=i role=tab></div>\
         <button id=j aria-expanded=true>j</button>\
         <button id=k aria-expanded=false>k</button>\
         <button id=l aria-expanded=maybe>l</button>",
    );
    let pr = |id: &str| p.node(id).and_then(|n| n.states().pressed);
    assert_eq!(pr("a"), Some(Tri::True));
    assert_eq!(pr("b"), Some(Tri::Mixed));
    assert_eq!(pr("c"), Some(Tri::False));
    assert_eq!(pr("d"), None);
    let sel = |id: &str| p.node(id).and_then(|n| n.states().selected);
    assert_eq!(sel("e"), Some(true));
    assert_eq!(sel("f"), Some(false));
    assert_eq!(sel("g"), Some(true));
    assert_eq!(sel("h"), Some(false));
    assert_eq!(sel("i"), None);
    let ex = |id: &str| p.node(id).and_then(|n| n.states().expanded);
    assert_eq!(ex("j"), Some(true));
    assert_eq!(ex("k"), Some(false));
    assert_eq!(ex("l"), None);
}

#[test]
fn a11y_state_required_invalid_readonly_and_friends() {
    let p = Page::new(
        "<input id=a required>\
         <input id=b aria-required=true>\
         <input id=c aria-invalid=true>\
         <input id=d aria-invalid=grammar>\
         <input id=e aria-invalid=spelling>\
         <input id=f aria-invalid=false>\
         <input id=g readonly>\
         <div id=h role=textbox aria-readonly=true aria-multiline=true></div>\
         <textarea id=i></textarea>\
         <div id=j role=dialog aria-modal=true aria-busy=true></div>\
         <a id=k href=u aria-current=page>k</a>\
         <a id=l href=u aria-current=whatever>l</a>\
         <a id=m href=u aria-current=false>m</a>\
         <button id=n aria-haspopup=true>n</button>\
         <button id=o aria-haspopup=listbox>o</button>",
    );
    let s = |id: &str| *p.node(id).expect("node").states();
    assert!(s("a").required && s("b").required);
    assert_eq!(s("c").invalid, Invalid::True);
    assert_eq!(s("d").invalid, Invalid::Grammar);
    assert_eq!(s("e").invalid, Invalid::Spelling);
    assert_eq!(s("f").invalid, Invalid::False);
    assert!(s("g").read_only);
    assert!(s("h").read_only && s("h").multiline);
    assert!(s("i").multiline);
    assert!(s("j").modal && s("j").busy);
    assert_eq!(s("k").current, Current::Page);
    assert_eq!(s("l").current, Current::True);
    assert_eq!(s("m").current, Current::False);
    assert_eq!(s("n").has_popup, HasPopup::Menu);
    assert_eq!(s("o").has_popup, HasPopup::Listbox);
}

// --- Level, position -------------------------------------------------------

#[test]
fn a11y_state_levels() {
    let p = Page::new(
        "<h1 id=a>a</h1><h3 id=b>b</h3><h2 id=c aria-level=5>c</h2>\
         <div id=d role=heading>d</div><div id=e role=heading aria-level=0>e</div>\
         <div id=f role=heading aria-level=99999999999999999999>f</div>\
         <div id=g role=heading aria-level=-3>g</div>",
    );
    let lv = |id: &str| p.node(id).and_then(|n| n.props().level);
    assert_eq!(lv("a"), Some(1));
    assert_eq!(lv("b"), Some(3));
    // aria-level overrides the element's level.
    assert_eq!(lv("c"), Some(5));
    // A heading role with no usable level defaults to 2 (ARIA).
    assert_eq!(lv("d"), Some(2));
    assert_eq!(lv("e"), Some(2));
    assert_eq!(lv("g"), Some(2));
    // Huge numbers saturate instead of failing.
    assert_eq!(lv("f"), Some(u32::MAX));
}

#[test]
fn a11y_state_set_position_is_computed_from_siblings() {
    let p = Page::new(
        "<ul><li id=a>a</li><li id=b>b</li><li id=c>c</li></ul>\
         <div role=tablist><div role=tab id=t1></div><div role=tab id=t2></div></div>\
         <select size=3><option id=o1>1</option><option id=o2>2</option></select>",
    );
    let pos = |id: &str| {
        let n = p.node(id).expect("node");
        (n.props().pos_in_set, n.props().set_size)
    };
    assert_eq!(pos("a"), (Some(1), Some(3)));
    assert_eq!(pos("b"), (Some(2), Some(3)));
    assert_eq!(pos("c"), (Some(3), Some(3)));
    assert_eq!(pos("t1"), (Some(1), Some(2)));
    assert_eq!(pos("t2"), (Some(2), Some(2)));
    assert_eq!(pos("o1"), (Some(1), Some(2)));
    assert_eq!(pos("o2"), (Some(2), Some(2)));
}

#[test]
fn a11y_state_explicit_set_position_overrides_computed() {
    let p = Page::new(
        "<ul><li id=a aria-posinset=7 aria-setsize=40>a</li><li id=b>b</li>\
         <li id=c aria-posinset=0 aria-setsize=-1>c</li></ul>",
    );
    let pos = |id: &str| {
        let n = p.node(id).expect("node");
        (n.props().pos_in_set, n.props().set_size)
    };
    assert_eq!(pos("a"), (Some(7), Some(40)));
    assert_eq!(pos("b"), (Some(2), Some(3)));
    // Invalid explicit values are ignored in favour of the computed ones.
    assert_eq!(pos("c"), (Some(3), Some(3)));
}

// --- Live regions ------------------------------------------------------------

#[test]
fn a11y_state_live_regions_explicit_and_implied() {
    let p = Page::new(
        "<div id=a aria-live=polite>a</div>\
         <div id=b aria-live=assertive aria-atomic=true aria-relevant='additions removals'>b</div>\
         <div id=c role=alert>c</div>\
         <div id=d role=status>d</div>\
         <div id=e role=log>e</div>\
         <div id=f role=timer>f</div>\
         <div id=g aria-live=bogus>g</div>\
         <div id=h role=alert aria-live=off aria-atomic=false>h</div>\
         <div id=i aria-live=polite aria-relevant=all>i</div>\
         <div id=j aria-live=polite aria-relevant='nonsense'>j</div>\
         <output id=k>k</output>",
    );
    let live = |id: &str| p.node(id).map(|n| n.props().live);
    let a = live("a").expect("a");
    assert_eq!(a.politeness, Politeness::Polite);
    assert!(!a.atomic);
    // ARIA default aria-relevant: additions text.
    assert!(a.relevant.additions && a.relevant.text && !a.relevant.removals);
    let b = live("b").expect("b");
    assert_eq!(b.politeness, Politeness::Assertive);
    assert!(b.atomic && b.relevant.additions && b.relevant.removals && !b.relevant.text);
    assert_eq!(live("c").expect("c").politeness, Politeness::Assertive);
    assert!(live("c").expect("c").atomic, "alert is implicitly atomic");
    assert_eq!(live("d").expect("d").politeness, Politeness::Polite);
    assert!(live("d").expect("d").atomic);
    assert_eq!(live("e").expect("e").politeness, Politeness::Polite);
    assert!(!live("e").expect("e").atomic);
    assert_eq!(live("f").expect("f").politeness, Politeness::Off);
    // An invalid aria-live value is no value: generic stays off and collapses.
    assert_eq!(p.role("g"), "-");
    // An explicit off overrides what the role implies.
    assert_eq!(live("h").expect("h").politeness, Politeness::Off);
    assert!(!live("h").expect("h").atomic);
    let i = live("i").expect("i").relevant;
    assert!(i.additions && i.removals && i.text);
    let j = live("j").expect("j").relevant;
    assert!(
        j.additions && j.text && !j.removals,
        "unusable value keeps the default"
    );
    // <output> is role status.
    assert_eq!(live("k").expect("k").politeness, Politeness::Polite);
}

#[test]
fn a11y_state_a_live_region_keeps_a_generic_in_the_tree() {
    assert_eq!(
        dump_of("<div aria-live=polite>x</div>"),
        "generic live=polite relevant=additions,text\n  text \"x\"\n"
    );
}

// --- Values --------------------------------------------------------------------

#[test]
fn a11y_state_values_and_ranges() {
    let p = Page::new(
        "<input id=a value='typed'>\
         <input id=b type=password value=secret>\
         <textarea id=c>area</textarea>\
         <input id=d type=range min=1 max=9 value=4>\
         <progress id=e value=0.5 max=2></progress>\
         <meter id=f value=3 min=0 max=10></meter>\
         <div id=g role=slider aria-valuenow=5 aria-valuemin=0 aria-valuemax=1e999></div>\
         <select id=h><option>one</option><option selected>two</option></select>",
    );
    assert_eq!(
        p.node("a").and_then(|n| n.props().value.clone()).as_deref(),
        Some("typed")
    );
    assert_eq!(p.node("b").and_then(|n| n.props().value.clone()), None);
    assert_eq!(
        p.node("c").and_then(|n| n.props().value.clone()).as_deref(),
        Some("area")
    );
    let r = |id: &str| p.node(id).and_then(|n| n.props().range).expect("range");
    assert_eq!(
        (r("d").min, r("d").max, r("d").now),
        (Some(1.0), Some(9.0), Some(4.0))
    );
    assert_eq!(
        (r("e").min, r("e").max, r("e").now),
        (Some(0.0), Some(2.0), Some(0.5))
    );
    assert_eq!(
        (r("f").min, r("f").max, r("f").now),
        (Some(0.0), Some(10.0), Some(3.0))
    );
    // 1e999 is not finite and is rejected rather than carried as infinity.
    assert_eq!((r("g").now, r("g").max), (Some(5.0), None));
    assert_eq!(
        p.node("h").and_then(|n| n.props().value.clone()).as_deref(),
        Some("two")
    );
    assert!(!p.tree.dump().contains("secret"));
}

#[test]
fn a11y_state_links_carry_their_href() {
    let p = Page::new("<a id=a href='https://example.test/x?y=1'>a</a>");
    assert_eq!(
        p.node("a").and_then(|n| n.props().href.clone()).as_deref(),
        Some("https://example.test/x?y=1")
    );
}

// --- Focus -----------------------------------------------------------------------

#[test]
fn a11y_state_focusability() {
    let p = Page::new(
        "<a id=a href=u>a</a><a id=b>b</a>\
         <button id=c>c</button><button id=d disabled>d</button>\
         <input id=e><input id=f type=hidden><select id=g></select><textarea id=h></textarea>\
         <div id=i tabindex=0>i</div><div id=j tabindex=-1>j</div><div id=k tabindex=abc>k</div>\
         <div id=l contenteditable>l</div><div id=m contenteditable=false>m</div>\
         <div inert><button id=n>n</button></div>\
         <button id=o inert>o</button>",
    );
    let foc = |id: &str| p.node(id).map(|n| n.states().focusable);
    assert_eq!(foc("a"), Some(true));
    assert_eq!(foc("b"), None, "a generic anchor is not even in the tree");
    assert_eq!(foc("c"), Some(true));
    assert_eq!(foc("d"), Some(false));
    assert_eq!(foc("e"), Some(true));
    assert_eq!(foc("f"), None);
    assert_eq!(foc("g"), Some(true));
    assert_eq!(foc("h"), Some(true));
    assert_eq!(foc("i"), Some(true));
    assert_eq!(foc("j"), Some(true));
    assert_eq!(foc("k"), None);
    assert_eq!(foc("l"), Some(true));
    assert_eq!(foc("m"), None);
    assert_eq!(foc("n"), Some(false));
    assert_eq!(foc("o"), Some(false));
    assert_eq!(p.node("j").and_then(|n| n.tab_index()), Some(-1));
}

#[test]
fn a11y_state_tab_order_is_positive_ascending_then_document_order() {
    let p = Page::new(
        "<button id=z1>z1</button>\
         <button id=p3 tabindex=3>p3</button>\
         <button id=p1 tabindex=1>p1</button>\
         <a id=z2 href=u>z2</a>\
         <button id=p1b tabindex=1>p1b</button>\
         <button id=neg tabindex=-1>neg</button>\
         <button id=p2 tabindex=2>p2</button>\
         <div id=z3 tabindex=0>z3</div>\
         <button id=off disabled>off</button>",
    );
    let order: Vec<String> = p
        .tree
        .tab_order()
        .into_iter()
        .filter_map(|id| p.tree.node(id))
        .filter_map(|n| {
            let dom = n.dom_node();
            ["z1", "p3", "p1", "z2", "p1b", "neg", "p2", "z3", "off"]
                .into_iter()
                .find(|name| p.element(name) == Some(dom))
                .map(str::to_string)
        })
        .collect();
    assert_eq!(order, ["p1", "p1b", "p2", "p3", "z1", "z2", "z3"]);
}

#[test]
fn a11y_state_dump_shows_states_readably() {
    assert_eq!(
        dump_of("<input type=checkbox checked disabled aria-label=Agree>"),
        "checkbox name=\"Agree\" [disabled checked]\n"
    );
    assert_eq!(
        dump_of("<button aria-pressed=mixed aria-haspopup=menu>Go</button>"),
        "button name=\"Go\" [focusable pressed=mixed haspopup=menu]\n"
    );
}
