//! Gate output and verdict: the replacement for `ci/lib.sh`'s
//! `info`/`ok`/`fail`/`verdict`.
//!
//! Same shapes on the same streams: `ok` and `info` lines on stdout, `FAIL`
//! lines and the failing verdict on stderr. Every `fail` call counts as one,
//! including continuation lines, exactly as the shell helpers did, so the `(n)`
//! in `<gate>: FAIL (n)` is unchanged by a port.

use std::io::Write;
use std::process::ExitCode;

#[derive(Debug)]
pub struct Gate {
    name: String,
    failures: Vec<String>,
}

fn out(s: &str) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{s}");
}

fn err(s: &str) {
    let mut e = std::io::stderr().lock();
    let _ = writeln!(e, "{s}");
}

impl Gate {
    pub fn new(name: &str) -> Self {
        Gate {
            name: name.to_string(),
            failures: Vec::new(),
        }
    }

    /// `  message` on stdout.
    pub fn info(&self, msg: impl AsRef<str>) {
        out(&format!("  {}", msg.as_ref()));
    }

    /// `ok   message` on stdout.
    pub fn ok(&self, msg: impl AsRef<str>) {
        out(&format!("ok   {}", msg.as_ref()));
    }

    /// `FAIL message` on stderr; counts toward the verdict.
    pub fn fail(&mut self, msg: impl AsRef<str>) {
        err(&format!("FAIL {}", msg.as_ref()));
        self.failures.push(msg.as_ref().to_string());
    }

    /// Indented context under a failure, on stderr. Not counted.
    pub fn detail(&self, msg: impl AsRef<str>) {
        err(&format!("       {}", msg.as_ref()));
    }

    pub fn fail_count(&self) -> usize {
        self.failures.len()
    }

    pub fn failures(&self) -> &[String] {
        &self.failures
    }

    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// Print the verdict and return the exit code (0 pass, 1 fail).
    pub fn verdict(&self) -> ExitCode {
        if self.failures.is_empty() {
            out(&format!("\n{}: PASS", self.name));
            ExitCode::SUCCESS
        } else {
            err(&format!("\n{}: FAIL ({})", self.name, self.failures.len()));
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_every_fail_call() {
        let mut g = Gate::new("t");
        assert!(g.passed());
        g.fail("a");
        g.fail("b");
        assert_eq!(g.fail_count(), 2);
        assert_eq!(g.failures(), ["a", "b"]);
        assert!(!g.passed());
    }

    #[test]
    fn verdict_exit_codes() {
        let mut g = Gate::new("t");
        assert_eq!(
            format!("{:?}", g.verdict()),
            format!("{:?}", ExitCode::SUCCESS)
        );
        g.fail("x");
        assert_eq!(
            format!("{:?}", g.verdict()),
            format!("{:?}", ExitCode::from(1))
        );
    }
}
