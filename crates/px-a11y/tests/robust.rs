//! Hostile input: deep nesting on a small stack, absurd attribute values, and
//! random attribute soup. The properties are "does not panic", "does not
//! overflow", "stays within its caps" and "is deterministic".

mod common;

use common::{Page, Rng};
use px_a11y::{A11yUpdate, AccessibilityTree, BuildInputs, MAX_STRING, StyleHints, diff};
use std::collections::HashMap;

/// 256 KB: small enough that one stack frame per document level would die at
/// a few thousand levels.
const SMALL_STACK: usize = 256 * 1024;

fn on_small_stack<F: FnOnce() + Send + 'static>(body: F) {
    let handle = match std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(body)
    {
        Ok(h) => h,
        Err(e) => unreachable!("could not spawn: {e}"),
    };
    // A stack overflow aborts the whole process, so reaching this line at all
    // is most of the test.
    assert!(handle.join().is_ok(), "panicked on a small stack");
}

fn exercise(dom: &px_dom::Dom) {
    let tree = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    let dump = tree.dump();
    assert!(!dump.is_empty());
    let again = AccessibilityTree::build(&dom.arena, BuildInputs::default());
    assert!(diff(&tree, &again).is_empty());
    for part in A11yUpdate::snapshot(&tree) {
        assert_eq!(part.validate(), Ok(()));
    }
    let _ = tree.tab_order();
    let _ = tree.all();
}

#[test]
fn a11y_robust_hundred_thousand_levels_of_nesting_on_a_small_stack() {
    // The parser refuses past its nesting limit rather than building 100,000
    // levels; the tree must cope with whatever it produced, and with the
    // refused remainder.
    for open in [
        "<div>",
        "<span>",
        "<ul><li>",
        "<div role=group>",
        "<b><div>",
    ] {
        let html = open.repeat(100_000);
        on_small_stack(move || {
            let dom = px_dom::parse(&html);
            exercise(&dom);
        });
    }
}

#[test]
fn a11y_robust_deep_chain_built_directly_in_the_arena() {
    // At the arena's own maximum depth, with something nameable at every level.
    on_small_stack(|| {
        let mut html = String::new();
        for _ in 0..400 {
            html.push_str("<div role=button aria-labelledby=x>t");
        }
        html.push_str("<span id=x>deep</span>");
        let dom = px_dom::parse(&html);
        exercise(&dom);
    });
}

#[test]
fn a11y_robust_deeply_nested_lists_and_tables() {
    on_small_stack(|| {
        let html = format!("{}x{}", "<ul><li>".repeat(2000), "</li></ul>".repeat(2000));
        exercise(&px_dom::parse(&html));
        let html = format!(
            "{}x{}",
            "<table><tr><td>".repeat(2000),
            "</td></tr></table>".repeat(2000)
        );
        exercise(&px_dom::parse(&html));
    });
}

#[test]
fn a11y_robust_hostile_attribute_values() {
    let huge = "A".repeat(2 << 20);
    let ids = (0..20_000)
        .map(|i| format!("i{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let cases: Vec<String> = vec![
        format!("<div role='{huge}'>x</div>"),
        format!("<div role='{}button'>x</div>", "foo ".repeat(100_000)),
        format!("<button aria-label='{huge}'>x</button>"),
        format!("<button aria-labelledby='{ids}'>x</button><span id=i0>y</span>"),
        format!("<div aria-owns='{ids}'></div><p id=i0></p>"),
        format!("<input value='{huge}' aria-valuenow='{huge}'>"),
        format!("<h1 aria-level='{huge}'>x</h1>"),
        format!("<li aria-posinset='{huge}' aria-setsize='-{huge}'>x</li>"),
        format!("<div tabindex='{huge}'>x</div>"),
        format!("<a href='{huge}'>x</a>"),
        "<button aria-label='\0\0\0'>x\0y</button>".to_string(),
        "<div role='\0button'>x</div><div role='button\0'>y</div>".to_string(),
        "<div id='\0' aria-labelledby='\0'>x</div>".to_string(),
        "<button aria-label='\u{202e}\u{200d}\u{fffd}\u{10ffff}'>x</button>".to_string(),
        "<input type=range aria-valuenow='1e999' aria-valuemin='-1e999' aria-valuemax='NaN'>"
            .to_string(),
        "<input type=range value='0x10' min='.' max='+' >".to_string(),
        "<h1 aria-level='+0'>x</h1><h1 aria-level='  7  '>y</h1><h1 aria-level='7.9'>z</h1>"
            .to_string(),
    ];
    for html in cases {
        let page = Page::new(&html);
        let dump = page.tree.dump();
        // Every string is within its cap, and nothing carries a NUL.
        assert!(!dump.contains('\0'));
        for id in page.tree.all() {
            let Some(n) = page.tree.node(id) else {
                continue;
            };
            assert!(n.name().len() <= MAX_STRING);
            assert!(n.description().len() <= MAX_STRING);
            assert!(!n.name().contains('\0'));
        }
        for part in A11yUpdate::snapshot(&page.tree) {
            assert_eq!(part.validate(), Ok(()), "{}", &html[..html.len().min(80)]);
        }
    }
}

#[test]
fn a11y_robust_level_parsing_follows_html_integer_rules() {
    let p = Page::new(
        "<h1 id=a aria-level='+0'>x</h1><h1 id=b aria-level='  7  '>y</h1><h1 id=c aria-level='7.9'>z</h1>",
    );
    let lv = |id: &str| p.node(id).and_then(|n| n.props().level);
    // Zero is not a valid level; the heading falls back to its element's.
    assert_eq!(lv("a"), Some(1));
    assert_eq!(lv("b"), Some(7));
    assert_eq!(lv("c"), Some(7));
}

const TAGS: [&str; 40] = [
    "div",
    "span",
    "a",
    "button",
    "input",
    "label",
    "p",
    "ul",
    "li",
    "ol",
    "table",
    "tr",
    "td",
    "th",
    "thead",
    "select",
    "option",
    "optgroup",
    "textarea",
    "img",
    "h1",
    "h2",
    "section",
    "nav",
    "main",
    "header",
    "footer",
    "aside",
    "form",
    "fieldset",
    "legend",
    "details",
    "summary",
    "dialog",
    "figure",
    "figcaption",
    "output",
    "progress",
    "meter",
    "iframe",
];

const ATTRS: [&str; 36] = [
    "role",
    "aria-label",
    "aria-labelledby",
    "aria-describedby",
    "aria-owns",
    "aria-hidden",
    "aria-live",
    "aria-atomic",
    "aria-relevant",
    "aria-checked",
    "aria-pressed",
    "aria-expanded",
    "aria-selected",
    "aria-level",
    "aria-posinset",
    "aria-setsize",
    "aria-valuenow",
    "aria-disabled",
    "aria-invalid",
    "aria-current",
    "aria-haspopup",
    "id",
    "for",
    "title",
    "alt",
    "href",
    "type",
    "value",
    "tabindex",
    "hidden",
    "disabled",
    "checked",
    "open",
    "placeholder",
    "list",
    "contenteditable",
];

const VALUES: [&str; 22] = [
    "",
    "true",
    "false",
    "mixed",
    "x",
    "y",
    "z",
    "a b c",
    "x y z",
    "button",
    "none",
    "widget",
    "navigation main",
    "-1",
    "0",
    "3",
    "99999999999999999999",
    "polite",
    "page",
    "text",
    "checkbox",
    "\u{202e}",
];

fn random_html(rng: &mut Rng, budget: usize) -> String {
    let mut out = String::new();
    let mut open: Vec<&str> = Vec::new();
    for _ in 0..budget {
        match rng.below(10) {
            0..=4 => {
                let tag = rng.pick(&TAGS);
                out.push('<');
                out.push_str(tag);
                for _ in 0..rng.below(4) {
                    let name = rng.pick(&ATTRS);
                    let value = if rng.below(4) == 0 {
                        format!("{}{}", rng.pick(&["x", "y", "z"]), rng.below(3))
                    } else {
                        rng.pick(&VALUES).to_string()
                    };
                    out.push_str(&format!(" {name}='{value}'"));
                }
                out.push('>');
                open.push(tag);
            }
            5..=6 => {
                if let Some(tag) = open.pop() {
                    out.push_str(&format!("</{tag}>"));
                }
            }
            7..=8 => out.push_str(rng.pick(&["text", " ", "&nbsp;", "\n", "word word", "\u{e9}"])),
            _ => out.push_str(&format!("<{} id='x{}'>", rng.pick(&TAGS), rng.below(3))),
        }
    }
    out
}

#[test]
fn a11y_robust_random_attribute_soup_never_panics() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut previous: Option<AccessibilityTree> = None;
    let mut nodes_seen = 0usize;
    for round in 0..1500 {
        let budget = 5 + rng.below(120);
        let html = random_html(&mut rng, budget);
        let dom = px_dom::parse(&html);

        // Random style hints on a random subset of nodes.
        let mut styles = HashMap::new();
        if round % 3 == 0 {
            for id in dom.arena.descendants(dom.arena.document()) {
                match rng.below(8) {
                    0 => {
                        styles.insert(
                            id,
                            StyleHints {
                                display_none: true,
                                visibility_hidden: false,
                            },
                        );
                    }
                    1 => {
                        styles.insert(
                            id,
                            StyleHints {
                                display_none: false,
                                visibility_hidden: true,
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
        let inputs = BuildInputs {
            style: if styles.is_empty() {
                None
            } else {
                Some(&styles)
            },
            bounds: None,
        };
        let tree = AccessibilityTree::build(&dom.arena, inputs);
        nodes_seen += tree.len();

        // Consistency: every node is reachable, parents agree with children.
        let all = tree.all();
        assert_eq!(all.len(), tree.len(), "unreachable nodes in {html}");
        for id in &all {
            for child in tree.children(*id).unwrap_or_default() {
                assert_eq!(tree.parent(child), Some(*id));
            }
            let n = tree.node(*id).expect("node");
            assert!(n.name().len() <= MAX_STRING && n.description().len() <= MAX_STRING);
        }
        // Determinism.
        let again = AccessibilityTree::build(&dom.arena, inputs);
        assert_eq!(tree.dump(), again.dump(), "{html}");
        assert!(diff(&tree, &again).is_empty());
        // Wire form of whatever came out is valid.
        for part in A11yUpdate::snapshot(&tree) {
            assert_eq!(part.validate(), Ok(()), "{html}");
        }
        // Diff against an unrelated tree never panics and its delta is valid.
        if let Some(prev) = &previous {
            let _ = diff(prev, &tree);
            for part in A11yUpdate::delta(prev, &tree) {
                assert_eq!(part.validate(), Ok(()));
            }
        }
        let _ = tree.tab_order();
        previous = Some(tree);
    }
    assert!(
        nodes_seen > 5_000,
        "the generator produced almost nothing: {nodes_seen}"
    );
}
