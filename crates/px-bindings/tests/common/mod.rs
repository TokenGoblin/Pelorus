//! Shared helpers: generate Rust from IDL text and compile it with `rustc`.

#![allow(dead_code)]

use px_bindings::emit::{Emitter, RustHostEmitter};
use px_bindings::resolve::BuildOptions;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn generate_from(files: &[(&str, &str)], strict: bool) -> Result<String, Vec<String>> {
    let sources: Vec<(String, String)> = files
        .iter()
        .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
        .collect();
    let emitters: [&dyn Emitter; 1] = [&RustHostEmitter];
    let out = px_bindings::generate(
        &sources,
        BuildOptions {
            require_exposed: strict,
        },
        &emitters,
    )?;
    Ok(out
        .into_iter()
        .next()
        .map(|o| o.contents)
        .unwrap_or_default())
}

pub fn generate(idl: &str) -> String {
    match generate_from(&[("t.idl", idl)], false) {
        Ok(s) => s,
        Err(e) => fail!("{e:#?}\n{idl}"),
    }
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Compile `src` as a library with warnings denied. `Ok` means rustc exited 0.
pub fn compile(src: &str) -> Result<(), String> {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir: PathBuf =
        std::env::temp_dir().join(format!("px-bindings-test-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("generated.rs");
    std::fs::write(&file, src).map_err(|e| e.to_string())?;
    let out = Command::new("rustc")
        .args(["--edition", "2024", "--crate-type", "lib", "--crate-name", "generated"])
        .args(["-D", "warnings", "--emit=metadata", "--out-dir"])
        .arg(&dir)
        .arg(&file)
        .output()
        .map_err(|e| format!("could not run rustc: {e}"))?;
    let _ = std::fs::remove_dir_all(&dir);
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[track_caller]
pub fn assert_compiles(src: &str) {
    if let Err(e) = compile(src) {
        let numbered: String = src
            .lines()
            .enumerate()
            .map(|(i, l)| format!("{:5} {l}\n", i + 1))
            .collect();
        fail!("generated Rust does not compile:\n{e}\n{numbered}");
    }
}
