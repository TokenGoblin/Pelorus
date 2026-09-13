//! The vendored CSS2 subset is readable, parseable, and paired.
//!
//! Not a gate item, and named so it cannot be mistaken for one: `ci/gate-layout.sh`
//! matches the reftest item on `layout_reftest_`, and nothing here starts with
//! that. A corpus that parses proves nothing about layout, and a gate item
//! satisfied by a corpus check would be the same mistake as a filter that matched
//! another crate's tests.
//!
//! What it does prove is that ADR 029's subset selection was sound — that the
//! `.html` files px-dom can actually parse are the ones that got vendored, and
//! that every test has the reference it claims. Both are assumptions the reftest
//! harness will be built on, and both are cheap to check now rather than to
//! discover as a confusing failure later.

use std::path::{Path, PathBuf};

/// Every `.html` file in the vendored subset.
fn corpus_files() -> Vec<PathBuf> {
    // From the crate directory up to the workspace root.
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/wpt/css2");
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "html") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn corpus_is_vendored_and_paired() {
    let files = corpus_files();
    assert!(
        files.len() >= 80,
        "the vendored subset has {} files; ci/gate-layout.sh pins the pair count \
         and this pins the file count, so both have to move deliberately",
        files.len()
    );

    let mut unpaired = Vec::new();
    for path in &files {
        let name = path.to_string_lossy().replace('\\', "/");
        if name.ends_with("-ref.html") {
            // Every reference must have a test.
            let test = name.replace("-ref.html", ".html");
            if !Path::new(&test).exists() {
                unpaired.push(format!("{name} has no test"));
            }
        } else {
            // Every test must have the reference it names.
            let reference = name.replace(".html", "-ref.html");
            if !Path::new(&reference).exists() {
                unpaired.push(format!("{name} has no reference"));
            }
        }
    }
    assert!(
        unpaired.is_empty(),
        "the vendoring script pairs tests with references; these are unpaired:\n  {}",
        unpaired.join("\n  ")
    );
}

/// Every file in the subset parses, and none is abandoned.
///
/// `px_dom::parse` sets `abandoned` when the input is pathological enough that it
/// stopped early. A corpus file that triggers it would silently contribute an
/// empty box tree to the reftest comparison — and an empty tree compared against
/// an empty tree *matches*, which is a false pass in the one place this phase
/// cannot afford one.
#[test]
fn corpus_every_file_parses_without_being_abandoned() {
    let mut bad = Vec::new();

    for path in corpus_files() {
        let Ok(source) = std::fs::read_to_string(&path) else {
            bad.push(format!("{} is not valid UTF-8", path.display()));
            continue;
        };
        let dom = px_dom::parse(&source);
        if dom.abandoned {
            bad.push(format!("{} was abandoned by the parser", path.display()));
            continue;
        }
        // A document with no element at all would compare equal to any other
        // empty document. Cheap to rule out here.
        let has_element = core::iter::once(dom.arena.document())
            .chain(dom.arena.descendants(dom.arena.document()))
            .any(|id| {
                dom.arena
                    .get(id)
                    .is_some_and(|n| n.element_name().is_some())
            });
        if !has_element {
            bad.push(format!("{} parsed to no elements at all", path.display()));
        }
    }

    assert!(
        bad.is_empty(),
        "ADR 029 selected .html files on the grounds that px-dom can parse them. \
         These it cannot:\n  {}",
        bad.join("\n  ")
    );
}
