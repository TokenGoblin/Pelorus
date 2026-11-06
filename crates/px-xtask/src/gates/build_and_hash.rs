//! `cargo xtask build-and-hash <build-dir>` -- one half of gate check 2,
//! reproducible builds (invariant 7). Port of `ci/build-and-hash.sh`.
//!
//! Copies the tracked tree (`git archive HEAD`) to a caller-chosen directory,
//! builds it there, and writes the SHA-256 of every release binary to stdout as
//! `<hash>  <name>`, sorted by name. The caller varies the directory, HOME,
//! CARGO_HOME and -- on Linux -- the user between the two runs. The comparison
//! is `cargo xtask compare-hashes`.
//!
//! Machine output: stdout carries the manifest and nothing else; progress, the
//! cargo build's own output, `FAIL` lines and the verdict all go to stderr, as in
//! the script (`>&2`).
//!
//! Differences from the script, all in the direction of fewer environments to be
//! wrong in:
//!
//! * the tree is extracted in-process (`crate::tar`), not by piping into the
//!   system `tar`, so a Windows-native path needs no dialect conversion;
//! * the manifest is sorted bytewise by name, not by `sort -k2` under whatever
//!   locale the runner has (both builds sort alike either way);
//! * `CARGO_TARGET_DIR` and the variables `cargo run` exports into this process
//!   are removed from the child build, because the manifest is read from
//!   `<build-dir>/target/release`.
//!
//! The build is not unit-testable (it compiles the workspace); everything it
//! decides is a pure function below, and `tests` covers them plus the
//! extraction and hashing against a fixture git repository.

use crate::Gate;
use crate::proc;
use crate::pybuild;
use crate::sha256;
use crate::tar;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// File name suffixes in `target/release` that are not release binaries.
const NOT_BINARY: &[&str] = &[".d", ".pdb", ".rlib", ".rmeta", ".exp", ".lib"];

/// `target/release/px-*` minus build by-products.
pub fn is_artifact(name: &str) -> bool {
    name.starts_with("px-") && !NOT_BINARY.iter().any(|s| name.ends_with(s))
}

/// The `host:` line of `rustc -vV`.
pub fn host_triple(rustc_vv: &str) -> Option<&str> {
    rustc_vv.lines().find_map(|l| l.strip_prefix("host: "))
}

/// The RUSTFLAGS for a build.
///
/// **Order matters: when several prefixes match a path, the last one wins**
/// (rustc and GCC agree). The checkout and `CARGO_HOME` can live inside `HOME`
/// (build B on Linux is `/home/builder2/work/build-b` under `HOME=/home/
/// builder2`), so `HOME` goes first and the narrower prefixes after it;
/// otherwise a path inside the checkout was rewritten by the `HOME` rule to
/// `/home/work/build-b/...` in one build and by the `/src` rule in the other,
/// which is the `px-shell` hash difference this gate reported (stylo's
/// generated `properties.rs` path is embedded in that binary).
///
/// Invariant 7: the binary must not contain the path it was built at or the name
/// of the user who built it. Cargo's `trim-paths` profile option is still
/// unstable, so remap explicitly here, where all three paths are known. Both
/// runs remap onto the SAME synthetic prefixes (`/src`, `/cargo`, `/home`),
/// which is what makes the hashes comparable across machines. rustc is a native
/// binary and reports native paths, so the prefixes are the native spelling; a
/// remap prefix in another dialect would silently match nothing.
///
/// MSVC's linker stamps the PE header with the wall-clock time of the link, so
/// two builds of identical source differ by construction. `/Brepro` replaces
/// that timestamp with a hash of the content, which is what makes a Windows
/// build reproducible at all. There is no equivalent to disable on ELF: the GNU
/// and LLVM linkers are already deterministic.
pub fn remap_flags(dest: &Path, cargo_home: &Path, home: &Path, host: &str) -> String {
    let mut f = format!(
        "--remap-path-prefix={}=/home --remap-path-prefix={}=/cargo --remap-path-prefix={}=/src",
        home.display(),
        cargo_home.display(),
        dest.display()
    );
    if host.ends_with("windows-msvc") {
        f.push_str(" -C link-arg=/Brepro");
    }
    f
}

/// `${CARGO_HOME:-$HOME/.cargo}` and `$HOME`, from the given lookups.
/// The same three prefixes for C and assembly compiled by build scripts.
///
/// `RUSTFLAGS` reaches `rustc` only. `ring` (linked by the shell through
/// rustls) assembles its `.S` files with the system C compiler through the `cc`
/// crate, which honours `CFLAGS`; without a map those objects carry the absolute
/// `CARGO_HOME` path of the checkout's registry source in their debug
/// information, so builds under two different homes differ in `px-shell`
/// alone, the one artifact that links `ring`. `-ffile-prefix-map` is the GNU and
/// LLVM spelling (implies the debug map and is passed on to the assembler);
/// MSVC's `cl` has no such flag, and its Windows builds do not show the
/// problem, so `None` there rather than a flag the compiler would reject.
pub fn c_flags(dest: &Path, cargo_home: &Path, home: &Path, host: &str) -> Option<String> {
    if host.ends_with("windows-msvc") {
        return None;
    }
    Some(format!(
        "-ffile-prefix-map={}=/home -ffile-prefix-map={}=/cargo -ffile-prefix-map={}=/src",
        home.display(),
        cargo_home.display(),
        dest.display()
    ))
}

pub fn homes(
    cargo_home: Option<String>,
    home: Option<String>,
) -> Result<(PathBuf, PathBuf), String> {
    let home = home
        .filter(|h| !h.is_empty())
        .ok_or("HOME is not set (it is one of the three paths remapped out of the binary)")?;
    let cargo_home = match cargo_home.filter(|c| !c.is_empty()) {
        Some(c) => PathBuf::from(c),
        None => Path::new(&home).join(".cargo"),
    };
    Ok((cargo_home, PathBuf::from(home)))
}

/// Manifest text: `<hash>  <name>` per line, sorted by name.
pub fn manifest(mut entries: Vec<(String, String)>) -> String {
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
        .into_iter()
        .map(|(name, hash)| format!("{hash}  {name}\n"))
        .collect()
}

/// Hash every artifact in `release_dir`.
pub fn hash_artifacts(release_dir: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(release_dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // `[ -f ]` follows symlinks, as is_file does.
        if !is_artifact(&name) || !e.path().is_file() {
            continue;
        }
        match sha256::sha256_file(&e.path()) {
            Ok(h) => out.push((name, h)),
            Err(er) => eprintln!("FAIL cannot hash {}: {er}", e.path().display()),
        }
    }
    out
}

/// Environment variables inherited from `cargo run` (or the caller) that must
/// not reach the build in the scratch directory.
fn scrubbed(name: &str) -> bool {
    name == "CARGO_TARGET_DIR"
        || name == "CARGO_MANIFEST_DIR"
        || name == "CARGO_MANIFEST_PATH"
        || name == "CARGO_BIN_NAME"
        || name == "CARGO_CRATE_NAME"
        || name == "CARGO_PRIMARY_PACKAGE"
        || name.starts_with("CARGO_PKG_")
}

/// `/d/a/_temp/x` (the form the MSYS shell uses for its own children) as
/// `D:/a/_temp/x`. rustc is a native binary and reports native paths, so a remap
/// prefix in the POSIX form would silently match nothing. The shell normally
/// converts what it passes to a native program; this makes the gate correct if
/// it did not. Anything not of that shape is returned unchanged.
pub fn msys_to_native(p: &str) -> String {
    let b = p.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b'/' {
        format!("{}:{}", (b[1] as char).to_ascii_uppercase(), &p[2..])
    } else if b.len() == 2 && b[0] == b'/' && b[1].is_ascii_alphabetic() {
        format!("{}:/", (b[1] as char).to_ascii_uppercase())
    } else {
        p.to_string()
    }
}

/// A path from the command line or the environment, in the spelling rustc uses.
fn native(p: &str) -> String {
    if cfg!(windows) {
        msys_to_native(p)
    } else {
        p.to_string()
    }
}

fn absolute(root: &Path, p: &str) -> PathBuf {
    let p = PathBuf::from(native(p));
    if p.is_absolute() { p } else { root.join(p) }
}

/// Extract `git archive HEAD` of `root` into `dest`. Only tracked files, so a
/// stray local file cannot make two builds differ for a reason that has nothing
/// to do with the source.
fn export_tree(root: &Path, dest: &Path) -> Result<usize, String> {
    let mut child = Command::new("git")
        .args(["archive", "--format=tar", "HEAD"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run git: {e}"))?;
    let stdout = child.stdout.take().ok_or("git archive has no stdout")?;
    let extracted = tar::extract(stdout, dest);
    let status = child.wait().map_err(|e| format!("git archive: {e}"))?;
    if !status.success() {
        return Err(format!("git archive HEAD failed ({status})"));
    }
    extracted
}

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    g.machine_output();
    let [dest] = args else {
        g.fail("usage: build-and-hash <build-dir>");
        return;
    };
    let dest = absolute(root, dest);

    let (cargo_home, home) = match homes(
        std::env::var("CARGO_HOME").ok().map(|c| native(&c)),
        std::env::var("HOME")
            .ok()
            .or_else(|| std::env::var("USERPROFILE").ok())
            .map(|h| native(&h)),
    ) {
        Ok(h) => h,
        Err(e) => {
            g.fail(e);
            return;
        }
    };

    let _ = std::fs::remove_dir_all(&dest);
    if let Err(e) = std::fs::create_dir_all(&dest) {
        g.fail(format!("cannot create {}: {e}", dest.display()));
        return;
    }
    match export_tree(root, &dest) {
        Ok(n) => g.info(format!("exported {n} tracked files to {}", dest.display())),
        Err(e) => {
            g.fail(e);
            return;
        }
    }

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let host = proc::capture(&rustc, &["-vV"], None)
        .map(|b| String::from_utf8_lossy(&b).into_owned())
        .unwrap_or_default();
    let flags = remap_flags(
        &dest,
        &cargo_home,
        &home,
        host_triple(&host).unwrap_or_default(),
    );

    // ADR 023: stylo's build.rs generates its property definitions by running
    // properties/build.py through Mako, so a Python interpreter and a Mako
    // version are build inputs. Invariant 7 says as much -- "same source + same
    // toolchain + same pinned build-time generators" -- and this is where the pin
    // is applied.
    //
    // Built inside the scratch directory rather than reused from the caller's
    // tree, because that is what gets built and an interpreter from somewhere
    // else is exactly the unpinned input this closes. It costs a venv creation
    // per build.
    //
    // Fail closed: if the pinned environment cannot be built, this does not fall
    // back to whatever `python3` resolves to on the machine. A reproducibility
    // check that quietly used a different generator than the one it claims to
    // have pinned would be worse than no check, because it would still print two
    // matching hashes.
    let c_flags = c_flags(
        &dest,
        &cargo_home,
        &home,
        host_triple(&host).unwrap_or_default(),
    );

    let python = match pybuild::ensure_pinned_python(&dest) {
        Ok(p) => p,
        Err(e) => {
            g.fail(e);
            g.fail("could not build the pinned Python environment (ADR 023);");
            g.fail("  refusing to build with an unpinned interpreter");
            return;
        }
    };
    if !python.is_file() {
        g.fail(format!("PYTHON3={} is not executable", python.display()));
        return;
    }
    g.info(format!("PYTHON3={}", python.display()));

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut build = Command::new(cargo);
    build
        .args(["build", "--workspace", "--locked", "--release"])
        .current_dir(&dest)
        .env("RUSTFLAGS", &flags)
        .stdin(Stdio::null())
        // The manifest owns stdout: cargo's output is diagnostics.
        .stdout(Stdio::from(std::io::stderr()));
    for (k, _) in std::env::vars() {
        if scrubbed(&k) {
            build.env_remove(k);
        }
    }
    // stylo's build.rs reads PYTHON3 and falls back to python3/python.exe. The
    // export is the whole point: without it the fallback silently wins and the
    // pin is decorative.
    if let Some(c) = c_flags {
        // Appended to, not replacing, whatever the environment already asks for.
        let existing = std::env::var("CFLAGS").unwrap_or_default();
        build.env("CFLAGS", format!("{existing} {c}").trim());
    }
    pybuild::apply_pin(&mut build, &python);
    match build.status() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            g.fail(format!("release build failed in {} ({s})", dest.display()));
            return;
        }
        Err(e) => {
            g.fail(format!("cannot run cargo: {e}"));
            return;
        }
    }

    let release = dest.join("target").join("release");
    let entries = hash_artifacts(&release);
    if entries.is_empty() {
        g.fail(format!(
            "no release binaries produced in {}",
            release.display()
        ));
        return;
    }
    g.payload(manifest(entries).trim_end());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn artifacts_are_px_binaries_without_build_byproducts() {
        for ok in ["px-broker", "px-render.exe", "px-x"] {
            assert!(is_artifact(ok), "{ok}");
        }
        for no in [
            "px-broker.d",
            "px-broker.pdb",
            "libpx_x.rlib",
            "px-x.rmeta",
            "px-x.exp",
            "px-x.lib",
            "other",
            "build",
            "px",
        ] {
            assert!(!is_artifact(no), "{no}");
        }
    }

    #[test]
    fn host_is_read_from_rustc_vv() {
        let vv = "rustc 1.98.1 (x 2026-09-01)\nbinary: rustc\nhost: x86_64-pc-windows-msvc\nrelease: 1.98.1\n";
        assert_eq!(host_triple(vv), Some("x86_64-pc-windows-msvc"));
        assert_eq!(host_triple("nothing"), None);
    }

    #[test]
    fn c_flags_map_the_same_three_prefixes_except_for_msvc() {
        let (d, c, h) = (Path::new("/b"), Path::new("/h/.cargo"), Path::new("/h"));
        let gnu = c_flags(d, c, h, "x86_64-unknown-linux-gnu").unwrap_or_default();
        assert_eq!(
            gnu,
            "-ffile-prefix-map=/h=/home -ffile-prefix-map=/h/.cargo=/cargo -ffile-prefix-map=/b=/src"
        );
        assert_eq!(c_flags(d, c, h, "x86_64-pc-windows-msvc"), None);
    }

    #[test]
    fn remap_flags_are_exactly_the_scripts() {
        let f = remap_flags(
            Path::new("/tmp/short"),
            Path::new("/h/.cargo"),
            Path::new("/h"),
            "x86_64-unknown-linux-gnu",
        );
        assert_eq!(
            f,
            "--remap-path-prefix=/h=/home --remap-path-prefix=/h/.cargo=/cargo --remap-path-prefix=/tmp/short=/src"
        );
    }

    /// rustc applies the LAST matching `--remap-path-prefix`. Both reproducible
    /// builds must map a file in their checkout to the same text even when the
    /// checkout (and `CARGO_HOME`) sit inside `HOME`, as build B does on Linux.
    #[test]
    fn a_checkout_inside_home_maps_to_src_not_home() {
        fn remap(flags: &str, path: &str) -> String {
            let rules: Vec<(&str, &str)> = flags
                .split_whitespace()
                .filter_map(|w| w.strip_prefix("--remap-path-prefix="))
                .filter_map(|r| r.split_once('='))
                .collect();
            for (from, to) in rules.iter().rev() {
                if let Some(rest) = path.strip_prefix(from) {
                    return format!("{to}{rest}");
                }
            }
            path.to_string()
        }
        let a = remap_flags(
            Path::new("/runner/tmp/short"),
            Path::new("/runner/tmp/home-a/.cargo"),
            Path::new("/runner/tmp/home-a"),
            "x86_64-unknown-linux-gnu",
        );
        let b = remap_flags(
            Path::new("/home/builder2/work/build-b"),
            Path::new("/home/builder2/.cargo"),
            Path::new("/home/builder2"),
            "x86_64-unknown-linux-gnu",
        );
        let in_a = "/runner/tmp/short/target/release/build/stylo/out/properties.rs";
        let in_b = "/home/builder2/work/build-b/target/release/build/stylo/out/properties.rs";
        assert_eq!(
            remap(&a, in_a),
            "/src/target/release/build/stylo/out/properties.rs"
        );
        assert_eq!(remap(&b, in_b), remap(&a, in_a));
        let reg_a = "/runner/tmp/home-a/.cargo/registry/src/x/ring/a.S";
        let reg_b = "/home/builder2/.cargo/registry/src/x/ring/a.S";
        assert_eq!(remap(&a, reg_a), remap(&b, reg_b));
    }

    #[test]
    fn msvc_hosts_get_brepro_and_others_do_not() {
        let msvc = remap_flags(
            Path::new("d"),
            Path::new("c"),
            Path::new("h"),
            "x86_64-pc-windows-msvc",
        );
        assert!(msvc.ends_with(" -C link-arg=/Brepro"));
        let gnu = remap_flags(
            Path::new("d"),
            Path::new("c"),
            Path::new("h"),
            "x86_64-pc-windows-gnu",
        );
        assert!(!gnu.contains("Brepro"));
    }

    #[test]
    fn cargo_home_defaults_under_home_and_home_is_required() {
        assert_eq!(
            homes(None, Some("/home/u".into())).unwrap(),
            (PathBuf::from("/home/u/.cargo"), PathBuf::from("/home/u"))
        );
        assert_eq!(
            homes(Some("/opt/c".into()), Some("/home/u".into()))
                .unwrap()
                .0,
            PathBuf::from("/opt/c")
        );
        // an empty CARGO_HOME is unset, as `${CARGO_HOME:-...}` treats it
        assert_eq!(
            homes(Some(String::new()), Some("/home/u".into()))
                .unwrap()
                .0,
            PathBuf::from("/home/u/.cargo")
        );
        assert!(homes(None, None).is_err());
        assert!(homes(None, Some(String::new())).is_err());
    }

    #[test]
    fn msys_drive_paths_become_native() {
        assert_eq!(msys_to_native("/d/a/_temp/short"), "D:/a/_temp/short");
        assert_eq!(msys_to_native("/c"), "C:/");
        assert_eq!(msys_to_native("D:/a/x"), "D:/a/x");
        assert_eq!(msys_to_native("/home/u"), "/home/u");
        assert_eq!(msys_to_native("/tmp"), "/tmp");
        assert_eq!(msys_to_native("rel/p"), "rel/p");
    }

    #[test]
    fn manifest_is_sorted_by_name_with_two_spaces() {
        let m = manifest(vec![
            ("px-render".into(), "bb".into()),
            ("px-broker".into(), "aa".into()),
        ]);
        assert_eq!(m, "aa  px-broker\nbb  px-render\n");
    }

    #[test]
    fn scrubs_cargo_run_leakage_but_not_cargo_home() {
        for v in [
            "CARGO_TARGET_DIR",
            "CARGO_MANIFEST_DIR",
            "CARGO_PKG_NAME",
            "CARGO_BIN_NAME",
        ] {
            assert!(scrubbed(v), "{v}");
        }
        for v in ["CARGO_HOME", "CARGO", "RUSTUP_HOME", "HOME"] {
            assert!(!scrubbed(v), "{v}");
        }
    }

    /// The manifest the gate prints is what `sha256sum` prints for the files in
    /// `target/release`, skipping by-products and non-px files.
    #[test]
    fn hash_artifacts_matches_sha256sum_output_format() {
        let f = Fixture::new("bah");
        f.write_bytes("px-broker", &[1, 2, 3, 4, 5]);
        f.write_bytes("px-render", &vec![9u8; 100_000]);
        f.write("px-render.d", "dep-info");
        f.write("libpx_core.rlib", "rlib");
        f.write("other-tool", "x");
        std::fs::create_dir_all(f.path().join("px-dir")).unwrap();
        let m = manifest(hash_artifacts(f.path()));
        let mut want = String::new();
        for n in ["px-broker", "px-render"] {
            if let Ok(o) = Command::new("sha256sum").arg(f.path().join(n)).output() {
                let h = String::from_utf8_lossy(&o.stdout);
                want.push_str(&format!(
                    "{}  {n}\n",
                    h.split_whitespace()
                        .next()
                        .unwrap()
                        // sha256sum escapes a line whose file name holds a
                        // backslash (every Windows path) with a leading one.
                        .trim_start_matches('\\')
                ));
            } else {
                // no sha256sum here: the format and the order still hold
                return;
            }
        }
        assert_eq!(m, want);
    }

    #[test]
    fn nothing_to_hash_is_an_empty_list() {
        let f = Fixture::new("bah-empty");
        assert!(hash_artifacts(f.path()).is_empty());
        assert!(hash_artifacts(&f.path().join("missing")).is_empty());
    }

    #[test]
    fn export_tree_is_the_committed_tree_only() {
        let repo = Fixture::new("bah-export");
        repo.write("Cargo.toml", "[workspace]\n")
            .write("crates/a/src/lib.rs", "");
        repo.commit_all();
        repo.write("stray.txt", "untracked");
        let dest = Fixture::new("bah-export-out");
        let n = export_tree(repo.path(), dest.path()).unwrap();
        assert_eq!(n, 2);
        assert!(dest.path().join("crates/a/src/lib.rs").is_file());
        assert!(!dest.path().join("stray.txt").exists());
    }

    #[test]
    fn export_tree_without_a_commit_fails() {
        let repo = Fixture::new("bah-nocommit");
        repo.git_init();
        let dest = Fixture::new("bah-nocommit-out");
        assert!(export_tree(repo.path(), dest.path()).is_err());
    }

    #[test]
    fn missing_argument_is_a_failure() {
        let mut g = Gate::new("build-and-hash");
        run(&mut g, Path::new("."), &[]);
        assert_eq!(g.fail_count(), 1);
    }
}
