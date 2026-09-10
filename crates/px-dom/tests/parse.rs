//! html5ever driving the arena.
//!
//! The conformance number the Phase 4 gate wants comes from the html5lib
//! corpus. These are the cases worth pinning by hand: the ones where a wrong
//! answer is a *security* difference rather than a rendering difference, and
//! the ones specific to putting the tree in an arena rather than in `Rc`s.

use px_dom::{Arena, MAX_DEPTH, Node, NodeData, NodeId, parse};

/// A crude serialisation, enough to assert tree shape without depending on a
/// serialiser this crate does not have yet.
fn shape(arena: &Arena, id: NodeId, out: &mut String) {
    let Some(node) = arena.get(id) else {
        out.push_str("<gone>");
        return;
    };
    match node.data() {
        NodeData::Document => out.push_str("#document"),
        NodeData::Fragment => out.push_str("#fragment"),
        NodeData::Doctype { name, .. } => {
            out.push_str("<!DOCTYPE ");
            out.push_str(name);
            out.push('>');
        }
        NodeData::Text { contents } => {
            out.push('"');
            out.push_str(contents);
            out.push('"');
        }
        NodeData::Comment { .. } => out.push_str("<!---->"),
        NodeData::Element { name, .. } => {
            out.push('<');
            out.push_str(&name.local);
            out.push('>');
        }
        NodeData::ProcessingInstruction { .. } => out.push_str("<?pi?>"),
    }

    let children: Vec<NodeId> = arena.child_ids(id).collect();
    if children.is_empty() {
        return;
    }
    out.push('(');
    for (i, child) in children.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        shape(arena, *child, out);
    }
    out.push(')');
}

fn shape_of(html: &str) -> String {
    let dom = parse(html);
    let mut out = String::new();
    shape(&dom.arena, dom.document(), &mut out);
    out
}

/// The implied `<html><head><body>` structure, which is the first thing that
/// breaks if the sink is wired up wrong.
#[test]
fn parse_builds_the_implied_document_structure() {
    assert_eq!(
        shape_of("<p>hi"),
        r#"#document(<html>(<head> <body>(<p>("hi"))))"#,
        "the implied html/head/body structure is the first thing that breaks          when a sink is wired up wrong"
    );
}

/// Consecutive character tokens become **one** text node.
///
/// The spec requires it, and getting it wrong is not cosmetic: every range
/// offset, every `textContent`, and every `Node.splitText` position is defined
/// against the text node's contents. A tree with `"a"`,`"b"` where the spec
/// says `"ab"` disagrees with every other browser about where character 1 is.
#[test]
fn parse_merges_adjacent_text_into_one_node() {
    let dom = parse("<p>one&amp;two");
    let text: Vec<String> = dom
        .arena
        .descendants(dom.document())
        .filter_map(|id| dom.arena.get(id))
        .filter_map(Node::text)
        .map(|t| t.to_string())
        .collect();
    assert_eq!(
        text,
        vec!["one&two".to_string()],
        "the entity split the character stream into three tokens; they must \
         land in one text node"
    );
}

/// Misnesting is repaired the way the spec says, not the way it is written.
///
/// This is the adoption agency algorithm, which is the single most intricate
/// part of HTML parsing and the reason Appendix A says not to write one.
#[test]
fn parse_repairs_misnested_formatting() {
    let shape = shape_of("<p><b>bold<i>both</b>italic</i>");
    assert!(
        shape.contains("<b>") && shape.contains("<i>"),
        "both elements survive: {shape}"
    );
    // The <i> is reconstructed inside the <p> after the </b>, so "italic" is
    // in an <i> that is *not* inside the <b>.
    assert!(
        shape.matches("<i>").count() >= 2,
        "the adoption agency algorithm should reconstruct the <i>: {shape}"
    );
}

/// A `<template>`'s contents are not its children.
///
/// Security-relevant rather than cosmetic: content inside a template is inert,
/// and a sink that made it an ordinary child would make it live. Scripts and
/// event handlers in a template would run.
#[test]
fn parse_keeps_template_contents_out_of_the_tree() {
    let dom = parse("<template><div>inside</div></template>");
    let shape = {
        let mut out = String::new();
        shape(&dom.arena, dom.document(), &mut out);
        out
    };
    assert!(
        !shape.contains("<div>"),
        "template contents must not be children of the template element: {shape}"
    );
    assert!(shape.contains("<template>"));

    // But they are reachable through the fragment.
    let template = dom
        .arena
        .descendants(dom.document())
        .find(|id| {
            dom.arena
                .get(*id)
                .and_then(Node::element_name)
                .map(|name| &*name.local == "template")
                .unwrap_or(false)
        })
        .expect("a template element");
    let contents = match dom.arena.get(template).map(Node::data) {
        Some(NodeData::Element {
            template_contents: Some(fragment),
            ..
        }) => *fragment,
        _ => panic!("the template has no contents fragment"),
    };
    let inside: Vec<String> = dom
        .arena
        .descendants(contents)
        .filter_map(|id| dom.arena.get(id))
        .filter_map(Node::text)
        .map(|t| t.to_string())
        .collect();
    assert_eq!(inside, vec!["inside".to_string()]);
}

/// Foster parenting: text inside a `<table>` but outside a cell is relocated
/// before the table rather than dropped.
#[test]
fn parse_foster_parents_stray_table_text() {
    let dom = parse("<table>stray<tr><td>cell");
    let text: Vec<String> = dom
        .arena
        .descendants(dom.document())
        .filter_map(|id| dom.arena.get(id))
        .filter_map(Node::text)
        .map(|t| t.to_string())
        .collect();
    assert!(
        text.contains(&"stray".to_string()),
        "stray table text must be relocated, not dropped: {text:?}"
    );
}

/// Quirks mode is recorded, because it changes layout and selector matching.
#[test]
fn parse_records_quirks_mode() {
    use html5ever::interface::QuirksMode;
    assert_eq!(
        parse("<!DOCTYPE html><p>x").quirks_mode,
        QuirksMode::NoQuirks
    );
    assert_eq!(parse("<p>no doctype").quirks_mode, QuirksMode::Quirks);
}

/// A nesting bomb is bounded in the tree **and** in time.
///
/// The tree half is §4.4's depth limit and is the obvious one. The time half
/// is not, and it is the one that was actually dangerous.
///
/// html5ever's tree builder is quadratic in nesting depth: its stack of open
/// elements keeps growing however shallow the tree we build, and the spec's
/// "has an element in scope" tests scan it on every start tag. The depth limit
/// does nothing about that, because that stack is not ours. Measured on
/// release builds before `parse` learned to stop feeding — 2,000 nested divs
/// 11 ms, 4,000 44 ms, 8,000 192 ms, 16,000 686 ms, while the arena's own cost
/// doubled with the input rather than quadrupling. A five-megabyte file of
/// nothing but `<div>` extrapolated to about three quarters of an hour.
///
/// So this asserts the bound rather than the tree shape.
#[test]
fn parse_bounds_a_nesting_bomb_in_time_as_well_as_depth() {
    use std::time::Instant;

    // A million elements: at the old quadratic rate this would not finish
    // today. The wall-clock ceiling is deliberately enormous -- three orders
    // of magnitude above the ~65 ms this actually takes -- because the point
    // is to catch a return to quadratic behaviour, not to measure a machine.
    let bomb = "<div>".repeat(1_000_000);
    let start = Instant::now();
    let dom = parse(&bomb);
    let elapsed = start.elapsed();

    assert!(
        dom.abandoned,
        "a million-deep document must be abandoned; without that the parse is          quadratic in html5ever's open-element stack and this input hangs the          tab for the better part of an hour"
    );
    assert!(
        elapsed.as_secs() < 30,
        "parsing a nesting bomb took {elapsed:?}; the feed bound in          px_dom::parse has stopped working"
    );

    // And the tree itself still obeys the depth limit.
    for id in dom.arena.descendants(dom.document()) {
        let depth = dom.arena.depth(id).expect("every live node has a depth");
        assert!(depth <= MAX_DEPTH, "a node reached depth {depth}");
    }
}

/// The bound does not fire on documents that are merely large.
///
/// A cheap way to make a nesting limit look safe is to make it fire early, and
/// then every real page silently loses its tail. This is the guard against
/// having done that: a hundred thousand elements of ordinary shallow markup
/// must parse whole.
#[test]
fn parse_does_not_abandon_a_large_but_shallow_document() {
    use std::time::Instant;

    let big = format!(
        "<html><body>{}</body></html>",
        "<p>text</p>".repeat(100_000)
    );
    let start = Instant::now();
    let dom = parse(&big);
    let elapsed = start.elapsed();

    // A ceiling as well as a correctness check, because this test caught a
    // quadratic once and would only have got slower the next time.
    //
    // `append_child` briefly walked the parent's child list on every append,
    // to hand a live-range notification an index it did not need. These
    // 100,000 paragraphs took 145 seconds; they now take about two. The
    // ceiling is deliberately far above that — it is here to catch an order of
    // magnitude, not to measure a machine.
    assert!(
        elapsed.as_secs() < 60,
        "parsing 100,000 shallow paragraphs took {elapsed:?}; something on the \
         append path has gone quadratic again"
    );

    assert!(!dom.abandoned, "a shallow document was abandoned");
    assert_eq!(dom.truncated, 0, "nothing here is past the depth limit");

    let paragraphs = dom
        .arena
        .descendants(dom.document())
        .filter(|id| {
            dom.arena
                .get(*id)
                .and_then(Node::element_name)
                .map(|name| &*name.local == "p")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(
        paragraphs, 100_000,
        "every paragraph must survive; a bound that eats real content is worse          than no bound"
    );
}

/// Deeply nested but *legal* markup is not abandoned either.
///
/// 400 levels is below the limit and well past anything real, so it must come
/// through untouched. This is the other side of the previous test: the bound
/// must not fire just because a document is deep, only because it is past what
/// the tree will hold.
#[test]
fn parse_keeps_deep_but_legal_nesting() {
    let depth = 400;
    let html = format!("{}{}", "<div>".repeat(depth), "</div>".repeat(depth));
    let dom = parse(&html);

    assert!(!dom.abandoned);
    assert_eq!(dom.truncated, 0);

    let divs = dom
        .arena
        .descendants(dom.document())
        .filter(|id| {
            dom.arena
                .get(*id)
                .and_then(Node::element_name)
                .map(|name| &*name.local == "div")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(divs, depth, "every legal level must survive");
}

/// Every handle the parse produced resolves, and the tree it built is a tree.
///
/// The arena's invariants have to survive a real parser, not only the
/// operation sequences the fuzz harness generates. html5ever moves subtrees
/// around in ways no hand-written test thinks of.
#[test]
fn parse_leaves_a_well_formed_tree() {
    let html = r#"<!DOCTYPE html><html><head><title>t</title></head>
        <body><p>a<b>b</b><i>c</i></p><table><tr><td>d</td></tr></table>
        <ul><li>1<li>2<li>3</ul><!-- c --><template><span>x</span></template>
        </body></html>"#;
    let dom = parse(html);

    assert_eq!(dom.truncated, 0, "nothing here is deep or large");

    for id in dom.arena.descendants(dom.document()) {
        let Some(node) = dom.arena.get(id) else {
            panic!("a handle from the traversal did not resolve");
        };
        let children: Vec<NodeId> = dom.arena.child_ids(id).collect();

        assert_eq!(children.first().copied(), node.first_child());
        assert_eq!(children.last().copied(), node.last_child());

        let mut previous = None;
        for child in &children {
            let child_node = dom.arena.get(*child).expect("a listed child resolves");
            assert_eq!(child_node.parent(), Some(id));
            assert_eq!(child_node.prev_sibling(), previous);
            previous = Some(*child);
        }
    }
}
