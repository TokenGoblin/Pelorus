//! `cargo xtask network` -- the Phase 3 gate (build-spec section 9). Port of
//! `ci/gate-network.sh`.
//!
//! ```text
//! 200 URLs fetched correctly                              -> here
//! 24h fuzz on HTTP framing                                -> campaign (not here)
//! packet capture: zero plaintext DNS, zero connections    -> here
//!   outside the requested set
//! PSL version asserted, and a stale-PSL test fails        -> here
//! ```
//!
//! Written failing, before px-net existed. The phase is done when this passes
//! on both operating systems, and not when the build is green.
//!
//! What this gate deliberately does NOT assert, so that a pass is not read as
//! more than it is:
//!
//! * HTTP/2. ADR 015 defers it and narrows section 9 Phase 3 accordingly. A
//!   gate that silently graded against the smaller target would hide that.
//! * That no plaintext DNS left the machine. Under ADR 014's default the
//!   *operating system* resolves names, so the capture measures this process's
//!   own sockets. The distinction is in ADR 014; repeating it here because a
//!   green line reading "zero plaintext DNS" invites the stronger reading.
//!
//! # Why there is NO release-binary scan here
//!
//! `cargo xtask sandbox` scans release binaries for
//! `PX_TEST_FORCE_SANDBOX_UNAVAILABLE`, and that scan is meaningful, because the
//! override is a string literal that lands in the binary when it is compiled in.
//! The obvious equivalent here would be to grep for `client_config_trusting`
//! (section 14.4 and the test-only trust anchor).
//!
//! It was written, and then measured, and it cannot fail. Two independent
//! reasons: px-browser and px-content do not link px-net at all yet, and a `pub
//! fn` nothing calls is eliminated in a release build even when its feature is
//! on. Building with `--features testing` and grepping finds nothing either way.
//!
//! A check that cannot fail is worse than no check: it reads as assurance and
//! provides none. So the guarantee here is the one the compiler actually gives
//! -- `client_config_trusting` is `#[cfg(feature = "testing")]` and does not
//! exist in a build without the feature -- and this comment is the record that
//! the scan was considered, tested, and found vacuous. Add the scan when px-net
//! is linked into a shipping binary, which is when it would start being able to
//! fail. Until then it would be theatre.

use super::suites::{self, Suite};
use crate::Gate;
use crate::scan::{self, is_word};
use std::path::Path;

/// Gate items as named tests, with the three conditions Phases 1 and 2 taught
/// this gate to apply (see `suites`). The third is not paranoia: an
/// `#[ignore]`d test satisfies a grep and proves nothing, and a gate that
/// counts it is a gate that reports work nobody did.
pub const SUITES: &[Suite] = &[
    (
        "two-hundred-urls",
        "net_fetches",
        "crates/px-net/tests/fetch.rs",
    ),
    ("psl-freshness", "psl_version", "crates/px-net/tests/psl.rs"),
    (
        "no-unrequested-connections",
        "net_connects_only",
        "crates/px-net/tests/connections.rs",
    ),
    (
        "partitioned-pools",
        "net_partitions",
        "crates/px-net/tests/partition.rs",
    ),
];

/// The 200 URLs are fetched against recorded traffic and a local server, not
/// against the live web. A gate that needs the internet is a gate that goes red
/// when a third party has an outage, and one that has been red for a reason
/// nobody controls is one people learn to ignore. Recorded archives also make
/// the corpus a fixed target: "200 URLs fetched correctly" means the same thing
/// in a year.
///
/// Deliberately NOT `tests/compat/archives`. That directory holds recorded
/// traffic from real sites and is gitignored on purpose -- ADR 002 -- because
/// those archives name every host they replay. This corpus is generated, every
/// host in it is the loopback address, and it is committed so the gate has a
/// fixed target. An earlier version of this gate pointed at the compat
/// directory and therefore required files that can never exist in CI.
pub const ARCHIVE: &str = "tests/net/corpus";
pub const ARCHIVE_FLOOR: usize = 200;

/// Crates allowed to name a socket type (invariant 4).
pub const NETWORK_CRATES: &[&str] = &["px-net", "px-update"];

/// The PSL is a security boundary, not a data file. A stale one silently
/// mis-partitions state -- invariant 2 -- and nothing observable breaks, which
/// is the worst shape a defect can have.
pub const PSL: &str = "data/public_suffix_list.dat";
pub const PSL_VERSION: &str = "data/public_suffix_list.version";

/// 24h fuzz on HTTP framing: same shape as Phase 1's IPC campaign -- a smoke
/// run here, the long campaign on a schedule, because a gate that takes a day
/// to report is a gate that gets bypassed.
pub const FUZZ_TARGETS: &[&str] = &["http_response", "http_chunked"];

/// The four std socket types, spelled with `concat!` so that this file (which
/// the scan below reaches, px-xtask being a crate like any other) does not name
/// them itself.
const SOCKET_TYPES: &[&str] = &[
    concat!("Tcp", "Stream"),
    concat!("Tcp", "Listener"),
    concat!("Udp", "Socket"),
    concat!("To", "SocketAddrs"),
];

/// The regex `\b(<the four socket types>)\b` anywhere on the line.
/// Comment lines are not excluded: naming a socket type in prose is also a
/// reason to look.
pub fn names_socket_type(line: &str) -> bool {
    SOCKET_TYPES.iter().any(|w| {
        line.match_indices(w).any(|(i, m)| {
            line[..i].chars().next_back().is_none_or(|c| !is_word(c))
                && line[i + m.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !is_word(c))
        })
    })
}

fn is_rs(path: &str) -> bool {
    path.ends_with(".rs")
}

fn check_corpus(g: &mut Gate, root: &Path) {
    let dir = root.join(ARCHIVE);
    if !dir.is_dir() {
        g.fail(format!(
            "{ARCHIVE} does not exist; run ci/make-fetch-corpus.py"
        ));
        return;
    }
    let count = scan::walk_entries(&dir)
        .iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".har"))
        })
        .count();
    if count < ARCHIVE_FLOOR {
        g.fail(format!(
            "the corpus holds {count} responses; §9 Phase 3 asks for {ARCHIVE_FLOOR}"
        ));
    } else {
        g.ok(format!("the corpus holds {count} generated responses"));
    }
}

/// Invariant 4, mechanically: only px-net and px-update may open a socket.
///
/// A test can show that the code we wrote does not connect elsewhere. It cannot
/// show that no other crate ever will. This is the symbol audit build-spec
/// section 3 names, and it is the half that keeps holding as the project grows.
fn check_network_confinement(g: &mut Gate, root: &Path) {
    g.info("no crate outside px-net and px-update reaches the network");
    let mut crates: Vec<String> = std::fs::read_dir(root.join("crates"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    crates.sort();
    let mut leaked = false;
    for name in crates {
        if NETWORK_CRATES.contains(&name.as_str()) {
            continue;
        }
        let hits = scan::grep_rn(
            root,
            &format!("crates/{name}/src"),
            is_rs,
            &names_socket_type,
        );
        if !hits.is_empty() {
            g.fail(format!(
                "{name} names a socket type; invariant 4 confines the network to px-net and px-update"
            ));
            leaked = true;
        }
    }
    if !leaked {
        g.ok("the network is confined to px-net and px-update");
    }
}

fn check_psl(g: &mut Gate, root: &Path) {
    if !root.join(PSL).is_file() {
        g.fail(format!("{PSL} does not exist"));
    } else if !root.join(PSL_VERSION).is_file() {
        g.fail(format!(
            "{PSL_VERSION} does not exist; a list with no version cannot be asserted stale"
        ));
    } else {
        g.ok("the PSL is present and versioned");
    }
}

fn check_fuzz_targets(g: &mut Gate, root: &Path) {
    for target in FUZZ_TARGETS {
        if !root
            .join(format!("fuzz/fuzz_targets/{target}.rs"))
            .is_file()
        {
            g.fail(format!(
                "fuzz target {target} does not exist; §9 Phase 3 requires HTTP framing fuzzed"
            ));
        } else {
            g.ok(format!("fuzz target {target} exists"));
        }
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if !suites::cargo_available(root) {
        g.fail("cargo is not on PATH");
        return;
    }
    suites::check_suites(g, root, SUITES);
    suites::run_workspace_tests(g, root);
    check_corpus(g, root);
    check_network_confinement(g, root);
    check_psl(g, root);
    check_fuzz_targets(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn gate_of(f: fn(&mut Gate, &Path), fx: &Fixture) -> Gate {
        let mut g = Gate::new("network");
        f(&mut g, fx.path());
        g
    }

    #[test]
    fn socket_type_is_whole_word() {
        let (tcp, udp, lis, addrs) = (
            concat!("Tcp", "Stream"),
            concat!("Udp", "Socket"),
            concat!("Tcp", "Listener"),
            concat!("To", "SocketAddrs"),
        );
        assert!(names_socket_type(&format!("use std::net::{tcp};")));
        assert!(names_socket_type(&format!("fn f(a: &{udp})")));
        assert!(names_socket_type(&format!(
            "x.to_socket_addrs() // {addrs}"
        )));
        assert!(names_socket_type(lis));
        assert!(!names_socket_type(&format!("My{tcp}")));
        assert!(!names_socket_type(&format!("{tcp}2")));
        assert!(!names_socket_type(&format!("{tcp}_x")));
        assert!(!names_socket_type("a socket"));
    }

    #[test]
    fn corpus_floor_counts_har_files() {
        let fx = Fixture::new("net-corpus");
        assert_eq!(gate_of(check_corpus, &fx).fail_count(), 1);
        for i in 0..ARCHIVE_FLOOR - 1 {
            fx.write(&format!("{ARCHIVE}/d/{i}.har"), "x");
        }
        fx.write(&format!("{ARCHIVE}/README.md"), "x");
        let g = gate_of(check_corpus, &fx);
        assert_eq!(
            g.failures(),
            ["the corpus holds 199 responses; §9 Phase 3 asks for 200"]
        );
        fx.write(&format!("{ARCHIVE}/last.har"), "x");
        assert!(gate_of(check_corpus, &fx).passed());
    }

    #[test]
    fn confinement_flags_other_crates_only() {
        let fx = Fixture::new("net-confine");
        let tcp = concat!("Tcp", "Stream");
        let udp = concat!("Udp", "Socket");
        fx.write(
            "crates/px-net/src/lib.rs",
            &format!("use std::net::{tcp};\n"),
        )
        .write("crates/px-update/src/a.rs", &format!("{udp}\n"))
        .write("crates/px-dom/src/lib.rs", "fn x() {}\n")
        .write(
            "crates/px-dom/tests/t.rs",
            &format!("{tcp} in tests is not scanned\n"),
        )
        .write(
            "crates/px-dom/src/notes.txt",
            &format!("{tcp} in a txt file\n"),
        );
        assert!(gate_of(check_network_confinement, &fx).passed());
        fx.write(
            "crates/px-content/src/deep/mod.rs",
            &format!("// {} in a comment\n", concat!("Tcp", "Listener")),
        );
        let g = gate_of(check_network_confinement, &fx);
        assert_eq!(
            g.failures(),
            [
                "px-content names a socket type; invariant 4 confines the network to px-net and px-update"
            ]
        );
    }

    #[test]
    fn psl_needs_both_files() {
        let fx = Fixture::new("net-psl");
        assert_eq!(
            gate_of(check_psl, &fx).failures(),
            ["data/public_suffix_list.dat does not exist"]
        );
        fx.write(PSL, "x");
        assert_eq!(
            gate_of(check_psl, &fx).failures(),
            [
                "data/public_suffix_list.version does not exist; a list with no version cannot be asserted stale"
            ]
        );
        fx.write(PSL_VERSION, "x");
        assert!(gate_of(check_psl, &fx).passed());
    }

    #[test]
    fn fuzz_targets_must_exist() {
        let fx = Fixture::new("net-fuzz");
        fx.write("fuzz/fuzz_targets/http_response.rs", "x");
        let g = gate_of(check_fuzz_targets, &fx);
        assert_eq!(
            g.failures(),
            ["fuzz target http_chunked does not exist; §9 Phase 3 requires HTTP framing fuzzed"]
        );
    }
}
