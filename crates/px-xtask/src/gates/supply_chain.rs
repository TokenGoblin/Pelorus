//! `cargo xtask supply-chain` -- gate check 3, supply chain (build-spec §5,
//! §4.5). Port of `ci/gate-supply-chain.sh`.
//!
//! ```text
//! cargo deny       licence and advisory policy
//! cargo vet        audit coverage, importing Mozilla's and Google's sets
//! cargo auditable  SBOM embedded in the shipped artifact, asserted present
//! unsafe audit     dependency-tree unsafe count against a committed baseline
//! ```
//!
//! The external tools stay external: they run as child processes, exactly as
//! the script ran them. What is decided here, in pure functions, is whether a
//! tool is on `PATH`, whether a built binary carries the SBOM section, and
//! how the unsafe census compares with the baseline.
//!
//! HONEST NOTE, true for Phase 0 only: the workspace has zero third-party
//! dependencies, so deny, vet and the unsafe baseline are all trivially
//! green. They prove the harness runs. They prove nothing about any
//! dependency until Phase 3 brings the first one in. That is the right time
//! to wire them and the wrong time to read a pass as assurance.
//!
//! The census itself is still `ci/unsafe-audit.sh` (a separate port); this
//! gate runs it and reads its JSON on stdout.

use crate::Gate;
use crate::json::{self, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BASELINE: &str = "ci/unsafe-baseline.json";

/// Into a separate target directory, always. Cargo will not re-link a binary
/// that is already up to date, so if a plain `cargo build` ran first the
/// auditable build is a no-op and the SBOM assertion fails on a stale artifact
/// for a reason that has nothing to do with the SBOM.
const AUDITABLE_DIR: &str = "target/auditable";

/// The release binaries that must carry the SBOM.
const SBOM_BINARIES: &[&str] = &["px-browser", "px-content"];

/// cargo-auditable writes zlib-compressed JSON into a section named `.dep-v0`,
/// so the section name is what is readable, not the contents. A flag that was
/// passed is not evidence; a section in the binary is.
const SBOM_MARKER: &[u8] = b"dep-v0";

// ---------------------------------------------------------------------------
// Pure decisions

/// `command -v name`: the first directory in `path_dirs` holding an
/// executable file called `name` (or `name.exe`, for Windows).
pub fn find_in_path(name: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    for d in path_dirs {
        for cand in [name.to_string(), format!("{name}.exe")] {
            let p = d.join(cand);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

/// `grep -a -q 'dep-v0' bin`.
pub fn has_sbom(binary: &[u8]) -> bool {
    binary.windows(SBOM_MARKER.len()).any(|w| w == SBOM_MARKER)
}

/// First existing of `<dir>/<stem>.exe`, `<dir>/<stem>`.
pub fn locate_binary(dir: &Path, stem: &str) -> Option<PathBuf> {
    [format!("{stem}.exe"), stem.to_string()]
        .iter()
        .map(|n| dir.join(n))
        .find(|p| p.is_file())
}

pub type Census = BTreeMap<String, u64>;

/// Parse the flat `{ "crate-version": count }` object the census emits.
pub fn parse_census(text: &str) -> Result<Census, String> {
    let Value::Object(pairs) = json::parse(text)? else {
        return Err("not a JSON object".into());
    };
    let mut m = Census::new();
    for (k, v) in pairs {
        let Value::Num(n) = v else {
            return Err(format!("`{k}` is not a number"));
        };
        let n: u64 = n.parse().map_err(|_| format!("`{k}`: bad count `{n}`"))?;
        m.insert(k, n);
    }
    Ok(m)
}

/// The outcome of comparing the current census with the baseline.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Comparison {
    /// Lines printed as `FAIL ...`; any entry fails the check.
    pub failures: Vec<String>,
    /// Lines printed as `note ...`; movement downwards, never fatal.
    pub notes: Vec<String>,
}

/// Any increase fails and needs an ADR; a decrease only asks for the baseline
/// to be regenerated.
pub fn compare(base: &Census, cur: &Census) -> Comparison {
    let mut c = Comparison::default();
    for (krate, n) in cur {
        match base.get(krate) {
            None => c
                .failures
                .push(format!("new crate with unsafe: {krate} ({n})")),
            Some(was) if n > was => c
                .failures
                .push(format!("{krate} unsafe count {was} -> {n}")),
            Some(_) => {}
        }
    }
    for (krate, n) in base {
        match cur.get(krate) {
            None => c.notes.push(format!(
                "baseline crate no longer present: {krate} ({n}) \u{2014} regenerate the baseline"
            )),
            Some(now) if now < n => c.notes.push(format!(
                "{krate} unsafe count fell {n} -> {now} \u{2014} regenerate the baseline"
            )),
            Some(_) => {}
        }
    }
    c
}

// ---------------------------------------------------------------------------
// Steps that run tools

fn cargo_program() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

fn need(g: &mut Gate, tool: &str, hint: &str) -> bool {
    let dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if find_in_path(tool, &dirs).is_some() {
        return true;
    }
    g.fail(format!("{tool} is not installed ({hint})"));
    false
}

/// Run `cargo <args>` in `root`, output inherited; true on exit status 0.
fn cargo_ok(root: &Path, args: &[&str], target_dir: Option<&str>) -> bool {
    let mut c = Command::new(cargo_program());
    c.args(args).current_dir(root);
    if let Some(t) = target_dir {
        c.env("CARGO_TARGET_DIR", t);
    }
    c.status().map(|s| s.success()).unwrap_or(false)
}

fn sbom_step(g: &mut Gate, root: &Path) {
    // `cargo auditable build` honours CARGO_TARGET_DIR relative to its cwd.
    g.info(format!(
        "cargo auditable build --release (into {AUDITABLE_DIR})"
    ));
    if !cargo_ok(
        root,
        &["auditable", "build", "--workspace", "--locked", "--release"],
        Some(AUDITABLE_DIR),
    ) {
        g.fail("auditable build failed");
    }

    check_binaries(g, &root.join(AUDITABLE_DIR).join("release"));
}

/// Assert the SBOM reached each shipped artifact in `release`.
pub fn check_binaries(g: &mut Gate, release: &Path) {
    let mut checked = 0;
    for stem in SBOM_BINARIES {
        let Some(bin) = locate_binary(release, stem) else {
            g.fail(format!("{stem} was not built"));
            continue;
        };
        checked += 1;
        let name = bin
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        match std::fs::read(&bin) {
            Ok(b) if has_sbom(&b) => g.ok(format!("{name} carries an embedded SBOM")),
            // An unreadable binary cannot be shown to carry the SBOM: fail closed.
            _ => g.fail(format!("{name} has no .dep-v0 SBOM section")),
        }
    }
    if checked == 0 {
        g.fail("no release binaries found to check for an SBOM");
    }
}

fn unsafe_step(g: &mut Gate, root: &Path) {
    let baseline_path = root.join(BASELINE);
    if !baseline_path.is_file() {
        g.fail(format!(
            "{BASELINE} is missing \u{2014} there is no committed unsafe baseline to compare against"
        ));
        return;
    }
    g.info(format!("unsafe audit against {BASELINE}"));
    let out = Command::new("bash")
        .arg("ci/unsafe-audit.sh")
        .current_dir(root)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output();
    let current = match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => {
            g.fail("unsafe audit could not run");
            return;
        }
    };
    let parsed = std::fs::read_to_string(&baseline_path)
        .map_err(|e| format!("cannot read {BASELINE}: {e}"))
        .and_then(|t| parse_census(&t).map_err(|e| format!("{BASELINE}: {e}")))
        .and_then(|b| {
            parse_census(&current)
                .map_err(|e| format!("census output: {e}"))
                .map(|c| (b, c))
        });
    match parsed {
        Ok((base, cur)) => {
            let cmp = compare(&base, &cur);
            for f in &cmp.failures {
                eprintln!("FAIL {f}");
            }
            for n in &cmp.notes {
                eprintln!("note {n}");
            }
            if !cmp.failures.is_empty() {
                g.fail("dependency-tree unsafe count increased");
            }
        }
        Err(e) => {
            g.detail(e);
            g.fail("dependency-tree unsafe count increased");
        }
    }
}

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    if need(g, "cargo-deny", "cargo install --locked cargo-deny") {
        g.info("cargo deny check");
        if !cargo_ok(root, &["deny", "check"], None) {
            g.fail("cargo deny found policy violations");
        }
    }

    if need(g, "cargo-vet", "cargo install --locked cargo-vet") {
        g.info("cargo vet --locked");
        if !cargo_ok(root, &["vet", "--locked"], None) {
            g.fail("cargo vet found unaudited dependencies");
        }
    }

    if need(
        g,
        "cargo-auditable",
        "cargo install --locked cargo-auditable",
    ) {
        sbom_step(g, root);
    }

    unsafe_step(g, root);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn census(pairs: &[(&str, u64)]) -> Census {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    #[test]
    fn sbom_marker_detection() {
        assert!(has_sbom(b"\x7fELF....\0.dep-v0\0more"));
        assert!(!has_sbom(b"\x7fELF no section here"));
        assert!(!has_sbom(b""));
        assert!(!has_sbom(b"dep-v"));
    }

    #[test]
    fn finds_binaries_exe_first() {
        let f = Fixture::new("sc-bin");
        assert!(locate_binary(f.path(), "px-browser").is_none());
        f.write("px-browser", "x");
        assert_eq!(
            locate_binary(f.path(), "px-browser").unwrap(),
            f.path().join("px-browser")
        );
        f.write("px-browser.exe", "x");
        assert_eq!(
            locate_binary(f.path(), "px-browser").unwrap(),
            f.path().join("px-browser.exe")
        );
    }

    #[test]
    fn path_lookup() {
        let f = Fixture::new("sc-path");
        f.write("a/cargo-vet", "");
        f.write("b/cargo-deny.exe", "");
        let dirs = vec![
            f.path().join("missing"),
            f.path().join("a"),
            f.path().join("b"),
        ];
        assert!(find_in_path("cargo-vet", &dirs).is_some());
        assert!(find_in_path("cargo-deny", &dirs).is_some());
        assert!(find_in_path("cargo-auditable", &dirs).is_none());
        assert!(find_in_path("cargo-vet", &[]).is_none());
    }

    #[test]
    fn unsafe_comparison() {
        let base = census(&[("a-1", 10), ("b-1", 5), ("gone-1", 3), ("same-1", 7)]);
        let cur = census(&[("a-1", 12), ("b-1", 4), ("new-1", 1), ("same-1", 7)]);
        let c = compare(&base, &cur);
        assert_eq!(
            c.failures,
            [
                "a-1 unsafe count 10 -> 12",
                "new crate with unsafe: new-1 (1)"
            ]
        );
        assert_eq!(
            c.notes,
            [
                "b-1 unsafe count fell 5 -> 4 \u{2014} regenerate the baseline",
                "baseline crate no longer present: gone-1 (3) \u{2014} regenerate the baseline"
            ]
        );
        // Equal and shrinking-only censuses do not fail.
        assert!(compare(&base, &base).failures.is_empty());
        assert!(compare(&base, &census(&[])).failures.is_empty());
        // Nothing to compare is not a failure either (Phase 0 workspace).
        assert_eq!(compare(&census(&[]), &census(&[])), Comparison::default());
    }

    #[test]
    fn census_parsing() {
        let c = parse_census("{\n  \"a-1.0.0\": 3,\n  \"b-2\": 40\n}\n").unwrap();
        assert_eq!(c, census(&[("a-1.0.0", 3), ("b-2", 40)]));
        assert_eq!(parse_census("{}\n").unwrap(), census(&[]));
        assert!(parse_census("[]").is_err());
        assert!(parse_census("{\"a\": \"x\"}").is_err());
        assert!(parse_census("{\"a\": -1}").is_err());
        assert!(parse_census("").is_err());
    }

    #[test]
    fn committed_baseline_parses() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../ci/unsafe-baseline.json");
        if let Ok(t) = std::fs::read_to_string(p) {
            let c = parse_census(&t).unwrap();
            assert!(!c.is_empty());
            assert!(compare(&c, &c).failures.is_empty());
        }
    }

    #[test]
    fn missing_baseline_fails_closed() {
        let f = Fixture::new("sc-nobase");
        let mut g = Gate::new("t");
        unsafe_step(&mut g, f.path());
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("is missing"));
    }

    #[test]
    fn sbom_assertion_over_a_release_dir() {
        let f = Fixture::new("sc-sbom");
        // Nothing built: each is reported, and so is the empty total.
        let mut g = Gate::new("t");
        check_binaries(&mut g, f.path());
        assert_eq!(
            g.failures(),
            [
                "px-browser was not built",
                "px-content was not built",
                "no release binaries found to check for an SBOM"
            ]
        );
        // One with the section, one without.
        f.write_bytes("px-browser", b"..dep-v0..");
        f.write_bytes("px-content.exe", b"plain binary");
        let mut g = Gate::new("t");
        check_binaries(&mut g, f.path());
        assert_eq!(g.failures(), ["px-content.exe has no .dep-v0 SBOM section"]);
        // Both fine.
        f.write_bytes("px-content.exe", b"x dep-v0 x");
        let mut g = Gate::new("t");
        check_binaries(&mut g, f.path());
        assert!(g.passed());
    }
}
