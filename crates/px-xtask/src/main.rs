#![forbid(unsafe_code)]
//! `cargo xtask <gate>` entry point. See `px_xtask::run`.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    px_xtask::run(&args)
}
