//! Testing only: a GPU process that misbehaves on command (§14.4).
//!
//! Compiled only under `cfg(feature = "testing")`. Each fault is a way a real
//! GPU process could go wrong, produced on purpose so a broker test can watch
//! the session survive it: dying, writing garbage, claiming an enormous
//! message, hanging, stopping mid-frame, flooding, and lying about the frame.

use std::io::{Read, Write};

use px_ipc::gpu::{GpuFailure, GpuFault, GpuResponse, MAX_STRIP_BYTES, StripPixels};
use px_ipc::{Channel, MAX_MESSAGE_BYTES};

use crate::server::{ServeError, send_frame};
use crate::wire;

fn hang() -> ! {
    loop {
        std::thread::sleep(std::time::Duration::from_secs(3600));
    }
}

/// Misbehave in the way `fault` names, in place of answering `canvas` honestly.
pub(crate) fn misbehave<R: Read, W: Write>(
    channel: &mut Channel<R, W>,
    fault: GpuFault,
    serial: u32,
    canvas: &px_paint::Canvas,
) -> Result<(), ServeError> {
    let strips = wire::strips_of(canvas);
    match fault {
        // Leave without a word. Returning `Ok` is the orderly end of `serve`,
        // so the binary exits with status 0 and the broker sees only a closed
        // channel with no reply.
        GpuFault::Exit => Ok(()),
        GpuFault::Malformed => {
            // A plausible length, the response tag, then bytes that decode to
            // nothing.
            let raw = channel.raw_writer();
            let _ = raw.write_all(&[5, 0, 0, 0, 0xA4, 0xFF, 0xFF, 0xFF, 0xFF]);
            let _ = raw.flush();
            Ok(())
        }
        GpuFault::Oversized => {
            let claimed = u32::try_from(MAX_MESSAGE_BYTES + 1).unwrap_or(u32::MAX);
            let raw = channel.raw_writer();
            let _ = raw.write_all(&claimed.to_le_bytes());
            let _ = raw.write_all(&[0xA4; 16]);
            let _ = raw.flush();
            Ok(())
        }
        GpuFault::Hang => hang(),
        GpuFault::StallMidFrame => {
            channel.send(&GpuResponse::FrameHeader {
                serial,
                width: canvas.width,
                height: canvas.height,
            })?;
            if let Some((first_row, rows, pixels)) = strips.first() {
                let pixels = StripPixels::new(pixels.to_vec())
                    .map_err(|_| ServeError::Protocol(GpuFailure::Backend))?;
                channel.send(&GpuResponse::Strip {
                    first_row: *first_row,
                    rows: *rows,
                    pixels,
                })?;
            }
            hang()
        }
        GpuFault::Flood => {
            send_frame(channel, serial, canvas)?;
            // Valid-looking strips with nobody asking for them, until the
            // broker closes the channel.
            let pixels = StripPixels::new(vec![0; MAX_STRIP_BYTES])
                .map_err(|_| ServeError::Protocol(GpuFailure::Backend))?;
            loop {
                channel.send(&GpuResponse::Strip {
                    first_row: 0,
                    rows: 1,
                    pixels: pixels.clone(),
                })?;
            }
        }
        GpuFault::WrongRows => {
            channel.send(&GpuResponse::FrameHeader {
                serial,
                width: canvas.width,
                height: canvas.height,
            })?;
            if let Some((first_row, rows, pixels)) = strips.first() {
                // One row short, with the bytes to match: self-consistent, and
                // not what the receiver's arithmetic allows.
                let short = rows.saturating_sub(1);
                let len = usize::try_from(short)
                    .unwrap_or(0)
                    .saturating_mul(usize::try_from(canvas.width).unwrap_or(0))
                    .saturating_mul(4);
                let pixels = StripPixels::new(pixels.get(..len).unwrap_or(&[]).to_vec())
                    .map_err(|_| ServeError::Protocol(GpuFailure::Backend))?;
                channel.send(&GpuResponse::Strip {
                    first_row: *first_row,
                    rows: short,
                    pixels,
                })?;
            }
            Ok(())
        }
        GpuFault::WrongSerial => {
            channel.send(&GpuResponse::FrameHeader {
                serial: serial.wrapping_add(1),
                width: canvas.width,
                height: canvas.height,
            })?;
            Ok(())
        }
    }
}
