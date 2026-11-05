//! System font discovery: the real `std::fs` side of `px_text::sysfonts`.
//!
//! `/CLAUDE.md` confines filesystem access to px-broker and px-store, so the scan
//! logic lives in px-text as a pure function over a `DirReader` and this module is
//! the one implementation that touches a disk. Symlinks are reported as symlinks
//! (`symlink_metadata`, never followed), so a link cannot lead the scan out of the
//! font directories. Reads are capped: a file longer than the cap is refused
//! without being allocated.
//!
//! System fonts are never used by the deterministic render corpus or its tests;
//! they reach text only through an explicit, non-baselined path.
//!
//! The Windows directories are produced by `px_text::sysfonts::default_roots`
//! from `%SystemRoot%` and `%LOCALAPPDATA%`; that branch has been run only against
//! a fake environment (this build was made on Linux).

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use px_text::sysfonts::{
    DirReader, EntryKind, Meta, Platform, ScanLimits, SystemFonts, default_roots,
};

/// [`DirReader`] over `std::fs`.
#[derive(Clone, Copy, Debug, Default)]
pub struct FsReader;

impl DirReader for FsReader {
    fn list_dir(&self, dir: &Path) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(dir)? {
            // A name that is not UTF-8 cannot be a font we would match by name;
            // skip it rather than guess.
            if let Ok(name) = entry?.file_name().into_string() {
                names.push(name);
            }
        }
        Ok(names)
    }

    fn metadata(&self, path: &Path) -> io::Result<Meta> {
        let m = fs::symlink_metadata(path)?;
        let t = m.file_type();
        let kind = if t.is_symlink() {
            EntryKind::Symlink
        } else if t.is_dir() {
            EntryKind::Dir
        } else if t.is_file() {
            EntryKind::File
        } else {
            EntryKind::Other
        };
        Ok(Meta { kind, len: m.len() })
    }

    fn read_bytes(&self, path: &Path, max_len: u64) -> io::Result<Vec<u8>> {
        let file = fs::File::open(path)?;
        let mut out = Vec::new();
        // One byte past the cap tells "exactly the cap" from "longer".
        file.take(max_len.saturating_add(1)).read_to_end(&mut out)?;
        if out.len() as u64 > max_len {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(out)
    }
}

/// The directories scanned on this OS.
#[must_use]
pub fn system_font_roots() -> Vec<PathBuf> {
    let platform = if cfg!(windows) {
        Platform::Windows
    } else {
        Platform::Linux
    };
    default_roots(platform, &|k| std::env::var(k).ok())
}

/// Scan this machine's font directories with the default limits.
#[must_use]
pub fn scan_system_fonts() -> SystemFonts {
    SystemFonts::scan(&FsReader, &system_font_roots(), &ScanLimits::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_text::registry::{NOTO_SANS_BYTES, NOTO_SANS_HEBREW_BYTES};
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicU32 = AtomicU32::new(0);
            let p = std::env::temp_dir().join(format!(
                "px-sysfonts-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).expect("create temp dir");
            Tmp(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn scans_a_real_directory_tree() {
        let t = Tmp::new();
        fs::create_dir_all(t.0.join("b/deeper")).expect("mkdir");
        fs::write(t.0.join("b/NotoSans.ttf"), NOTO_SANS_BYTES).expect("write");
        fs::write(t.0.join("b/deeper/Hebrew.TTF"), NOTO_SANS_HEBREW_BYTES).expect("write");
        fs::write(t.0.join("readme.txt"), b"x").expect("write");
        fs::write(t.0.join("junk.otf"), b"not a font").expect("write");
        let s = SystemFonts::scan(
            &FsReader,
            std::slice::from_ref(&t.0),
            &ScanLimits::default(),
        );
        let families: Vec<_> = s.faces().iter().map(|f| f.family.as_str()).collect();
        assert_eq!(families, ["Noto Sans", "Noto Sans Hebrew"]);
        assert_eq!(s.faces()[0].weight, 400);
        assert!(!s.faces()[0].italic);
        // Deterministic.
        assert_eq!(
            s,
            SystemFonts::scan(
                &FsReader,
                std::slice::from_ref(&t.0),
                &ScanLimits::default()
            )
        );
        assert!(s.lookup("Noto Sans Hebrew", 400, false).is_some());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_followed_out_of_the_tree() {
        let t = Tmp::new();
        let outside = Tmp::new();
        fs::write(outside.0.join("Outside.ttf"), NOTO_SANS_BYTES).expect("write");
        let root = t.0.join("fonts");
        fs::create_dir_all(&root).expect("mkdir");
        std::os::unix::fs::symlink(&outside.0, root.join("dirlink")).expect("symlink");
        std::os::unix::fs::symlink(outside.0.join("Outside.ttf"), root.join("filelink.ttf"))
            .expect("symlink");
        let s = SystemFonts::scan(&FsReader, &[root], &ScanLimits::default());
        assert!(s.is_empty(), "{:?}", s.faces());
    }

    #[test]
    fn missing_directory_and_oversize_file_are_handled() {
        let t = Tmp::new();
        assert!(FsReader.list_dir(&t.0.join("absent")).is_err());
        fs::write(t.0.join("big.ttf"), vec![0u8; 100]).expect("write");
        assert!(FsReader.read_bytes(&t.0.join("big.ttf"), 99).is_err());
        assert_eq!(
            FsReader
                .read_bytes(&t.0.join("big.ttf"), 100)
                .map(|v| v.len())
                .ok(),
            Some(100)
        );
        let s = SystemFonts::scan(&FsReader, &[t.0.join("absent")], &ScanLimits::default());
        assert!(s.is_empty());
    }

    #[test]
    fn this_machines_roots_scan_without_panicking() {
        // Whatever is installed (possibly nothing): bounded and deterministic.
        let a = scan_system_fonts();
        let b = scan_system_fonts();
        assert_eq!(a, b);
    }
}
