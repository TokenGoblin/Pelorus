//! The cross-process frame vocabulary (Phase 14, ADR 045 section 2).
//!
//! Three enums, each with its own direction tag so the codec's reflection
//! defence applies to each:
//!
//! - [`FrameRequest`]: content process to broker.
//! - [`FrameResponse`]: the broker's reply to one request.
//! - [`FrameEvent`]: broker to content process, unsolicited.
//!
//! # Nothing here names its sender
//!
//! Every frame is named by a [`FrameId`], a resource reference the broker
//! checks against the channel the message arrived on. There is no origin, no
//! partition key, no process and no tab anywhere in these types. The broker
//! computes a document's origin and process key from the navigation it
//! performed, never from a field (invariant 9). `cargo xtask ipc` scans this
//! file for a sender-shaped field.
//!
//! # Nothing here explains a refusal
//!
//! [`FrameResponse::Denied`] is a bare discriminant, for the reason on
//! [`crate::message::Response::Denied`]. It is variant 0 so that its encoding
//! is the single byte `0x00` and cannot drift when variants are added.
//!
//! # Unknown variants are refused
//!
//! All three enums are `non_exhaustive`: later phases add `PostMessage`,
//! `OpenWindow`, `ReportRemoteFrame` and input variants. A match on them must
//! have a refusing `_` arm, and a peer that sends a discriminant this build
//! does not know fails to decode (`Malformed`), which poisons its channel.
//!
//! # Bounds
//!
//! The one string, a navigation URL, is a [`BoundedStr`] checked against its
//! cap before a byte is copied. Viewport dimensions are `u16`, so a hostile
//! value is bounded by the type; the receiver still applies the pixel cap.
//! Counts such as the element ordinal are range-checked by the broker.

use serde::{Deserialize, Serialize};

use crate::Message;
use crate::message::FrameId;
use crate::script::BoundedStr;

/// Longest URL a [`FrameRequest::Navigate`] may carry, in bytes.
pub const MAX_NAVIGATE_URL_BYTES: usize = 2048;

/// A navigation target.
pub type NavigateUrl = BoundedStr<MAX_NAVIGATE_URL_BYTES>;

/// How a navigation was initiated. A claim about intent, not identity; the
/// broker's policy treats every kind as untrusted input.
#[derive(Serialize, Deserialize, Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum NavigationKind {
    /// A link, form or `location` assignment; adds a history entry.
    Push,
    /// `location.replace`; replaces the current entry.
    Replace,
    /// Reload of the current document.
    Reload,
}

/// Content process to broker.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameRequest {
    /// An `<iframe>` element was inserted in `parent`'s document.
    ///
    /// Authority: the channel hosts `parent`; depth, per-parent and process
    /// caps hold.
    CreateChildFrame {
        /// The embedding frame.
        parent: FrameId,
        /// Position of the element among the parent's frame elements. The
        /// broker accepts one live child per ordinal.
        element_ordinal: u32,
    },
    /// Navigate `frame` to `url`.
    ///
    /// Authority: the channel hosts `frame` or one of its ancestors.
    Navigate {
        /// The frame to navigate.
        frame: FrameId,
        /// Target, unparsed; the broker parses and fetches it.
        url: NavigateUrl,
        /// Why.
        kind: NavigationKind,
    },
}

/// The broker's reply to a [`FrameRequest`].
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameResponse {
    /// Refused. Bare, and variant 0; see the module documentation.
    Denied,
    /// The child frame exists. The creator hosts it until a navigation commits
    /// a document under a different process key.
    Created {
        /// The new frame.
        frame: FrameId,
    },
    /// The navigation was authorised. It says nothing about the load, which
    /// the broker performs and reports through [`FrameEvent`].
    NavigationAuthorised,
}

/// Broker to content process, unsolicited. Sent only to a channel that holds
/// or may see the frame.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameEvent {
    /// The frame's document now lives in another process. The recipient must
    /// replace its local document with a remote-frame proxy.
    FrameReplaced {
        /// The frame whose host changed. Where it went is not disclosed.
        frame: FrameId,
    },
    /// The process hosting the frame died. The frame id stays valid; draw a
    /// placeholder.
    FrameCrashed {
        /// The crashed frame.
        frame: FrameId,
    },
    /// The size the frame is laid out at. The child learns its own size and
    /// never the parent's geometry.
    SetViewport {
        /// The frame being sized.
        frame: FrameId,
        /// CSS pixels.
        width: u16,
        /// CSS pixels.
        height: u16,
    },
}

impl Message for FrameRequest {
    const TAG: u8 = 0xC1;
}

impl Message for FrameResponse {
    const TAG: u8 = 0xC2;
}

impl Message for FrameEvent {
    const TAG: u8 = 0xC3;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denied_is_one_byte() {
        let encoded = postcard::to_allocvec(&FrameResponse::Denied).expect("encode");
        assert_eq!(encoded, [0]);
    }
}
