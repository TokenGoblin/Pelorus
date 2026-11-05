//! Config-directory migration (build-spec section 2.3).
//!
//! A rename appends the outgoing directory name to
//! `px_brand::LEGACY_CONFIG_DIRS`. On the next start, [`migrate`] finds the
//! populated profile under the old name and carries it to the new one.
//!
//! # The protocol, and why it is resumable at every step
//!
//! ```text
//! root/
//!   <legacy>/             the old profile. NEVER modified before step 6.
//!   <config>.migrating/   staging. Ours; wiped and rebuilt on every attempt.
//!   <config>/             the new profile. Appears atomically at step 5.
//! ```
//!
//! 1. Pick the newest legacy name that exists (the list is append-ordered).
//!    A symlink, or anything that is not a directory, is refused.
//! 2. Build a *manifest* of the legacy tree: every directory, and for every
//!    file its length, CRC-32 and FNV-1a 64. A symlink anywhere in the tree,
//!    a non-UTF-8 name, or a special file refuses the whole migration:
//!    nothing is followed out of the profile tree.
//! 3. Copy into the staging directory.
//! 4. Verify: re-scan staging and compare the manifest entry by entry, then
//!    re-scan the legacy tree to prove it did not change under us. Only then
//!    write `.migrated` (the manifest, plus the source name) into staging.
//! 5. `rename(staging, config)`. Atomic: the new profile exists completely or
//!    not at all.
//! 6. Remove the legacy tree, **only the files whose length and both
//!    checksums still match the manifest**, then empty directories. Anything
//!    else (a file added or changed since) is left where it is.
//!
//! A crash, or an error, at any point leaves one of: legacy untouched and no
//! new profile (steps 1-4: rerun starts over, wiping staging); new profile
//! complete with legacy intact or partly cleaned (steps 5-6: rerun finds
//! `.migrated` in the new profile and finishes the cleanup from the manifest
//! it recorded). Running it again afterwards does nothing. The test
//! `fail_at_every_step` injects a failure before each filesystem operation and
//! checks all of this.
//!
//! An existing, non-empty `<config>` that has no `.migrated` marker is a
//! profile already in use: it is never overwritten or merged into, whatever
//! legacy directories exist.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use crate::checksum::{FNV_OFFSET, crc32_update, fnv1a64_update};

/// Suffix of the staging directory.
pub const STAGING_SUFFIX: &str = ".migrating";
/// Marker file written into the new profile.
pub const MARKER_NAME: &str = ".migrated";
const MARKER_MAGIC: &str = "pxmigrate 1";
const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 1_000_000;
const CHUNK: usize = 64 * 1024;

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// No legacy directory exists and no profile needs carrying over.
    NoLegacy,
    /// The new profile already exists and is in use; nothing was changed.
    AlreadyCurrent,
    /// The profile was copied, verified and activated.
    Migrated {
        /// Legacy directory name it came from.
        from: String,
        /// Files copied.
        files: usize,
        /// Directories created.
        dirs: usize,
        /// Bytes copied.
        bytes: u64,
        /// Whether the legacy tree is entirely gone afterwards.
        source_removed: bool,
    },
    /// An earlier run committed but was interrupted before cleaning up; the
    /// cleanup was finished now.
    CleanedUp {
        /// Whether the legacy tree is entirely gone afterwards.
        source_removed: bool,
    },
}

/// Why a migration did not happen. The legacy directory is intact for every
/// error returned before the new profile is committed.
#[derive(Debug)]
pub enum MigrateError {
    /// The operating system refused.
    Io {
        /// What was being attempted.
        op: &'static str,
        /// Where.
        path: PathBuf,
        /// The cause.
        source: std::io::Error,
    },
    /// A symlink was found where one must not be followed.
    SymlinkRefused(PathBuf),
    /// A file type that is neither a regular file nor a directory, or a name
    /// that collides with the marker.
    UnsupportedEntry(PathBuf),
    /// A name that is not valid UTF-8, or is not a single path component.
    BadName(PathBuf),
    /// The tree is deeper or larger than the bounds.
    TooLarge(PathBuf),
    /// The copy did not match the source.
    VerificationFailed(String),
    /// A configured directory name is not a single, plain path component.
    InvalidConfigName(String),
    /// A failure injected by the test hook before operation `n`.
    Injected(u32),
}

impl std::fmt::Display for MigrateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { op, path, source } => write!(f, "{op} {}: {source}", path.display()),
            Self::SymlinkRefused(p) => write!(f, "refusing to follow symlink {}", p.display()),
            Self::UnsupportedEntry(p) => write!(f, "unsupported entry {}", p.display()),
            Self::BadName(p) => write!(f, "unusable file name {}", p.display()),
            Self::TooLarge(p) => write!(f, "profile tree too deep or large at {}", p.display()),
            Self::VerificationFailed(why) => write!(f, "verification failed: {why}"),
            Self::InvalidConfigName(n) => write!(f, "invalid config directory name {n:?}"),
            Self::Injected(n) => write!(f, "injected failure before operation {n}"),
        }
    }
}

impl std::error::Error for MigrateError {}

/// A manifest entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kind {
    Dir,
    File { size: u64, crc: u32, fnv: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// `/`-joined relative path.
    pub rel: String,
    pub kind: Kind,
}

// ---------------------------------------------------------------- pure parts

fn hex(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.bytes() {
        for nibble in [b >> 4, b & 15] {
            out.push(char::from_digit(u32::from(nibble), 16).unwrap_or('0'));
        }
    }
    out
}

fn unhex(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < s.len() {
        bytes.push(u8::from_str_radix(s.get(i..i + 2)?, 16).ok()?);
        i += 2;
    }
    String::from_utf8(bytes).ok()
}

/// A relative path is safe to join onto a root only if no component can climb
/// or restart the path.
pub(crate) fn rel_is_safe(rel: &str) -> bool {
    !rel.is_empty()
        && rel.split('/').all(|c| {
            !c.is_empty() && c != "." && c != ".." && !c.contains('\\') && !c.contains('\0')
        })
}

/// A single plain path component usable as a directory name under `root`.
fn name_is_plain(name: &str) -> bool {
    rel_is_safe(name) && !name.contains('/') && !name.contains(':')
}

pub(crate) fn encode_marker(from: &str, entries: &[Entry]) -> String {
    let mut out = format!("{MARKER_MAGIC}\nfrom {from}\n");
    for e in entries {
        match &e.kind {
            Kind::Dir => out.push_str(&format!("D {}\n", hex(&e.rel))),
            Kind::File { size, crc, fnv } => {
                out.push_str(&format!("F {size} {crc:08x} {fnv:016x} {}\n", hex(&e.rel)));
            }
        }
    }
    out.push_str(&format!("end {}\n", entries.len()));
    out
}

pub(crate) fn parse_marker(text: &str) -> Option<(String, Vec<Entry>)> {
    let mut lines = text.split('\n');
    if lines.next()? != MARKER_MAGIC {
        return None;
    }
    let from = lines.next()?.strip_prefix("from ")?.to_owned();
    if !name_is_plain(&from) {
        return None;
    }
    let mut entries = Vec::new();
    loop {
        let line = lines.next()?;
        if let Some(n) = line.strip_prefix("end ") {
            // Exactly the trailer, then the final newline's empty remainder.
            let n: usize = n.parse().ok()?;
            if n != entries.len() || !lines.next()?.is_empty() || lines.next().is_some() {
                return None;
            }
            return Some((from, entries));
        }
        let mut parts = line.split(' ');
        let entry = match parts.next()? {
            "D" => Entry {
                rel: unhex(parts.next()?)?,
                kind: Kind::Dir,
            },
            "F" => {
                let size = parts.next()?.parse().ok()?;
                let crc = u32::from_str_radix(parts.next()?, 16).ok()?;
                let fnv = u64::from_str_radix(parts.next()?, 16).ok()?;
                Entry {
                    rel: unhex(parts.next()?)?,
                    kind: Kind::File { size, crc, fnv },
                }
            }
            _ => return None,
        };
        if parts.next().is_some() || !rel_is_safe(&entry.rel) || entries.len() >= MAX_ENTRIES {
            return None;
        }
        entries.push(entry);
    }
}

// ------------------------------------------------------------ filesystem part

fn io_err(op: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> MigrateError {
    let path = path.to_owned();
    move |source| MigrateError::Io { op, path, source }
}

fn hash_file(path: &Path) -> std::io::Result<(u64, u32, u64)> {
    let mut f = File::open(path)?;
    let mut buf = vec![0u8; CHUNK];
    let (mut size, mut crc, mut fnv) = (0u64, 0u32, FNV_OFFSET);
    loop {
        let n = f.read(&mut buf)?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
            return Ok((size, crc, fnv));
        };
        size += n as u64;
        crc = crc32_update(crc, chunk);
        fnv = fnv1a64_update(fnv, chunk);
    }
}

/// Scan a tree into a sorted manifest, refusing anything that is not a plain
/// directory or regular file.
fn scan(root: &Path, top_level: bool) -> Result<Vec<Entry>, MigrateError> {
    let mut out = Vec::new();
    scan_into(root, "", 0, top_level, &mut out)?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

fn scan_into(
    dir: &Path,
    prefix: &str,
    depth: usize,
    top_level: bool,
    out: &mut Vec<Entry>,
) -> Result<(), MigrateError> {
    if depth > MAX_DEPTH {
        return Err(MigrateError::TooLarge(dir.to_owned()));
    }
    for entry in fs::read_dir(dir).map_err(io_err("read directory", dir))? {
        let entry = entry.map_err(io_err("read directory", dir))?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| MigrateError::BadName(path.clone()))?;
        if !rel_is_safe(&name) || name.contains('/') {
            return Err(MigrateError::BadName(path));
        }
        if top_level && depth == 0 && name == MARKER_NAME {
            return Err(MigrateError::UnsupportedEntry(path));
        }
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        let ty = entry.file_type().map_err(io_err("stat", &path))?;
        if out.len() >= MAX_ENTRIES {
            return Err(MigrateError::TooLarge(path));
        }
        if ty.is_symlink() {
            return Err(MigrateError::SymlinkRefused(path));
        } else if ty.is_dir() {
            out.push(Entry {
                rel: rel.clone(),
                kind: Kind::Dir,
            });
            scan_into(&path, &rel, depth + 1, top_level, out)?;
        } else if ty.is_file() {
            let (size, crc, fnv) = hash_file(&path).map_err(io_err("read", &path))?;
            out.push(Entry {
                rel,
                kind: Kind::File { size, crc, fnv },
            });
        } else {
            return Err(MigrateError::UnsupportedEntry(path));
        }
    }
    Ok(())
}

/// Whether `root/rel` is reached without crossing a symlink in any component.
fn path_has_no_symlink(root: &Path, rel: &str) -> bool {
    let mut cur = root.to_owned();
    for comp in rel.split('/') {
        cur.push(comp);
        match fs::symlink_metadata(&cur) {
            Ok(m) if !m.file_type().is_symlink() => {}
            _ => return false,
        }
    }
    true
}

type Hook<'a> = &'a mut dyn FnMut(u32) -> bool;

struct Migrator<'a> {
    hook: Option<Hook<'a>>,
    ops: u32,
}

impl Migrator<'_> {
    /// Called before every operation that changes the filesystem. A hook that
    /// returns `true` makes the operation (and everything after it) fail,
    /// which is indistinguishable on disk from a crash at that point.
    fn tick(&mut self) -> Result<(), MigrateError> {
        let n = self.ops;
        self.ops += 1;
        if let Some(h) = self.hook.as_mut()
            && h(n)
        {
            return Err(MigrateError::Injected(n));
        }
        Ok(())
    }

    fn remove_tree(&mut self, path: &Path) -> Result<(), MigrateError> {
        match fs::symlink_metadata(path) {
            Ok(m) => {
                self.tick()?;
                if m.is_dir() {
                    fs::remove_dir_all(path).map_err(io_err("remove", path))
                } else {
                    fs::remove_file(path).map_err(io_err("remove", path))
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_err("stat", path)(e)),
        }
    }

    fn copy_file(&mut self, from: &Path, to: &Path) -> Result<(), MigrateError> {
        self.tick()?;
        let mut src = File::open(from).map_err(io_err("open", from))?;
        let mut dst = File::create(to).map_err(io_err("create", to))?;
        let mut buf = vec![0u8; CHUNK];
        loop {
            let n = src.read(&mut buf).map_err(io_err("read", from))?;
            let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
                break;
            };
            dst.write_all(chunk).map_err(io_err("write", to))?;
        }
        dst.sync_all().map_err(io_err("sync", to))?;
        drop(dst);
        let perms = fs::metadata(from)
            .map_err(io_err("stat", from))?
            .permissions();
        fs::set_permissions(to, perms).map_err(io_err("set permissions", to))
    }

    fn make_dir(&mut self, from: Option<&Path>, to: &Path) -> Result<(), MigrateError> {
        self.tick()?;
        fs::create_dir(to).map_err(io_err("create directory", to))?;
        let perms = match from {
            Some(f) => fs::metadata(f).map_err(io_err("stat", f))?.permissions(),
            None => fs::metadata(to).map_err(io_err("stat", to))?.permissions(),
        };
        // Only Unix has a mode to adjust; elsewhere the permissions are copied
        // as they are (and a `mut` binding would be an unused one).
        #[cfg(unix)]
        let perms = {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = perms;
            // The owner must keep rwx or the migration cannot finish; and a
            // profile directory is never more open than its source.
            let mode = if from.is_some() {
                perms.mode() | 0o700
            } else {
                0o700
            };
            perms.set_mode(mode);
            perms
        };
        fs::set_permissions(to, perms).map_err(io_err("set permissions", to))
    }

    /// Step 6: delete exactly what the manifest says was copied, if it is
    /// still exactly that.
    fn cleanup(&mut self, source: &Path, entries: &[Entry]) -> Result<bool, MigrateError> {
        for e in entries {
            let Kind::File { size, crc, fnv } = &e.kind else {
                continue;
            };
            if !path_has_no_symlink(source, &e.rel) {
                continue;
            }
            let path = source.join(&e.rel);
            if hash_file(&path).is_ok_and(|h| h == (*size, *crc, *fnv)) {
                self.tick()?;
                fs::remove_file(&path).map_err(io_err("remove", &path))?;
            }
        }
        for e in entries.iter().rev() {
            if matches!(e.kind, Kind::Dir) && path_has_no_symlink(source, &e.rel) {
                self.tick()?;
                // Fails if anything unrecognised is still inside: that is the
                // point. Not an error.
                let _ = fs::remove_dir(source.join(&e.rel));
            }
        }
        self.tick()?;
        let _ = fs::remove_dir(source);
        Ok(fs::symlink_metadata(source).is_err())
    }

    fn run(
        &mut self,
        root: &Path,
        config_dir: &str,
        legacy_dirs: &[&str],
    ) -> Result<Outcome, MigrateError> {
        for name in std::iter::once(&config_dir).chain(legacy_dirs.iter()) {
            if !name_is_plain(name) {
                return Err(MigrateError::InvalidConfigName((*name).to_owned()));
            }
        }
        let dest = root.join(config_dir);
        let staging = root.join(format!("{config_dir}{STAGING_SUFFIX}"));

        // Is the new profile already there?
        let mut remove_empty_dest = false;
        match fs::symlink_metadata(&dest) {
            Ok(m) if m.file_type().is_symlink() => {
                return Err(MigrateError::SymlinkRefused(dest));
            }
            Ok(m) if m.is_dir() => {
                if let Some((from, entries)) = fs::read_to_string(dest.join(MARKER_NAME))
                    .ok()
                    .and_then(|t| parse_marker(&t))
                {
                    let source = root.join(&from);
                    return match fs::symlink_metadata(&source) {
                        Ok(m) if m.is_dir() => Ok(Outcome::CleanedUp {
                            source_removed: self.cleanup(&source, &entries)?,
                        }),
                        _ => Ok(Outcome::AlreadyCurrent),
                    };
                }
                let empty = fs::read_dir(&dest)
                    .map_err(io_err("read directory", &dest))?
                    .next()
                    .is_none();
                if !empty {
                    return Ok(Outcome::AlreadyCurrent);
                }
                // An empty directory is a profile that was never populated;
                // it must not block the migration (rename onto it is not
                // portable), so it goes.
                remove_empty_dest = true;
            }
            Ok(_) => return Err(MigrateError::UnsupportedEntry(dest)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_err("stat", &dest)(e)),
        }

        // Newest legacy name that exists.
        let mut found = None;
        for name in legacy_dirs.iter().rev() {
            if *name == config_dir {
                continue;
            }
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(MigrateError::SymlinkRefused(path));
                }
                Ok(m) if m.is_dir() => {
                    found = Some((*name, path));
                    break;
                }
                Ok(_) => return Err(MigrateError::UnsupportedEntry(path)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(io_err("stat", &path)(e)),
            }
        }
        let Some((from, source)) = found else {
            return Ok(Outcome::NoLegacy);
        };

        // 2. Manifest of the source (refuses symlinks and oddities).
        let manifest = scan(&source, true)?;

        // 3. Copy into a fresh staging directory.
        if remove_empty_dest {
            self.tick()?;
            fs::remove_dir(&dest).map_err(io_err("remove", &dest))?;
        }
        self.remove_tree(&staging)?;
        self.make_dir(None, &staging)?;
        let (mut files, mut dirs, mut bytes) = (0, 0, 0u64);
        for e in &manifest {
            let to = staging.join(&e.rel);
            let from_path = source.join(&e.rel);
            match &e.kind {
                Kind::Dir => {
                    self.make_dir(Some(&from_path), &to)?;
                    dirs += 1;
                }
                Kind::File { size, .. } => {
                    self.copy_file(&from_path, &to)?;
                    files += 1;
                    bytes += size;
                }
            }
        }

        // 4. Verify the copy, and that the source did not move meanwhile.
        let copied = scan(&staging, true)?;
        if copied != manifest {
            let _ = self.remove_tree(&staging);
            return Err(MigrateError::VerificationFailed(
                "the copy differs from the source".into(),
            ));
        }
        if scan(&source, true)? != manifest {
            let _ = self.remove_tree(&staging);
            return Err(MigrateError::VerificationFailed(
                "the source changed during the copy".into(),
            ));
        }
        let marker_path = staging.join(MARKER_NAME);
        self.tick()?;
        let mut marker = File::create(&marker_path).map_err(io_err("create", &marker_path))?;
        marker
            .write_all(encode_marker(from, &manifest).as_bytes())
            .map_err(io_err("write", &marker_path))?;
        marker.sync_all().map_err(io_err("sync", &marker_path))?;
        drop(marker);

        // 5. Commit.
        self.tick()?;
        fs::rename(&staging, &dest).map_err(io_err("rename", &staging))?;

        // 6. Only now may the source go, and only what was verified.
        let source_removed = self.cleanup(&source, &manifest)?;
        Ok(Outcome::Migrated {
            from: from.to_owned(),
            files,
            dirs,
            bytes,
            source_removed,
        })
    }
}

/// Migrate `root/<legacy>` to `root/<config_dir>` if needed. See the module
/// documentation for the protocol and its guarantees.
pub fn migrate(
    root: &Path,
    config_dir: &str,
    legacy_dirs: &[&str],
) -> Result<Outcome, MigrateError> {
    Migrator { hook: None, ops: 0 }.run(root, config_dir, legacy_dirs)
}

/// [`migrate`] with this product's configured names: `px_brand::CONFIG_DIR`
/// and `px_brand::LEGACY_CONFIG_DIRS`.
pub fn migrate_default(root: &Path) -> Result<Outcome, MigrateError> {
    migrate(root, px_brand::CONFIG_DIR, px_brand::LEGACY_CONFIG_DIRS)
}

/// Test hook: `hook(n)` is called before the n-th filesystem-changing
/// operation; returning `true` fails it. Used to simulate a crash at each step.
#[doc(hidden)]
pub fn migrate_with_hook(
    root: &Path,
    config_dir: &str,
    legacy_dirs: &[&str],
    hook: &mut dyn FnMut(u32) -> bool,
) -> Result<Outcome, MigrateError> {
    Migrator {
        hook: Some(hook),
        ops: 0,
    }
    .run(root, config_dir, legacy_dirs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(rel: &str, size: u64) -> Entry {
        Entry {
            rel: rel.into(),
            kind: Kind::File {
                size,
                crc: 0xdead_beef,
                fnv: 0x0123_4567_89ab_cdef,
            },
        }
    }

    #[test]
    fn hex_round_trips_unicode() {
        for s in ["", "a", "naïve/日本語/файл", "with space"] {
            assert_eq!(unhex(&hex(s)).as_deref(), Some(s));
        }
        assert_eq!(hex("A/"), "412f");
        assert_eq!(unhex("4"), None);
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("ff"), None, "invalid UTF-8");
    }

    #[test]
    fn marker_by_hand_and_round_trip() {
        let entries = vec![
            Entry {
                rel: "d".into(),
                kind: Kind::Dir,
            },
            file("d/f", 5),
        ];
        let text = encode_marker("old", &entries);
        assert_eq!(
            text,
            "pxmigrate 1\nfrom old\nD 64\nF 5 deadbeef 0123456789abcdef 642f66\nend 2\n"
        );
        assert_eq!(parse_marker(&text), Some(("old".into(), entries)));
    }

    #[test]
    fn marker_rejects_truncation_and_tampering() {
        let text = encode_marker("old", &[file("a", 1), file("b", 2)]);
        for n in 0..text.len() {
            assert_eq!(
                parse_marker(text.get(..n).unwrap_or_default()),
                None,
                "n={n}"
            );
        }
        assert!(parse_marker(&text).is_some());
        assert_eq!(parse_marker(&format!("{text}extra\n")), None);
        assert_eq!(parse_marker(&text.replace("end 2", "end 3")), None);
        // A path that climbs out of the tree.
        let evil = format!("pxmigrate 1\nfrom old\nD {}\nend 1\n", hex("../x"));
        assert_eq!(parse_marker(&evil), None);
        let evil = "pxmigrate 1\nfrom ../old\nend 0\n";
        assert_eq!(parse_marker(evil), None);
        let evil = format!("pxmigrate 1\nfrom old\nD {}\nend 1\n", hex("/abs"));
        assert_eq!(parse_marker(&evil), None);
    }

    #[test]
    fn plain_names() {
        assert!(name_is_plain("legacy-profile"));
        for bad in ["", ".", "..", "a/b", "a\\b", "C:x", "a\0b"] {
            assert!(!name_is_plain(bad), "{bad:?}");
        }
        assert!(rel_is_safe("a/b/c"));
        assert!(!rel_is_safe("a//b"));
        assert!(!rel_is_safe("a/../b"));
    }
}
