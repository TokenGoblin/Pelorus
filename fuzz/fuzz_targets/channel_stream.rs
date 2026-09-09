#![no_main]

//! Fuzz a whole *stream*, not one frame.
//!
//! `decode_frame` reads one frame from a slice and discards the rest, so a
//! target built on it cannot express "two frames" — and framing desync, the
//! failure where one bad message misaligns every message after it, only exists
//! between frames. An adversarial review pointed out that the original targets
//! could never reach that state.
//!
//! This drives `Channel::recv` in a loop over the entire input, exactly as the
//! broker's reader thread does, until the stream ends or the channel poisons.

use libfuzzer_sys::fuzz_target;
use px_ipc::{Channel, IpcError, Request};

fuzz_target!(|data: &[u8]| {
    let mut cursor = std::io::Cursor::new(data);
    let mut channel = Channel::new(&mut cursor, std::io::sink());

    // Bounded so a pathological input cannot turn one case into an infinite
    // loop; a stream of empty frames is legal and cheap.
    for _ in 0..1024 {
        match channel.recv::<Request>() {
            Ok(_) => {}
            Err(IpcError::PeerClosed) => break,
            Err(_) => {
                // Every other error must poison. If one does not, the next
                // read resynchronises at an attacker-chosen offset.
                assert!(
                    channel.is_poisoned(),
                    "a non-EOF error left the channel usable"
                );
                break;
            }
        }
    }
});
