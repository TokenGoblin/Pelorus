#![no_main]

//! Fuzz invariant 9 as a property, across a sequence of channels and requests.
//!
//! The other targets attack the codec. This attacks the thing the codec exists
//! to protect: **no channel may ever be told `FrameHost::Local` for a frame it
//! was not issued.** An adversarial review noted that the broker, the frame
//! tree and the capability table had no fuzz coverage at all — only the
//! deserializer did, which is the narrower half of §4.5's requirement.
//!
//! The input drives a small state machine rather than being decoded with
//! `arbitrary`, deliberately: fuzz/ is already a second toolchain (ADR 006) and
//! adding a dependency to reach a byte is not worth it.

use libfuzzer_sys::fuzz_target;
use px_broker::{Broker, ChannelId};
use px_ipc::{FrameHost, FrameId, Request, Response};

fuzz_target!(|data: &[u8]| {
    let mut broker = Broker::new();
    let mut channels: Vec<ChannelId> = Vec::new();
    // What we believe each channel legitimately holds.
    let mut issued: Vec<(ChannelId, FrameId)> = Vec::new();

    let mut bytes = data.iter().copied();
    while let Some(op) = bytes.next() {
        let pick = |v: &Vec<ChannelId>, n: u8| -> Option<ChannelId> {
            if v.is_empty() {
                None
            } else {
                v.get(usize::from(n) % v.len()).copied()
            }
        };

        match op % 5 {
            0 => {
                if let Some(id) = broker.open_channel() {
                    channels.push(id);
                }
            }
            1 => {
                if let Some(id) = pick(&channels, bytes.next().unwrap_or(0)) {
                    if let Some(frame) = broker.create_frame(id) {
                        issued.push((id, frame));
                    }
                }
            }
            2 => {
                if let Some(id) = pick(&channels, bytes.next().unwrap_or(0)) {
                    broker.close_channel(id);
                    issued.retain(|(owner, _)| *owner != id);
                }
            }
            3 => {
                // A forged handle, from attacker-chosen bytes.
                let index = u32::from(bytes.next().unwrap_or(0));
                let generation = u32::from(bytes.next().unwrap_or(0));
                if let Some(id) = pick(&channels, bytes.next().unwrap_or(0)) {
                    let frame = FrameId::new(index, generation);
                    let decision = broker.dispatch(id, Request::FrameHost { frame });
                    if decision.response
                        == (Response::FrameHost {
                            host: FrameHost::Local,
                        })
                    {
                        assert!(
                            issued.contains(&(id, frame)),
                            "channel {id:?} was told it owns {frame:?}, which it \
                             was never issued"
                        );
                    }
                }
            }
            _ => {
                if let Some(id) = pick(&channels, bytes.next().unwrap_or(0)) {
                    let _ = broker.dispatch(id, Request::Ping);
                }
            }
        }
    }

    // Whatever the sequence was, every issued frame is still owned by exactly
    // the channel it was issued to, or by nobody.
    for (owner, frame) in &issued {
        assert!(
            broker.holds_frame(*owner, *frame),
            "an issued frame stopped belonging to its owner"
        );
    }
});
