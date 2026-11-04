//! Error types. Nothing in this crate panics on a limit; it returns one of these.

use std::fmt;

/// A documented cap was hit. The write did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuotaError {
    /// Key longer than [`crate::MAX_KEY_BYTES`].
    KeyTooLarge {
        /// Offered length.
        len: usize,
    },
    /// Value longer than [`crate::MAX_VALUE_BYTES`].
    ValueTooLarge {
        /// Offered length.
        len: usize,
    },
    /// The partition already holds [`crate::MAX_ENTRIES_PER_PARTITION`] entries.
    TooManyEntries,
    /// The write would take one partition over [`crate::PARTITION_QUOTA_BYTES`].
    PartitionFull {
        /// Bytes the partition would hold after the write.
        would_use: u64,
    },
    /// The write would take the whole store over [`crate::GLOBAL_QUOTA_BYTES`].
    GlobalFull {
        /// Bytes the store would hold after the write.
        would_use: u64,
    },
    /// The partition key encodes to more than [`crate::MAX_PARTITION_ID_BYTES`].
    PartitionKeyTooLarge,
}

impl fmt::Display for QuotaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyTooLarge { len } => write!(f, "key of {len} bytes exceeds the cap"),
            Self::ValueTooLarge { len } => write!(f, "value of {len} bytes exceeds the cap"),
            Self::TooManyEntries => write!(f, "partition holds the maximum number of entries"),
            Self::PartitionFull { would_use } => {
                write!(f, "partition quota exceeded ({would_use} bytes)")
            }
            Self::GlobalFull { would_use } => {
                write!(f, "global quota exceeded ({would_use} bytes)")
            }
            Self::PartitionKeyTooLarge => write!(f, "partition key is too large to store"),
        }
    }
}

impl std::error::Error for QuotaError {}

/// Why a store file could not be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    /// The file does not start with this format's magic. Not ours; not touched.
    NotAStore,
    /// A header written by a newer (or unknown) format. Refused, never
    /// downgraded: a newer writer may have records this reader would drop.
    UnsupportedVersion {
        /// Version found.
        version: u32,
        /// Flags found.
        flags: u32,
    },
    /// The header's checksum or length is wrong.
    BadHeader,
    /// A record exceeds the documented size bound.
    RecordTooLarge,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAStore => write!(f, "not a store file"),
            Self::UnsupportedVersion { version, flags } => {
                write!(
                    f,
                    "unsupported store format (version {version}, flags {flags:#x})"
                )
            }
            Self::BadHeader => write!(f, "store header is damaged"),
            Self::RecordTooLarge => write!(f, "record exceeds the size bound"),
        }
    }
}

impl std::error::Error for FormatError {}

/// Anything a [`crate::Store`] operation can fail with.
#[derive(Debug)]
pub enum StoreError {
    /// A cap was hit.
    Quota(QuotaError),
    /// The file on disk is not a store this version can use.
    Format(FormatError),
    /// The operating system refused.
    Io(std::io::Error),
    /// An earlier write failed in a way that left the log in an unknown state.
    /// The store refuses everything until it is reopened (which recovers).
    Poisoned,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quota(e) => write!(f, "quota: {e}"),
            Self::Format(e) => write!(f, "format: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Poisoned => write!(f, "store is poisoned by an earlier failed write; reopen it"),
        }
    }
}

impl std::error::Error for StoreError {}

impl From<QuotaError> for StoreError {
    fn from(e: QuotaError) -> Self {
        Self::Quota(e)
    }
}
impl From<FormatError> for StoreError {
    fn from(e: FormatError) -> Self {
        Self::Format(e)
    }
}
impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
