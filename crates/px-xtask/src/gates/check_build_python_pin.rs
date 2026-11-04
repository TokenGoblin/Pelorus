//! `cargo xtask check-build-python-pin <interpreter> <requirements.txt>` --
//! check that a Python environment matches `build-python/requirements.txt`
//! exactly. Port of `ci/check_build_python_pin.py` (ADR 023, ADR 032).
//!
//! ADR 023: `stylo`'s `build.rs` generates its property definitions through
//! Python 3 and Mako, so both are build inputs and invariant 7 reads "same
//! source + same toolchain + same pinned build-time generators". This is what
//! makes the pin real rather than described: the environment must contain
//! exactly the pinned versions and nothing else.
//!
//! The script was `<interpreter> ci/check_build_python_pin.py <requirements>`
//! and checked the interpreter running it; the gate takes the interpreter as its
//! first argument and still asks *that* interpreter (`python -m pip freeze`) as a
//! child process, because the environment is the thing being inspected.
//!
//! The script exited 2 for a requirements file that is not made of exact pins;
//! a gate has only pass (0) and fail (1), so that is a FAIL here with the reason.
//! The decision itself lives in `crate::pybuild`.

use crate::Gate;
use crate::pybuild::{PinCheck, check_pin};
use std::path::{Path, PathBuf};

pub fn run(g: &mut Gate, root: &Path, args: &[String]) {
    let [python, req] = args else {
        g.fail("usage: cargo xtask check-build-python-pin <interpreter> <requirements.txt>");
        return;
    };
    let req = {
        let p = PathBuf::from(req);
        if p.is_absolute() { p } else { root.join(p) }
    };
    report(g, check_pin(Path::new(python), &req), &req);
}

fn report(g: &mut Gate, check: PinCheck, req: &Path) {
    match check {
        PinCheck::Match(summary) => g.ok(summary),
        PinCheck::Mismatch(lines) => {
            g.fail(format!(
                "the environment does not match {} exactly",
                req.display()
            ));
            for l in lines {
                g.detail(l);
            }
        }
        PinCheck::Invalid(why) => g.fail(why),
        PinCheck::FreezeFailed(why) => g.fail(format!("pip freeze failed: {why}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_passes_and_everything_else_fails() {
        let req = Path::new("r.txt");
        let mut g = Gate::new("t");
        report(&mut g, PinCheck::Match("mako==1".into()), req);
        assert!(g.passed());
        report(&mut g, PinCheck::Mismatch(vec!["x".into()]), req);
        report(&mut g, PinCheck::Invalid("bad".into()), req);
        report(&mut g, PinCheck::FreezeFailed("boom".into()), req);
        assert_eq!(g.fail_count(), 3);
    }

    #[test]
    fn wrong_argument_count_fails() {
        let mut g = Gate::new("t");
        run(&mut g, Path::new("."), &[]);
        assert_eq!(g.fail_count(), 1);
    }
}
