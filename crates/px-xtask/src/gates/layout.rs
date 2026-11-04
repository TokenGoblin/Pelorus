//! `cargo xtask layout` -- the Phase 6 gate (build-spec section 9). Port of
//! `ci/gate-layout.sh`.
//!
//! ```text
//! WPT CSS2 reftest subset at threshold          -> here
//! identical box tree on repeat runs             -> here
//! iterative (non-recursive) tree walks, by a
//!   deep-nesting test                           -> here
//! ```
//!
//! Written failing, before px-layout had a box in it. The phase is done when
//! this passes in CI on both operating systems -- not when the build is green,
//! and not when it passes on the machine that wrote it.
//!
//! # The three items are three different kinds of claim
//!
//! **The reftest item is a conformance number**, and like every conformance
//! number in this project it is gradeable against a corpus this project
//! chooses. So the subset is committed, its size is pinned, and the gate prints
//! the graded and ungraded figures the way ADR 019 made Phase 4 do. A subset
//! that shrinks to fit an implementation that is struggling is the failure
//! mode, and pinning the count is what stops it.
//!
//! **The determinism item is not about correctness at all.** Two runs of the
//! same input must produce the same box tree, which is a claim about the
//! absence of iteration-order and hash-order dependence. It is cheap to check
//! and almost impossible to debug later: a layout that differs by a pixel every
//! hundredth run is a bug report nobody can reproduce. px-layout's CLAUDE.md
//! already says "non-determinism here is a bug even when the pixels match".
//!
//! **The nesting item is a structural claim about the whole crate**, and Phase 4
//! learned what that means. A hundred thousand nested boxes overflows the stack
//! of any recursive walk, and a layout engine has walks everywhere: box-tree
//! construction, block layout, inline layout, float placement, `Drop`. Passing
//! it once by fixing one function is not passing it. So this gate checks the
//! *shape* of the code as well as its behaviour, exactly as the dom gate does --
//! a test can show today's walks are iterative, not that tomorrow's will be.
//!
//! # Why there is no source scan for recursive ownership here
//!
//! There were two, and both fired on correct code.
//!
//! The first counted how often a function's name appeared in its own file, and
//! reported `geom::px` and `geom::au` as recursive because tests in the same
//! file call them. The second forbade `Vec<Fragment>` -- and `FragmentTree`
//! holds exactly that, because a flat arena of fragments *is* the design the
//! check was meant to encourage. A grep cannot tell an arena holding fragments
//! from a fragment holding fragments.
//!
//! The property is real and worth enforcing; a scan is the wrong instrument. The
//! right one is the compiler: `Fragment` is `Copy`, and a type that owns a `Vec`
//! cannot be. So a fragment that owned its children would not compile, which is
//! a stronger guarantee than any scan and needs no exception list.
//!
//! `layout_depth_fragments_are_copy_so_drop_cannot_recurse` is where that is
//! asserted, in gate item 3's own test file, so removing the guarantee fails the
//! gate item it belongs to rather than a scan somebody could silence.
//!
//! Recorded at length because three checks in the original script were wrong in
//! the same direction, and the lesson is not "write better greps". It is that a
//! structural claim wants a structural mechanism, and `Copy` was available the
//! whole time.

use super::suites::{self, Suite};
use crate::Gate;
use crate::git;
use crate::proc;
use crate::scan::{self, is_comment_line, is_space, is_word};
use std::path::Path;

/// The three items as named test suites, plus a robustness floor: `(item,
/// filter, file that must carry the tests)`.
///
/// Every filter carries a `layout_` prefix, and that is not decoration. The
/// first version used `depth_`, which also matches px-dom's `dom_depth_*` tests
/// -- so the gate reported the deep-nesting item as having tests before
/// px-layout had a file. A filter that matches another crate's tests is a gate
/// item satisfied by somebody else's work.
///
/// `css2-crashtests` is not one of section 9's three items. It is a robustness
/// floor underneath the third of them: "iterative tree walks verified by a
/// deep-nesting test" is one hand-written document, and these are 28 real ones
/// that made Chrome or Firefox crash, in the directories this phase implements.
/// They were invisible for the whole of Phase 6 because the vendoring script
/// pairs a test with a `-ref.html` and a crashtest has none. Listed here so they
/// cannot quietly stop running, not so the item count reads higher.
pub const SUITES: &[Suite] = &[
    (
        "css2-reftests",
        "layout_reftest_",
        "crates/px-layout/tests/reftest.rs",
    ),
    (
        "box-tree-determinism",
        "layout_determinism_",
        "crates/px-layout/tests/determinism.rs",
    ),
    (
        "deep-nesting",
        "layout_depth_",
        "crates/px-layout/tests/depth.rs",
    ),
    (
        "css2-crashtests",
        "layout_crashtest_",
        "crates/px-layout/tests/crashtest.rs",
    ),
];

/// The reftest corpus is committed and cannot quietly shrink. ADR 019's
/// pattern, third outing: a conformance threshold is only a threshold if the
/// denominator is fixed; otherwise "95% of the subset" is satisfiable by
/// deleting the tests that fail.
pub const REFTEST_DIR: &str = "tests/wpt/css2";
pub const REFTEST_FLOOR: usize = 40;

/// The crashtest corpus, pinned for the same reason and read from the index for
/// the same reason: Phase 4 lost five commits to a fuzz corpus that was an empty
/// directory git could not store and `git status` did not report. A crashtest
/// suite that passes because the files are gone is the identical failure.
pub const CRASH_DIR: &str = "tests/wpt/css2-crashtests";
pub const CRASH_FLOOR: usize = 20;

pub const LAYOUT_SRC: &str = "crates/px-layout/src";

fn is_op(c: char) -> bool {
    matches!(c, '-' | '+' | '*' | '/')
}

/// Reference files among `paths`: reftest pairs are counted by their `-ref`.
pub fn count_ref_files(paths: &[String]) -> usize {
    paths
        .iter()
        .filter(|p| p.ends_with("-ref.xht") || p.ends_with("-ref.html"))
        .count()
}

pub fn count_html_files(paths: &[String]) -> usize {
    paths.iter().filter(|p| p.ends_with(".html")).count()
}

/// `f32` / `f64` as a whole word. `f32` in a layout crate is the specific thing
/// §4.2 exists to prevent -- it makes "identical box tree on repeat runs"
/// depend on the order additions happened to be performed in.
pub fn names_float(line: &str) -> bool {
    ["f32", "f64"].iter().any(|w| {
        line.match_indices(w).any(|(i, _)| {
            let before = line[..i].chars().next_back();
            let after = line[i + 3..].chars().next();
            before.is_none_or(|c| !is_word(c)) && after.is_none_or(|c| !is_word(c))
        })
    })
}

/// Constructs `Au` through its tuple field: `Au(` not preceded by an identifier
/// character, on a line that does not contain `Au(0)`.
///
/// `app_units::Au` is `pub struct Au(pub i32)`, and its saturating `Add` is only
/// sound while every value is inside +-(2^30 - 1): two such values sum to at
/// most 2^31 - 2, which is why the raw `i32` addition inside `Add` cannot
/// overflow. `Au(i32::MAX)` is constructible, costs nothing, reads fine, and
/// turns the next addition into the panic-or-wrap §4.2 forbids.
/// app_units says so itself: "It is safe to construct invalid Au values, but it
/// may lead to panics and overflows."
///
/// `Au::new`, `Au::from_px` and this crate's `geom::px` / `geom::au` all clamp.
/// `Au(0)` is exempt: it is unambiguously in range and a `const fn` cannot call
/// `Au::new`, which is what the zero constants need.
///
/// A test can show today's arithmetic saturates. Only a scan says no
/// out-of-range value was constructed next to it.
///
/// The preceding-character class is "not `[A-Za-z0-9_]`" and not "not
/// `[a-zA-Z_:]`". The first version excluded `:`, which excludes `::Au(` -- the
/// fully-qualified form, and the one most likely to be written. Probed with a
/// real `app_units::Au(i32::MAX)` and the scan reported ok, which is how it was
/// found. A check that cannot fail is worse than no check, because it reads as
/// a guarantee.
pub fn constructs_raw_au(line: &str) -> bool {
    if line.contains("Au(0)") {
        return false;
    }
    let b = line.as_bytes();
    line.match_indices("Au(").any(|(i, _)| {
        i == 0 || {
            let p = b[i - 1];
            !(p.is_ascii_alphanumeric() || p == b'_')
        }
    })
}

/// Reads `Au`'s inner field to do arithmetic: `.0`, optional whitespace, one of
/// `- + * /`. The one exempt shape is `self.0 <op> other.0` (the saturating
/// `Add` implementation itself); a line containing it is not reported.
///
/// The tuple-constructor scan stops an out-of-range `Au` being *built*. It does
/// not stop one being taken apart: `au(font_size.0 * 6 / 5)` multiplies a raw
/// `i32` and clamps the result, which is the panic-in-debug, wrap-in-release
/// behaviour §4.2 forbids -- and it was in this crate, reachable from
/// page CSS with a large `font-size`, until a review found it.
///
/// app_units gives checked `Mul<i32>` and `Div<i32>` on `Au` itself, so
/// arithmetic never needs the field. Reads that do not compute -- comparisons,
/// `Reverse`, formatting -- are the legitimate uses, and are allowed by
/// requiring an operator after the field access rather than banning `.0`
/// outright.
///
/// The first version filtered by keyword -- `Au`, `font_size`, `margin` and so
/// on -- and reported one site while three existed. A scan that is narrowed to
/// the names you happened to think of finds the bugs you already knew about.
pub fn does_field_math(line: &str) -> bool {
    let hit = line.match_indices(".0").any(|(i, m)| {
        line[i + m.len()..]
            .trim_start_matches(is_space)
            .chars()
            .next()
            .is_some_and(is_op)
    });
    if !hit {
        return false;
    }
    let exempt = line.match_indices("self.0").any(|(i, m)| {
        let rest = line[i + m.len()..].trim_start_matches(is_space);
        match rest.chars().next() {
            Some(c) if is_op(c) => rest[c.len_utf8()..]
                .trim_start_matches(is_space)
                .starts_with("other.0"),
            _ => false,
        }
    });
    !exempt
}

/// `path:line:content` for every non-comment line under `crates/px-layout/src`
/// that satisfies `hit`, in path order -- the shape `grep -rn` printed.
pub fn scan(root: &Path, hit: fn(&str) -> bool) -> Vec<String> {
    let files = scan::walk_files(&root.join(LAYOUT_SRC));
    let mut found = Vec::new();
    for f in files {
        let Ok(bytes) = std::fs::read(&f) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        let rel = f
            .strip_prefix(root)
            .unwrap_or(&f)
            .to_string_lossy()
            .replace('\\', "/");
        for (n, line) in text.split('\n').enumerate() {
            if hit(line) && !is_comment_line(line) {
                found.push(format!("{rel}:{}:{line}", n + 1));
            }
        }
    }
    found
}

/// Pinned corpus floors, read from the git index.
pub fn check_corpora(g: &mut Gate, root: &Path) {
    if !root.join(REFTEST_DIR).is_dir() {
        g.fail(format!(
            "the CSS2 reftest subset is not vendored at {REFTEST_DIR}"
        ));
        g.fail("  §9's phrase is 'WPT CSS2 reftest subset at threshold'; a subset that");
        g.fail("  is not committed is a subset that changes between runs");
    } else {
        let files = git::ls_files(root, &[], &[REFTEST_DIR]).unwrap_or_default();
        let pairs = count_ref_files(&files);
        if pairs < REFTEST_FLOOR {
            g.fail(format!(
                "the subset has {pairs} reference files, floor is {REFTEST_FLOOR}"
            ));
        } else {
            g.ok(format!(
                "the CSS2 subset has {pairs} test/reference pairs in the index (floor {REFTEST_FLOOR})"
            ));
        }
    }
    if !root.join(CRASH_DIR).is_dir() {
        g.fail(format!(
            "the CSS2 crashtests are not vendored at {CRASH_DIR}"
        ));
        g.fail("  re-run the CSS2 vendoring tool");
    } else {
        let files = git::ls_files(root, &[], &[CRASH_DIR]).unwrap_or_default();
        let n = count_html_files(&files);
        if n < CRASH_FLOOR {
            g.fail(format!(
                "the crashtest corpus has {n} files, floor is {CRASH_FLOOR}"
            ));
        } else {
            g.ok(format!(
                "the CSS2 crashtests have {n} files in the index (floor {CRASH_FLOOR})"
            ));
        }
    }
}

fn report_scan(
    g: &mut Gate,
    root: &Path,
    hit: fn(&str) -> bool,
    ok: &str,
    fails: impl Fn(usize) -> Vec<String>,
) {
    let found = scan(root, hit);
    if found.is_empty() {
        g.ok(ok);
        return;
    }
    for m in fails(found.len()) {
        g.fail(m);
    }
    for l in found.iter().take(5) {
        g.detail(l);
    }
}

/// The source scans of `crates/px-layout/src`. Skipped if the directory is
/// absent, as the script did.
///
/// Au is the geometry type, and saturating is the arithmetic. Section 4.2:
/// "Layout uses Au -- app units, i32 at 1/60 px -- with explicit saturating
/// operations, so geometry overflow is defined and testable rather than a panic
/// or a wrap." Both halves are checkable in source and neither is checkable by a
/// test alone. A test can show that today's arithmetic saturates; only a scan
/// says that no `+` on a raw i32 crept in beside it.
pub fn check_sources(g: &mut Gate, root: &Path) {
    if !root.join(LAYOUT_SRC).is_dir() {
        return;
    }
    report_scan(g, root, names_float, "px-layout names no f32 or f64", |n| {
        vec![
            format!("px-layout names f32 or f64 in {n} place(s); geometry is Au (§4.2)."),
            "  Floating point makes repeat runs depend on the order operations".into(),
            "  happened in, which is the second gate item.".into(),
        ]
    });
    report_scan(
        g,
        root,
        constructs_raw_au,
        "px-layout constructs no Au through its tuple field",
        |n| {
            vec![
                format!("px-layout constructs Au(...) directly in {n} place(s)."),
                "  Use geom::px, geom::au, or Au::from_px -- all of which clamp.".into(),
                "  An out-of-range Au makes the next addition overflow, which is the".into(),
                "  panic-or-wrap §4.2 exists to prevent.".into(),
            ]
        },
    );
    report_scan(
        g,
        root,
        does_field_math,
        "px-layout does no arithmetic on Au's inner field",
        |n| {
            vec![
                format!("px-layout computes on a raw .0 field in {n} place(s)."),
                "  Au has checked Mul<i32> and Div<i32>; use those. Reaching into the".into(),
                "  field multiplies a raw i32, which panics in debug and wraps in".into(),
                "  release -- §4.2's whole subject, and reachable from page CSS.".into(),
            ]
        },
    );
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }
    suites::check_suites(g, root, SUITES);
    g.info("cargo test --workspace --locked");
    match proc::cargo(root, &["test", "--workspace", "--locked"]) {
        Ok(s) if s.success() => {}
        _ => g.fail("tests failed"),
    }
    check_corpora(g, root);
    check_sources(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn floors_count_the_right_suffixes() {
        let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let p = v(&[
            "a-ref.xht",
            "b-ref.html",
            "c.xht",
            "d-ref.htm",
            "e-ref.html.bak",
        ]);
        assert_eq!(count_ref_files(&p), 2);
        let h = v(&["a.html", "b.htm", "c.html.x", "d.html"]);
        assert_eq!(count_html_files(&h), 2);
    }

    #[test]
    fn float_scan_is_whole_word() {
        assert!(names_float("let x: f32 = 1.0;"));
        assert!(names_float("f64"));
        assert!(names_float("as f64)"));
        assert!(names_float("Vec<f32>"));
        assert!(!names_float("let f32x = 1;"));
        assert!(!names_float("my_f32"));
        assert!(!names_float("f320"));
        assert!(!names_float("Au::from_f32_px"));
        assert!(!names_float("f3"));
    }

    #[test]
    fn raw_au_scan() {
        assert!(constructs_raw_au("let a = Au(5);"));
        assert!(constructs_raw_au("Au(i32::MAX)"));
        assert!(constructs_raw_au("app_units::Au(i32::MAX)"));
        assert!(constructs_raw_au("(Au(1), 2)"));
        assert!(!constructs_raw_au("const ZERO: Au = Au(0);"));
        assert!(!constructs_raw_au("MyAu(5)"));
        assert!(!constructs_raw_au("Au_(5)"));
        assert!(!constructs_raw_au("Au::new(5)"));
        // The whole line is exempt if it mentions Au(0), as the script's was.
        assert!(!constructs_raw_au("Au(0) + Au(5)"));
    }

    #[test]
    fn field_math_scan() {
        assert!(does_field_math("au(font_size.0 * 6 / 5)"));
        assert!(does_field_math("x.0+1"));
        assert!(does_field_math("x.0\t- y"));
        assert!(does_field_math("let r = 1.0 / 3;"));
        assert!(!does_field_math("a.0 == b.0"));
        assert!(!does_field_math("a.0.cmp(&b.0)"));
        assert!(!does_field_math("Reverse(a.0)"));
        assert!(!does_field_math("a.0"));
        assert!(!does_field_math("a.0 "));
        assert!(!does_field_math("Au(self.0 + other.0)"));
        assert!(!does_field_math("self.0   *  other.0"));
        assert!(does_field_math("self.0 + 1"));
        assert!(does_field_math("self.0 + other.1"));
        // The exemption is per line, as the script's was.
        assert!(!does_field_math("self.0 + other.0 + y.0 * 2"));
    }

    fn sources(files: &[(&str, &str)]) -> Fixture {
        let fx = Fixture::new("layout-src");
        for (p, c) in files {
            fx.write(p, c);
        }
        fx
    }

    fn run_sources(fx: &Fixture) -> Gate {
        let mut g = Gate::new("layout");
        check_sources(&mut g, fx.path());
        g
    }

    #[test]
    fn clean_sources_pass_and_comments_are_excluded() {
        let fx = sources(&[
            (
                "crates/px-layout/src/a.rs",
                "// f32, Au(5), x.0 * 2 in a comment\n    /// f64 here too\nconst Z: Au = Au(0);\n",
            ),
            ("crates/px-layout/src/sub/b.rs", "fn f() {}\n"),
        ]);
        let g = run_sources(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn each_scan_fails_with_its_own_message() {
        let fx = sources(&[
            ("crates/px-layout/src/a.rs", "let x: f32 = 0.5;\n"),
            ("crates/px-layout/src/sub/b.rs", "let a = Au(5);\n"),
            ("crates/px-layout/src/c.rs", "let a = b.0 * 2;\n"),
        ]);
        let g = run_sources(&fx);
        let f = g.failures();
        let has = |p: &str| f.iter().any(|m| m.starts_with(p));
        assert!(has("px-layout names f32 or f64 in 1 place(s)"), "{f:?}");
        assert!(
            has("px-layout constructs Au(...) directly in 1 place(s)"),
            "{f:?}"
        );
        assert!(
            has("px-layout computes on a raw .0 field in 1 place(s)"),
            "{f:?}"
        );
        assert_eq!(f.len(), 11);
    }

    #[test]
    fn scan_reports_grep_shaped_lines_in_path_order() {
        let fx = sources(&[
            ("crates/px-layout/src/z.rs", "ok\nlet y: f64;\n"),
            ("crates/px-layout/src/a.rs", "let x: f32;\n"),
        ]);
        let found = scan(fx.path(), names_float);
        assert_eq!(
            found,
            [
                "crates/px-layout/src/a.rs:1:let x: f32;",
                "crates/px-layout/src/z.rs:2:let y: f64;"
            ]
        );
    }

    #[test]
    fn sources_are_skipped_when_the_crate_is_absent() {
        let fx = Fixture::new("layout-nosrc");
        fx.write("README.md", "x");
        let g = run_sources(&fx);
        assert!(g.passed());
    }

    fn corpus_fixture(refs: usize, crashes: usize) -> Fixture {
        let fx = Fixture::new("layout-corpus");
        for i in 0..refs {
            fx.write(&format!("{REFTEST_DIR}/t{i}-ref.xht"), "x");
            fx.write(&format!("{REFTEST_DIR}/t{i}.xht"), "x");
        }
        for i in 0..crashes {
            fx.write(&format!("{CRASH_DIR}/c{i}.html"), "x");
        }
        fx
    }

    fn run_corpora(fx: &Fixture) -> Gate {
        let mut g = Gate::new("layout");
        check_corpora(&mut g, fx.path());
        g
    }

    #[test]
    fn corpora_at_the_floor_pass() {
        let fx = corpus_fixture(REFTEST_FLOOR, CRASH_FLOOR);
        fx.commit_all();
        let g = run_corpora(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn corpora_below_the_floor_fail() {
        let fx = corpus_fixture(REFTEST_FLOOR - 1, CRASH_FLOOR - 1);
        fx.commit_all();
        let g = run_corpora(&fx);
        assert_eq!(
            g.failures(),
            [
                "the subset has 39 reference files, floor is 40",
                "the crashtest corpus has 19 files, floor is 20"
            ]
        );
    }

    #[test]
    fn corpus_files_only_on_disk_do_not_count() {
        let fx = corpus_fixture(REFTEST_FLOOR, CRASH_FLOOR);
        fx.commit_all();
        fx.git(&["rm", "-q", "--cached", &format!("{CRASH_DIR}/c0.html")]);
        let g = run_corpora(&fx);
        assert_eq!(
            g.failures(),
            ["the crashtest corpus has 19 files, floor is 20"]
        );
    }

    #[test]
    fn missing_corpus_directories_fail_with_their_explanations() {
        let fx = Fixture::new("layout-nocorpus");
        fx.write("README.md", "x").commit_all();
        let g = run_corpora(&fx);
        let f = g.failures();
        assert_eq!(f.len(), 5, "{f:?}");
        assert!(f[0].starts_with("the CSS2 reftest subset is not vendored at"));
        assert!(f[3].starts_with("the CSS2 crashtests are not vendored at"));
    }
}
