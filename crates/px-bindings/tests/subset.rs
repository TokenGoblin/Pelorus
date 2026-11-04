//! The Phase 11 gate for the generator: the committed DOM IDL subset parses,
//! resolves, lowers, generates, and the generated Rust compiles.

mod common;

use px_bindings::ir::lower;
use px_bindings::resolve::{BuildOptions, ContainerKind, Model};
use std::path::PathBuf;

fn idl_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("idl")
}

fn sources() -> Vec<(String, String)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(idl_dir())
        .expect("idl dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "idl"))
        .collect();
    files.sort();
    files
        .iter()
        .map(|p| {
            (
                p.file_name().expect("name").to_string_lossy().into_owned(),
                std::fs::read_to_string(p).expect("read"),
            )
        })
        .collect()
}

fn model() -> Model {
    Model::from_sources(&sources(), BuildOptions { require_exposed: true })
        .unwrap_or_else(|e| panic!("{e:#?}"))
}

#[test]
fn subset_contains_the_gate_interfaces() {
    let names: Vec<_> = sources().into_iter().map(|(n, _)| n).collect();
    assert_eq!(names.len(), 8, "{names:?}");
    let m = model();
    for n in [
        "EventTarget", "Event", "Node", "Element", "Document", "DOMTokenList", "Text",
        "CharacterData", "NodeList",
    ] {
        assert!(m.container(n).is_some(), "{n} missing");
    }
    assert!(m.dictionary("EventInit").is_some());
    assert!(m.dictionary("AddEventListenerOptions").is_some());
}

#[test]
fn subset_resolves_the_real_structure() {
    let m = model();
    let chain: Vec<_> = m.ancestors("Text").iter().map(|i| i.name.clone()).collect();
    assert_eq!(chain, ["CharacterData", "Node", "EventTarget"]);
    assert_eq!(m.container("EventListener").expect("cb").kind, ContainerKind::CallbackInterface);
    // Mixins arrived: Element has ParentNode.children-less members and ChildNode.
    let has = |iface: &str, member: &str| {
        m.container(iface).expect("iface").members.iter().any(|x| {
            matches!(&x.kind, px_bindings::resolve::MemberKind::Operation { name: Some(n), .. }
                | px_bindings::resolve::MemberKind::Attribute { name: n, .. } if n == member)
        })
    };
    assert!(has("Element", "querySelector"));
    assert!(has("Element", "remove"));
    assert!(has("Document", "getElementById"));
    assert!(has("CharacterData", "nextElementSibling"));
    assert!(has("Document", "append"));
    assert!(!has("Node", "remove"));
}

#[test]
fn subset_lowers_with_the_expected_conversions() {
    let b = lower(&model()).unwrap_or_else(|e| panic!("{e}"));
    use px_bindings::ir::{Conv, IntMode};
    let cd = b.interfaces.iter().find(|i| i.name == "CharacterData").expect("CharacterData");
    let data = cd.attributes.iter().find(|a| a.name == "data").expect("data");
    assert_eq!(
        data.conv,
        Conv::String {
            kind: px_bindings::ast::StringType::DOMString,
            null_to_empty: true
        }
    );
    let node = b.interfaces.iter().find(|i| i.name == "Node").expect("Node");
    assert!(node.constants.len() >= 12);
    assert_eq!(node.parent.as_deref(), Some("EventTarget"));
    let et = b.interfaces.iter().find(|i| i.name == "EventTarget").expect("EventTarget");
    let add = et.operations.iter().find(|o| o.name.as_deref() == Some("addEventListener")).expect("add");
    let opts = &add.overloads[0].args[2];
    assert!(opts.optional && opts.default.is_some());
    let Conv::Union(u) = &opts.conv else { panic!("{:?}", opts.conv) };
    assert_eq!(u.members.len(), 2);
    let _ = IntMode::Wrap;
}

#[test]
fn subset_generates_and_the_output_compiles() {
    let text = common::generate_from(
        &sources().iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
        true,
    )
    .unwrap_or_else(|e| panic!("{e:#?}"));
    for needle in [
        "pub trait NodeHost: EventTargetHost {",
        "pub trait TextHost: CharacterDataHost {",
        "pub const ELEMENT_NODE: u16 = 1;",
        "pub const DOCUMENT_POSITION_DISCONNECTED: u16 = 1;",
        "fn add_event_listener(",
        "fn get_element_by_id(",
        "impl rt::Upcast<EventTarget> for Text {}",
        "pub struct AddEventListenerOptions {",
        "pub enum NodeOrDomString {",
        "pub enum AddEventListenerOptionsOrBoolean {",
        "pub type DOMHighResTimeStamp",
    ] {
        // The typedef is expanded, not emitted; that line must be absent.
        if needle.contains("DOMHighResTimeStamp") {
            assert!(!text.contains(needle));
        } else {
            assert!(text.contains(needle), "missing {needle:?}");
        }
    }
    common::assert_compiles(&text);
}

#[test]
fn every_host_method_is_fallible() {
    // The generational-handle rule, carried into the glue: no method may hand
    // back a value without a way to say the handle is stale.
    let text = common::generate_from(
        &sources().iter().map(|(a, b)| (a.as_str(), b.as_str())).collect::<Vec<_>>(),
        true,
    )
    .expect("generates");
    let mut in_trait = false;
    let mut methods = 0;
    for line in text.lines() {
        if line.starts_with("pub trait ") {
            in_trait = true;
        } else if line == "}" {
            in_trait = false;
        } else if in_trait && line.trim_start().starts_with("fn ") {
            methods += 1;
            assert!(line.contains("&mut self"), "{line}");
            assert!(line.ends_with(";") && line.contains("-> rt::HostResult<"), "{line}");
        }
    }
    assert!(methods > 100, "only {methods} methods");
    assert!(!text.contains("unwrap") && !text.contains("panic!") && !text.contains("unsafe"));
}

#[test]
fn generation_is_deterministic() {
    let s = sources();
    let files: Vec<(&str, &str)> = s.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let a = common::generate_from(&files, true).expect("a");
    let b = common::generate_from(&files, true).expect("b");
    assert_eq!(a.as_bytes(), b.as_bytes());
    // And the model, lowering and emitter do not depend on allocation order:
    // a third run on a freshly built model is identical too.
    let m = model();
    let ir1 = lower(&m).expect("lower");
    let ir2 = lower(&model()).expect("lower");
    assert_eq!(ir1, ir2);
}
