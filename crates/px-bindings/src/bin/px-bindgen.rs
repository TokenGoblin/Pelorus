#![forbid(unsafe_code)]

//! `px-bindgen`: run the generator over IDL files.
//!
//! ```text
//! cargo run -p px-bindings --bin px-bindgen -- [--lenient] [--out FILE] PATH...
//! ```
//!
//! Each `PATH` is an `.idl` file or a directory whose `.idl` files are read in
//! sorted name order (so output does not depend on directory order). With no
//! `--out` the Rust host traits go to standard output. `--lenient` stops
//! requiring `[Exposed]` on every interface. Exit status is 0 on success and 1
//! on any diagnostic, each printed as `file:line:col: error: ...`.
//!
//! This is the only place in the crate that touches the file system: it is a
//! build-time tool, and the library itself is pure text in, text out.

use px_bindings::emit::{Emitter, RustHostEmitter};
use px_bindings::resolve::BuildOptions;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

fn collect(path: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
    if path.is_dir() {
        let rd = std::fs::read_dir(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut files: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "idl"))
            .collect();
        files.sort();
        out.extend(files);
    } else {
        out.push(path.to_path_buf());
    }
    Ok(())
}

fn run() -> Result<(), Vec<String>> {
    let mut lenient = false;
    let mut out_path: Option<PathBuf> = None;
    let mut inputs: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--lenient" => lenient = true,
            "--out" => match args.next() {
                Some(p) => out_path = Some(PathBuf::from(p)),
                None => return Err(vec!["--out needs a file name".to_owned()]),
            },
            other => inputs.push(PathBuf::from(other)),
        }
    }
    if inputs.is_empty() {
        return Err(vec!["usage: px-bindgen [--lenient] [--out FILE] PATH...".to_owned()]);
    }
    let mut files = Vec::new();
    for i in &inputs {
        collect(i, &mut files).map_err(|e| vec![e])?;
    }
    let mut sources = Vec::new();
    for f in &files {
        let text = std::fs::read_to_string(f).map_err(|e| vec![format!("{}: {e}", f.display())])?;
        sources.push((f.display().to_string(), text));
    }
    let opts = BuildOptions {
        require_exposed: !lenient,
    };
    let emitters: [&dyn Emitter; 1] = [&RustHostEmitter];
    let outputs = px_bindings::generate(&sources, opts, &emitters)?;
    for o in outputs {
        match &out_path {
            Some(p) => std::fs::write(p, o.contents).map_err(|e| vec![format!("{}: {e}", p.display())])?,
            None => print!("{}", o.contents),
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(errs) => {
            for e in errs {
                eprintln!("{e}");
            }
            ExitCode::FAILURE
        }
    }
}
