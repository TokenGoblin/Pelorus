//! The script vocabulary (Phase 10, ADR 040).
//!
//! [`ScriptRequest`] travels broker to content process and [`ScriptResponse`]
//! travels back, each with its own direction tag, so the codec's reflection
//! defence applies to this pair as to the others.
//!
//! # Whose bytes are hostile
//!
//! Both directions are decoded as hostile. The content process decodes
//! [`ScriptRequest`] from a broker that relays page text, and the broker decodes
//! [`ScriptResponse`] from a process that runs arbitrary script and may be
//! compromised by it: the result string is produced by that script (a
//! `toString` it defined), so it is the page's own bytes arriving at the
//! broker. Every string is therefore a [`BoundedStr`] whose decoder checks the
//! declared length against its cap **before** it copies a byte, and whose cap
//! is far below the 1 MiB message ceiling.
//!
//! # Nothing here names its sender
//!
//! There is no field naming a process, tab, origin or channel. The only
//! non-script datum, the [`ScriptRequest::Init`] seed, is entropy the broker
//! gives the process; it is not a claim about anyone. A seed is not identity
//! and authenticates nothing.
//!
//! # Canonical encoding
//!
//! [`BoundedStr`] serializes exactly as the `String` it wraps, so the codec's
//! rule (decode, re-encode, require byte equality) holds without change.
//!
//! # The testing vocabulary
//!
//! The variants under `cfg(feature = "testing")` come last in their enums so
//! that adding or removing them never renumbers a shipping variant. A release
//! build has no such variant, so a hostile peer cannot name one: it fails to
//! decode.

use std::fmt;

use serde::de::Visitor;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Message;

/// Largest script source one [`ScriptRequest::RunScript`] may carry, in bytes.
///
/// A quarter of [`crate::MAX_MESSAGE_BYTES`]. Larger scripts arrive as several
/// `RunScript` messages in a later phase; Phase 10 runs test text, not pages.
pub const MAX_SCRIPT_SOURCE_BYTES: usize = 256 * 1024;

/// Largest result string one [`ScriptResult::Value`] may carry, in bytes.
pub const MAX_SCRIPT_RESULT_BYTES: usize = 16 * 1024;

/// Largest error message one [`ScriptResult::Error`] may carry, in bytes.
pub const MAX_SCRIPT_ERROR_BYTES: usize = 4 * 1024;

/// A string that refuses to be longer than `MAX` bytes on the wire.
///
/// postcard encodes a string as a varint byte count and the bytes. The check is
/// made from that declared count, in both directions, before the bytes are
/// copied into an owned string.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoundedStr<const MAX: usize>(String);

impl<const MAX: usize> BoundedStr<MAX> {
    /// Wrap a string, or hand it back if it is longer than `MAX` bytes.
    pub fn new(text: String) -> Result<Self, String> {
        if text.len() > MAX {
            Err(text)
        } else {
            Ok(Self(text))
        }
    }

    /// Wrap a string, cutting it at the last character boundary at or under
    /// `MAX` bytes. For the producer of a result, which must say something
    /// rather than refuse.
    pub fn truncated(mut text: String) -> Self {
        if text.len() > MAX {
            let mut cut = MAX;
            while cut > 0 && !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
        }
        Self(text)
    }

    /// The text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Take the text.
    pub fn into_string(self) -> String {
        self.0
    }
}

impl<const MAX: usize> Serialize for BoundedStr<MAX> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        if self.0.len() > MAX {
            return Err(serde::ser::Error::custom("string exceeds its bound"));
        }
        serializer.serialize_str(&self.0)
    }
}

struct BoundedStrVisitor<const MAX: usize>;

impl<const MAX: usize> Visitor<'_> for BoundedStrVisitor<MAX> {
    type Value = BoundedStr<MAX>;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "a string of at most {MAX} bytes")
    }

    fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<Self::Value, E> {
        if text.len() > MAX {
            return Err(E::custom("string exceeds its bound"));
        }
        Ok(BoundedStr(text.to_owned()))
    }
}

impl<'de, const MAX: usize> Deserialize<'de> for BoundedStr<MAX> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(BoundedStrVisitor::<MAX>)
    }
}

/// Script source text.
pub type ScriptSource = BoundedStr<MAX_SCRIPT_SOURCE_BYTES>;

/// The printed form of a script's completion value.
pub type ResultText = BoundedStr<MAX_SCRIPT_RESULT_BYTES>;

/// The message of an uncaught script error.
pub type ErrorText = BoundedStr<MAX_SCRIPT_ERROR_BYTES>;

/// Why the host stopped running script in a content process. Never resumes.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
pub enum TerminateReason {
    /// The macrotask queue was full.
    TaskQueueOverflow,
    /// The microtask queue was full.
    MicrotaskQueueOverflow,
    /// More live timers than the host allows.
    TimerOverflow,
    /// One microtask checkpoint ran more jobs than its budget.
    MicrotaskBudget,
    /// The engine raised a runtime-limit error (loop iterations, recursion
    /// depth or stack size). Script is stopped, however it handled the error.
    RuntimeLimit,
    /// The engine reported an internal failure it cannot continue from.
    EnginePanic,
}

/// What a script run came to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ScriptResult {
    /// The completion value, converted to a string.
    Value {
        /// Capped by [`MAX_SCRIPT_RESULT_BYTES`].
        text: ResultText,
    },
    /// An uncaught error, as data.
    Error {
        /// Capped by [`MAX_SCRIPT_ERROR_BYTES`].
        message: ErrorText,
    },
    /// The host stopped script in this process, for good. May also arrive
    /// unsolicited, when a timer task (not a request) tripped a bound.
    Terminated {
        /// Which bound.
        reason: TerminateReason,
    },
}

/// Broker to content process.
///
/// `non_exhaustive`: the testing-only variants exist whenever ANY crate in the build
/// turns on `px-ipc/testing` (Cargo unifies features across a workspace build), not
/// only when the crate matching on this enum has its own `testing` feature. Without
/// this, `cargo test --workspace` failed to compile `px-content` because `px-gpu`'s
/// dev-dependency enabled the variants behind its back. Downstream matches must now
/// say what happens to a variant they do not know, and the content process says:
/// refuse it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScriptRequest {
    /// The first message the broker sends, and only the first: entropy for the
    /// coarse clock's jitter and `Math.random` (ADR 040). The process needs no
    /// entropy source of its own. A second `Init` is a protocol error.
    Init {
        /// Fresh from the broker's CSPRNG for each content process.
        seed: u64,
    },
    /// Run this text as a classic script in the process's one realm.
    RunScript {
        /// Capped by [`MAX_SCRIPT_SOURCE_BYTES`].
        source: ScriptSource,
    },
    /// Testing only: report the loop's high-water marks.
    #[cfg(feature = "testing")]
    Counters,
    /// Testing only: report the raw values the clock returned to script.
    #[cfg(feature = "testing")]
    ClockLog,
    /// Testing only: how long the content process lets script run without a
    /// task or microtask boundary before it aborts itself (the watchdog; ADR
    /// 040). Zero is refused. The shipping value is a constant, not a message.
    #[cfg(feature = "testing")]
    WatchdogMillis {
        /// The new limit, in milliseconds.
        millis: u32,
    },
}

/// Content process to broker. `non_exhaustive` for the reason on [`ScriptRequest`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScriptResponse {
    /// The outcome of the most recent [`ScriptRequest::RunScript`], or an
    /// unsolicited [`ScriptResult::Terminated`].
    Result(ScriptResult),
    /// Testing only: the high-water marks and the caps they are held to.
    #[cfg(feature = "testing")]
    Counters(CounterReport),
    /// Testing only: nanosecond values the coarse clock returned, in order.
    #[cfg(feature = "testing")]
    ClockLog {
        /// At most [`MAX_CLOCK_LOG`] values.
        nanos: crate::gpu::Bounded<u64, MAX_CLOCK_LOG>,
    },
}

/// Most clock values one [`ScriptResponse::ClockLog`] carries.
#[cfg(feature = "testing")]
pub const MAX_CLOCK_LOG: usize = 4096;

/// The loop's high-water marks (testing only). Each is the largest value the
/// quantity ever took, against the cap the host enforces on it.
#[cfg(feature = "testing")]
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct CounterReport {
    /// Starts with the marker that `cargo xtask sandbox` scans release builds
    /// for, so a build that carries this capability carries the proof.
    pub marker: BoundedStr<128>,
    /// Task queue depth, and its cap.
    pub tasks: (u32, u32),
    /// Microtask queue depth, and its cap.
    pub microtasks: (u32, u32),
    /// Live timers, and their cap.
    pub timers: (u32, u32),
    /// Most microtasks run in one checkpoint, and the budget.
    pub checkpoint: (u32, u32),
}

impl Message for ScriptRequest {
    const TAG: u8 = 0xB1;
}

impl Message for ScriptResponse {
    const TAG: u8 = 0xB2;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode_frame, encode_frame};

    #[test]
    fn script_tags_differ_from_every_other_direction() {
        let tags = [
            ScriptRequest::TAG,
            ScriptResponse::TAG,
            crate::Request::TAG,
            crate::Response::TAG,
            crate::gpu::GpuRequest::TAG,
            crate::gpu::GpuResponse::TAG,
        ];
        for (i, a) in tags.iter().enumerate() {
            for b in tags.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn a_bounded_string_round_trips_and_truncates_on_a_character_boundary() {
        let request = ScriptRequest::RunScript {
            source: ScriptSource::new("1+1".into()).expect("small"),
        };
        let frame = encode_frame(&request).expect("encode");
        assert_eq!(
            decode_frame::<ScriptRequest>(&frame).expect("decode"),
            request
        );

        // Three bytes per character; a cap of 4 must cut at 3, not 4.
        let cut = BoundedStr::<4>::truncated("\u{20ac}\u{20ac}".into());
        assert_eq!(cut.as_str(), "\u{20ac}");
        assert_eq!(
            BoundedStr::<4>::truncated("abcdefg".into()).as_str(),
            "abcd"
        );
        assert_eq!(BoundedStr::<0>::truncated("\u{20ac}".into()).as_str(), "");
    }

    #[test]
    fn new_refuses_over_the_cap_and_returns_the_text() {
        assert_eq!(BoundedStr::<2>::new("abc".into()), Err("abc".to_string()));
        assert!(BoundedStr::<3>::new("abc".into()).is_ok());
    }
}
