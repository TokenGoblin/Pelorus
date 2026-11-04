#![forbid(unsafe_code)]

//! Partitioned storage and config-directory migration (build-spec section 9,
//! Phase 13).
//!
//! # Invariant 2, as a type
//!
//! Every [`Store`] method that touches data takes a px-net
//! [`PartitionKey`](px_net::partition::PartitionKey), and there is no method
//! that does not. A `PartitionKey` can only be built through the public
//! suffix list, so a caller that has not decided which top-level site it acts
//! for cannot express a read or a write. Internally the state is
//! `BTreeMap<PartId, Partition>`: no code path iterates across partitions and
//! returns data. `store::tests::api_surface_*` fails if a public `Store`
//! method appears that neither takes a key nor is on a short, reviewed list of
//! data-free methods.
//!
//! # Layout
//!
//! * [`format`] -- the append-only log format and its recovery rules (pure).
//! * [`Store`] -- the log on disk, quotas, `clear_partition` / `clear_site`.
//! * [`cookies`] -- persistence of px-net cookie jar entries per partition.
//! * [`migrate`] -- the config-directory rename migration.
//!
//! # Caps
//!
//! | cap | value |
//! |---|---|
//! | key | 1 KiB |
//! | value | 1 MiB |
//! | entries per partition | 10 000 |
//! | bytes per partition (key + value + 48 per entry) | 4 MiB |
//! | bytes in the whole store | 64 MiB |
//! | encoded partition id | 1 KiB |
//! | log file read at open | 256 MiB |
//!
//! Hitting one returns [`QuotaError`]; nothing panics on a limit.
//!
//! # Testing under Miri
//!
//! CI runs this crate's *unit tests* under Miri (build-spec 4.5). Miri cannot
//! run the filesystem, so every unit test here is pure: the log format, the
//! engine, quotas, recovery at every truncation offset, cookie encoding, and
//! the manifest and marker parsers all operate on byte slices. Everything
//! that touches a directory (the `Store` over real files, compaction,
//! deletion-from-disk checks, the migration) lives in `tests/` as integration
//! tests, which build their directories under `std::env::temp_dir()`.
//!
//! # Not covered
//!
//! No cross-process locking: the broker is the only opener. A second opener
//! on the same directory is unsupported. Deleting a file does not scrub
//! flash or journal copies; "forget" means the bytes are gone from this
//! crate's files, not from the device.

pub mod checksum;
mod engine;
pub mod error;
pub mod format;
pub mod store;

pub use engine::{
    ENTRY_OVERHEAD_BYTES, GLOBAL_QUOTA_BYTES, MAX_ENTRIES_PER_PARTITION, MAX_KEY_BYTES,
    MAX_PARTITION_ID_BYTES, MAX_VALUE_BYTES, Namespace, PARTITION_QUOTA_BYTES,
};
pub use error::{FormatError, QuotaError, StoreError};
pub use store::Store;
