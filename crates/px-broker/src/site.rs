//! The process key and the process pool's bookkeeping (Phase 14, ADR 045
//! section 1 and 7).
//!
//! # Why this is not `px_net::PartitionKey`
//!
//! The key the storage layer uses is `(eTLD+1 of the top-level document, origin
//! of the frame)`. The broker needs the same value, but `px-broker` must not
//! link `px-net` (and so `rustls`, `ring`) in a product build. The resolution
//! ADR 045 allows: build the key from `px_websec::Origin`, which is std-only,
//! and a [`SiteResolver`] the caller supplies (`px-net`'s public suffix list in
//! production). The two types are equal by construction on the tuple-origin
//! cases they share; `px-net` additionally refuses non-http(s) schemes, and
//! this key additionally carries opaque origins, which the storage layer never
//! sees. A test in `tests/oopif_frames.rs` asserts the dependency edge stays
//! out.
//!
//! The broker computes keys. No message carries one.

use std::collections::HashMap;

use px_websec::origin::{Host, Origin, SiteResolver};

use crate::ChannelId;

/// Most content processes the broker will run. Proposed in ADR 045 and
/// **unmeasured**: unit 2's spawn-latency and idle-RSS figures should replace
/// it. Over the cap the broker refuses; it never shares a process across keys
/// to stay under a cap, because that would make the cap the isolation
/// boundary.
pub const MAX_CONTENT_PROCESSES: usize = 64;

/// A browsing context group: the set of top-level frames that may script each
/// other (an opener and its popups). Part of the process key, so two tabs on
/// one site get two processes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GroupId(pub(crate) u64);

impl GroupId {
    /// For logs and test assertions only.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

/// `(eTLD+1 of the top-level document, origin of the frame)`.
///
/// Constructed only through [`ProcessKey::new`]. A top-level origin with no
/// registrable domain (an IP literal, `localhost`, a bare public suffix, an
/// opaque origin) yields no key and the load is refused: ADR 045 section 1
/// records this as a compat cost to surface, not to work around.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    top_level_site: String,
    origin: Origin,
}

impl ProcessKey {
    /// Key for a document of `origin` inside a page whose top-level document
    /// has `top_level`. For the top-level frame the two are equal.
    ///
    /// An opaque `origin` keys by the opaque id, so each opaque document gets
    /// its own process, counted against the cap.
    pub fn new(
        top_level: &Origin,
        origin: &Origin,
        resolver: &dyn SiteResolver,
    ) -> Option<ProcessKey> {
        let host = top_level.host()?;
        let Host::Domain(domain) = host else {
            return None;
        };
        let top_level_site = resolver.registrable_domain(domain)?;
        if top_level_site.is_empty() {
            return None;
        }
        Some(ProcessKey {
            top_level_site,
            origin: origin.clone(),
        })
    }

    /// The top-level site half.
    pub fn top_level_site(&self) -> &str {
        &self.top_level_site
    }

    /// The frame-origin half.
    pub fn origin(&self) -> &Origin {
        &self.origin
    }
}

/// Why the pool would not register a process.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PoolError {
    /// [`MAX_CONTENT_PROCESSES`] are registered.
    AtCap,
    /// The `(group, key)` already has a process.
    KeyTaken,
    /// The channel already serves a key.
    ChannelBusy,
}

/// What the pool says about a `(group, key)`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PoolLookup {
    /// A process already serves this key in this group.
    Share(ChannelId),
    /// None does and there is room: the caller should spawn one.
    Spawn,
    /// None does and the cap is reached. Draw a placeholder; share nothing.
    AtCap,
}

/// `(group, key) -> channel`, one process per entry, bounded.
///
/// Pure bookkeeping: spawning is the caller's (it needs a sandbox and an
/// executable). A channel serves exactly one `(group, key)`.
#[derive(Debug, Default)]
pub struct ProcessPool {
    by_key: HashMap<(GroupId, ProcessKey), ChannelId>,
    by_channel: HashMap<ChannelId, (GroupId, ProcessKey)>,
}

impl ProcessPool {
    /// Whether to share, spawn, or refuse.
    pub fn lookup(&self, group: GroupId, key: &ProcessKey) -> PoolLookup {
        if let Some(channel) = self.by_key.get(&(group, key.clone())) {
            PoolLookup::Share(*channel)
        } else if self.by_channel.len() >= MAX_CONTENT_PROCESSES {
            PoolLookup::AtCap
        } else {
            PoolLookup::Spawn
        }
    }

    /// Record that `channel` serves `(group, key)`.
    pub fn register(
        &mut self,
        group: GroupId,
        key: ProcessKey,
        channel: ChannelId,
    ) -> Result<(), PoolError> {
        if self.by_channel.contains_key(&channel) {
            return Err(PoolError::ChannelBusy);
        }
        if self.by_key.contains_key(&(group, key.clone())) {
            return Err(PoolError::KeyTaken);
        }
        if self.by_channel.len() >= MAX_CONTENT_PROCESSES {
            return Err(PoolError::AtCap);
        }
        self.by_key.insert((group, key.clone()), channel);
        self.by_channel.insert(channel, (group, key));
        Ok(())
    }

    /// The key a channel serves, if registered.
    pub fn key_of(&self, channel: ChannelId) -> Option<&(GroupId, ProcessKey)> {
        self.by_channel.get(&channel)
    }

    /// Forget a channel (it died or was torn down).
    pub fn forget(&mut self, channel: ChannelId) {
        if let Some(entry) = self.by_channel.remove(&channel) {
            self.by_key.remove(&entry);
        }
    }

    /// Registered processes.
    pub fn len(&self) -> usize {
        self.by_channel.len()
    }

    /// Whether no process is registered.
    pub fn is_empty(&self) -> bool {
        self.by_channel.is_empty()
    }
}
