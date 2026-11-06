//! Test-only helpers: fixture directory layouts in a temp dir, optionally a
//! real git repository, so a gate can be run end to end against a tree we
//! control. No third-party tempdir crate; this is std only.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Identity that satisfies the structure gate's noreply rule.
pub const GOOD_EMAIL: &str = "1+dev@users.noreply.github.com";

pub struct Fixture {
    pub root: PathBuf,
}

impl Fixture {
    pub fn new(tag: &str) -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!("px-xtask-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create fixture dir");
        Fixture { root }
    }

    /// Write `content` at `rel`, creating parent directories.
    pub fn write(&self, rel: &str, content: &str) -> &Self {
        self.write_bytes(rel, content.as_bytes())
    }

    pub fn write_bytes(&self, rel: &str, content: &[u8]) -> &Self {
        let p = self.root.join(rel);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("create parent");
        }
        std::fs::write(&p, content).expect("write fixture file");
        self
    }

    pub fn remove(&self, rel: &str) -> &Self {
        std::fs::remove_file(self.root.join(rel)).expect("remove fixture file");
        self
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Run git in the fixture; panics with git's stderr on failure.
    pub fn git(&self, args: &[&str]) -> &Self {
        let o = Command::new("git")
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .expect("spawn git");
        assert!(
            o.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        self
    }

    pub fn git_init(&self) -> &Self {
        // `core.autocrlf` is often `true` on Windows, which would turn the
        // fixture's LF files into CRLF in `archive` output and make every
        // byte-exact comparison against them a test of the machine's config.
        self.git(&["init", "-q"])
            .git(&["config", "core.autocrlf", "false"])
    }

    pub fn stage_all(&self) -> &Self {
        self.git(&["add", "-A"])
    }

    /// Commit everything staged as `email` with `message`.
    pub fn commit_as(&self, email: &str, message: &str) -> &Self {
        self.git(&[
            "-c",
            "user.name=fixture",
            "-c",
            &format!("user.email={email}"),
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            message,
        ])
    }

    /// `git init`, stage everything, commit with a compliant identity.
    pub fn commit_all(&self) -> &Self {
        self.git_init().stage_all().commit_as(GOOD_EMAIL, "fixture")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
