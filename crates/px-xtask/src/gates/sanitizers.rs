//! `cargo xtask sanitizers` -- sanitizers over the audited unsafe core (ADR
//! 011, build-spec §4.5). Port of `ci/gate-sanitizers.sh`.
//!
//! ```text
//! ASAN  px-sandbox, px-broker    Linux and Windows
//! TSAN  px-sandbox, px-broker    Linux only -- TSAN has no Windows support
//! ```
//!
//! ADR 011 amends ADR 006's nightly fence to admit this one extra consumer.
//! The fence itself is untouched: the toolchain is named explicitly with
//! `+name`, so the repository root still resolves to stable and `fuzz-smoke`
//! still asserts it, unmodified.
//!
//! ```text
//! cargo xtask sanitizers               run the sanitizers
//! cargo xtask sanitizers --self-check  prove ASAN can still detect a fault
//! ```
//!
//! The self-check exists because a sanitizer that cannot detect anything is
//! worse than no sanitizer: it converts an absence of evidence into apparent
//! assurance. ADR 011's Verification section names that as the way this
//! decision would be wrong, so the check is runnable rather than a claim in a
//! document.
//!
//! Needs the pinned nightly (with `rust-src` for TSAN's `-Z build-std`), so it
//! cannot run end to end on a stable-only machine. `cargo +<nightly>` goes
//! through the rustup proxy `cargo` on `PATH`, with `CARGO` and
//! `RUSTUP_TOOLCHAIN` removed from the child environment (`cargo xtask` sets
//! both for the stable toolchain).

use crate::Gate;
use crate::proc;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET_DIR: &str = "target/sanitizers";
const PROBE: &str = "crates/px-sandbox/src/sanitizer_self_check.rs";
const LIB: &str = "crates/px-sandbox/src/lib.rs";

/// What the self-check's sanitizer report must contain.
const ASAN_OVERFLOW: &str = "AddressSanitizer: heap-buffer-overflow";

/// The deliberate fault. Never committed with the module declaration active:
/// the gate writes this file, appends the declaration, runs ASAN, and restores
/// `lib.rs`.
const PROBE_SOURCE: &str = "\
//! Deliberate fault, compiled only by `cargo xtask sanitizers --self-check`.
//!
//! Never committed with the module declaration active. The gate writes this
//! file, appends the declaration, runs ASAN, and restores lib.rs.
#[test]
fn sanitizer_self_check_reads_past_the_end_of_a_heap_buffer() {
    let buffer: Vec<u8> = vec![0u8; 8];
    // SAFETY: none. This is the point \u{2014} the read is out of bounds on purpose,
    // to prove the sanitizer is armed.
    let past_the_end = unsafe { *buffer.as_ptr().add(64) };
    println!(\"{past_the_end}\");
}
";

/// Appended to `lib.rs` for the duration of the self-check.
const MOD_DECL: &str = "\n#[cfg(test)]\nmod sanitizer_self_check;\n";

#[derive(Debug, PartialEq, Eq)]
pub struct Opts {
    pub self_check: bool,
}

pub fn parse_args(args: &[String]) -> Result<Opts, String> {
    match args {
        [] => Ok(Opts { self_check: false }),
        [a] if a == "--self-check" => Ok(Opts { self_check: true }),
        _ => Err(format!(
            "unexpected arguments {args:?} (only --self-check is accepted)"
        )),
    }
}

/// The one nightly this project has, from `fuzz/rust-toolchain.toml`. Read
/// rather than duplicated: two pins that drift apart would be two nightlies,
/// which is exactly what ADR 011 says it will not create.
///
/// The script's `sed -n 's/^channel *= *"\(.*\)"/\1/p'`: on the first line
/// that is `channel`, optional spaces, `=`, optional spaces, a quote, then
/// anything up to the *last* quote on the line (greedy), the text between;
/// whatever follows that last quote stays on the line, as with `sed`.
pub fn parse_channel(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("channel") else {
            continue;
        };
        let Some(rest) = rest.trim_start_matches(' ').strip_prefix('=') else {
            continue;
        };
        let Some(rest) = rest.trim_start_matches(' ').strip_prefix('"') else {
            continue;
        };
        let Some(last) = rest.rfind('"') else {
            continue;
        };
        let out = format!("{}{}", &rest[..last], &rest[last + 1..]);
        return if out.is_empty() { None } else { Some(out) };
    }
    None
}

/// `rustup toolchain list | grep -q "^$NIGHTLY"`.
pub fn toolchain_installed(list: &str, nightly: &str) -> bool {
    list.lines().any(|l| l.starts_with(nightly))
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum HostOs {
    Linux,
    Windows,
    Other,
}

pub fn host_os() -> HostOs {
    match std::env::consts::OS {
        "linux" => HostOs::Linux,
        "windows" => HostOs::Windows,
        _ => HostOs::Other,
    }
}

pub fn sanitizer_target(os: HostOs) -> Option<&'static str> {
    match os {
        HostOs::Windows => Some("x86_64-pc-windows-msvc"),
        HostOs::Linux => Some("x86_64-unknown-linux-gnu"),
        HostOs::Other => None,
    }
}

/// `RUSTFLAGS` value and cargo arguments (after `cargo`) for each run.
pub fn asan_args(nightly: &str, target: &str) -> Vec<String> {
    strings(&[
        &format!("+{nightly}"),
        "test",
        "-p",
        "px-sandbox",
        "-p",
        "px-broker",
        "--all-targets",
        "--target",
        target,
    ])
}

/// `-Z build-std` because TSAN reports on uninstrumented std otherwise, which
/// is noise rather than findings.
pub fn tsan_args(nightly: &str, target: &str) -> Vec<String> {
    strings(&[
        &format!("+{nightly}"),
        "test",
        "-Z",
        "build-std",
        "-p",
        "px-sandbox",
        "-p",
        "px-broker",
        "--all-targets",
        "--target",
        target,
    ])
}

pub fn self_check_args(nightly: &str, target: &str) -> Vec<String> {
    strings(&[
        &format!("+{nightly}"),
        "test",
        "-p",
        "px-sandbox",
        "--target",
        target,
        "sanitizer_self_check",
    ])
}

fn strings(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

/// Did the sanitizer name the injected fault?
pub fn asan_detected(output: &str) -> bool {
    output.contains(ASAN_OVERFLOW)
}

/// `tail -20`.
pub fn tail_lines(s: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

// ---------------------------------------------------------------------------
// Windows: the ASAN runtime is a DLL that is not on PATH by default.
//
// Without it the test binary exits 0xc0000135 (STATUS_DLL_NOT_FOUND) before
// running a single test, which reads as a broken build rather than a missing
// environment variable. Found the first time this was run locally.

const VSWHERE: &str = r"C:\Program Files (x86)\Microsoft Visual Studio\Installer\vswhere.exe";
const ASAN_DLL: &str = "clang_rt.asan_dynamic-x86_64.dll";

/// First non-empty line of `vswhere` output, `\r` stripped (`tr -d '\r' | head -1`).
pub fn first_install_path(vswhere_stdout: &str) -> Option<String> {
    vswhere_stdout
        .lines()
        .map(|l| l.trim_end_matches('\r').trim())
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// `dir` prepended to a `PATH`-style list.
pub fn prepend_path(
    dir: &Path,
    existing: Option<std::ffi::OsString>,
) -> Option<std::ffi::OsString> {
    let mut parts = vec![dir.to_path_buf()];
    if let Some(e) = existing {
        parts.extend(std::env::split_paths(&e));
    }
    std::env::join_paths(parts).ok()
}

/// `find <root> -name <name>`, last in path order (the newest MSVC toolset
/// directory sorts last), or `None`.
pub fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str, out: &mut Vec<PathBuf>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, name, out);
            } else if p.file_name().is_some_and(|n| n == name) {
                out.push(p);
            }
        }
    }
    let mut v = Vec::new();
    walk(root, name, &mut v);
    v.sort();
    v.pop()
}

/// Returns the new `PATH` to hand to child processes, or reports a failure.
fn windows_asan_path(g: &mut Gate) -> Option<std::ffi::OsString> {
    let vswhere = Path::new(VSWHERE);
    if !vswhere.is_file() {
        g.fail("vswhere.exe not found; cannot locate the ASAN runtime");
        return None;
    }
    let vs = |extra: &[&str]| -> String {
        let mut args = vec!["-latest", "-products", "*"];
        args.extend_from_slice(extra);
        args.extend_from_slice(&["-property", "installationPath"]);
        proc::capture(VSWHERE, &args, None)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default()
    };
    let install = first_install_path(&vs(&[])).or_else(|| {
        // BuildTools installs are not always reported by the default query.
        first_install_path(&vs(&["-all", "-prerelease"]))
    });
    let Some(install) = install else {
        g.fail("vswhere reported no Visual Studio installation");
        return None;
    };
    let msvc = Path::new(&install).join("VC").join("Tools").join("MSVC");
    let Some(dll) = find_file(&msvc, ASAN_DLL) else {
        g.fail(format!("{ASAN_DLL} not found under {install}"));
        g.fail("  install the 'C++ AddressSanitizer' component");
        return None;
    };
    g.info(format!("ASAN runtime: {}", dll.display()));
    prepend_path(dll.parent()?, std::env::var_os("PATH"))
}

// ---------------------------------------------------------------------------

/// A `cargo +<nightly> ...` child, with the sanitizer environment.
fn sanitizer_cargo(
    root: &Path,
    args: &[String],
    rustflags: &str,
    path: &Option<std::ffi::OsString>,
) -> Command {
    let mut c = Command::new("cargo");
    c.args(args)
        .current_dir(root)
        .env_remove("CARGO")
        .env_remove("RUSTUP_TOOLCHAIN")
        // Sanitized artifacts must not be mistaken for shippable ones, and
        // must not evict the ordinary build cache.
        .env("CARGO_TARGET_DIR", TARGET_DIR)
        .env("RUSTFLAGS", rustflags);
    if let Some(p) = path {
        c.env("PATH", p);
    }
    c
}

/// Restores `lib.rs` and removes the probe when dropped, so an interruption
/// between injecting the fault and cleaning up cannot leave the tree dirty.
struct Injected<'a> {
    lib: PathBuf,
    probe: PathBuf,
    original: &'a str,
}

impl Drop for Injected<'_> {
    fn drop(&mut self) {
        let _ = std::fs::write(&self.lib, self.original);
        let _ = std::fs::remove_file(&self.probe);
    }
}

/// The self-check. Injects a heap overflow, asserts ASAN names it, reverts.
fn self_check(
    g: &mut Gate,
    root: &Path,
    nightly: &str,
    target: &str,
    path: &Option<std::ffi::OsString>,
) {
    let lib = root.join(LIB);
    let probe = root.join(PROBE);
    let original = match std::fs::read_to_string(&lib) {
        Ok(s) => s,
        Err(e) => {
            g.fail(format!("cannot read {LIB}: {e}"));
            return;
        }
    };

    let out = {
        let _restore = Injected {
            lib: lib.clone(),
            probe: probe.clone(),
            original: &original,
        };
        if let Err(e) = std::fs::write(&probe, PROBE_SOURCE)
            .and_then(|_| std::fs::write(&lib, format!("{original}{MOD_DECL}")))
        {
            g.fail(format!("cannot inject the probe: {e}"));
            return;
        }
        g.info("running ASAN against a deliberate heap overflow");
        // `2>&1 || true`: the run is expected to fail; only its report matters.
        let o = sanitizer_cargo(
            root,
            &self_check_args(nightly, target),
            "-Zsanitizer=address",
            path,
        )
        .stdin(std::process::Stdio::null())
        .output();
        match o {
            Ok(o) => format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
            Err(_) => String::new(),
        }
    };

    if asan_detected(&out) {
        g.ok("ASAN detected the injected fault; the sanitizer is armed");
    } else {
        g.fail("ASAN did NOT detect an injected heap overflow");
        g.fail("  a green sanitizer that cannot detect a fault is worse than none");
        for l in tail_lines(&out, 20) {
            eprintln!("{l}");
        }
    }
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let opts = match parse_args(args) {
        Ok(o) => o,
        Err(e) => {
            g.fail(e);
            return;
        }
    };

    let nightly = std::fs::read_to_string(root.join("fuzz/rust-toolchain.toml"))
        .ok()
        .and_then(|t| parse_channel(&t));
    let Some(nightly) = nightly else {
        g.fail("could not read the pinned nightly from fuzz/rust-toolchain.toml");
        return;
    };
    g.info(format!("pinned nightly: {nightly}"));

    let installed = proc::capture("rustup", &["toolchain", "list"], None)
        .map(|b| toolchain_installed(&String::from_utf8_lossy(&b), &nightly))
        .unwrap_or(false);
    if !installed {
        g.fail(format!(
            "{nightly} is not installed (rustup toolchain install {nightly})"
        ));
        return;
    }

    let os = host_os();
    let mut path = None;
    let Some(target) = sanitizer_target(os) else {
        g.fail(format!(
            "unsupported host for sanitizers: {}",
            std::env::consts::OS
        ));
        return;
    };
    if os == HostOs::Windows {
        match windows_asan_path(g) {
            Some(p) => path = Some(p),
            None => return,
        }
    }
    g.info(format!("target: {target}"));

    if opts.self_check {
        self_check(g, root, &nightly, target, &path);
        return;
    }

    // --all-targets is what excludes doctests, and it is not a tidiness
    // choice. RUSTFLAGS does not reach rustdoc, so a doctest crate is built
    // *without* the sanitizer while its dependencies are built with it, and
    // rustc rejects the mix: "mixing -Zsanitizer will cause an ABI mismatch".
    // Found on the first CI run, where it surfaced as 26 errors in px-broker
    // rather than as anything to do with threads. Sanitizing documentation
    // examples buys nothing anyway; ordinary `cargo test` still runs them,
    // unsanitized, on every push.
    //
    // No -Z build-std for ASAN: it is not needed to find faults in this
    // project's own code, and it triples the build.
    g.info("ASAN: px-sandbox, px-broker");
    let asan_ok = sanitizer_cargo(
        root,
        &asan_args(&nightly, target),
        "-Zsanitizer=address",
        &path,
    )
    .status()
    .map(|s| s.success())
    .unwrap_or(false);
    if asan_ok {
        g.ok("ASAN clean");
    } else {
        // Deliberately not "ASAN reported a fault": this branch is also
        // reached by a build failure, and naming a cause the run did not
        // establish is how a report stops being worth reading.
        g.fail("the ASAN run failed \u{2014} a detected fault, or the build. See above.");
    }

    // TSAN, Linux only -- there is no Windows support.
    if os == HostOs::Linux {
        g.info("TSAN: px-sandbox, px-broker");
        let tsan_ok = sanitizer_cargo(
            root,
            &tsan_args(&nightly, target),
            "-Zsanitizer=thread",
            &path,
        )
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
        if tsan_ok {
            g.ok("TSAN clean");
        } else {
            g.fail("the TSAN run failed \u{2014} a detected race, or the build. See above.");
        }
    } else {
        g.info("TSAN skipped: no Windows support (ADR 011)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn args_parse() {
        assert_eq!(parse_args(&[]).unwrap(), Opts { self_check: false });
        assert_eq!(
            parse_args(&["--self-check".into()]).unwrap(),
            Opts { self_check: true }
        );
        assert!(parse_args(&["--nope".into()]).is_err());
        assert!(parse_args(&["--self-check".into(), "x".into()]).is_err());
    }

    #[test]
    fn channel_extraction_matches_the_sed() {
        assert_eq!(
            parse_channel("[toolchain]\nchannel = \"nightly-2026-09-01\"\n").as_deref(),
            Some("nightly-2026-09-01")
        );
        assert_eq!(parse_channel("channel=\"n\"").as_deref(), Some("n"));
        // Not at the start of the line, or not a quoted value: no match.
        assert_eq!(parse_channel("  channel = \"x\""), None);
        assert_eq!(parse_channel("channel = nightly"), None);
        assert_eq!(parse_channel("# channel = \"x\""), None);
        assert_eq!(parse_channel("channel = \"\""), None);
        assert_eq!(parse_channel(""), None);
        // Greedy to the last quote, and the tail of the line survives, as in sed.
        assert_eq!(
            parse_channel("channel = \"a\" # \"b\" c").as_deref(),
            Some("a\" # \"b c")
        );
        assert_eq!(
            parse_channel("channel = \"a\" # note").as_deref(),
            Some("a # note")
        );
    }

    #[test]
    fn committed_pin_is_found() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/rust-toolchain.toml");
        if let Ok(t) = std::fs::read_to_string(p) {
            let c = parse_channel(&t).unwrap();
            assert!(c.starts_with("nightly-"), "{c}");
        }
    }

    #[test]
    fn installed_is_a_line_prefix_match() {
        let list = "stable-x86_64-unknown-linux-gnu (default)\nnightly-2026-09-01-x86_64-unknown-linux-gnu\n";
        assert!(toolchain_installed(list, "nightly-2026-09-01"));
        assert!(!toolchain_installed(list, "nightly-2026-09-02"));
        assert!(!toolchain_installed(list, "x86_64"));
        assert!(!toolchain_installed("", "nightly-2026-09-01"));
    }

    #[test]
    fn host_targets() {
        assert_eq!(
            sanitizer_target(HostOs::Linux),
            Some("x86_64-unknown-linux-gnu")
        );
        assert_eq!(
            sanitizer_target(HostOs::Windows),
            Some("x86_64-pc-windows-msvc")
        );
        assert_eq!(sanitizer_target(HostOs::Other), None);
    }

    #[test]
    fn command_lines() {
        let n = "nightly-2026-09-01";
        let t = "x86_64-unknown-linux-gnu";
        assert_eq!(
            asan_args(n, t).join(" "),
            "+nightly-2026-09-01 test -p px-sandbox -p px-broker --all-targets --target x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            tsan_args(n, t).join(" "),
            "+nightly-2026-09-01 test -Z build-std -p px-sandbox -p px-broker --all-targets --target x86_64-unknown-linux-gnu"
        );
        assert_eq!(
            self_check_args(n, t).join(" "),
            "+nightly-2026-09-01 test -p px-sandbox --target x86_64-unknown-linux-gnu sanitizer_self_check"
        );
    }

    #[test]
    fn detection_and_tail() {
        assert!(asan_detected(
            "==1==ERROR: AddressSanitizer: heap-buffer-overflow on address 0x1"
        ));
        assert!(!asan_detected("test result: ok. 1 passed"));
        // A different sanitizer finding is not the injected fault.
        assert!(!asan_detected("AddressSanitizer: stack-buffer-overflow"));
        assert!(!asan_detected(""));
        let s: String = (1..=30).map(|i| format!("l{i}\n")).collect();
        let t = tail_lines(&s, 20);
        assert_eq!((t.len(), t[0], t[19]), (20, "l11", "l30"));
        assert_eq!(tail_lines("a\nb", 20), ["a", "b"]);
        assert!(tail_lines("", 20).is_empty());
    }

    #[test]
    fn windows_helpers() {
        assert_eq!(
            first_install_path("C:\\VS\\Two\r\nC:\\VS\\One\r\n").as_deref(),
            Some("C:\\VS\\Two")
        );
        assert_eq!(first_install_path("\r\n\r\n"), None);
        assert_eq!(first_install_path(""), None);

        let f = Fixture::new("san-dll");
        assert!(find_file(f.path(), ASAN_DLL).is_none());
        f.write(
            "14.29.0/bin/Hostx64/x64/clang_rt.asan_dynamic-x86_64.dll",
            "",
        );
        f.write(
            "14.40.0/bin/Hostx64/x64/clang_rt.asan_dynamic-x86_64.dll",
            "",
        );
        let got = find_file(f.path(), ASAN_DLL).unwrap();
        assert!(got.starts_with(f.path().join("14.40.0")));

        let p = prepend_path(Path::new("/dll"), Some("/a:/b".into())).unwrap();
        let parts: Vec<PathBuf> = std::env::split_paths(&p).collect();
        assert_eq!(parts[0], PathBuf::from("/dll"));
        assert!(parts.len() >= 2);
    }

    #[test]
    fn probe_is_a_heap_overflow_test_and_the_decl_is_appended() {
        assert!(
            PROBE_SOURCE.contains("fn sanitizer_self_check_reads_past_the_end_of_a_heap_buffer")
        );
        assert!(PROBE_SOURCE.contains("vec![0u8; 8]"));
        assert!(PROBE_SOURCE.contains(".add(64)"));
        assert_eq!(MOD_DECL, "\n#[cfg(test)]\nmod sanitizer_self_check;\n");
    }

    #[test]
    fn injection_is_always_reverted() {
        let f = Fixture::new("san-inject");
        f.write("crates/px-sandbox/src/lib.rs", "// original\n");
        {
            let lib = f.path().join(LIB);
            let probe = f.path().join(PROBE);
            std::fs::write(&probe, PROBE_SOURCE).unwrap();
            std::fs::write(&lib, "// original\n// modified\n").unwrap();
            let _g = Injected {
                lib: lib.clone(),
                probe: probe.clone(),
                original: "// original\n",
            };
        }
        assert_eq!(
            std::fs::read_to_string(f.path().join(LIB)).unwrap(),
            "// original\n"
        );
        assert!(!f.path().join(PROBE).exists());
    }

    #[test]
    fn unreadable_pin_fails_closed() {
        let f = Fixture::new("san-nopin");
        let mut g = Gate::new("sanitizers");
        run(&mut g, f.path(), &[]);
        assert_eq!(
            g.failures(),
            ["could not read the pinned nightly from fuzz/rust-toolchain.toml"]
        );
    }
}
