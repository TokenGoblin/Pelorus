//! Child processes. Gates run `git` and `cargo` as children, never as
//! libraries, so what they see is what a developer would see at a prompt.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

/// Run `prog args` in `cwd`, return stdout; `Err` carries stderr on a
/// non-zero exit or the spawn error. Stdin is closed.
pub fn capture(prog: &str, args: &[&str], cwd: Option<&Path>) -> Result<Vec<u8>, String> {
    let mut c = Command::new(prog);
    c.args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let o = c
        .output()
        .map_err(|e| format!("cannot run `{prog}`: {e}"))?;
    if o.status.success() {
        Ok(o.stdout)
    } else {
        Err(format!(
            "`{prog} {}` exited with {}: {}",
            args.join(" "),
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ))
    }
}

/// Run `prog args` in `cwd` with stdout and stderr merged into one stream (what
/// `cmd 2>&1` gave a pipeline); returns the combined text and whether the
/// process exited successfully. Stdin is closed. The output is collected, not
/// echoed.
pub fn capture_merged(
    prog: &str,
    args: &[&str],
    cwd: Option<&Path>,
) -> Result<(String, bool), String> {
    let mut c = Command::new(prog);
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    capture_merged_cmd(c)
}

/// [`capture_merged`] for a command the caller has already configured
/// (environment, working directory). Stdin and both output streams are
/// replaced.
pub fn capture_merged_cmd(mut c: Command) -> Result<(String, bool), String> {
    use std::io::Read;
    let prog = c.get_program().to_string_lossy().into_owned();
    let (mut recv, send) = std::io::pipe().map_err(|e| format!("cannot create a pipe: {e}"))?;
    let send2 = send
        .try_clone()
        .map_err(|e| format!("cannot clone a pipe: {e}"))?;
    c.stdin(Stdio::null()).stdout(send).stderr(send2);
    let mut child = c.spawn().map_err(|e| format!("cannot run `{prog}`: {e}"))?;
    // Drop the Command so the parent's copies of the write end close.
    drop(c);
    let mut buf = Vec::new();
    let _ = recv.read_to_end(&mut buf);
    let status = child
        .wait()
        .map_err(|e| format!("cannot wait for `{prog}`: {e}"))?;
    Ok((String::from_utf8_lossy(&buf).into_owned(), status.success()))
}

/// A `cargo` command in `root` for gates that run it themselves rather than
/// through [`cargo`] (to capture or filter its output, or to set environment).
/// Same program as [`cargo`] and the same `PYTHON3` pin, so a build it triggers
/// compiles px-css exactly as `ci/lib.sh`'s exported pin made every gate
/// script's cargo do.
pub fn cargo_command(root: &Path) -> Command {
    let prog = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut c = Command::new(prog);
    c.current_dir(root);
    apply_gate_env(&mut c, root);
    c
}

/// Export the `PYTHON3` pin (when the pinned environment can be built; see
/// `pybuild::pinned_python_lenient`) to a command that may compile px-css.
pub fn apply_gate_env(c: &mut Command, root: &Path) {
    if let Some(py) = crate::pybuild::pinned_python_lenient(root) {
        crate::pybuild::apply_pin(c, &py);
    }
}

/// Run `cargo args` in `root` with inherited stdio (the user sees cargo's own
/// output). Honours `$CARGO`, which cargo sets when it runs `cargo xtask`, so
/// the child is the same toolchain as the parent.
///
/// Every workspace build compiles px-css and so stylo's `build.rs`, so the
/// pinned Python (ADR 023) is applied here, once, as `ci/lib.sh` did for the
/// gate scripts: `PYTHON3` is exported to the child when the pinned environment
/// can be built (`pybuild::pinned_python_lenient`; not fatal, see there).
pub fn cargo(root: &Path, args: &[&str]) -> Result<ExitStatus, String> {
    let prog = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut c = Command::new(&prog);
    c.args(args).current_dir(root);
    if let Some(py) = crate::pybuild::pinned_python_lenient(root) {
        crate::pybuild::apply_pin(&mut c, &py);
    }
    c.status().map_err(|e| format!("cannot run cargo: {e}"))
}

/// The directory cargo writes to: `$CARGO_TARGET_DIR` (relative to `root`) if
/// set, else `root/target`. The shell gates assumed `target/`.
pub fn target_dir(root: &Path) -> std::path::PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(d) if !d.is_empty() => root.join(d),
        _ => root.join("target"),
    }
}

/// `<target>/<profile_dir>/<stem>`, or its `.exe` twin if that exists (the
/// Windows spelling the scripts probed for).
pub fn built_binary(root: &Path, profile_dir: &str, stem: &str) -> std::path::PathBuf {
    let dir = target_dir(root).join(profile_dir);
    let exe = dir.join(format!("{stem}.exe"));
    if exe.is_file() { exe } else { dir.join(stem) }
}
