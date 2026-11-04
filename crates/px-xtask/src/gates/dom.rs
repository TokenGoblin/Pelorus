//! `cargo xtask dom` -- the Phase 4 gate (build-spec section 9). Port of
//! `ci/gate-dom.sh`.
//!
//! ```text
//! html5lib-tests >= 99%                                   -> here
//! stale-handle fuzz target: every stale lookup is None     -> here
//! 24h mutation fuzz clean                                  -> campaign (not here)
//! 100,000-level nesting without stack overflow             -> here
//! ```
//!
//! Written failing, before px-dom had an arena. The phase is done when this
//! passes on both operating systems, and not when the build is green.
//!
//! # The one that is not like the others
//!
//! Three of these are ordinary: a conformance percentage, a fuzz target, a
//! campaign. The nesting item is a structural claim about the whole crate. A
//! hundred thousand levels of nesting overflows the stack of any recursive tree
//! walk, and a DOM has walks everywhere -- parsing, dropping, serialising,
//! querying, styling. Passing it once by fixing one function is not passing it;
//! the crate has to be written without recursion over tree depth, and `Drop` is
//! the one everybody forgets because the compiler writes it for you.
//!
//! So this gate also checks the *shape* of the code, not only its behaviour. A
//! test can show that today's walks are iterative. It cannot show that
//! tomorrow's will be.
//!
//! # Scan the code, not the prose
//!
//! `grep -rn` over a crate matches its comments too, and that breaks a scan in
//! both directions at once. Forward: the ban has to be stateable. The rules
//! name `Vec<Node>` as the thing they forbid, and px-dom's module docs explain
//! at length why a node must not own its children; without the comment filter
//! every one of those sentences is a violation, and a rule that cannot be
//! written down without tripping is a rule people comply with by deleting the
//! explanation. Backward, and worse: the non-vacuity guard passes when it finds
//! an `Option<&Node>` anywhere in the crate, and a doc comment mentioning the
//! shape would satisfy it, reporting "the phase has started" on the strength of
//! a sentence about starting the phase. Only whole-line comments are dropped;
//! `let v: Vec<Node> = x; // note` still trips, and should.
//!
//! # Known quirk kept from the script
//!
//! The attribute-shortcut scan runs `grep -rn` on a single *file*, which prints
//! `N:content` with no path, while its comment filter expects `path:N:content`.
//! So that one scan does not drop whole-line comments. Ported as it was.

use super::suites::{self, Suite};
use crate::Gate;
use crate::proc;
use crate::scan::{self, is_space, starts_with_word};
use std::path::Path;

/// The gate items as named test suites (see `suites` for the three conditions).
///
/// `ranges` is not one of section 9 Phase 4's four gate items. It is here
/// because section 9's *phase* names four deliverables -- "mutation-safe
/// iteration, tree ordering, ranges, depth limits" -- and its gate only covers
/// three of them. A deliverable named in the phase and absent from its gate is
/// one that can be skipped with nothing going red, which was very nearly what
/// happened: ranges were the last thing found missing, after the gate had
/// already gone green. This gate has always been allowed to check more than
/// section 9 lists; the no-infallible-accessor and no-owned-children scans are
/// not gate items either.
pub const SUITES: &[Suite] = &[
    (
        "stale-handles",
        "dom_stale",
        "crates/px-dom/tests/handles.rs",
    ),
    ("deep-nesting", "dom_depth", "crates/px-dom/tests/depth.rs"),
    ("tree-order", "dom_order", "crates/px-dom/tests/order.rs"),
    (
        "mutation-safety",
        "dom_mutation",
        "crates/px-dom/tests/mutation.rs",
    ),
    ("ranges", "range_", "crates/px-dom/tests/ranges.rs"),
];

const DOM_SRC: &str = "crates/px-dom/src";
const SINK: &str = "crates/px-dom/src/sink.rs";
const HTML5LIB: &str = "tests/html5lib";
const FUZZ_TARGETS: &[&str] = &["dom_stale_handle", "dom_mutation", "dom_parse"];

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `(mut[[:space:]]+)?Node\b` at the start of `s`.
fn opt_mut_then_node(s: &str) -> bool {
    let s = skip_ws(s);
    if let Some(r) = s.strip_prefix("mut") {
        let t = skip_ws(r);
        if t.len() < r.len() {
            return starts_with_word(t, "Node");
        }
    }
    starts_with_word(s, "Node")
}

/// `->[[:space:]]*Option<[[:space:]]*&?[[:space:]]*(mut[[:space:]]+)?Node\b`:
/// a function returning `Option<Node>` / `Option<&Node>` / `Option<&mut Node>`.
///
/// Anchored on the arrow rather than matching `Option<...Node>` anywhere.
/// Without it this matched the arena's own `node: Option<Node>` slot field, so
/// the guard reported "px-dom has an Option-returning accessor" on the strength
/// of a struct member and would have gone green on a crate with storage and no
/// accessors at all. Found by deleting the real accessors and watching it pass.
pub fn is_option_accessor(line: &str) -> bool {
    line.match_indices("->").any(|(i, m)| {
        let Some(r) = skip_ws(&line[i + m.len()..]).strip_prefix("Option<") else {
            return false;
        };
        let r = skip_ws(r);
        let r = r.strip_prefix('&').unwrap_or(r);
        opt_mut_then_node(r)
    })
}

/// `^\s*(pub(\([^)]*\))?\s+)?fn\s+\w+\s*\([^)]*\)\s*->\s*&?\s*(mut\s+)?Node\b`:
/// a function returning `Node`, `&Node` or `&mut Node` rather than an `Option`.
/// Deliberately matches private ones too.
pub fn is_bare_accessor(line: &str) -> bool {
    let mut r = skip_ws(line);
    if let Some(p) = r.strip_prefix("pub") {
        let p = match p.strip_prefix('(') {
            Some(inner) => match inner.split_once(')') {
                Some((body, after)) if !body.contains(')') => after,
                _ => return false,
            },
            None => p,
        };
        let t = skip_ws(p);
        if t.len() == p.len() {
            return false;
        }
        r = t;
    }
    let Some(r) = r.strip_prefix("fn") else {
        return false;
    };
    let t = skip_ws(r);
    if t.len() == r.len() {
        return false;
    }
    let name_len = t
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '_'))
        .map_or(t.len(), |(i, _)| i);
    if name_len == 0 {
        return false;
    }
    let Some(r) = skip_ws(&t[name_len..]).strip_prefix('(') else {
        return false;
    };
    let Some((_, r)) = r.split_once(')') else {
        return false;
    };
    let Some(r) = skip_ws(r).strip_prefix("->") else {
        return false;
    };
    let r = skip_ws(r);
    let r = r.strip_prefix('&').unwrap_or(r);
    opt_mut_then_node(r)
}

/// `Box<\s*Node\b|Vec<\s*Node\b|Rc<\s*Node\b|Arc<\s*Node\b`: a node owning
/// nodes. A recursive `Drop` on a linked tree is the classic 100,000-nesting
/// crash, and it is invisible: nobody writes `fn drop` recursively on purpose;
/// it happens because dropping a node drops its children. The arena makes that
/// avoidable -- nodes live in a Vec and dropping the arena drops a flat
/// allocation -- but only if nothing holds a `Box<Node>` or `Vec<Node>`
/// *inside* a node.
pub fn is_owned_child(line: &str) -> bool {
    ["Box<", "Vec<", "Rc<", "Arc<"].iter().any(|p| {
        line.match_indices(p)
            .any(|(i, m)| starts_with_word(skip_ws(&line[i + m.len()..]), "Node"))
    })
}

/// `(struct|enum)[[:space:]]+Node\b`: the declaration of a `Node` type.
pub fn declares_node_type(line: &str) -> bool {
    ["struct", "enum"].iter().any(|kw| {
        line.match_indices(kw).any(|(i, m)| {
            let r = &line[i + m.len()..];
            let t = skip_ws(r);
            t.len() < r.len() && starts_with_word(t, "Node")
        })
    })
}

/// `fn add_attributes_if_missing`: the arena method every attribute write must
/// go through.
pub fn declares_add_attributes(line: &str) -> bool {
    line.contains("fn add_attributes_if_missing")
}

/// `NodeData::Element[[:space:]]*\{[^}]*attrs`: destructuring an element's
/// attrs directly (the shortcut that bypasses snapshot recording).
pub fn is_attr_shortcut(line: &str) -> bool {
    line.match_indices("NodeData::Element").any(|(i, m)| {
        skip_ws(&line[i + m.len()..])
            .strip_prefix('{')
            .is_some_and(|r| r.split('}').next().unwrap_or("").contains("attrs"))
    })
}

/// `grep -vE ':[0-9]+:[[:space:]]*//'` applied to a record: true if the pattern
/// occurs anywhere in it (so it drops `path:N:  // x`, and also would drop a
/// record that merely contains such a shape).
pub fn has_comment_record_shape(rec: &str) -> bool {
    let b = rec.as_bytes();
    (0..b.len()).any(|i| {
        if b[i] != b':' {
            return false;
        }
        let digits = b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || b.get(i + 1 + digits) != Some(&b':') {
            return false;
        }
        rec[i + 2 + digits..]
            .trim_start_matches(is_space)
            .starts_with("//")
    })
}

/// The `scan_code` of the script: non-comment lines of `crates/px-dom/src`.
fn scan_code(root: &Path, hit: &dyn Fn(&str) -> bool) -> Vec<String> {
    scan::drop_comments(scan::grep_rn(root, DOM_SRC, |_| true, hit))
}

fn detail_head(g: &Gate, found: &[String], n: usize) {
    for l in found.iter().take(n) {
        g.detail(l);
    }
}

/// Section 4.1: there is no infallible index API, not even a private one.
///
/// The rule the whole design rests on, and the one that erodes quietly.
/// Somebody writing a tree walk gets tired of `?` on every hop and adds a small
/// private helper that unwraps -- and every caller after that is one stale
/// handle away from DOM-level type confusion, in safe Rust, with no unsafe
/// block to review. The tests can show that today's accessors return Option.
/// Only a source check can say that no private one appeared.
///
/// This check must not be able to pass by finding nothing. px-dom starts as a
/// stub, and "no bad accessor exists" is trivially true of a crate with no
/// accessors at all -- a green line meaning the phase has not started. So it
/// requires the good shape to be present before it reports on the bad one.
fn check_accessors(g: &mut Gate, root: &Path) {
    g.info("no infallible node accessor");
    if !root.join(DOM_SRC).is_dir() {
        g.fail("crates/px-dom/src does not exist");
        return;
    }
    if scan_code(root, &is_option_accessor).is_empty() {
        g.fail("px-dom declares no Option-returning node accessor; this check");
        g.fail("  cannot pass by finding nothing");
        return;
    }
    let bare = scan_code(root, &is_bare_accessor);
    if !bare.is_empty() {
        g.fail("px-dom has an accessor returning Node rather than Option<Node>");
        detail_head(g, &bare, 5);
    } else {
        g.ok("every node accessor returns Option");
    }
}

/// Tree walks are iterative, including the one the compiler writes. Same rule
/// as above: a crate with no `Node` type trivially has no node owning another,
/// so the check requires a `Node` declaration first.
fn check_owned_children(g: &mut Gate, root: &Path) {
    g.info("no owned child nodes inside a node");
    if scan_code(root, &declares_node_type).is_empty() {
        g.fail("px-dom declares no Node type; this check cannot pass by finding nothing");
        return;
    }
    let owned = scan_code(root, &is_owned_child);
    if !owned.is_empty() {
        g.fail("a node owns child nodes directly; dropping the tree will recurse");
        detail_head(g, &owned, 5);
    } else {
        g.ok("nodes are held in the arena, not inside each other");
    }
}

/// Attribute writes go through the arena, so stylo's snapshots get recorded.
///
/// `docs/research/stylo-requirements.md` item 6: stylo's invalidation needs
/// prior-state records captured at mutation time, and it is a mutation-path
/// feature -- every attribute setter has to record one. Checked in the source
/// rather than by a test, because a test cannot see it: a sink that reaches
/// into `NodeData::Element { attrs }` and pushes directly produces exactly the
/// right tree, passes the whole conformance corpus, and is wrong only in that
/// nothing recorded what the attribute used to be -- which nothing observes
/// until Phase 5 turns recording on, months later. A first attempt at testing
/// it called the arena method directly, so the sink could be rewritten to
/// bypass the arena and the test still passed.
fn check_attr_writes(g: &mut Gate, root: &Path) {
    g.info("attribute writes go through the arena");
    if scan_code(root, &declares_add_attributes).is_empty() {
        g.fail("px-dom has no Arena::add_attributes_if_missing; this check cannot");
        g.fail("  pass by finding nothing");
        return;
    }
    // `grep -rnE ... sink.rs` on one file: records are `N:content`.
    let records: Vec<String> = std::fs::read(root.join(SINK))
        .map(|b| {
            String::from_utf8_lossy(&b)
                .split('\n')
                .enumerate()
                .filter(|(_, l)| is_attr_shortcut(l))
                .map(|(n, l)| format!("{}:{l}", n + 1))
                .collect()
        })
        .unwrap_or_default();
    let kept: Vec<&String> = records
        .iter()
        .filter(|r| !has_comment_record_shape(r))
        .collect();
    if !kept.is_empty() {
        g.fail("px-dom's sink destructures an element's attrs directly; attribute");
        g.fail("  writes must go through Arena so snapshots are recorded");
        for l in records.iter().take(3) {
            g.detail(l);
        }
    } else {
        g.ok("the sink writes attributes through the arena");
    }
}

/// html5lib-tests: the conformance corpus section 9 sets at 99%. Vendored
/// rather than fetched: a gate that needs the internet goes red for reasons
/// nobody here controls.
fn check_corpus(g: &mut Gate, root: &Path) {
    let dir = root.join(HTML5LIB);
    if !dir.is_dir() {
        g.fail(format!(
            "{HTML5LIB} does not exist; the conformance corpus is not vendored"
        ));
        return;
    }
    let count = scan::walk_entries(&dir)
        .iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".dat"))
        })
        .count();
    if count == 0 {
        g.fail(format!("{HTML5LIB} holds no .dat files"));
    } else {
        g.ok(format!("the conformance corpus holds {count} files"));
    }
}

/// Keep lines of the conformance run that report a number:
/// `grep -E "^(html5lib |  set aside:)"`.
pub fn conformance_lines(output: &str) -> Vec<&str> {
    output
        .lines()
        .filter(|l| l.starts_with("html5lib ") || l.starts_with("  set aside:"))
        .collect()
}

/// The percentage itself, printed rather than merely asserted.
///
/// ADR 019: the graded figure sets two causes aside -- tests needing a
/// JavaScript engine, and the whatwg/html#12118 processing-instruction change
/// that html5ever 0.39 predates -- while the unadjusted figure is measured on
/// every run and held to a floor. Both numbers reach the log here, because
/// "Phase 4 passed" and "94.62%" have to be reconcilable by somebody reading
/// the output rather than only by somebody who finds the ADR.
///
/// One thread, so the two suites' output does not interleave into a line that
/// reads as neither number. As in the script (`set -o pipefail`), a failing
/// test run is also "did not report a number".
fn check_conformance(g: &mut Gate, root: &Path) {
    g.info("html5lib conformance (see docs/adr/019-conformance-gate-exclusions.md)");
    let mut cmd = proc::cargo_command(root);
    cmd.args([
        "test",
        "-p",
        "px-dom",
        "--locked",
        "--test",
        "html5lib",
        "--",
        "--nocapture",
        "--test-threads=1",
    ]);
    let reported = match proc::capture_merged_cmd(cmd) {
        Ok((out, success)) => {
            let lines = conformance_lines(&out);
            for l in &lines {
                println!("{l}");
            }
            success && !lines.is_empty()
        }
        Err(_) => false,
    };
    if reported {
        g.ok("conformance reported above");
    } else {
        g.fail("the conformance suite did not report a number");
    }
}

/// Section 14.3 asks Phase 4 to measure two NodeId layouts and record the
/// choice: 32/32 index/generation at 8 bytes, against 24/8 with slot
/// retirement at 4. A handle is in every node, every range, every style
/// computation, so the difference is real at scale -- and the decision is only
/// meaningful if the measurement was actually taken.
fn node_id_choice_recorded(root: &Path) -> bool {
    let adr = root.join("docs/adr");
    let named = std::fs::read_dir(&adr).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            !n.starts_with('.') && n.contains("node-id")
        })
    });
    named
        || scan::walk_files(&adr)
            .iter()
            .any(|f| std::fs::read(f).is_ok_and(|b| String::from_utf8_lossy(&b).contains("24/8")))
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }

    suites::check_suites(g, root, SUITES);
    suites::run_workspace_tests(g, root);

    // The fuzz targets' bodies, as ordinary tests. `px_dom::harness` holds what
    // `fuzz_targets/dom_stale_handle.rs` and `dom_mutation.rs` run, and it is
    // behind the `testing` feature, so the workspace run above does not execute
    // a line of it. Without this step the two targets are checked by nothing
    // until the next campaign -- and Phase 1 already shipped a 24-hour campaign
    // that reported clean while never reaching the code it was built for. Cheap
    // enough to be unconditional: about a tenth of a second for twelve hundred
    // operation sequences.
    g.info("cargo test -p px-dom --features testing (the fuzz bodies)");
    match proc::cargo(
        root,
        &["test", "-p", "px-dom", "--features", "testing", "--locked"],
    ) {
        Ok(s) if s.success() => {}
        _ => g.fail("the fuzz target bodies failed as tests"),
    }

    check_accessors(g, root);
    check_owned_children(g, root);
    check_attr_writes(g, root);
    check_corpus(g, root);
    check_conformance(g, root);

    // 24h mutation fuzz, and the stale-handle target section 4.1 names
    // explicitly.
    for target in FUZZ_TARGETS {
        if !root
            .join(format!("fuzz/fuzz_targets/{target}.rs"))
            .is_file()
        {
            g.fail(format!(
                "fuzz target {target} does not exist; §9 Phase 4 requires it"
            ));
        } else {
            g.ok(format!("fuzz target {target} exists"));
        }
    }

    if node_id_choice_recorded(root) {
        g.ok("the NodeId layout choice is recorded in an ADR");
    } else {
        g.fail("§14.3 asks Phase 4 to measure both NodeId layouts and record the choice");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn option_accessor_needs_the_arrow() {
        assert!(is_option_accessor(
            "pub fn get(&self, id: NodeId) -> Option<&Node> {"
        ));
        assert!(is_option_accessor("fn m(&mut self) -> Option<&mut Node> {"));
        assert!(is_option_accessor("fn m() ->Option< Node> {"));
        assert!(is_option_accessor("fn m() -> Option<& mut Node>"));
        // The slot field is not an accessor.
        assert!(!is_option_accessor("    node: Option<Node>,"));
        assert!(!is_option_accessor("fn m() -> Option<NodeId>"));
        assert!(!is_option_accessor("fn m() -> Option<&Nodes>"));
        assert!(!is_option_accessor("fn m() -> Option<mutNode>"));
    }

    #[test]
    fn bare_accessor_shapes() {
        assert!(is_bare_accessor("fn node(&self, id: NodeId) -> &Node {"));
        assert!(is_bare_accessor("    pub fn node(&self) -> Node {"));
        assert!(is_bare_accessor(
            "pub(crate) fn n(&mut self) -> &mut Node {"
        ));
        assert!(is_bare_accessor("pub(in crate::a) fn n() -> & mut Node"));
        assert!(is_bare_accessor("fn n( ) ->&Node"));
        assert!(!is_bare_accessor("fn n(&self) -> Option<&Node> {"));
        assert!(!is_bare_accessor("fn n(&self) -> NodeId {"));
        assert!(!is_bare_accessor("fn n(&self) -> &Nodes {"));
        // `[^)]*` stops at the first `)`: a nested call in the args escapes it.
        assert!(!is_bare_accessor("fn n(f: impl Fn()) -> &Node {"));
        assert!(!is_bare_accessor("pubfn n() -> Node"));
        assert!(!is_bare_accessor("let x = fn_x() -> Node"));
        assert!(!is_bare_accessor("async fn n() -> Node"));
    }

    #[test]
    fn owned_child_shapes() {
        assert!(is_owned_child("children: Vec<Node>,"));
        assert!(is_owned_child("next: Option<Box< Node>>"));
        assert!(is_owned_child("Rc<Node>"));
        assert!(is_owned_child("x: Arc<\tNode>"));
        assert!(!is_owned_child("Vec<NodeId>"));
        assert!(!is_owned_child("Vec<NodeData>"));
        assert!(!is_owned_child("Box<dyn Node2>"));
    }

    #[test]
    fn node_type_declaration() {
        assert!(declares_node_type("pub struct Node {"));
        assert!(declares_node_type("enum Node"));
        assert!(!declares_node_type("struct NodeId(u32);"));
        assert!(!declares_node_type("struct Nodes"));
        assert!(!declares_node_type("struct\n"));
    }

    #[test]
    fn attr_shortcut_shape() {
        assert!(is_attr_shortcut("NodeData::Element { attrs, .. } => {"));
        assert!(is_attr_shortcut(
            "if let NodeData::Element{name, attrs} = x"
        ));
        assert!(!is_attr_shortcut("NodeData::Element { name } => attrs"));
        assert!(!is_attr_shortcut("NodeData::Text { attrs }"));
    }

    #[test]
    fn comment_record_shape() {
        assert!(has_comment_record_shape("p.rs:12:   // x"));
        assert!(!has_comment_record_shape("12:    // x"));
        assert!(!has_comment_record_shape("p.rs:12:let x = 1; // x"));
        assert!(has_comment_record_shape("12:5: //"));
    }

    #[test]
    fn conformance_line_filter() {
        let out = "running 2 tests\nhtml5lib passed 9\n  set aside: 3\nnoise html5lib x\n";
        assert_eq!(
            conformance_lines(out),
            ["html5lib passed 9", "  set aside: 3"]
        );
        assert!(conformance_lines("nothing\n").is_empty());
    }

    const GOOD_SRC: &str = "pub struct Node { x: u8 }\n\
        impl Arena {\n    pub fn get(&self, id: NodeId) -> Option<&Node> { None }\n    \
        pub fn add_attributes_if_missing(&mut self) {}\n}\n";

    fn fixture(tag: &str, src: &str) -> Fixture {
        let fx = Fixture::new(tag);
        fx.write("crates/px-dom/src/lib.rs", src);
        fx.write("crates/px-dom/src/sink.rs", "fn s() {}\n");
        fx
    }

    fn gate_of(f: fn(&mut Gate, &Path), fx: &Fixture) -> Gate {
        let mut g = Gate::new("dom");
        f(&mut g, fx.path());
        g
    }

    #[test]
    fn accessors_pass_fail_and_cannot_pass_vacuously() {
        let fx = fixture("dom-acc-ok", GOOD_SRC);
        assert!(gate_of(check_accessors, &fx).passed());

        let bad = format!("{GOOD_SRC}fn helper(&self) -> &Node {{ unwrap() }}\n");
        let g = gate_of(check_accessors, &fixture("dom-acc-bad", &bad));
        assert_eq!(
            g.failures(),
            ["px-dom has an accessor returning Node rather than Option<Node>"]
        );

        // Only a comment mentions the good shape: still vacuous.
        let vac = "// pub fn get() -> Option<&Node>\n";
        let g = gate_of(check_accessors, &fixture("dom-acc-vac", vac));
        assert_eq!(g.fail_count(), 2);

        let none = Fixture::new("dom-acc-none");
        none.write("README.md", "x");
        assert_eq!(
            gate_of(check_accessors, &none).failures(),
            ["crates/px-dom/src does not exist"]
        );
    }

    #[test]
    fn owned_children_pass_and_fail() {
        let fx = fixture("dom-own-ok", GOOD_SRC);
        assert!(gate_of(check_owned_children, &fx).passed());
        // The rule written down in a comment does not trip itself.
        let doc = format!("{GOOD_SRC}// never Vec<Node> inside a Node\n");
        assert!(gate_of(check_owned_children, &fixture("dom-own-doc", &doc)).passed());
        let bad = "pub struct Node { kids: Vec<Node> }\n";
        let g = gate_of(check_owned_children, &fixture("dom-own-bad", bad));
        assert_eq!(
            g.failures(),
            ["a node owns child nodes directly; dropping the tree will recurse"]
        );
        let none = fixture("dom-own-none", "fn x() {}\n");
        assert_eq!(gate_of(check_owned_children, &none).fail_count(), 1);
    }

    #[test]
    fn attr_writes_pass_and_fail() {
        let fx = fixture("dom-attr-ok", GOOD_SRC);
        assert!(gate_of(check_attr_writes, &fx).passed());
        let fx = fixture("dom-attr-bad", GOOD_SRC);
        fx.write(
            "crates/px-dom/src/sink.rs",
            "match d { NodeData::Element { attrs, .. } => attrs.push(a) }\n",
        );
        assert_eq!(gate_of(check_attr_writes, &fx).fail_count(), 2);
        let none = fixture("dom-attr-none", "fn x() {}\n");
        assert_eq!(gate_of(check_attr_writes, &none).fail_count(), 2);
    }

    #[test]
    fn corpus_counts_dat_files() {
        let fx = Fixture::new("dom-corpus");
        assert_eq!(gate_of(check_corpus, &fx).fail_count(), 1);
        fx.write("tests/html5lib/README.md", "x");
        assert_eq!(
            gate_of(check_corpus, &fx).failures(),
            ["tests/html5lib holds no .dat files"]
        );
        fx.write("tests/html5lib/sub/a.dat", "x");
        assert!(gate_of(check_corpus, &fx).passed());
    }

    #[test]
    fn node_id_choice_by_name_or_by_text() {
        let fx = Fixture::new("dom-adr");
        fx.write("docs/adr/001-x.md", "nothing");
        assert!(!node_id_choice_recorded(fx.path()));
        fx.write("docs/adr/018-node-id-layout.md", "x");
        assert!(node_id_choice_recorded(fx.path()));
        let fx = Fixture::new("dom-adr2");
        fx.write("docs/adr/sub/002-y.md", "chose 24/8 here");
        assert!(node_id_choice_recorded(fx.path()));
    }
}
