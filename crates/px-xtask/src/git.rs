//! Thin wrappers over the `git` CLI. Everything takes the repository root so a
//! gate can be pointed at a fixture repository in a test.
//!
//! Paths are returned as `String` (lossily decoded); `-z` is always used so
//! spaces and newlines in names cannot split a record.

use crate::proc::capture;
use std::path::Path;

pub fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    capture("git", args, Some(root))
}

fn split_nul(bytes: &[u8]) -> Vec<String> {
    bytes
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

/// `git ls-files -z [flags] -- [pathspecs]`.
pub fn ls_files(root: &Path, flags: &[&str], pathspecs: &[&str]) -> Result<Vec<String>, String> {
    let mut args = vec!["ls-files", "-z"];
    args.extend_from_slice(flags);
    if !pathspecs.is_empty() {
        args.push("--");
        args.extend_from_slice(pathspecs);
    }
    Ok(split_nul(&git(root, &args)?))
}

/// One index entry from `git ls-files -s`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexEntry {
    pub mode: String,
    pub path: String,
}

pub fn ls_files_staged(root: &Path, pathspecs: &[&str]) -> Result<Vec<IndexEntry>, String> {
    let mut args = vec!["ls-files", "-s", "-z"];
    if !pathspecs.is_empty() {
        args.push("--");
        args.extend_from_slice(pathspecs);
    }
    let mut v = Vec::new();
    for rec in split_nul(&git(root, &args)?) {
        // "<mode> <sha> <stage>\t<path>"
        let Some((meta, path)) = rec.split_once('\t') else {
            continue;
        };
        let mode = meta.split(' ').next().unwrap_or("").to_string();
        v.push(IndexEntry {
            mode,
            path: path.to_string(),
        });
    }
    Ok(v)
}
