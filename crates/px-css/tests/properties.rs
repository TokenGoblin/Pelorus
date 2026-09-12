//! Every property in the defined set is exercised, and the set and the fixtures
//! are held to each other.
//!
//! `crates/px-css/tests/properties.toml` defines the property set §9 Phase 5's
//! first gate item asks for, and `ci/gate-style.sh` holds it to a floor so it
//! cannot shrink. This file is the other half: it declares a value for every
//! property in that set, resolves it, and reads the computed value back.
//!
//! # What this proves, and what it does not
//!
//! For each of the 64 properties: the declaration **parses**, it **cascades**, and
//! the computed value **differs from the initial value**. That is the check that
//! catches a property silently ignored — which is the failure mode that matters
//! here, because a property stylo does not support does not error, it simply
//! leaves the initial value in place and the page renders wrong.
//!
//! It is *not* a check that each computed value is the specific value the spec
//! says. That is `computed.rs`'s job, for the handful of properties where the
//! computed value is interesting rather than a keyword echo — unit resolution,
//! inheritance, `currentColor`. Sixty-four hand-written expected strings would be
//! sixty-four chances to encode stylo's serialisation as if it were the spec.
//!
//! # Why the values live here and the names live in the manifest
//!
//! The manifest is the *definition* of the set and is what the gate checks. The
//! values are how this file exercises it. `properties_cover_the_whole_manifest`
//! asserts the two agree in both directions, so a property added to the manifest
//! without a fixture fails, and a fixture for a property that is not in the set
//! fails too. Without that, the gate's floor could be satisfied by a manifest full
//! of properties nothing tested — which the gate's own comments say is worse than
//! not listing them.

use px_css::engine::StyleEngine;

/// A property in the defined set, and a declaration that should change it.
///
/// Values are chosen to differ from the initial value and to need no font metrics
/// — `em`, `rem`, percentages and absolute units only. `InitialFontMetrics` in
/// `px-css` answers `ex`, `ch`, `ic` and `cap` from a 16px base with nothing
/// behind it, so a fixture using one of those would be testing the stub.
const FIXTURES: &[(&str, &str)] = &[
    // Box model and box generation.
    ("display", "inline-block"),
    ("position", "relative"),
    ("box-sizing", "border-box"),
    ("width", "42px"),
    ("height", "43px"),
    ("min-width", "1px"),
    ("min-height", "2px"),
    ("max-width", "100px"),
    ("max-height", "101px"),
    ("margin-top", "1px"),
    ("margin-right", "2px"),
    ("margin-bottom", "3px"),
    ("margin-left", "4px"),
    ("padding-top", "5px"),
    ("padding-right", "6px"),
    ("padding-bottom", "7px"),
    ("padding-left", "8px"),
    // Each border width declares a style alongside it, and that is CSS rather
    // than a workaround: `border-*-width` computes to 0 whenever the
    // corresponding `border-*-style` is `none`, which is the initial value. The
    // first version of these fixtures declared only the width and the sweep
    // reported all four as not cascading -- the test was right and the fixture
    // was wrong, which is the more useful way round.
    (
        "border-top-width",
        "border-top-style: solid; border-top-width: 9px",
    ),
    (
        "border-right-width",
        "border-right-style: solid; border-right-width: 10px",
    ),
    (
        "border-bottom-width",
        "border-bottom-style: solid; border-bottom-width: 11px",
    ),
    (
        "border-left-width",
        "border-left-style: solid; border-left-width: 12px",
    ),
    ("border-top-style", "dashed"),
    ("border-top-color", "rgb(1, 2, 3)"),
    ("top", "13px"),
    ("right", "14px"),
    ("bottom", "15px"),
    ("left", "16px"),
    ("float", "left"),
    ("clear", "both"),
    ("overflow-x", "scroll"),
    ("overflow-y", "hidden"),
    // Inline and text.
    ("font-family", "monospace"),
    ("font-size", "20px"),
    ("font-style", "italic"),
    ("font-weight", "700"),
    ("line-height", "2"),
    ("color", "rgb(4, 5, 6)"),
    ("text-align", "center"),
    ("text-decoration-line", "underline"),
    ("text-indent", "17px"),
    ("white-space", "pre"),
    ("letter-spacing", "1px"),
    ("word-spacing", "2px"),
    ("vertical-align", "middle"),
    ("direction", "rtl"),
    ("writing-mode", "vertical-rl"),
    // Flex and grid.
    ("flex-direction", "column"),
    ("flex-wrap", "wrap"),
    ("flex-grow", "2"),
    ("flex-shrink", "3"),
    ("flex-basis", "18px"),
    ("justify-content", "center"),
    ("align-items", "center"),
    ("align-self", "flex-end"),
    ("align-content", "space-between"),
    ("row-gap", "19px"),
    ("column-gap", "20px"),
    ("grid-template-columns", "1fr 2fr"),
    ("grid-template-rows", "10px 20px"),
    ("grid-auto-flow", "column"),
    // Paint and visibility.
    ("background-color", "rgb(7, 8, 9)"),
    ("opacity", "0.25"),
    ("visibility", "hidden"),
    ("z-index", "3"),
];

/// The property names the manifest declares, in declaration order.
fn manifest_properties() -> Vec<String> {
    let text = std::fs::read_to_string("tests/properties.toml").expect("the manifest is readable");
    let doc: toml::Value = toml::from_str(&text).expect("the manifest is valid TOML");
    let groups = doc
        .get("groups")
        .and_then(toml::Value::as_table)
        .expect("the manifest has groups");

    let mut names = Vec::new();
    for (_, group) in groups {
        let list = group
            .get("properties")
            .and_then(toml::Value::as_array)
            .expect("every group lists properties");
        for value in list {
            names.push(
                value
                    .as_str()
                    .expect("a property name is a string")
                    .to_owned(),
            );
        }
    }
    names
}

/// The longhands a manifest entry covers.
///
/// Most entries are longhands and map to themselves. Some are shorthands — and
/// deliberately so: the manifest names what Phase 6 will *read*, and an author
/// writes `white-space`, which in current CSS is a shorthand over
/// `white-space-collapse` and `text-wrap-mode`. Rather than rewrite the manifest
/// into whatever stylo's current decomposition happens to be, a shorthand entry is
/// satisfied when **any** of its longhands changes, because that is what "the
/// declaration cascaded" means for a shorthand.
fn longhands_of(property: &str) -> Vec<style::properties::LonghandId> {
    let id = style::properties::PropertyId::parse_unchecked(property, None)
        .unwrap_or_else(|()| panic!("{property} is not a property stylo knows"));
    match id {
        // `longhand_or_shorthand` lives on `NonCustomPropertyId`, not on
        // `PropertyId`, and resolves aliases on the way — which matters, because
        // several manifest entries are the names authors write rather than
        // whatever stylo calls them internally.
        style::properties::PropertyId::NonCustom(non_custom) => {
            match non_custom.longhand_or_shorthand() {
                Ok(longhand) => vec![longhand],
                Err(shorthand) => shorthand.longhands().collect(),
            }
        }
        style::properties::PropertyId::Custom(_) => {
            panic!("{property} parsed as a custom property; the manifest has none")
        }
    }
}

/// The computed values of `property`'s longhands on `#t`, with `declaration`
/// applied or not.
fn computed_value(property: &str, declaration: Option<&str>) -> Vec<String> {
    let longhands = longhands_of(property);

    let dom = px_dom::parse("<html><body><div id=outer><p id=t>x</p></div></body></html>");
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = StyleEngine::new(800.0, 600.0, quirks);
    if let Some(declaration) = declaration {
        engine.add_author_stylesheet(
            &format!("#t {{ {declaration} }}"),
            "https://example.invalid/a.css",
        );
    }
    let root = engine.style_root_for(&arena);
    engine
        .resolve(&arena, &root)
        .expect("the document has a root element");

    let target = core::iter::once(arena.document())
        .chain(arena.descendants(arena.document()))
        .find(|id| {
            arena.get(*id).is_some_and(|n| {
                n.attrs().is_some_and(|attrs| {
                    attrs
                        .iter()
                        .any(|a| a.name.local == html5ever::local_name!("id") && &*a.value == "t")
                })
            })
        })
        .expect("the fixture has an element with id=t");

    px_css::view::with_dom(&arena, &root, |dom| {
        let node = dom.node(target).expect("the target resolves");
        let element = px_css::dom::StyleElement::new(node).expect("the target is an element");
        let data = style::dom::TElement::borrow_data(&element).expect("the target was styled");
        let primary = data.styles.primary();
        longhands
            .iter()
            .map(|longhand| {
                primary.computed_value_to_string(
                    style::properties::PropertyDeclarationId::Longhand(*longhand),
                )
            })
            .collect()
    })
    .expect("the document resolves")
}

/// The fixture table and the manifest must agree, in both directions.
///
/// Without this the gate's floor could be met by a manifest full of properties
/// nothing exercises — which `ci/gate-style.sh`'s own comments call worse than not
/// listing them, because it reads as coverage.
#[test]
fn properties_cover_the_whole_manifest() {
    let declared: std::collections::BTreeSet<String> = manifest_properties().into_iter().collect();
    let exercised: std::collections::BTreeSet<String> = FIXTURES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();

    let missing: Vec<&String> = declared.difference(&exercised).collect();
    assert!(
        missing.is_empty(),
        "these properties are in the manifest with no fixture: {missing:?}"
    );

    let extra: Vec<&String> = exercised.difference(&declared).collect();
    assert!(
        extra.is_empty(),
        "these fixtures name properties that are not in the manifest: {extra:?}"
    );

    assert_eq!(
        declared.len(),
        64,
        "the manifest's property count changed; ci/gate-style.sh pins the floor \
         and this pins the number, so both have to move deliberately"
    );
}

/// Every property in the set parses, cascades, and computes to something other
/// than its initial value.
///
/// One test rather than 64 because the failure message names the property and the
/// values, so a single failure is as legible as a named test would be — and 64
/// resolves in one process is about a second.
#[test]
fn properties_each_one_cascades_and_changes_the_computed_value() {
    let mut unchanged = Vec::new();

    for (property, value) in FIXTURES {
        let initial = computed_value(property, None);
        // A fixture value is either a bare value or, where CSS requires a
        // companion declaration, the whole declaration list. Detected by the
        // colon rather than by a second table column: one column that sometimes
        // carries more is easier to read than two that are usually redundant.
        let declaration = if value.contains(':') {
            (*value).to_owned()
        } else {
            format!("{property}: {value}")
        };
        let declared = computed_value(property, Some(&declaration));

        // A shorthand is satisfied when any of its longhands moved; a longhand is
        // the one-element case of the same rule.
        if initial == declared {
            unchanged.push(format!(
                "{property}: `{value}` computed to {declared:?}, same as the initial value"
            ));
        }
    }

    assert!(
        unchanged.is_empty(),
        "these properties did not cascade — a declaration that parses but changes \
         nothing is how an unsupported property looks, because CSS has no error \
         for one:\n  {}",
        unchanged.join("\n  ")
    );
}
