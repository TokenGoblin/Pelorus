//! The pinned Python environment `stylo`'s `build.rs` needs (ADR 023, ADR 032).
//!
//! This is the one named exception to "the project is 100% Rust": `stylo`'s
//! `build.rs` generates its property definitions by running
//! `properties/build.py` through Mako, so a Python 3 interpreter and a Mako
//! version are build inputs, and invariant 7 reads "same source + same
//! toolchain + same pinned build-time generators". The interpreter stays a
//! child process because the dependency needs it; every decision around it is
//! here.
//!
//! The pin is *materialised* rather than described. `build-python/requirements.txt`
//! names exact versions; the environment built from it contains exactly those
//! and nothing else, so a Mako that happens to be installed system-wide cannot
//! be the one that generates the release binary's contents.
//!
//! Replaces `ci/check_build_python_pin.py` (this file's `check_pin`),
//! `ci/setup-build-python.sh` (`ensure_pinned_python`) and the `PYTHON3` block
//! of `ci/lib.sh` (`pinned_python_lenient`, `apply_pin`, used by
//! `proc::cargo`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Parse `name==version` lines into a map, rejecting anything inexact.
///
/// A range or a bare name would reintroduce exactly what ADR 023 closed: a
/// release binary whose contents depend on what happened to be installed on the
/// machine that built it. Names are normalised (PEP 503, minus the parts that
/// cannot appear here): pip reports `MarkupSafe`, a requirements file may say
/// `markupsafe`, and they are the same package.
///
/// Comparing parsed name/version pairs cannot be defeated by a line ending. The
/// comparison this replaces was written in shell and was wrong four times,
/// because `pip freeze` emits CRLF on Windows.
pub fn parse_pins(text: &str, source: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((name, version)) = line.split_once("==") else {
            return Err(format!("{source}: '{line}' is not an exact pin"));
        };
        out.insert(
            name.trim().to_lowercase().replace('_', "-"),
            version.trim().to_string(),
        );
    }
    Ok(out)
}

/// Outcome of comparing an environment against the pin.
#[derive(Debug, PartialEq, Eq)]
pub enum PinCheck {
    /// Exactly the pinned set; carries `name==version, ...` for the log.
    Match(String),
    /// Wrong or missing versions and/or undeclared extras (diagnostic lines).
    Mismatch(Vec<String>),
    /// The requirements file or the freeze output is not made of exact pins, or
    /// pins nothing. The original exited 2 for this.
    Invalid(String),
    /// `pip freeze` itself failed.
    FreezeFailed(String),
}

/// Pure comparison of the wanted pins against what is installed.
pub fn compare_pins(want: &BTreeMap<String, String>, have: &BTreeMap<String, String>) -> PinCheck {
    if want == have {
        let summary = want
            .iter()
            .map(|(k, v)| format!("{k}=={v}"))
            .collect::<Vec<_>>()
            .join(", ");
        return PinCheck::Match(summary);
    }
    let mut lines = Vec::new();
    let wrong: Vec<String> = want
        .iter()
        .filter(|(k, v)| have.get(*k) != Some(*v))
        .map(|(k, v)| {
            format!(
                "{k}=={v} (have {})",
                have.get(k).map_or("nothing", String::as_str)
            )
        })
        .collect();
    if !wrong.is_empty() {
        lines.push(format!("pin not satisfied: {}", wrong.join(", ")));
    }
    let extra: Vec<String> = have
        .iter()
        .filter(|(k, _)| !want.contains_key(*k))
        .map(|(k, v)| format!("{k}=={v}"))
        .collect();
    if !extra.is_empty() {
        // An undeclared package in the environment is a build input nobody wrote
        // down, which is the same failure as an unpinned one.
        lines.push(format!("present but not pinned: {}", extra.join(", ")));
    }
    PinCheck::Mismatch(lines)
}

fn python_command(python: &Path) -> Command {
    let mut c = Command::new(python);
    // Output contains non-ASCII; without this Python on a Windows console falls
    // back to cp1252 and mangles it.
    c.env("PYTHONIOENCODING", "utf-8").stdin(Stdio::null());
    c
}

/// Check that the environment of `python` matches `requirements` exactly:
/// `python -m pip freeze` against the parsed pins.
pub fn check_pin(python: &Path, requirements: &Path) -> PinCheck {
    let text = match std::fs::read_to_string(requirements) {
        Ok(t) => t,
        Err(e) => return PinCheck::Invalid(format!("cannot read {}: {e}", requirements.display())),
    };
    let src = requirements.display().to_string();
    let want = match parse_pins(&text, &src) {
        Ok(w) => w,
        Err(e) => return PinCheck::Invalid(e),
    };
    if want.is_empty() {
        return PinCheck::Invalid(format!("{src} pins nothing"));
    }
    let out = match python_command(python)
        .args(["-m", "pip", "freeze", "--disable-pip-version-check"])
        .output()
    {
        Ok(o) => o,
        Err(e) => return PinCheck::FreezeFailed(format!("cannot run {}: {e}", python.display())),
    };
    if !out.status.success() {
        return PinCheck::FreezeFailed(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    match parse_pins(&String::from_utf8_lossy(&out.stdout), "pip freeze") {
        Ok(have) => compare_pins(&want, &have),
        Err(e) => PinCheck::Invalid(e),
    }
}

/// The venv, relative to the repository root. Gitignored: the pin is committed,
/// the venv is only its materialisation.
pub const VENV: &str = ".build-python-venv";
/// The pin, relative to the repository root.
pub const REQUIREMENTS: &str = "build-python/requirements.txt";

/// First of `python3`, `python` that runs and is >= 3.11.
///
/// `python3` is the name on Linux; on Windows the same interpreter is usually
/// `python`, and a bare `python3` resolves to a Microsoft Store stub that exits
/// without running anything, which is why each candidate is *run*, not just
/// found.
pub fn find_python() -> Option<PathBuf> {
    ["python3", "python"].iter().map(PathBuf::from).find(|c| {
        python_command(c)
            .args([
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// The venv's interpreter: `bin/python`, or `Scripts/python.exe` on Windows.
pub fn venv_python(root: &Path) -> Option<PathBuf> {
    [
        Path::new(VENV).join("bin").join("python"),
        Path::new(VENV).join("Scripts").join("python.exe"),
    ]
    .into_iter()
    .map(|p| root.join(p))
    .find(|p| p.is_file())
}

/// Build (or reuse) the pinned Python environment and return the absolute path
/// of its interpreter. Progress goes through `say`.
///
/// Idempotent: an existing venv whose pins already match is reused, because this
/// runs in front of every reproducible build and creating a venv twice per CI
/// job is pure latency. Reuse is only on an exact match against the pin: a venv
/// holding Mako 1.4.0 when the pin says 1.4.1 is not one this build may use;
/// "close enough" is the whole failure mode ADR 023 exists to close.
///
/// Fails closed: there is no fallback to whatever `python3` resolves to.
pub fn ensure_pinned_python_with(root: &Path, say: &dyn Fn(&str)) -> Result<PathBuf, String> {
    let req = root.join(REQUIREMENTS);
    if !req.is_file() {
        return Err(format!(
            "{REQUIREMENTS} is missing; there is no pin to build against"
        ));
    }
    let Some(py) = find_python() else {
        return Err("no python >= 3.11 on PATH to create the build venv with".into());
    };

    if let Some(existing) = venv_python(root) {
        // A mismatch here is not an error and its diagnostics are not shown:
        // the venv is simply rebuilt.
        if let PinCheck::Match(_) = check_pin(&existing, &req) {
            say(&format!("reusing {VENV} (pins match)"));
            return Ok(existing);
        }
        say(&format!("{VENV} does not match the pin; rebuilding it"));
        std::fs::remove_dir_all(root.join(VENV))
            .map_err(|e| format!("cannot remove {VENV}: {e}"))?;
    }

    let version = python_command(&py)
        .arg("--version")
        .output()
        .map(|o| {
            // Older Pythons print the version on stderr.
            let mut t = String::from_utf8_lossy(&o.stdout).trim().to_string();
            t.push_str(String::from_utf8_lossy(&o.stderr).trim());
            t
        })
        .unwrap_or_default();
    say(&format!("creating {VENV} with {version}"));
    run_to_stderr(
        python_command(&py)
            .args(["-m", "venv", VENV])
            .current_dir(root),
    )
    .map_err(|e| format!("python -m venv failed: {e}"))?;

    let Some(interpreter) = venv_python(root) else {
        return Err(format!(
            "{VENV} was created but has no interpreter in bin/ or Scripts/"
        ));
    };

    // --no-deps with a fully-enumerated requirements file: pip is not permitted
    // to resolve anything. If Mako gains a dependency the build fails here and
    // the requirements file is updated deliberately, rather than the closure
    // growing on its own between two builds that were supposed to be identical.
    say("installing the pinned generators");
    run_to_stderr(
        python_command(&interpreter)
            .args([
                "-m",
                "pip",
                "install",
                "--quiet",
                "--no-deps",
                "--disable-pip-version-check",
                "-r",
                REQUIREMENTS,
            ])
            .current_dir(root),
    )
    .map_err(|e| format!("pip install failed: {e}"))?;

    match check_pin(&interpreter, &req) {
        PinCheck::Match(summary) => {
            say(&summary);
            Ok(interpreter)
        }
        other => {
            if let PinCheck::Mismatch(lines) = &other {
                for l in lines {
                    say(l);
                }
            }
            Err(format!(
                "{VENV} does not match {REQUIREMENTS} after install"
            ))
        }
    }
}

/// `ensure_pinned_python_with`, narrating on stderr as the script did.
pub fn ensure_pinned_python(root: &Path) -> Result<PathBuf, String> {
    ensure_pinned_python_with(root, &|m| eprintln!("  {m}"))
}

fn run_to_stderr(cmd: &mut Command) -> Result<(), String> {
    // The child's stdout becomes our stderr: whatever the caller reads from
    // stdout must stay clean.
    let status = cmd
        .stdout(Stdio::from(std::io::stderr()))
        .status()
        .map_err(|e| e.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("exited with {status}"))
    }
}

/// The name stylo's `build.rs` reads. It falls back to `python3`/`python.exe`
/// when unset, so the export is the whole point: without it the fallback
/// silently wins and the pin is decorative.
pub const ENV_VAR: &str = "PYTHON3";

/// Export the pin to a child process (a `cargo` that will compile `px-css`).
pub fn apply_pin(cmd: &mut Command, python: &Path) {
    cmd.env(ENV_VAR, python);
}

/// The pin for gates that merely *might* compile: `Some(interpreter)` when the
/// environment could be built, `None` (with notes on stderr) when not.
///
/// Deliberately NOT fatal, and the one place where "fail closed" is not the
/// right reading of the rule. Source-only gates compile nothing and run with no
/// network; making them reach PyPI to check a shebang would be a worse gate, not
/// a stricter one. A gate that does compile fails on its own when stylo's
/// `build.rs` cannot find Mako, in stylo's own words, so there is no
/// silent-success path. What does need closing is the reproducibility claim,
/// which is why `build-and-hash` uses `ensure_pinned_python` and refuses.
///
/// The reason is shown, not discarded: a diagnostic nobody can see costs more
/// than the noise it saves. A repository without a pin returns `None` silently.
pub fn pinned_python_lenient(root: &Path) -> Option<PathBuf> {
    if !root.join(REQUIREMENTS).is_file() {
        return None;
    }
    let notes = std::cell::RefCell::new(Vec::<String>::new());
    let r = ensure_pinned_python_with(root, &|m| notes.borrow_mut().push(m.to_string()));
    match r {
        Ok(p) => Some(p),
        Err(e) => {
            eprintln!("  note: the pinned Python environment could not be built, so a");
            eprintln!("  note: gate that compiles px-css will fail in stylo build.rs:");
            for n in notes.borrow().iter() {
                eprintln!("  note:   {n}");
            }
            eprintln!("  note:   FAIL {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn parses_exact_pins_skipping_comments_and_blanks() {
        let t = "# c\n\nMako==1.4.1\n  MarkupSafe == 3.0.3  \n";
        assert_eq!(
            parse_pins(t, "r").unwrap(),
            map(&[("mako", "1.4.1"), ("markupsafe", "3.0.3")])
        );
    }

    #[test]
    fn crlf_cannot_defeat_the_comparison() {
        let t = "Mako==1.4.1\r\nMarkupSafe==3.0.3\r\n";
        assert_eq!(parse_pins(t, "r").unwrap().get("mako").unwrap(), "1.4.1");
    }

    #[test]
    fn names_are_normalised() {
        let p = parse_pins("Foo_Bar==1\n", "r").unwrap();
        assert_eq!(p, map(&[("foo-bar", "1")]));
    }

    #[test]
    fn a_range_or_bare_name_is_rejected() {
        let e = parse_pins("Mako>=1.4\n", "req.txt").unwrap_err();
        assert_eq!(e, "req.txt: 'Mako>=1.4' is not an exact pin");
        assert!(parse_pins("Mako\n", "r").is_err());
        assert!(parse_pins("pkg @ file:///x\n", "pip freeze").is_err());
    }

    #[test]
    fn identical_sets_match() {
        let w = map(&[("mako", "1.4.1"), ("markupsafe", "3.0.3")]);
        assert_eq!(
            compare_pins(&w, &w.clone()),
            PinCheck::Match("mako==1.4.1, markupsafe==3.0.3".into())
        );
    }

    #[test]
    fn wrong_missing_and_extra_are_all_reported() {
        let want = map(&[("mako", "1.4.1"), ("markupsafe", "3.0.3")]);
        let have = map(&[("mako", "1.4.0"), ("pip", "24.0")]);
        let PinCheck::Mismatch(lines) = compare_pins(&want, &have) else {
            panic!("expected a mismatch");
        };
        assert_eq!(
            lines,
            [
                "pin not satisfied: mako==1.4.1 (have 1.4.0), markupsafe==3.0.3 (have nothing)",
                "present but not pinned: pip==24.0"
            ]
        );
    }

    #[test]
    fn extra_package_alone_is_a_mismatch() {
        let want = map(&[("mako", "1.4.1")]);
        let have = map(&[("mako", "1.4.1"), ("six", "1")]);
        assert_eq!(
            compare_pins(&want, &have),
            PinCheck::Mismatch(vec!["present but not pinned: six==1".into()])
        );
    }

    #[test]
    fn an_empty_requirements_file_is_invalid() {
        let f = crate::testutil::Fixture::new("pin-empty");
        f.write("r.txt", "# nothing\n");
        let c = check_pin(Path::new("python3"), &f.path().join("r.txt"));
        assert!(matches!(c, PinCheck::Invalid(m) if m.ends_with("pins nothing")));
    }

    #[test]
    fn a_missing_requirements_file_is_invalid() {
        let f = crate::testutil::Fixture::new("pin-missing");
        let c = check_pin(Path::new("python3"), &f.path().join("nope.txt"));
        assert!(matches!(c, PinCheck::Invalid(_)));
    }

    #[test]
    fn apply_pin_exports_python3() {
        let mut c = Command::new("true");
        apply_pin(&mut c, Path::new("/x/python"));
        let v: Vec<_> = c.get_envs().collect();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].0, ENV_VAR);
        assert_eq!(v[0].1, Some(std::ffi::OsStr::new("/x/python")));
    }

    #[test]
    fn without_a_pin_file_ensure_fails_closed_and_lenient_is_none() {
        let f = crate::testutil::Fixture::new("pin-nofile");
        let e = ensure_pinned_python_with(f.path(), &|_| {}).unwrap_err();
        assert!(e.contains("there is no pin to build against"));
        assert_eq!(pinned_python_lenient(f.path()), None);
    }

    #[test]
    fn venv_python_looks_in_bin_then_scripts() {
        let f = crate::testutil::Fixture::new("pin-venv");
        assert_eq!(venv_python(f.path()), None);
        f.write(".build-python-venv/Scripts/python.exe", "");
        assert_eq!(
            venv_python(f.path()),
            Some(f.path().join(".build-python-venv/Scripts/python.exe"))
        );
        f.write(".build-python-venv/bin/python", "");
        assert_eq!(
            venv_python(f.path()),
            Some(f.path().join(".build-python-venv/bin/python"))
        );
    }

    /// A venv whose fake `python` reports (CRLF, as on Windows) exactly the
    /// pins is reused without touching the network or creating anything.
    #[cfg(unix)]
    #[test]
    fn a_matching_venv_is_reused() {
        use std::os::unix::fs::PermissionsExt;
        if find_python().is_none() {
            return;
        }
        let f = crate::testutil::Fixture::new("pin-reuse");
        f.write(REQUIREMENTS, "Mako==1.4.1\nMarkupSafe==3.0.3\n");
        f.write(
            ".build-python-venv/bin/python",
            "#!/bin/sh\nprintf 'mako==1.4.1\\r\\nMarkupSafe==3.0.3\\r\\n'\n",
        );
        let p = f.path().join(".build-python-venv/bin/python");
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        let said = std::cell::RefCell::new(Vec::new());
        let got = ensure_pinned_python_with(f.path(), &|m| said.borrow_mut().push(m.to_string()));
        assert_eq!(got, Ok(p.clone()));
        assert_eq!(
            said.borrow().as_slice(),
            [format!("reusing {VENV} (pins match)")]
        );
        assert_eq!(pinned_python_lenient(f.path()), Some(p));
    }

    #[test]
    fn an_interpreter_that_cannot_run_is_a_freeze_failure() {
        let f = crate::testutil::Fixture::new("pin-nopy");
        f.write("r.txt", "Mako==1.4.1\n");
        let c = check_pin(Path::new("/nonexistent/python"), &f.path().join("r.txt"));
        assert!(matches!(c, PinCheck::FreezeFailed(_)));
    }
}
