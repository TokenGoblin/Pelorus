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

/// Run `cargo args` in `root` with inherited stdio (the user sees cargo's own
/// output). Honours `$CARGO`, which cargo sets when it runs `cargo xtask`, so
/// the child is the same toolchain as the parent.
pub fn cargo(root: &Path, args: &[&str]) -> Result<ExitStatus, String> {
    let prog = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    Command::new(&prog)
        .args(args)
        .current_dir(root)
        .status()
        .map_err(|e| format!("cannot run cargo: {e}"))
}
