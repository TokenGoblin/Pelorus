//! `cargo xtask ipc` -- the Phase 1 gate (build-spec section 9). Port of
//! `ci/gate-ipc.sh`.
//!
//! ```text
//! IPC deserializers fuzz clean for 24h        -> `cargo xtask fuzz-smoke`
//!                                                + the scheduled campaign
//! broker rejects any message asserting        -> here
//!   its own identity
//! killing the content process is recovered    -> here
//!   from cleanly
//! message size limits enforced and tested     -> here
//!   with a hostile length prefix
//! ```
//!
//! Three of the four are Rust tests, so most of this gate names which suites
//! carry which gate item. A suite that stops existing -- or stops running --
//! should be visible, not quietly absent from a green run.
//!
//! # What differs from the script
//!
//! * `ci/check_not_serializable.py` became `cargo xtask not-serializable`
//!   (the `not_serializable` module); this gate calls its check in-process.
//! * The product binary is looked up under `$CARGO_TARGET_DIR` when set (the
//!   script assumed `target/`); see `proc::target_dir`.

use super::not_serializable;
use super::suites::{self, Problem, Suite};
use crate::Gate;
use crate::git;
use crate::proc;
use crate::scan::{is_space, is_word};
use std::path::Path;

/// Each entry is gate item, test filter, file that must carry it.
///
/// Three conditions, each closing a way the previous version could pass while
/// testing nothing:
///
/// 1. the filter matches at least one test (it was renamed away);
/// 2. the named file contains `#[test]` fns for it (deleting the only tests
///    that cross a process boundary still matched similarly-named in-memory
///    tests elsewhere in the workspace);
/// 3. none of the matching tests is `#[ignore]`d (measured: marking every gate
///    test ignored left this reporting PASS for a phase in which none of them
///    ran).
pub const SUITES: &[Suite] = &[
    (
        "hostile-identity",
        "hostile_identity",
        "crates/px-broker/src/lib.rs",
    ),
    (
        "crash-restart",
        "crash_restart",
        "crates/px-content/tests/crash_restart.rs",
    ),
    ("size-limits", "size_limit", "crates/px-ipc/src/lib.rs"),
    // Phase 8: the GPU vocabulary is decoded from a sandboxed process.
    (
        "gpu-wire-hostile",
        "gpu_hostile",
        "crates/px-ipc/tests/hostile_gpu.rs",
    ),
];

/// The wire vocabulary may not carry a field that names the sender.
pub const IDENTITY_NAMES: &[&str] = &[
    "sender",
    "from",
    "identity",
    "channel",
    "channel_id",
    "pid",
    "process",
    "process_id",
    "tab",
    "tab_id",
    "origin",
    "partition",
    "partition_key",
    "caller",
    "principal",
];

fn skip_ws(s: &str) -> &str {
    s.trim_start_matches(is_space)
}

/// `^[[:space:]]*(pub[[:space:]]+)?(sender|from|...)[[:space:]]*:`
///
/// `(pub )?` is load-bearing, and its absence made the check close to
/// inverted. The regex used to anchor the field name straight after leading
/// whitespace, so `pub sender: ChannelId` did not match -- and a struct field
/// must be `pub` to be readable from px-broker at all.
pub fn names_sender_field(line: &str) -> bool {
    let r = skip_ws(line);
    let after_pub = r.strip_prefix("pub").and_then(|p| {
        let t = skip_ws(p);
        (t.len() < p.len()).then_some(t)
    });
    [Some(r), after_pub].into_iter().flatten().any(|r| {
        IDENTITY_NAMES.iter().any(|n| {
            r.strip_prefix(n)
                .is_some_and(|a| skip_ws(a).starts_with(':'))
        })
    })
}

/// `^[[:space:]]*serde\b` on some line of a `Cargo.toml`.
pub fn depends_on_serde(cargo_toml: &str) -> bool {
    cargo_toml.lines().any(|l| {
        skip_ws(l)
            .strip_prefix("serde")
            .is_some_and(|r| r.chars().next().is_none_or(|c| !is_word(c)))
    })
}

/// The first `channel = "..."` value in a `rust-toolchain.toml`
/// (`grep -oE 'channel = "[^"]+"' | head -1 | cut -d'"' -f2`).
pub fn pinned_channel(toolchain: &str) -> Option<String> {
    toolchain.lines().find_map(|l| {
        l.match_indices("channel = \"").find_map(|(i, m)| {
            let rest = &l[i + m.len()..];
            let v = &rest[..rest.find('"')?];
            (!v.is_empty()).then(|| v.to_string())
        })
    })
}

/// Findings of `cargo xtask not-serializable <ty> <files>`, run in-process: a
/// derive or hand-written impl of a serde trait on `ty`, or `ty` not declared
/// in any of `files` (this check must not pass by failing to find its subject).
fn not_serializable_findings(ty: &str, files: &[(String, String)]) -> Vec<String> {
    let mut found = false;
    let mut out = Vec::new();
    for (path, text) in files {
        let (f, findings) = not_serializable::check_text(path, text, ty);
        found |= f;
        out.extend(findings);
    }
    if !found {
        let paths: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        out.push(format!(
            "{ty} was not declared in any of: {}\n      this check must not pass by failing to find its subject",
            paths.join(" ")
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// The gate.
// ---------------------------------------------------------------------------

fn read_lossy(p: &Path) -> Option<String> {
    std::fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Suite problems, in the words this gate has always used.
fn report_problem(g: &mut Gate, item: &str, filter: &str, home: &str, p: &Problem, ignored: &str) {
    match p {
        Problem::NoAttr => {
            g.fail(format!(
                "gate item '{item}' has no #[test] fn matching '{filter}' in {home};"
            ));
            g.fail("  matching tests elsewhere do not substitute for that file");
        }
        Problem::Ignored(n) => {
            g.fail(format!(
                "gate item '{item}' has {n} #[ignore]d test(s); a gate item"
            ));
            g.fail("  whose tests do not run is not a gate item");
            for l in suites::matching_lines(ignored, filter) {
                g.detail(l);
            }
        }
        other => g.fail(suites::problem_message(item, filter, home, other)),
    }
}

/// The product, end to end.
///
/// Nothing used to run px-browser. It exited FAILURE on a completely clean run
/// -- broker and content process each waiting for the other, the deadline
/// breaking the deadlock by reaping a healthy child -- and every check above
/// stayed green, because they test libraries and this is the only artifact that
/// puts them together.
fn check_browser(g: &mut Gate, root: &Path) {
    g.info("px-browser end to end");
    match proc::cargo(root, &["build", "--workspace", "--locked", "--quiet"]) {
        Ok(s) if s.success() => {}
        _ => {
            g.fail("debug build failed; cannot run px-browser");
            return;
        }
    }
    let browser = proc::built_binary(root, "debug", "px-browser");
    if !browser.is_file() {
        g.fail("px-browser was not built; the end-to-end check cannot run");
        return;
    }
    let prog = browser.to_string_lossy().into_owned();
    match proc::capture_merged(&prog, &[], Some(root)) {
        Ok((out, true)) => g.ok(format!("px-browser: {}", out.trim_end_matches('\n'))),
        Ok((out, false)) => g.fail(format!(
            "px-browser exited non-zero on a clean run: {}",
            out.trim_end_matches('\n')
        )),
        Err(e) => g.fail(format!("px-browser exited non-zero on a clean run: {e}")),
    }
}

/// Invariant 9, checked in source as well as in tests.
///
/// The tests prove the broker refuses a request naming somebody else's
/// resource. They cannot prove that no FUTURE message type carries an identity
/// field -- and a message that said who it was from would be believed by
/// whatever new code was written to read it. So the field names are forbidden
/// outright in the wire vocabulary.
///
/// Naming a RESOURCE is fine, and is how the capability model works:
/// `FrameHost` carries the `FrameId` it asks about, and the broker decides
/// whether the channel it arrived on is entitled to it. What is forbidden is a
/// message describing its own sender.
fn check_wire_identity(g: &mut Gate, root: &Path) {
    let sources = git::ls_files(root, &[], &["crates/px-ipc/src/*.rs"]).unwrap_or_default();
    if sources.is_empty() {
        g.fail("no px-ipc sources found to scan; this check cannot pass by finding nothing");
        return;
    }
    let mut leaks = false;
    for src in &sources {
        let text = read_lossy(&root.join(src)).unwrap_or_default();
        let hits: Vec<String> = text
            .split('\n')
            .enumerate()
            .filter(|(_, l)| names_sender_field(l))
            .map(|(n, l)| format!("{}:{l}", n + 1))
            .collect();
        if !hits.is_empty() {
            g.fail(format!(
                "{src} declares a field naming the sender; authority comes from"
            ));
            g.fail("  the channel, never from the message (invariant 9)");
            for h in hits.iter().take(5) {
                g.detail(h);
            }
            leaks = true;
        }
    }
    if !leaks {
        g.ok("no message type names its own sender");
    }
}

/// `ChannelId` is the broker's answer to "who sent this". If it were ever
/// serialisable it could travel inside a message, and then it would be a claim
/// rather than an observation.
///
/// Two checks, because the stronger one is structural. px-broker has no serde
/// dependency of its own, and without one `#[derive(Serialize)]` cannot name
/// the trait -- so the property holds by construction rather than by
/// inspection. The parser stays as defence in depth, and because it also
/// catches a hand-written impl.
fn check_channel_id(g: &mut Gate, root: &Path) {
    // A missing Cargo.toml reads as "no serde", as `grep -q` on a missing file
    // did in the script.
    let serde_dep =
        read_lossy(&root.join("crates/px-broker/Cargo.toml")).is_some_and(|t| depends_on_serde(&t));
    if serde_dep {
        g.fail("px-broker depends on serde directly; nothing in the broker should be");
        g.fail("  able to derive Serialize, least of all ChannelId");
    } else {
        g.ok("px-broker cannot name serde, so it cannot derive Serialize");
    }

    let sources = git::ls_files(root, &[], &["crates/px-broker/src/*.rs"]).unwrap_or_default();
    let homes: Vec<(String, String)> = sources
        .into_iter()
        .filter_map(|p| {
            let t = read_lossy(&root.join(&p))?;
            t.contains("struct ChannelId").then_some((p, t))
        })
        .collect();
    if homes.is_empty() {
        g.fail("no 'struct ChannelId' found under crates/px-broker/src/; this check");
        g.fail("  cannot pass by not finding what it is looking for");
        return;
    }
    // `ci/check_not_serializable.py` printed its findings uncounted; the
    // verdict is one counted failure.
    let errs = not_serializable_findings("ChannelId", &homes);
    if errs.is_empty() {
        g.ok("ChannelId cannot be serialised onto the wire");
    } else {
        for e in errs {
            eprintln!("FAIL {e}");
        }
        g.fail("ChannelId is serialisable; it must never be able to reach the wire");
    }
}

/// The fuzz targets carry gate item 1 and are compiled by exactly one CI job,
/// which installs cargo-fuzz first and takes minutes. A target that stopped
/// compiling would silently stop testing anything, and would look like a slow
/// job rather than a broken one. Type-check them here in seconds, on the stable
/// pin -- `fuzz/` overrides to nightly, and nothing about `cargo check` needs
/// it. Skipped when there is no `fuzz/` or no `rustup`.
fn check_fuzz_compiles(g: &mut Gate, root: &Path) {
    if !root.join("fuzz").is_dir() {
        return;
    }
    if proc::capture("rustup", &["--version"], Some(root)).is_err() {
        return;
    }
    let tc = read_lossy(&root.join("rust-toolchain.toml")).and_then(|t| pinned_channel(&t));
    let Some(tc) = tc else {
        g.fail("cannot read the pinned toolchain from rust-toolchain.toml");
        return;
    };
    // The rustup proxy, not `$CARGO`: only the proxy understands `+toolchain`.
    let mut cmd = std::process::Command::new("cargo");
    cmd.args([&format!("+{tc}"), "check", "--all-targets", "--quiet"])
        .current_dir(root.join("fuzz"));
    proc::apply_gate_env(&mut cmd, root);
    let ok = cmd.status().is_ok_and(|s| s.success());
    if ok {
        g.ok("fuzz targets compile");
    } else {
        g.fail("a fuzz target does not compile; gate item 1 tests nothing");
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }

    suites::check_suites_with(g, root, SUITES, report_problem);
    suites::run_workspace_tests(g, root);

    // Section 14.2 / ADR 004: the content process ships from a profile that
    // aborts. A build gate that only exercises `release` would never compile it.
    g.info("cargo build --locked --profile content-release -p px-content");
    match proc::cargo(
        root,
        &[
            "build",
            "--locked",
            "--profile",
            "content-release",
            "-p",
            "px-content",
        ],
    ) {
        Ok(s) if s.success() => {}
        _ => g.fail("content-release build failed"),
    }

    check_browser(g, root);
    check_wire_identity(g, root);
    check_channel_id(g, root);
    check_fuzz_compiles(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn sender_fields() {
        assert!(names_sender_field("    pub sender: ChannelId,"));
        assert!(names_sender_field("sender : X"));
        assert!(names_sender_field("  from: u32,"));
        assert!(names_sender_field("pub  partition_key: K"));
        assert!(names_sender_field("channel_id: u64"));
        assert!(names_sender_field("  tab:u32"));
        // Naming a resource is fine.
        assert!(!names_sender_field("pub frame: FrameId,"));
        assert!(!names_sender_field("pub origin_hint: X,"));
        assert!(!names_sender_field("    let sender = 1;"));
        assert!(!names_sender_field("pub(crate) sender: X"));
        assert!(!names_sender_field("// sender: x"));
        assert!(!names_sender_field("pubsender: X"));
    }

    #[test]
    fn serde_dependency() {
        assert!(depends_on_serde("[dependencies]\nserde = \"1\"\n"));
        assert!(depends_on_serde("  serde = { workspace = true }"));
        assert!(depends_on_serde("serde.workspace = true"));
        assert!(!depends_on_serde("serde_json = \"1\""));
        assert!(!depends_on_serde("postcard = \"1\" # serde"));
        assert!(!depends_on_serde(""));
    }

    #[test]
    fn toolchain_channel() {
        let t = "# c\n[toolchain]\nchannel = \"1.98.1\"\ncomponents = [\"x\"]\n";
        assert_eq!(pinned_channel(t).as_deref(), Some("1.98.1"));
        assert_eq!(pinned_channel("channel = \"\"\n"), None);
        assert_eq!(pinned_channel("components = []\n"), None);
    }

    fn gate_of(f: fn(&mut Gate, &Path), fx: &Fixture) -> Gate {
        let mut g = Gate::new("ipc");
        f(&mut g, fx.path());
        g
    }

    #[test]
    fn wire_identity_gate() {
        let fx = Fixture::new("ipc-wire");
        fx.write(
            "crates/px-ipc/src/message.rs",
            "pub struct M {\n    pub frame: u32,\n}\n",
        )
        .commit_all();
        assert!(gate_of(check_wire_identity, &fx).passed());

        let fx = Fixture::new("ipc-wire-bad");
        fx.write(
            "crates/px-ipc/src/message.rs",
            "pub struct M {\n    pub sender: u32,\n}\n",
        )
        .commit_all();
        assert_eq!(gate_of(check_wire_identity, &fx).fail_count(), 2);

        let fx = Fixture::new("ipc-wire-none");
        fx.write("README.md", "x").commit_all();
        assert_eq!(gate_of(check_wire_identity, &fx).fail_count(), 1);
    }

    #[test]
    fn channel_id_gate() {
        let fx = Fixture::new("ipc-chan");
        fx.write(
            "crates/px-broker/Cargo.toml",
            "[dependencies]\npostcard = \"1\"\n",
        )
        .write(
            "crates/px-broker/src/lib.rs",
            "pub struct ChannelId(u64);\n",
        )
        .commit_all();
        assert!(gate_of(check_channel_id, &fx).passed());

        let fx = Fixture::new("ipc-chan-serde");
        fx.write("crates/px-broker/Cargo.toml", "serde = \"1\"\n")
            .write(
                "crates/px-broker/src/lib.rs",
                "#[derive(Serialize)]\npub struct ChannelId(u64);\n",
            )
            .commit_all();
        // serde dependency (2) + serialisable (1).
        assert_eq!(gate_of(check_channel_id, &fx).fail_count(), 3);

        let fx = Fixture::new("ipc-chan-gone");
        fx.write("crates/px-broker/Cargo.toml", "x = 1\n")
            .write("crates/px-broker/src/lib.rs", "struct Other;\n")
            .commit_all();
        assert_eq!(gate_of(check_channel_id, &fx).fail_count(), 2);
    }

    #[test]
    fn not_serializable_findings_cover_derive_impl_and_missing_subject() {
        let f = |t: &str| vec![("b.rs".to_string(), t.to_string())];
        assert!(
            not_serializable_findings(
                "ChannelId",
                &f("#[derive(Clone)]\npub struct ChannelId(u64);\n")
            )
            .is_empty()
        );
        assert_eq!(
            not_serializable_findings(
                "ChannelId",
                &f("#[derive(Serialize)]\npub struct ChannelId(u64);\n")
            )
            .len(),
            1
        );
        let t = "pub struct ChannelId(u64);\nimpl serde::Serialize for ChannelId {\n}\n";
        assert_eq!(not_serializable_findings("ChannelId", &f(t)).len(), 1);
        let e = not_serializable_findings("ChannelId", &f("struct Other;\n"));
        assert_eq!(e.len(), 1);
        assert!(e[0].starts_with("ChannelId was not declared in any of: b.rs"));
    }

    #[test]
    fn suite_problem_wording_is_this_gates_own() {
        let mut g = Gate::new("ipc");
        report_problem(&mut g, "i", "f_", "h.rs", &Problem::NoAttr, "");
        assert_eq!(
            g.failures(),
            [
                "gate item 'i' has no #[test] fn matching 'f_' in h.rs;",
                "  matching tests elsewhere do not substitute for that file"
            ]
        );
        let mut g = Gate::new("ipc");
        report_problem(
            &mut g,
            "i",
            "f_",
            "h.rs",
            &Problem::Ignored(1),
            "x::f_a: test\ny: test\n",
        );
        assert_eq!(
            g.failures(),
            [
                "gate item 'i' has 1 #[ignore]d test(s); a gate item",
                "  whose tests do not run is not a gate item"
            ]
        );
        let mut g = Gate::new("ipc");
        report_problem(&mut g, "i", "f_", "h.rs", &Problem::NoTests, "");
        assert_eq!(g.failures(), ["gate item 'i' has no tests matching 'f_'"]);
    }
}
