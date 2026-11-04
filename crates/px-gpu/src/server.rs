//! The GPU process's side of the channel: read display-list requests, rasterize,
//! write strips.
//!
//! This is a library function over any [`Read`] and [`Write`] so the whole
//! exchange is tested in memory and the `px-gpu` binary is a few lines around
//! it. The binary's first act, before this loop runs, is to confine itself.
//!
//! # A violation ends the process
//!
//! The broker is this process's only peer and it speaks a fixed protocol. A
//! message out of order, a count that does not add up, a coordinate layout
//! cannot produce: none of these is a case to recover from, because the broker
//! is a first-party program that would not send them. They mean the stream is
//! not what this side thinks it is. The process answers [`GpuResponse::Failed`]
//! if it can, and returns an error, which the binary turns into an exit. The
//! broker's session notices, renders in software, and starts a new process.
//! There is no resynchronisation logic to get wrong.

use std::io::{Read, Write};

use px_ipc::gpu::{GpuFailure, GpuRequest, GpuResponse, MAX_DISPLAY_ITEMS, StripPixels};
use px_ipc::{Channel, IpcError};
use px_paint::raster::MAX_CLIP_DEPTH;
use px_paint::{DisplayList, Item};

use crate::rasterizer::{Rasterizer, RenderError};
use crate::wire;

/// Why [`serve`] stopped.
#[derive(Debug)]
pub enum ServeError {
    /// The transport or the codec failed. A peer that closes the channel is
    /// not an error: that is how a session ends, and [`serve`] returns `Ok`.
    Ipc(IpcError),
    /// The peer broke the protocol; `Failed` was sent if the channel allowed.
    Protocol(GpuFailure),
}

impl std::fmt::Display for ServeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ipc(error) => write!(f, "{error}"),
            Self::Protocol(why) => write!(f, "protocol violation: {why:?}"),
        }
    }
}

impl std::error::Error for ServeError {}

impl From<IpcError> for ServeError {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

/// A frame being received.
struct Pending {
    serial: u32,
    width: u32,
    height: u32,
    declared: u32,
    items: Vec<Item>,
    clip_depth: usize,
}

/// Serve requests until the peer closes the channel.
pub fn serve<R: Read, W: Write>(
    channel: &mut Channel<R, W>,
    backend: &mut dyn Rasterizer,
) -> Result<(), ServeError> {
    let mut pending: Option<Pending> = None;
    #[cfg(feature = "testing")]
    let mut armed: Option<px_ipc::gpu::GpuFault> = None;

    loop {
        let request = match channel.recv::<GpuRequest>() {
            Ok(request) => request,
            Err(IpcError::PeerClosed) => return Ok(()),
            Err(error) => return Err(ServeError::Ipc(error)),
        };

        match request {
            GpuRequest::Begin {
                serial,
                width,
                height,
                item_count,
            } => {
                if pending.is_some() {
                    return fail(channel, GpuFailure::OutOfOrder);
                }
                if !wire::viewport_ok(width, height) {
                    return fail(channel, GpuFailure::BadViewport);
                }
                if item_count > MAX_DISPLAY_ITEMS {
                    return fail(channel, GpuFailure::BadItemCount);
                }
                pending = Some(Pending {
                    serial,
                    width,
                    height,
                    declared: item_count,
                    // Not sized from `item_count`: the count is a claim, and
                    // memory follows the items that actually arrive.
                    items: Vec::new(),
                    clip_depth: 0,
                });
            }
            GpuRequest::Items { items } => {
                let Some(frame) = pending.as_mut() else {
                    return fail(channel, GpuFailure::OutOfOrder);
                };
                let have = u32::try_from(frame.items.len()).unwrap_or(u32::MAX);
                let more = u32::try_from(items.len()).unwrap_or(u32::MAX);
                if have.saturating_add(more) > frame.declared {
                    return fail(channel, GpuFailure::BadItemCount);
                }
                for wire_item in items.as_slice() {
                    let Ok(item) = wire::item_from_wire(wire_item) else {
                        return fail(channel, GpuFailure::BadCoordinate);
                    };
                    match item {
                        Item::PushClip { .. } => {
                            frame.clip_depth += 1;
                            if frame.clip_depth > MAX_CLIP_DEPTH {
                                return fail(channel, GpuFailure::BadClips);
                            }
                        }
                        Item::PopClip => match frame.clip_depth.checked_sub(1) {
                            Some(depth) => frame.clip_depth = depth,
                            None => return fail(channel, GpuFailure::BadClips),
                        },
                        _ => {}
                    }
                    frame.items.push(item);
                }
            }
            GpuRequest::Draw => {
                let Some(frame) = pending.take() else {
                    return fail(channel, GpuFailure::OutOfOrder);
                };
                if u32::try_from(frame.items.len()).ok() != Some(frame.declared) {
                    return fail(channel, GpuFailure::BadItemCount);
                }
                if frame.clip_depth != 0 {
                    return fail(channel, GpuFailure::BadClips);
                }
                let list = DisplayList { items: frame.items };
                let canvas = match backend.rasterize(&list, frame.width, frame.height) {
                    Ok(canvas) => canvas,
                    Err(RenderError::Input(_)) => return fail(channel, GpuFailure::BadViewport),
                    Err(RenderError::Backend(_)) => return fail(channel, GpuFailure::Backend),
                };

                #[cfg(feature = "testing")]
                if let Some(fault) = armed.take() {
                    return crate::faults::misbehave(channel, fault, frame.serial, &canvas);
                }

                send_frame(channel, frame.serial, &canvas)?;
            }
            #[cfg(feature = "testing")]
            GpuRequest::Arm { fault } => armed = Some(fault),
            #[cfg(feature = "testing")]
            GpuRequest::Probe => {
                let attempts = px_sandbox::probe::forbidden_operations()
                    .into_iter()
                    .map(|a| px_ipc::gpu::ProbeAttempt {
                        op: match a.op {
                            px_sandbox::probe::Op::OpenSystemFile => {
                                px_ipc::gpu::ProbeOp::OpenSystemFile
                            }
                            px_sandbox::probe::Op::OpenWorkingDirectory => {
                                px_ipc::gpu::ProbeOp::OpenWorkingDirectory
                            }
                            px_sandbox::probe::Op::Socket => px_ipc::gpu::ProbeOp::Socket,
                            px_sandbox::probe::Op::Connect => px_ipc::gpu::ProbeOp::Connect,
                            px_sandbox::probe::Op::Bind => px_ipc::gpu::ProbeOp::Bind,
                            px_sandbox::probe::Op::Exec => px_ipc::gpu::ProbeOp::Exec,
                        },
                        succeeded: a.succeeded,
                        errno: a.errno,
                    })
                    .collect();
                let attempts = px_ipc::gpu::Bounded::new(attempts)
                    .map_err(|_| ServeError::Protocol(GpuFailure::Backend))?;
                channel.send(&GpuResponse::Probe { attempts })?;
            }
        }
    }
}

/// Send the header and then the strips of one finished frame.
pub(crate) fn send_frame<R: Read, W: Write>(
    channel: &mut Channel<R, W>,
    serial: u32,
    canvas: &px_paint::Canvas,
) -> Result<(), ServeError> {
    let strips = wire::strips_of(canvas);
    if strips.is_empty() {
        return Err(ServeError::Protocol(GpuFailure::Backend));
    }
    channel.send(&GpuResponse::FrameHeader {
        serial,
        width: canvas.width,
        height: canvas.height,
    })?;
    for (first_row, rows, pixels) in strips {
        let pixels = StripPixels::new(pixels.to_vec())
            .map_err(|_| ServeError::Protocol(GpuFailure::Backend))?;
        channel.send(&GpuResponse::Strip {
            first_row,
            rows,
            pixels,
        })?;
    }
    Ok(())
}

fn fail<R: Read, W: Write>(channel: &mut Channel<R, W>, why: GpuFailure) -> Result<(), ServeError> {
    // Best effort: the peer may already be gone, and the answer to a broken
    // protocol is to stop either way.
    let _ = channel.send(&GpuResponse::Failed { why });
    Err(ServeError::Protocol(why))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rasterizer::{FailingRasterizer, SoftwareRasterizer};
    use px_ipc::gpu::{ItemChunk, WireColor, WireItem, WireRect};
    use px_ipc::{MAX_MESSAGE_BYTES, encode_frame};
    use std::io::Cursor;

    fn fill(x: i32, color: u8) -> WireItem {
        WireItem::FillRect {
            rect: WireRect {
                x: x * 60,
                y: 0,
                width: 60 * 2,
                height: 60 * 2,
            },
            color: WireColor {
                r: color,
                g: 0,
                b: 0,
                a: 255,
            },
        }
    }

    fn script(requests: &[GpuRequest]) -> Vec<u8> {
        requests
            .iter()
            .flat_map(|r| encode_frame(r).expect("encode"))
            .collect()
    }

    /// Run `serve` over a scripted request stream, returning its replies and
    /// how it ended.
    fn run(
        requests: &[GpuRequest],
        backend: &mut dyn Rasterizer,
    ) -> (Vec<GpuResponse>, Result<(), ServeError>) {
        let input = script(requests);
        let mut output = Vec::new();
        let result = {
            let mut channel = Channel::new(Cursor::new(input), &mut output);
            serve(&mut channel, backend)
        };
        let mut replies = Vec::new();
        let mut reader = Channel::new(Cursor::new(output), std::io::sink());
        while let Ok(reply) = reader.recv::<GpuResponse>() {
            replies.push(reply);
        }
        (replies, result)
    }

    fn items(list: Vec<WireItem>) -> GpuRequest {
        GpuRequest::Items {
            items: ItemChunk::new(list).expect("chunk"),
        }
    }

    fn begin(serial: u32, w: u32, h: u32, n: u32) -> GpuRequest {
        GpuRequest::Begin {
            serial,
            width: w,
            height: h,
            item_count: n,
        }
    }

    #[test]
    fn a_frame_comes_back_as_the_software_baseline() {
        let (replies, result) = run(
            &[
                begin(9, 6, 4, 2),
                items(vec![fill(0, 200), fill(2, 100)]),
                GpuRequest::Draw,
            ],
            &mut SoftwareRasterizer,
        );
        assert!(result.is_ok());
        let Some(GpuResponse::FrameHeader {
            serial,
            width,
            height,
        }) = replies.first().cloned()
        else {
            panic!("no header in {replies:?}");
        };
        assert_eq!((serial, width, height), (9, 6, 4));
        let mut assembler = wire::FrameAssembler::new(width, height).expect("assembler");
        for reply in &replies[1..] {
            let GpuResponse::Strip {
                first_row,
                rows,
                pixels,
            } = reply
            else {
                panic!("not a strip: {reply:?}");
            };
            assembler
                .push(*first_row, *rows, pixels.as_slice())
                .expect("strip");
        }
        let got = assembler.finish().expect("frame");

        let mut list = DisplayList::new();
        for item in [fill(0, 200), fill(2, 100)] {
            list.push(wire::item_from_wire(&item).expect("item"));
        }
        assert_eq!(got, px_paint::rasterize(&list, 6, 4).expect("baseline"));
    }

    #[test]
    fn a_large_frame_is_sent_in_strips_each_within_the_limit() {
        let (replies, result) = run(
            &[begin(1, 1000, 500, 0), GpuRequest::Draw],
            &mut SoftwareRasterizer,
        );
        assert!(result.is_ok());
        let strips: Vec<_> = replies
            .iter()
            .filter_map(|r| match r {
                GpuResponse::Strip { pixels, .. } => Some(pixels.len()),
                _ => None,
            })
            .collect();
        assert!(strips.len() > 1, "2 MB cannot be one strip");
        assert!(
            strips
                .iter()
                .all(|len| *len <= px_ipc::gpu::MAX_STRIP_BYTES)
        );
        const { assert!(px_ipc::gpu::MAX_STRIP_BYTES + 1024 < MAX_MESSAGE_BYTES) };
    }

    #[test]
    fn every_protocol_violation_ends_the_exchange_with_a_failure() {
        use GpuFailure::*;
        let cases: Vec<(Vec<GpuRequest>, GpuFailure)> = vec![
            (vec![GpuRequest::Draw], OutOfOrder),
            (vec![items(vec![WireItem::PopClip])], OutOfOrder),
            (vec![begin(1, 0, 4, 0)], BadViewport),
            (vec![begin(1, 4, 4, MAX_DISPLAY_ITEMS + 1)], BadItemCount),
            (vec![begin(1, 4, 4, 0), begin(2, 4, 4, 0)], OutOfOrder),
            // More items than declared.
            (
                vec![begin(1, 4, 4, 1), items(vec![fill(0, 1), fill(1, 1)])],
                BadItemCount,
            ),
            // Fewer items than declared, at Draw.
            (
                vec![begin(1, 4, 4, 2), items(vec![fill(0, 1)]), GpuRequest::Draw],
                BadItemCount,
            ),
            // Unbalanced clips either way.
            (
                vec![begin(1, 4, 4, 1), items(vec![WireItem::PopClip])],
                BadClips,
            ),
            (
                vec![
                    begin(1, 4, 4, 1),
                    items(vec![WireItem::PushClip {
                        rect: WireRect {
                            x: 0,
                            y: 0,
                            width: 60,
                            height: 60,
                        },
                    }]),
                    GpuRequest::Draw,
                ],
                BadClips,
            ),
            // A coordinate layout cannot produce.
            (
                vec![
                    begin(1, 4, 4, 1),
                    items(vec![WireItem::PushClip {
                        rect: WireRect {
                            x: i32::MAX,
                            y: 0,
                            width: 1,
                            height: 1,
                        },
                    }]),
                ],
                BadCoordinate,
            ),
        ];
        for (requests, expected) in cases {
            let (replies, result) = run(&requests, &mut SoftwareRasterizer);
            assert!(
                matches!(result, Err(ServeError::Protocol(why)) if why == expected),
                "{requests:?}: {result:?}"
            );
            assert_eq!(replies.last(), Some(&GpuResponse::Failed { why: expected }));
        }
    }

    #[test]
    fn a_backend_failure_is_reported_not_swallowed() {
        let (replies, result) = run(
            &[begin(1, 4, 4, 0), GpuRequest::Draw],
            &mut FailingRasterizer::always(),
        );
        assert!(matches!(
            result,
            Err(ServeError::Protocol(GpuFailure::Backend))
        ));
        assert_eq!(
            replies,
            vec![GpuResponse::Failed {
                why: GpuFailure::Backend
            }]
        );
    }

    #[test]
    fn a_closed_channel_is_an_orderly_end() {
        let (replies, result) = run(&[], &mut SoftwareRasterizer);
        assert!(result.is_ok());
        assert!(replies.is_empty());
    }

    #[test]
    fn two_frames_in_a_row_are_independent() {
        let (replies, result) = run(
            &[
                begin(1, 4, 4, 1),
                items(vec![fill(0, 9)]),
                GpuRequest::Draw,
                begin(2, 4, 4, 0),
                GpuRequest::Draw,
            ],
            &mut SoftwareRasterizer,
        );
        assert!(result.is_ok());
        let headers: Vec<u32> = replies
            .iter()
            .filter_map(|r| match r {
                GpuResponse::FrameHeader { serial, .. } => Some(*serial),
                _ => None,
            })
            .collect();
        assert_eq!(headers, vec![1, 2]);
    }
}
