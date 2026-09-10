//! Mutation-side snapshots, which nothing in this crate reads.
//!
//! `docs/research/stylo-requirements.md` item 6: stylo's invalidation needs
//! prior-state records captured *at mutation time*, and it is a mutation-path
//! feature, so the cost of adding it is proportional to the number of mutation
//! sites that exist when you do. The note says plainly: *"Retrofitting it in
//! Phase 5 means touching every mutation site twice. Add it to the Phase 4
//! scope and gate."*
//!
//! Nothing consumes these until Phase 5, which means these tests are the only
//! thing standing between a correct implementation and a plausible one. The
//! failure mode if they are wrong is not a crash — it is stylo re-matching the
//! wrong selectors, months from now, with no way to tell that the snapshots
//! were the cause.

use html5ever::{Attribute, LocalName, QualName, local_name, ns};
use px_dom::{Arena, NodeData, NodeId, TreeError, parse};

fn qual(name: &str) -> QualName {
    QualName::new(None, ns!(), LocalName::from(name))
}

fn attr(name: &str, value: &str) -> Attribute {
    Attribute {
        name: qual(name),
        value: value.into(),
    }
}

fn element(arena: &mut Arena, attrs: Vec<Attribute>) -> NodeId {
    match arena.create(NodeData::Element {
        name: QualName::new(None, ns!(html), local_name!("div")),
        attrs,
        template_contents: None,
        script_already_started: false,
    }) {
        Ok(id) => id,
        Err(error) => unreachable!("a fresh arena has slots: {error:?}"),
    }
}

/// Recording is off by default, and a parse records nothing.
///
/// Every element in a parse is new, so there is no prior state to describe and
/// a snapshot saying "it did not exist" invalidates nothing. Recording through
/// a parse would clone the attribute list of every element on the page for no
/// consumer.
#[test]
fn snapshots_are_not_recorded_during_parsing() {
    let dom = parse(r#"<div id="a" class="b" data-x="1">text</div>"#);
    assert!(!dom.arena.is_recording_snapshots());
    assert_eq!(dom.arena.snapshot_count(), 0);
}

/// The first write since a flush captures prior state.
#[test]
fn snapshots_capture_the_value_before_the_first_write() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "before")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    arena
        .set_attribute(id, qual("class"), "after".into())
        .expect("set");

    let snapshot = arena.snapshot(id).expect("a snapshot was recorded");
    assert_eq!(snapshot.attr(&qual("class")), Some("before"));
    assert!(snapshot.class_changed());
    assert!(!snapshot.id_changed());
    assert!(!snapshot.other_attributes_changed());

    // And the element itself really did change.
    let current = arena
        .get(id)
        .and_then(px_dom::Node::attrs)
        .and_then(|attrs| attrs.iter().find(|a| a.name == qual("class")))
        .map(|a| a.value.to_string());
    assert_eq!(current.as_deref(), Some("after"));
}

/// **The rule that makes a snapshot useful.** A second write must not
/// overwrite what the first captured.
///
/// A snapshot describes the element as of the last restyle, not the last
/// mutation. Getting this backwards yields a record of a change from the
/// second-most-recent value to the most recent — a change that never happened
/// — and it is invisible: the flags are right and the values are plausible.
#[test]
fn snapshots_keep_the_oldest_value_across_repeated_writes() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "first")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    for value in ["second", "third", "fourth"] {
        arena
            .set_attribute(id, qual("class"), value.into())
            .expect("set");
    }

    let snapshot = arena.snapshot(id).expect("recorded");
    assert_eq!(
        snapshot.attr(&qual("class")),
        Some("first"),
        "the snapshot must hold the value as of the last flush, not the value \
         before the most recent write"
    );
}

/// class, id and everything else are flagged separately.
///
/// Stylo's invalidation treats them separately because `class` and `id` have
/// their own selector-matching fast paths, and a change to either invalidates
/// a different set of rules than a change to `href`.
#[test]
fn snapshots_distinguish_class_id_and_other_attributes() {
    let cases: [(&str, [bool; 3]); 3] = [
        ("class", [true, false, false]),
        ("id", [false, true, false]),
        ("href", [false, false, true]),
    ];

    for (name, [class, id_changed, other]) in cases {
        let mut arena = Arena::new();
        let doc = arena.document();
        let node = element(&mut arena, vec![attr(name, "old")]);
        arena.append_child(doc, node).expect("append");

        arena.record_snapshots(true);
        arena
            .set_attribute(node, qual(name), "new".into())
            .expect("set");

        let snapshot = arena.snapshot(node).expect("recorded");
        assert_eq!(snapshot.class_changed(), class, "class flag for {name}");
        assert_eq!(snapshot.id_changed(), id_changed, "id flag for {name}");
        assert_eq!(
            snapshot.other_attributes_changed(),
            other,
            "other flag for {name}"
        );
    }
}

/// A namespaced `class` is not the `class` selectors match on.
///
/// Counting `xlink:class` as a class change would invalidate every class rule
/// on the page for an attribute nothing selects on. Cheap to get wrong, and
/// the symptom is a performance problem rather than a visible defect.
#[test]
fn snapshots_do_not_treat_a_namespaced_class_as_a_class_change() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, Vec::new());
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    let namespaced = QualName::new(None, ns!(xlink), LocalName::from("class"));
    arena
        .set_attribute(id, namespaced, "v".into())
        .expect("set");

    let snapshot = arena.snapshot(id).expect("recorded");
    assert!(
        !snapshot.class_changed(),
        "a namespaced class is not the class selectors match against"
    );
    assert!(snapshot.other_attributes_changed());
}

/// Flags accumulate across writes even though the values do not.
#[test]
fn snapshots_accumulate_flags_while_holding_one_set_of_values() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "c"), attr("id", "i")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    arena
        .set_attribute(id, qual("class"), "c2".into())
        .expect("set");
    arena
        .set_attribute(id, qual("id"), "i2".into())
        .expect("set");
    arena
        .set_attribute(id, qual("lang"), "en".into())
        .expect("set");

    let snapshot = arena.snapshot(id).expect("recorded");
    assert!(snapshot.class_changed());
    assert!(snapshot.id_changed());
    assert!(snapshot.other_attributes_changed());
    assert_eq!(snapshot.attr(&qual("class")), Some("c"));
    assert_eq!(snapshot.attr(&qual("id")), Some("i"));
    assert_eq!(
        snapshot.attr(&qual("lang")),
        None,
        "lang did not exist at snapshot time, and the record must say so \
         rather than showing its new value"
    );
}

/// Removal is a mutation too.
#[test]
fn snapshots_record_attribute_removal() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("id", "gone")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    assert_eq!(arena.remove_attribute(id, &qual("id")), Ok(true));
    assert_eq!(
        arena.remove_attribute(id, &qual("id")),
        Ok(false),
        "removing it twice reports that the second did nothing"
    );

    let snapshot = arena.snapshot(id).expect("recorded");
    assert!(snapshot.id_changed());
    assert_eq!(
        snapshot.attr(&qual("id")),
        Some("gone"),
        "the snapshot holds what was removed, which is the whole point"
    );
}

/// `add_attributes_if_missing` records, and flags only what it actually wrote.
///
/// This tests the **arena** method. It deliberately does not claim to test
/// that the sink calls it — an earlier version of this test did claim that,
/// and it was wrong: rewriting the sink to bypass the arena entirely left it
/// passing. A test cannot see which path the sink took, so `ci/gate-dom.sh`
/// checks the source instead.
#[test]
fn snapshots_are_recorded_by_the_add_if_missing_path() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "kept")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    arena
        .add_attributes_if_missing(id, vec![attr("class", "ignored"), attr("id", "added")])
        .expect("add");

    let snapshot = arena.snapshot(id).expect("recorded");
    assert!(
        snapshot.id_changed(),
        "the id that was actually added must be flagged"
    );
    assert!(
        !snapshot.class_changed(),
        "the class was already present, so nothing changed and nothing should \
         be flagged -- a flag here would invalidate every class rule for a \
         write that did not happen"
    );

    let class = arena
        .get(id)
        .and_then(px_dom::Node::attrs)
        .and_then(|attrs| attrs.iter().find(|a| a.name == qual("class")))
        .map(|a| a.value.to_string());
    assert_eq!(
        class.as_deref(),
        Some("kept"),
        "the existing value survives"
    );
}

/// One snapshot per element, however many times it is written.
///
/// Not a stylistic point. `snapshot()` returns the first match, so a bug that
/// pushes a fresh record per write is invisible through that accessor — the
/// first entry still holds the oldest values and still looks right. It shows
/// up only in the count, and in flags that stopped accumulating because each
/// write started a new record.
#[test]
fn snapshots_hold_exactly_one_record_per_element() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "c")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    for value in ["1", "2", "3", "4", "5"] {
        arena
            .set_attribute(id, qual("class"), value.into())
            .expect("set");
    }
    arena
        .set_attribute(id, qual("id"), "x".into())
        .expect("set");

    assert_eq!(
        arena.snapshot_count(),
        1,
        "six writes to one element must leave one record, not six"
    );
    let snapshot = arena.snapshot(id).expect("recorded");
    assert!(snapshot.class_changed());
    assert!(
        snapshot.id_changed(),
        "flags must accumulate into the same record; a fresh record per write          would leave this one knowing only about the last write"
    );
}

/// Taking the snapshots clears them, and the next write captures afresh.
#[test]
fn snapshots_are_taken_once_and_then_start_over() {
    let mut arena = Arena::new();
    let doc = arena.document();
    let id = element(&mut arena, vec![attr("class", "one")]);
    arena.append_child(doc, id).expect("append");

    arena.record_snapshots(true);
    arena
        .set_attribute(id, qual("class"), "two".into())
        .expect("set");
    assert!(arena.has_snapshot(id));

    let taken = arena.take_snapshots();
    assert_eq!(taken.len(), 1);
    assert_eq!(taken.first().map(|(node, _)| *node), Some(id));
    assert_eq!(
        taken.first().and_then(|(_, s)| s.attr(&qual("class"))),
        Some("one")
    );

    assert!(
        !arena.has_snapshot(id),
        "taking the snapshots is the flush; stylo's handled_snapshot bit \
         exists to stop one being processed twice, and taking by value makes \
         that structural instead"
    );

    arena
        .set_attribute(id, qual("class"), "three".into())
        .expect("set");
    let snapshot = arena.snapshot(id).expect("recorded again");
    assert_eq!(
        snapshot.attr(&qual("class")),
        Some("two"),
        "after a flush the next write captures the value as of the flush"
    );
}

/// Attribute mutation refuses a stale handle and a non-element.
#[test]
fn snapshots_and_attribute_writes_refuse_what_they_cannot_do() {
    let mut arena = Arena::new();
    let doc = arena.document();

    let text = match arena.create(NodeData::Text {
        contents: "t".into(),
    }) {
        Ok(id) => id,
        Err(error) => unreachable!("{error:?}"),
    };
    arena.append_child(doc, text).expect("append");
    assert_eq!(
        arena.set_attribute(text, qual("class"), "x".into()),
        Err(TreeError::NoSuchNode),
        "a text node has no attributes"
    );

    let gone = element(&mut arena, Vec::new());
    arena.append_child(doc, gone).expect("append");
    arena.remove_subtree(gone).expect("remove");
    assert_eq!(
        arena.set_attribute(gone, qual("class"), "x".into()),
        Err(TreeError::NoSuchNode)
    );
    assert!(!arena.has_snapshot(gone));
}

/// The sink's merge behaviour, exercised through a real parse.
///
/// `add_attrs_if_missing` is reached when the tree builder meets a second
/// `<html>` or `<body>` start tag: the spec says to merge in the attributes
/// that are not already present and ignore the rest. This asserts the
/// behaviour end to end, which is the part a test *can* see; whether the sink
/// reached it through the arena is `ci/gate-dom.sh`'s business.
#[test]
fn snapshots_the_sink_merges_duplicate_root_attributes_per_spec() {
    let dom = parse(r#"<html class="first" lang="en"><body><html class="second" dir="rtl">"#);

    let html = dom
        .arena
        .descendants(dom.document())
        .find(|id| {
            dom.arena
                .get(*id)
                .and_then(px_dom::Node::element_name)
                .is_some_and(|name| &*name.local == "html")
        })
        .expect("an html element");

    let value = |name: &str| {
        dom.arena
            .get(html)
            .and_then(px_dom::Node::attrs)
            .and_then(|attrs| attrs.iter().find(|a| &*a.name.local == name))
            .map(|a| a.value.to_string())
    };

    assert_eq!(
        value("class").as_deref(),
        Some("first"),
        "an attribute already present keeps its original value"
    );
    assert_eq!(value("lang").as_deref(), Some("en"));
    assert_eq!(
        value("dir").as_deref(),
        Some("rtl"),
        "an attribute not already present is merged in"
    );
}
