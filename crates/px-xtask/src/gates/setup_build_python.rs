//! `cargo xtask setup-build-python` -- build the pinned Python environment
//! stylo's `build.rs` needs, and print the path to its interpreter on stdout.
//! Port of `ci/setup-build-python.sh` (ADR 023, ADR 032: the one named
//! exception to the 100% Rust rule; the interpreter is a child process, all the
//! logic around it is `crate::pybuild`).
//!
//! ADR 023: `stylo`'s `build.rs` reads `PYTHON3` from the environment (falling
//! back to `python3` / `python.exe`), then runs `properties/build.py` through
//! Mako. So an interpreter and a Mako version are build inputs. This command
//! makes the pin real: `build-python/requirements.txt` names exact versions and
//! the environment built from it contains exactly those and nothing else.
//!
//! ```text
//! PYTHON3="$(cargo xtask setup-build-python)"; export PYTHON3
//! ```
//!
//! Machine output: everything the script said went to stderr and stdout carried
//! exactly the interpreter path, because the caller substitutes it into
//! `PYTHON3`. The same holds here -- progress lines, `ok`/`FAIL` and the verdict
//! go to stderr; stdout is the (absolute) path, one line. Idempotent: an
//! existing venv whose pins match is reused.

use crate::Gate;
use crate::pybuild;
use std::path::Path;

pub fn run(g: &mut Gate, root: &Path, _args: &[String]) {
    g.machine_output();
    let result = pybuild::ensure_pinned_python_with(root, &|m| g.info(m));
    match result {
        Ok(p) => {
            g.ok(format!("pinned interpreter: {}", p.display()));
            g.payload(p.display().to_string());
        }
        Err(e) => g.fail(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    #[test]
    fn no_pin_file_is_a_failure_with_no_payload() {
        let f = Fixture::new("setup-nopin");
        let mut g = Gate::new("setup-build-python");
        run(&mut g, f.path(), &[]);
        assert_eq!(g.fail_count(), 1);
        assert!(g.failures()[0].contains("there is no pin to build against"));
    }
}
