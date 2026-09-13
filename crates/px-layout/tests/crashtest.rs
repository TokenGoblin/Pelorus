//! The WPT CSS2 crashtests: inputs that made a shipping browser crash.
//!
//! A crashtest carries no reference and is not compared against one. It passes if
//! the engine does not crash on it. That is a weaker assertion than a reftest's and
//! a much cheaper one — no oracle, no threshold, no reference pairing, and no
//! argument about what "at threshold" means.
//!
//! # Why these were missing for the whole of Phase 6
//!
//! `ci/vendor-css2-reftests.sh` pairs a test with a `-ref.html` of the same name.
//! A crashtest has none, so all 28 in these seven directories were silently
//! skipped, and nobody looked until Phase 7's corpus was being surveyed and 14
//! turned up in `css-flexbox` and `css-grid`. They are the inputs Chrome and
//! Firefox actually crashed on, for exactly the features Phase 6 implements, which
//! makes them a better-chosen adversarial corpus than anything written by hand.
//!
//! # What "does not crash" means when there is no unsafe code
//!
//! `px-layout` is `#![forbid(unsafe_code)]`, so it cannot corrupt memory. Three
//! things are left, and this file checks all three:
//!
//! - **A panic.** Arithmetic overflow in debug, an index out of bounds, an
//!   `unwrap` on `None`. Caught by the thread's `join` returning `Err`.
//! - **A stack overflow.** Every walk in the crate is supposed to be iterative;
//!   `depth.rs` proves that for one hand-written deep-nesting case. These run on
//!   the same deliberately small stack, so a recursive walk reached by a real-world
//!   document fails here too.
//! - **Non-termination.** The float placement search and the line-breaking loop
//!   both step through positions until a condition holds, and both have an
//!   argument for why they terminate. An argument is not a test. A worker thread
//!   that stops responding cannot be killed from Rust, so a timeout here reports
//!   the file and ends the process rather than hanging until CI's own deadline.
//!
//! # Not a conformance measure
//!
//! Passing every crashtest says nothing about whether the layout is *right*.
//! `MATCH_FLOOR` in `reftest.rs` is the conformance number; this is a robustness
//! floor underneath it.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use px_layout::block::layout_document;
use px_layout::geom::px;

/// The viewport every crashtest is laid out against.
const VIEWPORT_PX: i32 = 800;

/// The stack each file gets.
///
/// The same 256 KB `depth.rs` uses, and for the same reason: a recursive walk
/// survives 100,000 levels on a main thread on some platforms and not others,
/// which would make this pass on Linux and fail on Windows. Constraining the stack
/// makes the property independent of the platform's default.
const STACK: usize = 256 * 1024;

/// How long one file may take.
///
/// Generous — the largest of these lays out in milliseconds — because the failure
/// this catches is a loop that never ends rather than one that is slow. A budget
/// tight enough to catch slowness would be a flaky test on a loaded CI runner.
const BUDGET: Duration = Duration::from_secs(20);

/// How many crashtests must be present.
///
/// ADR 019's pattern: a floor on the corpus so that "every crashtest passes"
/// cannot be achieved by having none. 28 were vendored; the floor is lower so a
/// WPT re-vendor that drops one is not a gate failure, and low enough that
/// emptying the directory is.
const CRASHTEST_FLOOR: usize = 20;

/// Enough of a user-agent stylesheet that these documents generate boxes.
///
/// Deliberately the same shape as `reftest.rs`'s rather than a minimal one: a
/// crashtest that lays out nothing cannot crash, so the point is to reach as much
/// of the engine as the real corpus does. The duplication is real and is the price
/// of Cargo's one-binary-per-test-file model; the two are independent on purpose,
/// because a crashtest run is not supposed to fail when the reftest sheet changes.
const UA_STYLESHEET: &str = "html, body, div, p, section, article, header, footer, \
     nav, aside, main, figure, blockquote, h1, h2, h3, h4, h5, h6, ul, ol, li, \
     table, form, fieldset, pre, hr, span, a, b, i, em, strong { display: block } \
     body { margin: 8px } \
     p, blockquote, figure { margin: 1em 0 } \
     head, style, script, title, meta, link { display: none }";

/// Every vendored crashtest, in a stable order.
fn crashtests() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/wpt/css2-crashtests");
    let mut out = Vec::new();
    collect(&root, &mut out);
    out.sort();
    out
}

/// Walk `dir` for `.html` files. Iterative, like everything else here.
fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "html") {
                out.push(path);
            }
        }
    }
}

/// Parse, style and lay out one file. Returns the fragment count.
///
/// Every step is fallible and none of the failures are crashes: a file that does
/// not parse, or has no root element, is not a crash and not this file's business.
fn lay_out(source: &str) -> usize {
    let dom = px_dom::parse(source);
    if dom.abandoned {
        return 0;
    }
    let quirks = px_css::engine::quirks_mode_of(&dom);
    let arena = dom.arena;

    let mut engine = px_css::engine::StyleEngine::new(VIEWPORT_PX as f32, 600.0, quirks);
    engine.add_author_stylesheet(UA_STYLESHEET, "about:ua");
    for css in inline_stylesheets(&arena) {
        engine.add_author_stylesheet(&css, "https://wpt.invalid/test.css");
    }

    let style_root = engine.style_root_for(&arena);
    if engine.resolve(&arena, &style_root).is_none() {
        return 0;
    }
    layout_document(&arena, &style_root, px(VIEWPORT_PX)).map_or(0, |tree| tree.len())
}

/// The text of every `<style>` element, in document order.
fn inline_stylesheets(arena: &px_dom::Arena) -> Vec<String> {
    let document = arena.document();
    let mut out = Vec::new();
    for id in core::iter::once(document).chain(arena.descendants(document)) {
        let Some(node) = arena.get(id) else { continue };
        if !node
            .element_name()
            .is_some_and(|q| q.local == html5ever::local_name!("style"))
        {
            continue;
        }
        out.push(px_layout::inline::collect_text(arena, id));
    }
    out
}

/// The corpus is present. Pinned, so "all crashtests pass" cannot mean "there are
/// none".
#[test]
fn layout_crashtest_the_corpus_is_present() {
    let files = crashtests();
    assert!(
        files.len() >= CRASHTEST_FLOOR,
        "only {} crashtests vendored, floor is {CRASHTEST_FLOOR}. \
         Re-run ci/vendor-css2-reftests.sh",
        files.len()
    );
}

/// No crashtest panics, overflows its stack, or fails to terminate.
///
/// One thread per file rather than one for all of them, so a failure names the
/// file. The count of files that laid out something is printed, because a run
/// where every file produced zero fragments would pass this test and mean nothing
/// — and that is a real possibility, since half of these documents drive their
/// crash through script this engine does not run.
#[test]
fn layout_crashtest_nothing_crashes() {
    let files = crashtests();
    let mut laid_out = 0usize;
    let mut empty = 0usize;

    for path in &files {
        let Ok(source) = std::fs::read_to_string(path) else {
            continue;
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("?")
            .to_owned();

        let (send, recv) = mpsc::channel();
        let worker = std::thread::Builder::new()
            .name(name.clone())
            .stack_size(STACK)
            .spawn(move || {
                let fragments = lay_out(&source);
                // A send failure means the receiver timed out and gave up. There
                // is nothing to report to and nothing to clean up.
                let _ = send.send(fragments);
            })
            .expect("the worker thread spawns");

        match recv.recv_timeout(BUDGET) {
            Ok(fragments) => {
                if fragments > 0 {
                    laid_out += 1;
                } else {
                    empty += 1;
                }
                // Joined only after a result arrives, so a panic is reported as a
                // panic rather than as a timeout.
                worker
                    .join()
                    .unwrap_or_else(|_| panic!("{name} panicked during layout"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // The thread is still running and cannot be stopped. Report and
                // end the process: hanging until CI's deadline would lose the
                // filename, which is the only useful thing here.
                eprintln!("CRASHTEST HUNG: {name} did not finish in {BUDGET:?}");
                eprintln!(
                    "  layout failed to terminate. This is a real defect, \
                           not a slow machine -- the budget is 20s and these files \
                           lay out in milliseconds."
                );
                std::process::exit(1);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The sender was dropped without sending: the thread unwound.
                worker
                    .join()
                    .unwrap_or_else(|_| panic!("{name} panicked during layout"));
                panic!("{name} ended without a result and without panicking");
            }
        }
    }

    println!(
        "css2 crashtests: {} files, none crashed; {laid_out} produced boxes, \
         {empty} produced none",
        files.len()
    );
    assert!(
        laid_out > 0,
        "no crashtest produced a single box, so this test exercised nothing"
    );
}
