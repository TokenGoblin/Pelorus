#![forbid(unsafe_code)]

//! The GPU process.
//!
//! Its channel is its inherited stdin and stdout, handed to it by the broker at
//! spawn (ADR 005). It opens nothing and connects to nothing, and it makes sure
//! of that before it reads a byte: the first thing it does is enter stage two of
//! its sandbox profile (see `px_sandbox::enter_gpu_profile`), and if that fails
//! for any reason it exits without serving. Only then does it tell the broker
//! it is ready, and the broker checks with the kernel, not with this process,
//! that the profile is really in place before sending it anything.
//!
//! The rasterizer is the software one (ADR 034): the process boundary, the
//! profile and the fallback are what Phase 8 proves, and they are the same
//! whichever backend sits inside.

use std::io::{BufReader, BufWriter, stdin, stdout};
use std::process::ExitCode;

use px_gpu::rasterizer::SoftwareRasterizer;
use px_gpu::server;
use px_ipc::Channel;
use px_ipc::gpu::GpuResponse;

fn main() -> ExitCode {
    // Everything that allocates or touches stdio for the first time happens
    // before the profile lands, so the profile never has to allow it.
    let mut channel = Channel::new(BufReader::new(stdin()), BufWriter::new(stdout()));

    // No granted paths: the software rasterizer needs no file. A backend that
    // does (a driver, a render node) names them here, and the profile grants
    // read access to exactly those.
    let profile = match px_sandbox::enter_gpu_profile(&[]) {
        Ok(profile) => profile,
        Err(error) => {
            eprintln!("px-gpu: refusing to serve: {error}");
            return ExitCode::FAILURE;
        }
    };

    let ready = GpuResponse::Ready {
        landlock_abi: profile.landlock_abi,
    };
    if channel.send(&ready).is_err() {
        return ExitCode::FAILURE;
    }

    match server::serve(&mut channel, &mut SoftwareRasterizer) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("px-gpu: {error}");
            ExitCode::FAILURE
        }
    }
}
