//! A minimal tar extractor, std only: just enough for `git archive
//! --format=tar` output.
//!
//! Why not `tar -x`: the script piped `git archive` into the system `tar`, which
//! on Windows is either Git Bash's GNU tar (which reads `D:\path` as `host:path`)
//! or bsdtar, and the old script spent a function normalising paths for it.
//! Extracting in-process removes the second program and the path-dialect problem
//! from the reproducibility job.
//!
//! Handles ustar entries (with the `prefix` field), pax extended headers
//! (`x`, for long paths; `g`, the global header carrying the commit id, is
//! skipped) and GNU long names (`L`); regular files, directories and (on Unix)
//! symlinks. Anything else is an error, and so is a path that is absolute or
//! climbs out of the destination: fail closed.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

const BLOCK: usize = 512;

fn read_block<R: Read>(r: &mut R) -> Result<Option<[u8; BLOCK]>, String> {
    let mut b = [0u8; BLOCK];
    let mut got = 0;
    while got < BLOCK {
        let n = r
            .read(&mut b[got..])
            .map_err(|e| format!("tar read: {e}"))?;
        if n == 0 {
            if got == 0 {
                return Ok(None);
            }
            return Err("tar: truncated header".into());
        }
        got += n;
    }
    Ok(Some(b))
}

fn cstr(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// Octal number field (NUL/space padded). Base-256 (high bit set) is the GNU
/// extension for sizes over 8 GiB.
pub fn parse_octal(field: &[u8]) -> Result<u64, String> {
    if field.first().is_some_and(|b| b & 0x80 != 0) {
        let mut v: u64 = (field[0] & 0x7f) as u64;
        for b in &field[1..] {
            v = v
                .checked_mul(256)
                .and_then(|v| v.checked_add(*b as u64))
                .ok_or("tar: number too large")?;
        }
        return Ok(v);
    }
    let s: String = field
        .iter()
        .take_while(|b| **b != 0)
        .map(|b| *b as char)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(s, 8).map_err(|_| format!("tar: bad octal field `{s}`"))
}

/// Records of a pax extended header: `<len> <key>=<value>\n`.
pub fn parse_pax(data: &[u8]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let Some(sp) = data[i..].iter().position(|b| *b == b' ') else {
            break;
        };
        let Ok(len) = String::from_utf8_lossy(&data[i..i + sp]).parse::<usize>() else {
            break;
        };
        if len == 0 || i + len > data.len() {
            break;
        }
        let rec = &data[i + sp + 1..i + len];
        let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
        let text = String::from_utf8_lossy(rec);
        if let Some((k, v)) = text.split_once('=') {
            out.push((k.to_string(), v.to_string()));
        }
        i += len;
    }
    out
}

/// A destination-relative path that cannot escape `dest`.
pub fn safe_join(dest: &Path, name: &str) -> Result<PathBuf, String> {
    let rel = Path::new(name.trim_end_matches('/'));
    let mut out = dest.to_path_buf();
    for c in rel.components() {
        match c {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return Err(format!("tar: refusing path `{name}`")),
        }
    }
    Ok(out)
}

fn read_exact_vec<R: Read>(r: &mut R, n: u64) -> Result<Vec<u8>, String> {
    let mut v = Vec::new();
    r.take(n)
        .read_to_end(&mut v)
        .map_err(|e| format!("tar read: {e}"))?;
    if v.len() as u64 != n {
        return Err("tar: truncated entry".into());
    }
    Ok(v)
}

fn skip_padding<R: Read>(r: &mut R, size: u64) -> Result<(), String> {
    let pad = (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64;
    read_exact_vec(r, pad).map(|_| ())
}

/// Extract the archive on `r` under `dest` (which must exist). Returns the
/// number of regular files written.
pub fn extract<R: Read>(mut r: R, dest: &Path) -> Result<usize, String> {
    let mut files = 0;
    let mut next_path: Option<String> = None;
    let mut next_link: Option<String> = None;
    while let Some(h) = read_block(&mut r)? {
        if h.iter().all(|b| *b == 0) {
            break;
        }
        let name = cstr(&h[0..100]);
        let mode = parse_octal(&h[100..108])?;
        let size = parse_octal(&h[124..136])?;
        let kind = h[156];
        let link = cstr(&h[157..257]);
        let prefix = if &h[257..262] == b"ustar" {
            cstr(&h[345..500])
        } else {
            String::new()
        };
        let mut path = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };

        match kind {
            b'x' => {
                let data = read_exact_vec(&mut r, size)?;
                skip_padding(&mut r, size)?;
                for (k, v) in parse_pax(&data) {
                    match k.as_str() {
                        "path" => next_path = Some(v),
                        "linkpath" => next_link = Some(v),
                        _ => {}
                    }
                }
                continue;
            }
            b'g' => {
                read_exact_vec(&mut r, size)?;
                skip_padding(&mut r, size)?;
                continue;
            }
            b'L' => {
                let data = read_exact_vec(&mut r, size)?;
                skip_padding(&mut r, size)?;
                next_path = Some(cstr(&data));
                continue;
            }
            _ => {}
        }

        if let Some(p) = next_path.take() {
            path = p;
        }
        let link = next_link.take().unwrap_or(link);
        let target = safe_join(dest, &path)?;
        match kind {
            b'0' | 0 | b'7' => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                }
                let data = read_exact_vec(&mut r, size)?;
                skip_padding(&mut r, size)?;
                std::fs::write(&target, &data)
                    .map_err(|e| format!("write {}: {e}", target.display()))?;
                set_mode(&target, mode)?;
                files += 1;
            }
            b'5' => {
                std::fs::create_dir_all(&target)
                    .map_err(|e| format!("mkdir {}: {e}", target.display()))?;
            }
            b'2' => {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
                }
                symlink(&link, &target)?;
            }
            other => {
                return Err(format!(
                    "tar: unsupported entry type `{}` for `{path}`",
                    other as char
                ));
            }
        }
    }
    Ok(files)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u64) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    // git records 0644 or 0755 only; keep the execute bits.
    let m = if mode & 0o111 != 0 { 0o755 } else { 0o644 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(m))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u64) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn symlink(link: &str, target: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(link, target)
        .map_err(|e| format!("symlink {}: {e}", target.display()))
}

#[cfg(not(unix))]
fn symlink(_link: &str, target: &Path) -> Result<(), String> {
    Err(format!(
        "tar: symlink `{}` cannot be extracted on this platform",
        target.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::Fixture;

    fn header(name: &str, kind: u8, size: usize, mode: u32) -> [u8; BLOCK] {
        let mut h = [0u8; BLOCK];
        h[..name.len()].copy_from_slice(name.as_bytes());
        h[100..108].copy_from_slice(format!("{mode:07o}\0").as_bytes());
        h[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        h[156] = kind;
        h[257..262].copy_from_slice(b"ustar");
        h
    }

    fn entry(name: &str, kind: u8, data: &[u8], mode: u32) -> Vec<u8> {
        let mut v = header(name, kind, data.len(), mode).to_vec();
        v.extend_from_slice(data);
        v.resize(v.len().next_multiple_of(BLOCK), 0);
        v
    }

    #[test]
    fn octal_fields() {
        assert_eq!(parse_octal(b"0000644\0").unwrap(), 0o644);
        assert_eq!(parse_octal(b"00000000012\0").unwrap(), 10);
        assert_eq!(parse_octal(b"\0\0\0").unwrap(), 0);
        assert!(parse_octal(b"89\0").is_err());
        assert_eq!(parse_octal(&[0x80, 0, 0, 0, 0, 0, 1, 0]).unwrap(), 256);
    }

    #[test]
    fn pax_records() {
        // each record's length counts its own digits, the space and the newline
        let r2 = "comment=0123456789abcdef\n";
        let n = r2.len() + 3;
        let mut data = b"17 path=a/b/c.rs\n".to_vec();
        data.extend_from_slice(format!("{n} {r2}").as_bytes());
        let got = parse_pax(&data);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], ("path".into(), "a/b/c.rs".into()));
        assert_eq!(got[1], ("comment".into(), "0123456789abcdef".into()));
    }

    #[test]
    fn extracts_files_dirs_and_modes_and_a_pax_long_name() {
        let long = format!("deep/{}/file.txt", "d".repeat(150));
        let rec_body = format!(" path={long}\n");
        let len = rec_body.len() + 3;
        let pax = format!("{len}{rec_body}");
        assert_eq!(pax.len(), len);
        let mut tar = Vec::new();
        tar.extend(entry("pax_global_header", b'g', b"52 comment=abc\n", 0o666));
        tar.extend(entry("dir/", b'5', b"", 0o755));
        tar.extend(entry("dir/a.txt", b'0', b"hello", 0o644));
        tar.extend(entry("dir/run.sh", b'0', b"#!/bin/sh\n", 0o755));
        tar.extend(entry("PaxHeader", b'x', pax.as_bytes(), 0o644));
        tar.extend(entry("truncated-name", b'0', b"long", 0o644));
        tar.extend(vec![0u8; 1024]);
        let f = Fixture::new("tar-basic");
        let n = extract(&tar[..], f.path()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(std::fs::read(f.path().join("dir/a.txt")).unwrap(), b"hello");
        assert_eq!(std::fs::read(f.path().join(&long)).unwrap(), b"long");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = |p: &str| {
                std::fs::metadata(f.path().join(p))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777
            };
            assert_eq!(m("dir/run.sh"), 0o755);
            assert_eq!(m("dir/a.txt"), 0o644);
        }
    }

    #[test]
    fn refuses_paths_that_escape() {
        let f = Fixture::new("tar-escape");
        for bad in ["../evil", "/abs/evil", "a/../../evil"] {
            let mut tar = entry(bad, b'0', b"x", 0o644);
            tar.extend(vec![0u8; 1024]);
            let e = extract(&tar[..], f.path()).unwrap_err();
            assert!(e.contains("refusing path"), "{bad}: {e}");
        }
    }

    #[test]
    fn unsupported_entry_types_and_truncation_fail_closed() {
        let f = Fixture::new("tar-bad");
        let tar = entry("x", b'3', b"", 0o644);
        assert!(
            extract(&tar[..], f.path())
                .unwrap_err()
                .contains("unsupported")
        );
        let mut t = entry("x", b'0', &[1u8; 600], 0o644);
        t.truncate(700);
        assert!(extract(&t[..], f.path()).is_err());
    }

    /// The real producer: a git repository's `git archive` output extracts to
    /// exactly the tracked files, with the exec bit kept, and nothing untracked.
    #[test]
    fn extracts_real_git_archive_output() {
        let repo = Fixture::new("tar-git");
        repo.write("src/lib.rs", "pub fn f() {}\n");
        repo.write("ci/run.sh", "#!/bin/sh\n");
        repo.write(&format!("{}/deep.txt", "n".repeat(120)), "deep");
        repo.git_init().git(&["add", "-A"]);
        repo.git(&["update-index", "--chmod=+x", "ci/run.sh"]);
        repo.commit_as(crate::testutil::GOOD_EMAIL, "x");
        repo.write("untracked.txt", "no");
        let out = crate::git::git(repo.path(), &["archive", "--format=tar", "HEAD"]).unwrap();
        let dest = Fixture::new("tar-git-out");
        let n = extract(&out[..], dest.path()).unwrap();
        assert_eq!(n, 3);
        assert_eq!(
            std::fs::read_to_string(dest.path().join("src/lib.rs")).unwrap(),
            "pub fn f() {}\n"
        );
        assert_eq!(
            std::fs::read(dest.path().join(format!("{}/deep.txt", "n".repeat(120)))).unwrap(),
            b"deep"
        );
        assert!(!dest.path().join("untracked.txt").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let m = std::fs::metadata(dest.path().join("ci/run.sh"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(m & 0o111, 0o111);
        }
    }
}
