//! The in-memory state and the rules that govern it. Pure: no filesystem, so
//! Miri can run every test here.
//!
//! The engine is where partitioning is structural: state lives in
//! `BTreeMap<PartId, Partition>`, a read or write names exactly one
//! `PartId`, and a `PartId` can only be built from a px-net `PartitionKey` (or
//! decoded from a log record, which is the same value written down). There is
//! no iteration across partitions that returns data; the only cross-partition
//! operations are `remove_site` (deletes) and the byte counters.

use std::collections::BTreeMap;

use crate::error::{FormatError, QuotaError};
use crate::format::{self, Header, Next, Op};
use px_net::partition::PartitionKey;

/// Largest key, bytes.
pub const MAX_KEY_BYTES: usize = format::MAX_KEY_BYTES;
/// Largest value, bytes (1 MiB).
pub const MAX_VALUE_BYTES: usize = format::MAX_VALUE_BYTES;
/// Largest encoded partition id, bytes.
pub const MAX_PARTITION_ID_BYTES: usize = format::MAX_PARTITION_ID_BYTES;
/// Entries per partition.
pub const MAX_ENTRIES_PER_PARTITION: usize = 10_000;
/// Bytes one partition may account for (4 MiB).
pub const PARTITION_QUOTA_BYTES: u64 = 4 << 20;
/// Bytes the whole store may account for (64 MiB).
pub const GLOBAL_QUOTA_BYTES: u64 = 64 << 20;
/// Fixed accounting cost of one entry on top of its key and value, so that
/// millions of empty values cannot dodge the byte quotas.
pub const ENTRY_OVERHEAD_BYTES: u64 = 48;

/// What a stored value is for. Separate namespaces keep cookie persistence
/// from colliding with page storage *inside* one partition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Namespace {
    /// Web storage and similar key-value data.
    Storage,
    /// Persisted cookies (written by [`crate::cookies`]).
    Cookies,
}

impl Namespace {
    pub(crate) fn code(self) -> u8 {
        match self {
            Self::Storage => 0,
            Self::Cookies => 1,
        }
    }

    pub(crate) fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::Storage),
            1 => Some(Self::Cookies),
            _ => None,
        }
    }
}

/// A partition key reduced to what is stored: top-level site and origin
/// parts. Length-prefixed on disk so no host string can imitate another
/// partition's key by embedding a separator.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PartId {
    site: String,
    scheme: String,
    host: String,
    port: u16,
}

impl PartId {
    pub(crate) fn from_key(key: &PartitionKey) -> Result<Self, QuotaError> {
        let origin = key.origin();
        let id = Self {
            site: key.top_level_site().to_owned(),
            scheme: origin.scheme().to_owned(),
            host: origin.host().to_owned(),
            port: origin.port(),
        };
        if id.encode().ok_or(QuotaError::PartitionKeyTooLarge)?.len() > MAX_PARTITION_ID_BYTES {
            return Err(QuotaError::PartitionKeyTooLarge);
        }
        Ok(id)
    }

    pub(crate) fn site(&self) -> &str {
        &self.site
    }

    /// `site_len u16, site, scheme_len u16, scheme, host_len u16, host, port u16`.
    pub(crate) fn encode(&self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        for field in [&self.site, &self.scheme, &self.host] {
            out.extend_from_slice(&u16::try_from(field.len()).ok()?.to_le_bytes());
            out.extend_from_slice(field.as_bytes());
        }
        out.extend_from_slice(&self.port.to_le_bytes());
        Some(out)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Option<Self> {
        let mut r = format::Reader::new(bytes);
        let mut field = || -> Option<String> {
            let len = usize::from(r.u16()?);
            let raw = r.take(len)?;
            if !raw.is_ascii() {
                return None;
            }
            String::from_utf8(raw.to_vec()).ok()
        };
        let site = field()?;
        let scheme = field()?;
        let host = field()?;
        let port = r.u16()?;
        if !r.is_empty() || site.is_empty() || host.is_empty() {
            return None;
        }
        if scheme != "https" && scheme != "http" {
            return None;
        }
        Some(Self {
            site,
            scheme,
            host,
            port,
        })
    }
}

#[derive(Debug, Default, Clone)]
struct Partition {
    entries: BTreeMap<(u8, Vec<u8>), Vec<u8>>,
    bytes: u64,
}

fn cost(key: &[u8], value: &[u8]) -> u64 {
    (key.len() as u64)
        .saturating_add(value.len() as u64)
        .saturating_add(ENTRY_OVERHEAD_BYTES)
}

/// All state, in memory.
#[derive(Debug, Default, Clone)]
pub(crate) struct Engine {
    parts: BTreeMap<PartId, Partition>,
    total: u64,
}

/// The result of reading a log.
pub(crate) struct Recovered {
    pub engine: Engine,
    /// Bytes of the input that form the consistent prefix. `0` with `fresh`.
    pub valid_len: usize,
    /// The input held no header (empty or torn at creation).
    pub fresh: bool,
}

impl Engine {
    pub(crate) fn recover(bytes: &[u8]) -> Result<Recovered, FormatError> {
        let mut engine = Self::default();
        if format::check_header(bytes)? == Header::Fresh {
            return Ok(Recovered {
                engine,
                valid_len: 0,
                fresh: true,
            });
        }
        let mut pos = format::HEADER_LEN;
        loop {
            let rest = bytes.get(pos..).unwrap_or_default();
            let Next::Record(op, used) = format::decode_record(rest) else {
                break;
            };
            if engine.apply_logged(&op).is_none() {
                break;
            }
            pos = pos.saturating_add(used);
        }
        Ok(Recovered {
            engine,
            valid_len: pos,
            fresh: false,
        })
    }

    /// Apply a record read from disk, enforcing every rule a live write would.
    /// `None` means the record is not acceptable and the log ends before it.
    fn apply_logged(&mut self, op: &Op<'_>) -> Option<()> {
        match op {
            Op::Put {
                ns,
                pid,
                key,
                value,
            } => {
                let ns = Namespace::from_code(*ns)?;
                let pid = PartId::decode(pid)?;
                self.check_put(&pid, ns, key, value).ok()?;
                self.put(pid, ns, key, value);
            }
            Op::Delete { ns, pid, key } => {
                let ns = Namespace::from_code(*ns)?;
                let pid = PartId::decode(pid)?;
                self.delete(&pid, ns, key);
            }
        }
        Some(())
    }

    pub(crate) fn check_put(
        &self,
        pid: &PartId,
        ns: Namespace,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), QuotaError> {
        if key.len() > MAX_KEY_BYTES {
            return Err(QuotaError::KeyTooLarge { len: key.len() });
        }
        if value.len() > MAX_VALUE_BYTES {
            return Err(QuotaError::ValueTooLarge { len: value.len() });
        }
        let part = self.parts.get(pid);
        let existing = part
            .and_then(|p| p.entries.get(&(ns.code(), key.to_vec())))
            .map(|old| cost(key, old));
        if existing.is_none()
            && part.is_some_and(|p| p.entries.len() >= MAX_ENTRIES_PER_PARTITION)
        {
            return Err(QuotaError::TooManyEntries);
        }
        let old = existing.unwrap_or(0);
        let new_cost = cost(key, value);
        let part_bytes = part.map_or(0, |p| p.bytes);
        let would_part = part_bytes.saturating_sub(old).saturating_add(new_cost);
        if would_part > PARTITION_QUOTA_BYTES {
            return Err(QuotaError::PartitionFull {
                would_use: would_part,
            });
        }
        let would_total = self.total.saturating_sub(old).saturating_add(new_cost);
        if would_total > GLOBAL_QUOTA_BYTES {
            return Err(QuotaError::GlobalFull {
                would_use: would_total,
            });
        }
        Ok(())
    }

    /// Store a value. Callers run [`Self::check_put`] first.
    pub(crate) fn put(&mut self, pid: PartId, ns: Namespace, key: &[u8], value: &[u8]) {
        let part = self.parts.entry(pid).or_default();
        let new_cost = cost(key, value);
        let old = part
            .entries
            .insert((ns.code(), key.to_vec()), value.to_vec())
            .map_or(0, |old| cost(key, &old));
        part.bytes = part.bytes.saturating_sub(old).saturating_add(new_cost);
        self.total = self.total.saturating_sub(old).saturating_add(new_cost);
    }

    pub(crate) fn delete(&mut self, pid: &PartId, ns: Namespace, key: &[u8]) -> bool {
        let Some(part) = self.parts.get_mut(pid) else {
            return false;
        };
        let Some(old) = part.entries.remove(&(ns.code(), key.to_vec())) else {
            return false;
        };
        let freed = cost(key, &old);
        part.bytes = part.bytes.saturating_sub(freed);
        self.total = self.total.saturating_sub(freed);
        if part.entries.is_empty() {
            self.parts.remove(pid);
        }
        true
    }

    pub(crate) fn get(&self, pid: &PartId, ns: Namespace, key: &[u8]) -> Option<&[u8]> {
        self.parts
            .get(pid)?
            .entries
            .get(&(ns.code(), key.to_vec()))
            .map(Vec::as_slice)
    }

    pub(crate) fn keys(&self, pid: &PartId, ns: Namespace) -> Vec<&[u8]> {
        self.parts.get(pid).map_or_else(Vec::new, |p| {
            p.entries
                .keys()
                .filter(|(n, _)| *n == ns.code())
                .map(|(_, k)| k.as_slice())
                .collect()
        })
    }

    pub(crate) fn usage(&self, pid: &PartId) -> u64 {
        self.parts.get(pid).map_or(0, |p| p.bytes)
    }

    pub(crate) fn total_usage(&self) -> u64 {
        self.total
    }

    pub(crate) fn partition_count(&self) -> usize {
        self.parts.len()
    }

    /// Drop one partition; returns how many entries went.
    pub(crate) fn remove_partition(&mut self, pid: &PartId) -> usize {
        match self.parts.remove(pid) {
            Some(p) => {
                self.total = self.total.saturating_sub(p.bytes);
                p.entries.len()
            }
            None => 0,
        }
    }

    /// Drop every partition whose top-level site is `site`.
    pub(crate) fn remove_site(&mut self, site: &str) -> usize {
        let doomed: Vec<PartId> = self
            .parts
            .keys()
            .filter(|id| id.site() == site)
            .cloned()
            .collect();
        doomed.iter().map(|id| self.remove_partition(id)).sum()
    }

    /// A complete log (header plus one `Put` per live entry) of the state
    /// that would remain if every partition for which `drop` is true were
    /// removed. Overwritten and deleted values are not in it: this is what
    /// makes `clear_*` physically remove bytes.
    pub(crate) fn snapshot_excluding(
        &self,
        drop: &dyn Fn(&PartId) -> bool,
    ) -> Result<Vec<u8>, FormatError> {
        let mut out = format::header_bytes();
        for (pid, part) in &self.parts {
            if drop(pid) {
                continue;
            }
            let pid_bytes = pid.encode().ok_or(FormatError::RecordTooLarge)?;
            for ((ns, key), value) in &part.entries {
                out.extend_from_slice(&format::encode_record(&Op::Put {
                    ns: *ns,
                    pid: &pid_bytes,
                    key,
                    value,
                })?);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use px_net::partition::Origin;
    use px_net::psl::PublicSuffixList;

    pub(crate) fn psl() -> PublicSuffixList {
        PublicSuffixList::parse("com\nexample\n").unwrap()
    }

    pub(crate) fn pk(top: &str, origin_host: &str) -> PartitionKey {
        PartitionKey::new(&psl(), top, Origin::new("https", origin_host, 443).unwrap()).unwrap()
    }

    fn id(top: &str, host: &str) -> PartId {
        PartId::from_key(&pk(top, host)).unwrap()
    }

    #[test]
    fn partition_id_round_trips_and_distinguishes_everything() {
        let a = id("a.example", "cdn.example");
        assert_eq!(PartId::decode(&a.encode().unwrap()), Some(a.clone()));
        let b = id("b.example", "cdn.example");
        assert_ne!(a, b);
        assert_ne!(a.encode(), b.encode());
        // Same site and host, different port or scheme: different partitions.
        let other_port = PartId::from_key(
            &PartitionKey::new(&psl(), "a.example", Origin::new("https", "cdn.example", 8443).unwrap())
                .unwrap(),
        )
        .unwrap();
        assert_ne!(a, other_port);
    }

    #[test]
    fn partition_id_hand_encoding() {
        let a = id("a.example", "cdn.example");
        let mut want = Vec::new();
        for f in ["a.example", "https", "cdn.example"] {
            want.extend_from_slice(&(f.len() as u16).to_le_bytes());
            want.extend_from_slice(f.as_bytes());
        }
        want.extend_from_slice(&443u16.to_le_bytes());
        assert_eq!(a.encode(), Some(want));
    }

    #[test]
    fn decode_rejects_hostile_ids() {
        assert_eq!(PartId::decode(b""), None);
        assert_eq!(PartId::decode(&[0xff; 7]), None);
        let mut ok = id("a.example", "x.example").encode().unwrap();
        ok.push(0); // trailing byte
        assert_eq!(PartId::decode(&ok), None);
    }

    #[test]
    fn put_get_delete_and_accounting() {
        let mut e = Engine::default();
        let p = id("a.example", "cdn.example");
        e.check_put(&p, Namespace::Storage, b"k", b"vvv").unwrap();
        e.put(p.clone(), Namespace::Storage, b"k", b"vvv");
        // cost = 1 + 3 + 48
        assert_eq!(e.usage(&p), 52);
        assert_eq!(e.total_usage(), 52);
        assert_eq!(e.get(&p, Namespace::Storage, b"k"), Some(&b"vvv"[..]));
        assert_eq!(e.get(&p, Namespace::Cookies, b"k"), None);
        // Overwrite replaces the cost rather than adding.
        e.put(p.clone(), Namespace::Storage, b"k", b"v");
        assert_eq!(e.usage(&p), 50);
        assert!(e.delete(&p, Namespace::Storage, b"k"));
        assert!(!e.delete(&p, Namespace::Storage, b"k"));
        assert_eq!(e.total_usage(), 0);
        assert_eq!(e.partition_count(), 0);
    }

    #[test]
    fn quotas_return_errors() {
        let mut e = Engine::default();
        let p = id("a.example", "cdn.example");
        assert_eq!(
            e.check_put(&p, Namespace::Storage, &vec![0; MAX_KEY_BYTES + 1], b""),
            Err(QuotaError::KeyTooLarge { len: MAX_KEY_BYTES + 1 })
        );
        assert_eq!(
            e.check_put(&p, Namespace::Storage, b"k", &vec![0; MAX_VALUE_BYTES + 1]),
            Err(QuotaError::ValueTooLarge { len: MAX_VALUE_BYTES + 1 })
        );
        // Four 1 MiB values: each costs 1 MiB + 1 + 48, so the fourth fits
        // only if 4*(1048576+1+48) <= 4 MiB. It does not: the 4th is refused.
        for i in 0..3u8 {
            let k = [i];
            e.check_put(&p, Namespace::Storage, &k, &vec![0; MAX_VALUE_BYTES]).unwrap();
            e.put(p.clone(), Namespace::Storage, &k, &vec![0; MAX_VALUE_BYTES]);
        }
        let would = 4 * (MAX_VALUE_BYTES as u64 + 1 + ENTRY_OVERHEAD_BYTES);
        assert_eq!(
            e.check_put(&p, Namespace::Storage, &[9], &vec![0; MAX_VALUE_BYTES]),
            Err(QuotaError::PartitionFull { would_use: would })
        );
        // A different partition is unaffected by this one being full.
        let q = id("b.example", "cdn.example");
        assert!(e.check_put(&q, Namespace::Storage, &[9], &vec![0; MAX_VALUE_BYTES]).is_ok());
        // Overwriting inside the full partition with a same-size value is fine.
        assert!(e.check_put(&p, Namespace::Storage, &[0], &vec![1; MAX_VALUE_BYTES]).is_ok());
    }

    #[test]
    fn entry_count_cap() {
        let mut e = Engine::default();
        let p = id("a.example", "cdn.example");
        for i in 0..MAX_ENTRIES_PER_PARTITION as u32 {
            e.put(p.clone(), Namespace::Storage, &i.to_le_bytes(), b"");
        }
        assert_eq!(
            e.check_put(&p, Namespace::Storage, b"new", b""),
            Err(QuotaError::TooManyEntries)
        );
        // 10_000 * (4 + 48) = 520_000 < 4 MiB, so it was the count, not bytes.
        assert_eq!(e.usage(&p), 520_000);
        // Replacing an existing key is not a new entry.
        assert!(e.check_put(&p, Namespace::Storage, &0u32.to_le_bytes(), b"x").is_ok());
    }

    #[test]
    fn global_quota() {
        let mut e = Engine::default();
        // 16 partitions of 4 MiB minus a little fill 64 MiB; fabricate by
        // direct accounting rather than writing 64 MiB of values.
        e.total = GLOBAL_QUOTA_BYTES - 10;
        let p = id("a.example", "cdn.example");
        assert!(matches!(
            e.check_put(&p, Namespace::Storage, b"k", b"v"),
            Err(QuotaError::GlobalFull { .. })
        ));
    }

    fn log_of(ops: &[(&PartId, Namespace, &[u8], Option<&[u8]>)]) -> Vec<u8> {
        let mut out = format::header_bytes();
        for (pid, ns, key, value) in ops {
            let pid = pid.encode().unwrap();
            let op = match value {
                Some(value) => Op::Put { ns: ns.code(), pid: &pid, key, value },
                None => Op::Delete { ns: ns.code(), pid: &pid, key },
            };
            out.extend_from_slice(&format::encode_record(&op).unwrap());
        }
        out
    }

    #[test]
    fn recover_replays_puts_and_deletes() {
        let p = id("a.example", "cdn.example");
        let log = log_of(&[
            (&p, Namespace::Storage, b"a", Some(b"1")),
            (&p, Namespace::Storage, b"b", Some(b"2")),
            (&p, Namespace::Storage, b"a", None),
        ]);
        let r = Engine::recover(&log).unwrap();
        assert_eq!(r.valid_len, log.len());
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"a"), None);
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"b"), Some(&b"2"[..]));
    }

    /// The headline recovery property, at EVERY byte offset: the store opens,
    /// never panics, exposes exactly the records that were wholly written
    /// before the cut, and none of a partial one.
    #[test]
    fn truncating_the_log_at_every_offset_yields_a_consistent_prefix() {
        let p = id("a.example", "cdn.example");
        let q = id("b.example", "cdn.example");
        let ops: Vec<(&PartId, Namespace, &[u8], Option<&[u8]>)> = vec![
            (&p, Namespace::Storage, b"k1", Some(b"first")),
            (&q, Namespace::Storage, b"k1", Some(b"other")),
            (&p, Namespace::Storage, b"k1", Some(b"second")),
            (&p, Namespace::Cookies, b"c", Some(b"")),
            (&p, Namespace::Storage, b"k1", None),
        ];
        let log = log_of(&ops);
        // Record end offsets, derived independently of the decoder.
        let mut ends = vec![format::HEADER_LEN];
        for (pid, ns, key, value) in &ops {
            let n = 8 + 1 + 1 + 2 + pid.encode().unwrap().len() + 2 + key.len()
                + value.map_or(0, <[u8]>::len);
            ends.push(ends.last().unwrap() + n);
        }
        assert_eq!(*ends.last().unwrap(), log.len());
        let _ = ops.iter().map(|o| o.1).count();

        for cut in 0..=log.len() {
            let r = Engine::recover(log.get(..cut).unwrap())
                .unwrap_or_else(|e| panic!("cut {cut} refused: {e}"));
            let whole = ends.iter().filter(|&&e| e <= cut).count();
            if cut < format::HEADER_LEN {
                assert!(r.fresh && r.valid_len == 0, "cut {cut}");
                continue;
            }
            // `whole` counts the header end too, so records = whole - 1.
            assert_eq!(r.valid_len, *ends.get(whole - 1).unwrap(), "cut {cut}");
            let applied = whole - 1;
            let mut expect = Engine::default();
            for (pid, ns, key, value) in ops.iter().take(applied) {
                match value {
                    Some(v) => expect.put((*pid).clone(), *ns, key, v),
                    None => {
                        expect.delete(pid, *ns, key);
                    }
                }
            }
            for pid in [&p, &q] {
                for ns in [Namespace::Storage, Namespace::Cookies] {
                    for key in [&b"k1"[..], b"c"] {
                        assert_eq!(
                            r.engine.get(pid, ns, key),
                            expect.get(pid, ns, key),
                            "cut {cut}"
                        );
                    }
                }
            }
            assert_eq!(r.engine.total_usage(), expect.total_usage(), "cut {cut}");
        }
    }

    #[test]
    fn corruption_mid_log_keeps_only_the_prefix() {
        let p = id("a.example", "cdn.example");
        let mut log = log_of(&[
            (&p, Namespace::Storage, b"a", Some(b"1")),
            (&p, Namespace::Storage, b"b", Some(b"2")),
            (&p, Namespace::Storage, b"c", Some(b"3")),
        ]);
        let first_end = format::HEADER_LEN + 8 + 6 + p.encode().unwrap().len() + 1 + 1;
        // Flip a byte inside the second record.
        *log.get_mut(first_end + 10).unwrap() ^= 0x01;
        let r = Engine::recover(&log).unwrap();
        assert_eq!(r.valid_len, first_end);
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"a"), Some(&b"1"[..]));
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"b"), None);
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"c"), None);
    }

    #[test]
    fn a_checksummed_record_that_breaks_the_rules_ends_the_log() {
        let p = id("a.example", "cdn.example");
        // Unknown namespace 9, correctly framed and checksummed.
        let pid = p.encode().unwrap();
        let mut log = format::header_bytes();
        log.extend_from_slice(
            &format::encode_record(&Op::Put { ns: 9, pid: &pid, key: b"k", value: b"v" }).unwrap(),
        );
        let r = Engine::recover(&log).unwrap();
        assert_eq!(r.valid_len, format::HEADER_LEN);
        assert_eq!(r.engine.total_usage(), 0);
        // Undecodable partition id.
        let mut log = format::header_bytes();
        log.extend_from_slice(
            &format::encode_record(&Op::Put { ns: 0, pid: b"junk", key: b"k", value: b"v" }).unwrap(),
        );
        assert_eq!(Engine::recover(&log).unwrap().valid_len, format::HEADER_LEN);
    }

    #[test]
    fn newer_format_is_refused_not_truncated() {
        let mut log = format::header_bytes();
        *log.get_mut(4).unwrap() = 2;
        assert!(matches!(
            Engine::recover(&log),
            Err(FormatError::UnsupportedVersion { version: 2, .. })
        ));
    }

    #[test]
    fn snapshot_round_trips_and_omits_dropped_partitions() {
        let mut e = Engine::default();
        let p = id("a.example", "cdn.example");
        let q = id("b.example", "cdn.example");
        e.put(p.clone(), Namespace::Storage, b"k", b"secret-a");
        e.put(q.clone(), Namespace::Storage, b"k", b"value-b");
        let snap = e.snapshot_excluding(&|id| id == &p).unwrap();
        assert!(!snap.windows(8).any(|w| w == b"secret-a"));
        let r = Engine::recover(&snap).unwrap();
        assert_eq!(r.valid_len, snap.len());
        assert_eq!(r.engine.get(&p, Namespace::Storage, b"k"), None);
        assert_eq!(r.engine.get(&q, Namespace::Storage, b"k"), Some(&b"value-b"[..]));
    }

    #[test]
    fn remove_site_drops_every_origin_under_it_and_only_it() {
        let mut e = Engine::default();
        for (top, host) in [
            ("a.example", "a.example"),
            ("a.example", "cdn.example"),
            ("b.example", "cdn.example"),
        ] {
            e.put(id(top, host), Namespace::Storage, b"k", b"v");
        }
        assert_eq!(e.remove_site("a.example"), 2);
        assert_eq!(e.partition_count(), 1);
        assert_eq!(e.total_usage(), 50);
    }
}
