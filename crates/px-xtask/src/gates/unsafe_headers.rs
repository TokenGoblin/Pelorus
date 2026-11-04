//! `cargo xtask unsafe-headers` -- gate check 4, the per-crate unsafe rules
//! (build-spec §4, CLAUDE.md). Port of `ci/gate-unsafe-headers.sh`.
//!
//! A source-text assertion, not a convention. Every crate's entry point must
//! open with `#![forbid(unsafe_code)]`. Two crates are excepted, with
//! different rules, and the difference is the point:
//!
//! - `px-sandbox` (ADR 008) must carry `#![deny(unsafe_op_in_unsafe_fn)]` and
//!   must NOT carry forbid, which would make the crate unable to do the one
//!   job it exists for.
//! - `px-css` (ADR 024) may drop forbid, because stylo's `TElement` declares
//!   six unsafe fn methods and forbid rejects *implementing* an unsafe method.
//!   In exchange the crate must contain zero unsafe blocks and zero unsafe
//!   impl -- an unsafe fn body needs neither, so nothing in the crate does
//!   anything the compiler is not checking.
//!
//! px-css is checked in both states rather than only the one it will end up
//! in. While it still carries forbid -- true today, and true until stylo
//! actually lands -- the gate says so and requires nothing else. The moment
//! forbid goes, the two zero-counts become the rule. Written this way because
//! the commit that removes forbid is the one window in which the guarantee
//! could lapse unnoticed, and that is exactly the kind of window this project
//! keeps falling through.
//!
//! OS-independent: this reads source text.
//!
//! This file must not itself contain the waiver spellings the scans below look
//! for (the scan reads every tracked `crates/**/*.rs`, including this one), so
//! they are assembled from pieces.

use crate::Gate;
use crate::git;
use std::path::{Path, PathBuf};

const SANDBOX: &str = "px-sandbox";
const STYLO_CRATE: &str = "px-css";

/// Crates carrying `[lints] workspace = true`. px-ipc was added to that set
/// and omitted here once, which left a waiver in the crate that decodes
/// hostile bytes passing CI silently -- the exact bypass this scan exists for.
const PANIC_LINT_CRATES: [&str; 11] = [
    "px-content",
    "px-net",
    "px-mcp",
    "px-ipc",
    "px-websec",
    "px-bindings",
    "px-store",
    "px-a11y",
    "px-block",
    "px-update",
    "px-image",
];

const PANIC_LINTS: [&str; 4] = ["unwrap_used", "expect_used", "indexing_slicing", "panic"];

fn has_line_starting(src: &[u8], prefix: &str) -> bool {
    src.split(|b| *b == b'\n')
        .any(|l| l.starts_with(prefix.as_bytes()))
}

pub fn has_forbid(src: &[u8]) -> bool {
    has_line_starting(src, "#![forbid(unsafe_code)]")
}

pub fn has_deny_op(src: &[u8]) -> bool {
    has_line_starting(src, "#![deny(unsafe_op_in_unsafe_fn)]")
}

/// One reported line of a per-entry check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Ok(String),
    Fail(String),
}

/// The header rule for one crate root. `name` is the crate directory name,
/// `entry` the repository-relative path used in the messages.
pub fn check_entry(name: &str, entry: &str, src: &[u8]) -> Vec<Outcome> {
    let mut out = Vec::new();
    if name == SANDBOX {
        if has_deny_op(src) {
            out.push(Outcome::Ok(format!("{entry} deny(unsafe_op_in_unsafe_fn)")));
        } else {
            out.push(Outcome::Fail(format!(
                "{entry} is missing #![deny(unsafe_op_in_unsafe_fn)]"
            )));
        }
        if has_forbid(src) {
            out.push(Outcome::Fail(format!(
                "{entry} carries forbid(unsafe_code); {SANDBOX} is the crate that needs unsafe"
            )));
        }
    } else if name == STYLO_CRATE {
        // Either state is legal; neither is unchecked.
        if has_forbid(src) {
            out.push(Outcome::Ok(format!(
                "{entry} forbid(unsafe_code), still (ADR 024 not yet needed)"
            )));
        } else {
            out.push(Outcome::Ok(format!(
                "{entry} has dropped forbid under ADR 024"
            )));
        }
    } else if has_forbid(src) {
        out.push(Outcome::Ok(format!("{entry} forbid(unsafe_code)")));
    } else {
        out.push(Outcome::Fail(format!(
            "{entry} is missing #![forbid(unsafe_code)]"
        )));
    }
    out
}

/// Extra crate roots named in a `Cargo.toml`: only `path = "src/....rs"` lines,
/// not any `src/*.rs` the file happens to mention. The looser pattern once
/// scraped a path out of a *comment* in px-css's manifest and reported a file
/// that is not a crate root as one that had "dropped forbid" -- harmless
/// because it printed an ok line, which is exactly why it would have gone
/// unnoticed: a check that invents a subject can also invent a pass.
/// Sorted and de-duplicated.
pub fn manifest_roots(toml: &str) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for line in toml.lines() {
        let l = line.trim_start_matches([' ', '\t', '\r', '\u{b}', '\u{c}']);
        let Some(rest) = l.strip_prefix("path") else {
            continue;
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start_matches([' ', '\t']);
        let Some(rest) = rest.strip_prefix('"') else {
            continue;
        };
        let Some(run) = rest.strip_prefix("src/") else {
            continue;
        };
        let end = run
            .find(|c: char| !(c.is_ascii_alphanumeric() || "_/.-".contains(c)))
            .unwrap_or(run.len());
        let (body, after) = run.split_at(end);
        if after.starts_with('"') && body.len() > 3 && body.ends_with(".rs") {
            v.push(format!("src/{body}"));
        }
    }
    v.sort();
    v.dedup();
    v
}

/// Lines of `src` (1-based number, text) containing a local switch-off of the
/// `unsafe_code` lint (an allow or an expect attribute).
pub fn unsafe_code_waivers(src: &[u8]) -> Vec<(usize, String)> {
    let needles = [
        concat!("allow", "(unsafe_code)"),
        concat!("expect", "(unsafe_code)"),
    ];
    matching_lines(src, |l| needles.iter().any(|n| l.contains(n)))
}

/// Lines waiving one of the panic lints CLAUDE.md denies.
pub fn panic_lint_waivers(src: &[u8]) -> Vec<(usize, String)> {
    matching_lines(src, |l| {
        ["allow(clippy::", "expect(clippy::"].iter().any(|p| {
            l.match_indices(p).any(|(i, _)| {
                let rest = l.get(i + p.len()..).unwrap_or("");
                PANIC_LINTS
                    .iter()
                    .any(|lint| rest.strip_prefix(lint).is_some_and(|r| r.starts_with(')')))
            })
        })
    })
}

fn matching_lines(src: &[u8], pred: impl Fn(&str) -> bool) -> Vec<(usize, String)> {
    String::from_utf8_lossy(src)
        .split('\n')
        .enumerate()
        .filter(|(_, l)| pred(l))
        .map(|(i, l)| (i + 1, l.to_string()))
        .collect()
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn is_word_ish(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_'
}

/// Whether the line has `unsafe` (not preceded by a letter or underscore)
/// followed by `follow`'s shape: `{` after optional whitespace (a block), or
/// `impl` after at least one whitespace (an impl).
fn line_has_unsafe(line: &[u8], impl_form: bool) -> bool {
    const U: &[u8] = b"unsafe";
    (0..line.len()).any(|i| {
        let Some(rest) = line.get(i..).and_then(|r| r.strip_prefix(U)) else {
            return false;
        };
        if i > 0 && line.get(i - 1).is_some_and(|b| is_word_ish(*b)) {
            return false;
        }
        let ws = rest.iter().take_while(|b| is_space(**b)).count();
        let after = rest.get(ws..).unwrap_or(&[]);
        if impl_form {
            ws > 0 && after.starts_with(b"impl")
        } else {
            after.starts_with(b"{")
        }
    })
}

/// Count lines in one file that match (block or impl form) and are not
/// `//` comment lines, as `grep -rn | grep -v` did. A file with a NUL byte is
/// binary to grep: it contributes one line if anything matches.
pub fn count_unsafe_lines(src: &[u8], impl_form: bool) -> usize {
    let lines = src.split(|b| *b == b'\n');
    if src.contains(&0) {
        return usize::from(lines.into_iter().any(|l| line_has_unsafe(l, impl_form)));
    }
    lines
        .filter(|l| line_has_unsafe(l, impl_form))
        .filter(|l| {
            let t = l.iter().skip_while(|b| is_space(**b));
            !t.take(2).copied().eq(*b"//")
        })
        .count()
}

fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => walk_files(&p, out),
            Ok(t) if t.is_file() => out.push(p),
            _ => {}
        }
    }
}

fn read_lossy_path(root: &Path, rel: &str) -> Option<Vec<u8>> {
    std::fs::read(root.join(rel)).ok()
}

fn report(g: &mut Gate, o: Outcome) {
    match o {
        Outcome::Ok(m) => g.ok(m),
        Outcome::Fail(m) => g.fail(m),
    }
}

fn sorted_names(dir: &Path, want_dirs: bool) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().is_dir() == want_dirs)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn check_headers(g: &mut Gate, root: &Path) -> bool {
    let crates: Vec<String> = sorted_names(&root.join("crates"), true)
        .into_iter()
        .filter(|n| n.starts_with("px-"))
        .collect();
    if crates.is_empty() {
        g.fail("no crates found under crates/ \u{2014} nothing to audit");
        return false;
    }
    for name in &crates {
        let dir = format!("crates/{name}/");
        // Every crate root, not just the two conventional ones. A crate with
        // an extra [[bin]] target -- which px-broker already has -- carries a
        // SECOND crate root, and forbid(unsafe_code) in lib.rs does not cover
        // it. The gate used to look at exactly two paths and never at that
        // file.
        let mut entries = vec![format!("{dir}src/lib.rs"), format!("{dir}src/main.rs")];
        entries.extend(
            sorted_names(&root.join(&dir).join("src/bin"), false)
                .into_iter()
                .filter(|n| n.ends_with(".rs") && !n.starts_with('.'))
                .map(|n| format!("{dir}src/bin/{n}")),
        );
        let manifest = std::fs::read(root.join(&dir).join("Cargo.toml")).unwrap_or_default();
        entries.extend(
            manifest_roots(&String::from_utf8_lossy(&manifest))
                .into_iter()
                .map(|e| format!("{dir}{e}")),
        );
        let mut found = false;
        for entry in &entries {
            let p = root.join(entry);
            if !p.is_file() {
                continue;
            }
            found = true;
            let src = std::fs::read(&p).unwrap_or_default();
            for o in check_entry(name, entry, &src) {
                report(g, o);
            }
        }
        if !found {
            g.fail(format!("{dir} has no src/lib.rs or src/main.rs"));
        }
    }
    true
}

/// The header check above is what makes unsafe a compile error. What it cannot
/// see is somebody switching the attribute off locally, so scan for the bypass
/// itself rather than for the word `unsafe`, which appears legitimately in
/// prose. Assert the scan has something to scan: if a pathspec matches nothing
/// the loop body never runs and the gate passes having inspected zero files --
/// the same "a check that cannot fail looks like a check that passes" shape
/// that defeated two other checks here.
fn check_unsafe_code_waivers(g: &mut Gate, root: &Path) {
    let paths = match git::ls_files(root, &[], &["crates/*.rs"]) {
        Ok(p) => p,
        Err(e) => {
            g.fail(format!("cannot list tracked files: {e}"));
            return;
        }
    };
    if paths.is_empty() {
        g.fail("the unsafe_code waiver scan matched no files at all");
    }
    let sandbox_prefix = format!("crates/{SANDBOX}/");
    for path in &paths {
        if path.starts_with(&sandbox_prefix) {
            continue;
        }
        // px-css is allowed to not carry forbid (ADR 024), so a waiver there is
        // redundant rather than a bypass -- but it is also a sign somebody
        // reached for the blanket exemption the ADR rejected, so it still
        // fails. The rule for that crate is enforced positively below.
        let hits = read_lossy_path(root, path)
            .map(|b| unsafe_code_waivers(&b))
            .unwrap_or_default();
        if !hits.is_empty() {
            g.fail(format!(
                "{path} switches off unsafe_code locally; only {SANDBOX} may use unsafe"
            ));
            for (n, l) in hits.iter().take(5) {
                g.detail(format!("{n}:{l}"));
            }
        }
    }
}

/// CLAUDE.md, hard rules: "Clippy denies these; do not add allow attributes to
/// get around it." A denied lint that can be waived at the call site is a style
/// preference, not a rule, so the waiver is what the gate looks for.
fn check_panic_lint_waivers(g: &mut Gate, root: &Path) {
    let mut scanned = false;
    for krate in PANIC_LINT_CRATES {
        if !root.join("crates").join(krate).is_dir() {
            continue;
        }
        let spec = format!("crates/{krate}/*.rs");
        let paths = match git::ls_files(root, &[], &[&spec]) {
            Ok(p) => p,
            Err(e) => {
                g.fail(format!("cannot list tracked files: {e}"));
                continue;
            }
        };
        for path in &paths {
            scanned = true;
            let hits = read_lossy_path(root, path)
                .map(|b| panic_lint_waivers(&b))
                .unwrap_or_default();
            if !hits.is_empty() {
                g.fail(format!(
                    "{path} waives a panic lint that CLAUDE.md denies in {krate}"
                ));
                for (n, l) in hits.iter().take(5) {
                    g.detail(format!("{n}:{l}"));
                }
            }
        }
    }
    if !scanned {
        g.fail("the panic-lint waiver scan matched no files at all");
    }
}

/// ADR 024's two counts, positively enforced.
///
/// Duplicated from cargo xtask style deliberately. That gate is Phase 5's and
/// phase gates are not run forever; this one is permanent, and the rule it
/// encodes outlives the phase that needed it. A rule enforced only by the gate
/// of the phase that introduced it stops being enforced when the phase closes.
fn check_css_counts(g: &mut Gate, root: &Path) {
    let src_dir = root.join("crates").join(STYLO_CRATE).join("src");
    if !src_dir.is_dir() {
        return;
    }
    let mut files = Vec::new();
    walk_files(&src_dir, &mut files);
    let (mut blocks, mut impls) = (0, 0);
    for f in &files {
        let b = std::fs::read(f).unwrap_or_default();
        blocks += count_unsafe_lines(&b, false);
        impls += count_unsafe_lines(&b, true);
    }
    if blocks == 0 {
        g.ok(format!("{STYLO_CRATE} contains no unsafe block (ADR 024)"));
    } else {
        g.fail(format!(
            "{STYLO_CRATE} contains {blocks} unsafe block(s); ADR 024 says zero."
        ));
        g.fail("  An unsafe fn required by a stylo trait signature needs no unsafe");
        g.fail(format!(
            "  body. A real unsafe operation belongs in {SANDBOX} (ADR 008)."
        ));
    }
    if impls == 0 {
        g.ok(format!("{STYLO_CRATE} contains no unsafe impl (ADR 024)"));
    } else {
        g.fail(format!(
            "{STYLO_CRATE} contains {impls} unsafe impl(s); ADR 024 says zero."
        ));
        g.fail("  Send/Sync over a borrowed view are derivable rather than asserted.");
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !check_headers(g, root) {
        return;
    }
    check_unsafe_code_waivers(g, root);
    check_panic_lint_waivers(g, root);
    check_css_counts(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    const FORBID: &str = "#![forbid(unsafe_code)]\n";

    fn allow_attr() -> String {
        format!("#![{}(unsafe_code)]\n", "allow")
    }

    fn healthy(fx: &Fixture) {
        fx.write("crates/px-a/src/lib.rs", FORBID)
            .write("crates/px-a/src/bin/tool.rs", FORBID)
            .write(
                "crates/px-sandbox/src/lib.rs",
                "#![deny(unsafe_op_in_unsafe_fn)]\n",
            )
            .write("crates/px-css/src/lib.rs", FORBID)
            .write("crates/px-net/src/lib.rs", FORBID);
    }

    fn run_on(fx: &Fixture) -> Gate {
        fx.commit_all();
        let mut g = Gate::new("unsafe-headers");
        run(&mut g, fx.path(), &[]);
        g
    }

    #[test]
    fn healthy_tree_passes() {
        let fx = Fixture::new("uh-ok");
        healthy(&fx);
        let g = run_on(&fx);
        assert!(g.passed(), "{:?}", g.failures());
    }

    #[test]
    fn header_rules() {
        let f = |n: &str, s: &str| check_entry(n, "p", s.as_bytes());
        assert_eq!(
            f("px-a", FORBID),
            [Outcome::Ok("p forbid(unsafe_code)".into())]
        );
        assert!(matches!(f("px-a", "fn x(){}")[0], Outcome::Fail(_)));
        // Only a line-initial attribute counts.
        assert!(matches!(
            f("px-a", "  #![forbid(unsafe_code)]")[0],
            Outcome::Fail(_)
        ));
        assert!(matches!(
            f("px-a", "// #![forbid(unsafe_code)]")[0],
            Outcome::Fail(_)
        ));
        // The sandbox needs deny and must not forbid.
        let both = "#![deny(unsafe_op_in_unsafe_fn)]\n#![forbid(unsafe_code)]\n";
        let o = f("px-sandbox", both);
        assert!(matches!(o[0], Outcome::Ok(_)) && matches!(o[1], Outcome::Fail(_)));
        assert!(matches!(f("px-sandbox", FORBID)[0], Outcome::Fail(_)));
        // px-css is legal in both states.
        assert!(matches!(f("px-css", FORBID)[0], Outcome::Ok(_)));
        assert!(matches!(f("px-css", "")[0], Outcome::Ok(_)));
    }

    #[test]
    fn missing_header_and_extra_roots_fail() {
        let fx = Fixture::new("uh-hdr");
        healthy(&fx);
        fx.write("crates/px-a/src/bin/other.rs", "fn main() {}\n");
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["crates/px-a/src/bin/other.rs is missing #![forbid(unsafe_code)]"]
        );
    }

    #[test]
    fn manifest_declared_root_is_checked() {
        let fx = Fixture::new("uh-manifest");
        healthy(&fx);
        fx.write(
            "crates/px-a/Cargo.toml",
            "[[bin]]\nname = \"x\"\npath = \"src/extra.rs\"\n# path = \"src/ghost.rs\"\n",
        )
        .write("crates/px-a/src/extra.rs", "fn main() {}\n");
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["crates/px-a/src/extra.rs is missing #![forbid(unsafe_code)]"]
        );
    }

    #[test]
    fn manifest_root_extraction() {
        let t = "path = \"src/a.rs\"\n  path=\"src/b/c.rs\"\n# path = \"src/no.rs\"\npath = \"other/x.rs\"\npath = \"src/x.txt\"\nname = \"src/y.rs\"\npath = \"src/a.rs\"\npath = \"src/z.rs\" # c\n";
        assert_eq!(manifest_roots(t), ["src/a.rs", "src/b/c.rs", "src/z.rs"]);
    }

    #[test]
    fn crate_without_roots_fails() {
        let fx = Fixture::new("uh-noroot");
        healthy(&fx);
        fx.write("crates/px-empty/README", "x");
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["crates/px-empty/ has no src/lib.rs or src/main.rs"]
        );
    }

    #[test]
    fn no_crates_fails() {
        let fx = Fixture::new("uh-nocrates");
        fx.write("README.md", "x");
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["no crates found under crates/ \u{2014} nothing to audit"]
        );
    }

    #[test]
    fn local_unsafe_code_waiver_fails_outside_the_sandbox() {
        let fx = Fixture::new("uh-waiver");
        healthy(&fx);
        fx.write(
            "crates/px-a/src/m.rs",
            &format!("fn a() {{}}\n{}", allow_attr()),
        )
        .write("crates/px-sandbox/src/m.rs", &allow_attr());
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            [
                "crates/px-a/src/m.rs switches off unsafe_code locally; only px-sandbox may use unsafe"
            ]
        );
    }

    #[test]
    fn panic_lint_waiver_fails_in_listed_crates_only() {
        let fx = Fixture::new("uh-panic");
        healthy(&fx);
        let w = format!("#[{}(clippy::unwrap_used)]\nfn f() {{}}\n", "allow");
        fx.write("crates/px-net/src/w.rs", &w)
            .write("crates/px-a/src/w.rs", &w);
        let g = run_on(&fx);
        assert_eq!(
            g.failures(),
            ["crates/px-net/src/w.rs waives a panic lint that CLAUDE.md denies in px-net"]
        );
        assert!(panic_lint_waivers(b"expect(clippy::panic)").len() == 1);
        assert!(panic_lint_waivers(b"allow(clippy::panic_in_result_fn)").is_empty());
        assert!(panic_lint_waivers(b"allow(clippy::too_many_arguments)").is_empty());
    }

    #[test]
    fn scans_that_match_nothing_fail() {
        let fx = Fixture::new("uh-empty-scan");
        // Only a crate that is not in the panic-lint set, and no .rs files
        // tracked under crates/ at all.
        fx.write("crates/px-a/Cargo.toml", "[package]\n");
        let g = run_on(&fx);
        assert!(
            g.failures()
                .contains(&"the unsafe_code waiver scan matched no files at all".to_string())
        );
        assert!(
            g.failures()
                .contains(&"the panic-lint waiver scan matched no files at all".to_string())
        );
    }

    #[test]
    fn css_unsafe_blocks_and_impls_fail() {
        let fx = Fixture::new("uh-css");
        healthy(&fx);
        fx.write(
            "crates/px-css/src/dom.rs",
            "// unsafe { in a comment }\nunsafe fn ok_by_adr() {}\nfn f() { unsafe { g() } }\nunsafe impl Send for X {}\nlet s = \"unsafe  impl\";\n",
        );
        let g = run_on(&fx);
        let f = g.failures();
        assert_eq!(
            f[0],
            "px-css contains 1 unsafe block(s); ADR 024 says zero."
        );
        assert_eq!(f.len(), 3 + 2);
        assert_eq!(f[3], "px-css contains 2 unsafe impl(s); ADR 024 says zero.");
    }

    #[test]
    fn unsafe_line_counting() {
        assert_eq!(count_unsafe_lines(b"unsafe {\n", false), 1);
        assert_eq!(count_unsafe_lines(b"x = unsafe   {", false), 1);
        assert_eq!(count_unsafe_lines(b"is_unsafe {", false), 0);
        assert_eq!(count_unsafe_lines(b"  // unsafe {", false), 0);
        assert_eq!(count_unsafe_lines(b"unsafe fn f() {", false), 0);
        assert_eq!(count_unsafe_lines(b"unsafe impl X", true), 1);
        assert_eq!(count_unsafe_lines(b"unsafeimpl", true), 0);
        assert_eq!(count_unsafe_lines(b"1unsafe {", false), 1);
    }
}
