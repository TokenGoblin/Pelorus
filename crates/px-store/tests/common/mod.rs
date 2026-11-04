//! Shared fixtures for the integration tests. Everything is inside a
//! `cfg(test)` module so clippy's test allowances (unwrap, indexing) apply:
//! these helpers are test code, and this crate may not waive the lints.
#![cfg(test)]
#![allow(dead_code)]

pub mod fx {
    use px_net::partition::{Origin, PartitionKey};
    use px_net::psl::PublicSuffixList;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A unique directory under the system temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);

    impl TempDir {
        pub fn new(label: &str) -> Self {
            let n = COUNTER.fetch_add(1, Ordering::SeqCst);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let p = std::env::temp_dir().join(format!(
                "pxstore-{label}-{}-{n}-{nanos}",
                std::process::id()
            ));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    pub fn psl() -> PublicSuffixList {
        PublicSuffixList::parse("com\nexample\n").unwrap()
    }

    pub fn pk(top: &str, origin_host: &str) -> PartitionKey {
        PartitionKey::new(&psl(), top, Origin::new("https", origin_host, 443).unwrap()).unwrap()
    }
}
