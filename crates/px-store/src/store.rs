//! The file-backed store: one append-only log, rewritten atomically.
//!
//! # Durability
//!
//! * Creation and compaction write a temporary file in the same directory,
//!   `sync_all` it, `rename` it over the log, then sync the directory. A crash
//!   leaves the old log or the new log, never a mixture.
//! * A mutation appends one checksummed record and `sync_data`s before it is
//!   applied in memory or acknowledged. A crash mid-append leaves a torn tail,
//!   which the next open recognises (see [`crate::format`]) and truncates.
//! * If an append fails, the store tries to truncate back to the last good
//!   length; if that also fails it is *poisoned* and refuses everything until
//!   reopened. It never carries on past a log it cannot vouch for.
//!
//! # "Forget means forget"
//!
//! Overwrites and single-key deletes leave the old value in the log until the
//! next compaction. [`Store::clear_partition`] and [`Store::clear_site`] always
//! rewrite the whole log from the surviving state (and [`Store::compact`] does
//! it on demand; writes trigger it when the log is more than twice the live
//! data), so after they return no byte of the cleared partitions, nor of any
//! earlier overwritten or deleted value, remains in the store's files.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use px_net::partition::PartitionKey;

use crate::engine::{Engine, Namespace, PartId};
use crate::error::{QuotaError, StoreError};
use crate::format::{self, Op};

/// File name of the log inside the store directory.
pub const LOG_NAME: &str = "store.log";
/// File name of the temporary used by atomic replacement.
pub const TMP_NAME: &str = "store.log.tmp";
/// Largest log file this version will read at open.
pub const MAX_LOG_BYTES: u64 = 256 << 20;
/// Slack allowed before a write triggers compaction.
const COMPACT_SLACK: u64 = 1 << 20;

/// Partitioned key-value storage on disk. See the crate docs.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    file: File,
    engine: Engine,
    log_len: u64,
    poisoned: bool,
}

fn sync_dir(dir: &Path) {
    // Best effort: opening a directory is not possible on every platform, and
    // the rename has already happened.
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Replace `dir/LOG_NAME` with `bytes`, atomically.
fn write_atomic(dir: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = dir.join(TMP_NAME);
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, dir.join(LOG_NAME))?;
    sync_dir(dir);
    Ok(())
}

fn open_append(dir: &Path) -> std::io::Result<File> {
    OpenOptions::new().append(true).open(dir.join(LOG_NAME))
}

impl Store {
    /// Open (creating if needed) the store in `dir`.
    ///
    /// Recovers a consistent prefix of a torn log and truncates the tail off.
    /// Refuses a log written by a newer format, a foreign file, and a log
    /// larger than [`MAX_LOG_BYTES`].
    pub fn open(dir: &Path) -> Result<Self, StoreError> {
        fs::create_dir_all(dir)?;
        // A leftover temporary is an unfinished replacement; the real log is
        // intact. Ours to delete.
        match fs::remove_file(dir.join(TMP_NAME)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let path = dir.join(LOG_NAME);
        let bytes = match fs::metadata(&path) {
            Ok(m) if m.len() > MAX_LOG_BYTES => {
                return Err(StoreError::Format(crate::FormatError::RecordTooLarge));
            }
            Ok(_) => fs::read(&path)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let recovered = Engine::recover(&bytes)?;
        if recovered.fresh {
            write_atomic(dir, &format::header_bytes())?;
        } else if recovered.valid_len < bytes.len() {
            let f = OpenOptions::new().write(true).open(&path)?;
            f.set_len(recovered.valid_len as u64)?;
            f.sync_all()?;
        }
        let file = open_append(dir)?;
        let log_len = file.metadata()?.len();
        Ok(Self {
            dir: dir.to_owned(),
            file,
            engine: recovered.engine,
            log_len,
            poisoned: false,
        })
    }

    fn live(&self) -> Result<(), StoreError> {
        if self.poisoned {
            Err(StoreError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Store `value` under `key` in `ns` of partition `partition`.
    pub fn put(
        &mut self,
        partition: &PartitionKey,
        ns: Namespace,
        key: &[u8],
        value: &[u8],
    ) -> Result<(), StoreError> {
        self.live()?;
        let pid = PartId::from_key(partition)?;
        self.engine.check_put(&pid, ns, key, value)?;
        let pid_bytes = pid.encode().ok_or(QuotaError::PartitionKeyTooLarge)?;
        let record = format::encode_record(&Op::Put {
            ns: ns.code(),
            pid: &pid_bytes,
            key,
            value,
        })?;
        self.append(&record)?;
        self.engine.put(pid, ns, key, value);
        self.maybe_compact();
        Ok(())
    }

    /// Remove `key`. Returns whether it existed. The old value stays in the
    /// log until the next compaction; use `clear_partition` to force it out.
    pub fn delete(
        &mut self,
        partition: &PartitionKey,
        ns: Namespace,
        key: &[u8],
    ) -> Result<bool, StoreError> {
        self.live()?;
        let pid = PartId::from_key(partition)?;
        if self.engine.get(&pid, ns, key).is_none() {
            return Ok(false);
        }
        let pid_bytes = pid.encode().ok_or(QuotaError::PartitionKeyTooLarge)?;
        let record = format::encode_record(&Op::Delete {
            ns: ns.code(),
            pid: &pid_bytes,
            key,
        })?;
        self.append(&record)?;
        self.engine.delete(&pid, ns, key);
        Ok(true)
    }

    /// Read a value. Only this partition is consulted.
    pub fn get(&self, partition: &PartitionKey, ns: Namespace, key: &[u8]) -> Option<&[u8]> {
        if self.poisoned {
            return None;
        }
        let pid = PartId::from_key(partition).ok()?;
        self.engine.get(&pid, ns, key)
    }

    /// The keys present in `ns` of this partition, in byte order.
    pub fn keys(&self, partition: &PartitionKey, ns: Namespace) -> Vec<&[u8]> {
        match PartId::from_key(partition) {
            Ok(pid) if !self.poisoned => self.engine.keys(&pid, ns),
            _ => Vec::new(),
        }
    }

    /// Bytes this partition accounts for against its quota.
    pub fn partition_usage(&self, partition: &PartitionKey) -> u64 {
        PartId::from_key(partition).map_or(0, |pid| self.engine.usage(&pid))
    }

    /// Forget one partition entirely and rewrite the log without it.
    /// Returns the number of entries removed.
    pub fn clear_partition(&mut self, partition: &PartitionKey) -> Result<usize, StoreError> {
        self.live()?;
        let pid = PartId::from_key(partition)?;
        self.rewrite(&|id| id == &pid)?;
        Ok(self.engine.remove_partition(&pid))
    }

    /// Forget every partition whose top-level site is `partition`'s, whatever
    /// origin was embedded under it, and rewrite the log without them.
    /// Returns the number of entries removed.
    pub fn clear_site(&mut self, partition: &PartitionKey) -> Result<usize, StoreError> {
        self.live()?;
        let site = partition.top_level_site().to_owned();
        self.rewrite(&|id| id.site() == site)?;
        Ok(self.engine.remove_site(&site))
    }

    /// Rewrite the log from live state, dropping every overwritten and
    /// deleted value.
    pub fn compact(&mut self) -> Result<(), StoreError> {
        self.live()?;
        self.rewrite(&|_| false)
    }

    /// Total bytes accounted against the global quota. Carries no content.
    pub fn total_usage(&self) -> u64 {
        self.engine.total_usage()
    }

    /// Number of non-empty partitions. Carries no content.
    pub fn partition_count(&self) -> usize {
        self.engine.partition_count()
    }

    fn append(&mut self, record: &[u8]) -> Result<(), StoreError> {
        if self.log_len.saturating_add(record.len() as u64) > MAX_LOG_BYTES {
            self.rewrite(&|_| false)?;
        }
        let result = self.file.write_all(record).and_then(|()| self.file.sync_data());
        match result {
            Ok(()) => {
                self.log_len = self.log_len.saturating_add(record.len() as u64);
                Ok(())
            }
            Err(e) => {
                // Put the log back to the last length we vouched for.
                let restored = self
                    .file
                    .set_len(self.log_len)
                    .and_then(|()| self.file.sync_data());
                if restored.is_err() {
                    self.poisoned = true;
                }
                Err(e.into())
            }
        }
    }

    fn rewrite(&mut self, drop: &dyn Fn(&PartId) -> bool) -> Result<(), StoreError> {
        let snapshot = self.engine.snapshot_excluding(drop)?;
        write_atomic(&self.dir, &snapshot)?;
        // The rename happened; the old handle points at the unlinked file.
        match open_append(&self.dir) {
            Ok(f) => {
                self.file = f;
                self.log_len = snapshot.len() as u64;
                Ok(())
            }
            Err(e) => {
                self.poisoned = true;
                Err(e.into())
            }
        }
    }

    fn maybe_compact(&mut self) {
        let live = self.engine.total_usage();
        if self.log_len > live.saturating_mul(2).saturating_add(COMPACT_SLACK) {
            // Failure here is not a failure of the write that triggered it.
            let _ = self.rewrite(&|_| false);
        }
    }
}

#[cfg(test)]
mod tests {
    /// The type system is the property: every public `Store` method either
    /// takes a `PartitionKey` or is on this reviewed list of methods that
    /// carry no stored content. Adding an unpartitioned data method makes this
    /// test fail, which is the compile-fail-style guard invariant 2 asks for.
    #[test]
    fn api_surface_has_no_method_without_a_partition_key() {
        const NO_KEY_ALLOWED: &[&str] = &["open", "compact", "total_usage", "partition_count"];
        let src = include_str!("store.rs");
        let production = src.split("#[cfg(test)]").next().unwrap();
        let mut seen = Vec::new();
        for (i, _) in production.match_indices("    pub fn ") {
            let rest = production.get(i + "    pub fn ".len()..).unwrap();
            let name: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            let signature = rest.split('{').next().unwrap();
            if !signature.contains("PartitionKey") {
                assert!(
                    NO_KEY_ALLOWED.contains(&name.as_str()),
                    "pub fn {name} takes no PartitionKey and is not reviewed"
                );
            }
            seen.push(name);
        }
        // Every data method is present, so the scan is not vacuous.
        for want in ["put", "get", "delete", "keys", "clear_partition", "clear_site"] {
            assert!(seen.iter().any(|n| n == want), "{want} missing from scan");
        }
        // The no-key list is exact: no stale entries.
        for name in NO_KEY_ALLOWED {
            assert!(seen.iter().any(|n| n == name), "{name} not a pub fn");
        }
    }
}
