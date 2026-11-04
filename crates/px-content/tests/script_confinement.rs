//! Phase 10: script runs only in the sandboxed content process (ADR 040
//! decisions 1 and 2, invariant 1).
//!
//! "The broker does not link the engine" is a statement about the dependency
//! graph, so it is checked on the graph (`cargo tree`), with a positive control:
//! the same query on `px-content` must find `boa_engine`, or a query that finds
//! nothing anywhere would pass this test for the wrong reason.

use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(test)]
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root")
}

/// `cargo tree -p <package> -e all --all-features --prefix none`, deduplicated.
#[cfg(test)]
fn tree(package: &str) -> Vec<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let out = Command::new(cargo)
        .current_dir(workspace_root())
        .args([
            "tree",
            "--locked",
            "--offline",
            "-p",
            package,
            "-e",
            "all",
            "--all-features",
            "--prefix",
            "none",
        ])
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "cargo tree -p {package} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect();
    names.sort();
    names.dedup();
    names
}

#[cfg(test)]
fn engine_crates(names: &[String]) -> Vec<&String> {
    names
        .iter()
        .filter(|n| n.starts_with("boa_") || n.as_str() == "px-script")
        .collect()
}

#[test]
fn script_confinement_the_broker_links_no_boa() {
    let broker = tree("px-broker");
    assert!(
        broker.iter().any(|n| n == "px-ipc"),
        "the query did not see the broker's own dependencies: {broker:?}"
    );
    let found = engine_crates(&broker);
    assert!(found.is_empty(), "px-broker links the engine: {found:?}");
}

#[test]
fn script_confinement_the_positive_control_finds_boa_under_the_content_process() {
    let content = tree("px-content");
    for expected in ["px-script", "boa_engine", "boa_gc"] {
        assert!(
            content.iter().any(|n| n == expected),
            "px-content's tree lacks {expected}: the confinement query cannot fail"
        );
    }
}

#[test]
fn script_confinement_no_other_process_binary_links_the_engine() {
    for package in ["px-ipc", "px-sandbox", "px-gpu", "px-net", "px-store"] {
        let names = tree(package);
        let found = engine_crates(&names);
        assert!(found.is_empty(), "{package} links the engine: {found:?}");
    }
}

/// The graph query is the property; this is the declaration it follows from:
/// only `px-content` names `px-script`, and only `px-script` names `boa_*`.
#[test]
fn script_confinement_only_the_expected_manifests_name_the_engine() {
    let crates_dir = workspace_root().join("crates");
    let mut script_dependents = Vec::new();
    let mut boa_dependents = Vec::new();
    for entry in std::fs::read_dir(crates_dir).expect("crates dir") {
        let dir = entry.expect("entry").path();
        let Ok(manifest) = std::fs::read_to_string(dir.join("Cargo.toml")) else {
            continue;
        };
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        for line in manifest.lines() {
            let key = line.split('=').next().unwrap_or_default().trim();
            if key == "px-script" && name != "px-script" {
                script_dependents.push(name.clone());
            }
            if key.starts_with("boa_") {
                boa_dependents.push(name.clone());
            }
        }
    }
    script_dependents.sort();
    script_dependents.dedup();
    boa_dependents.sort();
    boa_dependents.dedup();
    assert_eq!(script_dependents, ["px-content"]);
    assert_eq!(boa_dependents, ["px-script"]);
}
