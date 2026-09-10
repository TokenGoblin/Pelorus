//! The connection pool, keyed so that no connection can cross a partition.
//!
//! # What a pool is for, and what it must not become
//!
//! Reusing a connection saves a TCP handshake and a TLS handshake, which is
//! most of the cost of a small request. Reusing the *wrong* connection is a
//! cross-site identifier: a CDN that sees one connection carry requests from
//! two different top-level sites has linked them, which is precisely the
//! linkage invariant 2 exists to break.
//!
//! So the pool is keyed by [`PartitionKey`] — the full `(top-level site,
//! origin)` pair — and there is no lookup that takes anything less.
//!
//! # Warmth is an oracle
//!
//! Even without carrying data across a boundary, a shared pool leaks: if
//! `a.example` can tell that a connection to `cdn.example` is already warm, it
//! has learned the user visited some other site that uses that CDN. The timing
//! difference is small and entirely sufficient. Partitioning the pool closes
//! it, and it is worth naming because a "share connections but not cookies"
//! design sounds reasonable and is not.
//!
//! # The protocol component
//!
//! Entries are keyed by protocol as well, and it takes exactly one value
//! today (ADR 015 defers HTTP/2). It is here anyway because a pool that
//! assumes one connection per origin is a pool that gets rewritten when
//! multiplexing arrives — and rewriting a security boundary later is how the
//! boundary moves without anyone deciding it should.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::partition::PartitionKey;

/// Which wire protocol a pooled connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Protocol {
    /// HTTP/1.1. The only value today; see ADR 015.
    Http11,
}

/// How many idle connections are kept for one key.
///
/// Per key, not per origin: an attacker who can cause many partitions must not
/// be able to make the pool grow without bound, and the total is bounded by
/// [`MAX_KEYS`] below.
const MAX_IDLE_PER_KEY: usize = 6;

/// How many distinct partition keys the pool tracks before evicting.
///
/// Partitioning multiplies pool entries by the number of top-level sites, so
/// the bound that mattered before partitioning is not the bound that matters
/// now. A page embedding resources from many origins creates many keys, and
/// nothing about that is hostile — so this is a cap with eviction rather than
/// a refusal.
const MAX_KEYS: usize = 256;

/// How long an idle connection may sit before it is dropped.
///
/// Servers close idle keep-alive connections on their own schedule, and a
/// connection that the server has already closed looks identical to a usable
/// one until a write fails. A shorter timeout than the typical server's is the
/// cheap way to lose that race less often.
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

/// A pooled connection and when it went idle.
#[derive(Debug)]
struct Idle<C> {
    connection: C,
    since: Instant,
}

/// A partitioned pool of reusable connections.
///
/// Generic over the connection type so that the partitioning rules can be
/// tested exhaustively without opening a socket — the rules are the security
/// property, and they should not need a network to verify.
#[derive(Debug)]
pub struct ConnectionPool<C> {
    idle: HashMap<(PartitionKey, Protocol), Vec<Idle<C>>>,
    /// Insertion order of keys, for eviction. Oldest first.
    order: Vec<(PartitionKey, Protocol)>,
}

impl<C> Default for ConnectionPool<C> {
    fn default() -> Self {
        Self::new()
    }
}

impl<C> ConnectionPool<C> {
    /// An empty pool.
    pub fn new() -> Self {
        Self {
            idle: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Take a reusable connection for this key, if one is warm.
    ///
    /// There is no variant of this that takes an origin without a partition
    /// key. That is the whole design: the unpartitioned path does not exist to
    /// be called by accident.
    pub fn take(&mut self, key: &PartitionKey, protocol: Protocol) -> Option<C> {
        let slot = self.idle.get_mut(&(key.clone(), protocol))?;

        // Drop anything that has sat too long, newest-first so the survivor is
        // the freshest. A connection the server has already closed is
        // indistinguishable from a usable one until a write fails, so age is
        // the only signal available here.
        let now = Instant::now();
        slot.retain(|entry| now.duration_since(entry.since) < IDLE_TIMEOUT);

        slot.pop().map(|entry| entry.connection)
    }

    /// Return a connection to the pool for reuse.
    ///
    /// Dropped rather than stored when the key is at its limit: a pool that
    /// grows to hold whatever it is given is a memory leak whose size a remote
    /// party chooses.
    pub fn put(&mut self, key: PartitionKey, protocol: Protocol, connection: C) {
        let entry_key = (key, protocol);

        if !self.idle.contains_key(&entry_key) {
            if self.order.len() >= MAX_KEYS {
                self.evict_oldest();
            }
            self.order.push(entry_key.clone());
        }

        let slot = self.idle.entry(entry_key).or_default();
        if slot.len() >= MAX_IDLE_PER_KEY {
            return;
        }
        slot.push(Idle {
            connection,
            since: Instant::now(),
        });
    }

    /// Drop every connection for one partition key, across protocols.
    ///
    /// What a caller uses when a site's state is cleared: leaving a warm
    /// connection behind after clearing cookies would leave the linkage the
    /// clearing was meant to remove.
    pub fn forget(&mut self, key: &PartitionKey) {
        self.idle.retain(|(stored, _), _| stored != key);
        self.order.retain(|(stored, _)| stored != key);
    }

    /// Drop connections that have been idle past the timeout.
    pub fn expire_idle(&mut self) {
        let now = Instant::now();
        for slot in self.idle.values_mut() {
            slot.retain(|entry| now.duration_since(entry.since) < IDLE_TIMEOUT);
        }
        self.idle.retain(|_, slot| !slot.is_empty());
        let live: Vec<(PartitionKey, Protocol)> = self.idle.keys().cloned().collect();
        self.order.retain(|key| live.contains(key));
    }

    /// How many connections are pooled in total.
    pub fn len(&self) -> usize {
        self.idle.values().map(Vec::len).sum()
    }

    /// Whether the pool holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many distinct keys are tracked.
    pub fn key_count(&self) -> usize {
        self.idle.len()
    }

    fn evict_oldest(&mut self) {
        if self.order.is_empty() {
            return;
        }
        let oldest = self.order.remove(0);
        self.idle.remove(&oldest);
    }
}
