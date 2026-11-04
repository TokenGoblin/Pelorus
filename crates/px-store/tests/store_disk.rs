//! The `Store` over real files: durability, recovery from torn writes at every
//! byte offset, compaction, and "forget really deletes".
#![cfg(test)]

mod common;

mod disk {
    use super::common::fx::{TempDir, pk};
    use px_store::error::{FormatError, QuotaError};
    use px_store::store::{LOG_NAME, TMP_NAME};
    use px_store::{MAX_VALUE_BYTES, Namespace, Store, StoreError};
    use std::collections::BTreeMap;
    use std::fs;

    const S: Namespace = Namespace::Storage;

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn values_survive_reopen() {
        let d = TempDir::new("reopen");
        let p = pk("a.example", "cdn.example");
        {
            let mut s = Store::open(d.path()).unwrap();
            s.put(&p, S, b"k", b"v1").unwrap();
            s.put(&p, S, b"k", b"v2").unwrap();
            s.put(&p, S, b"gone", b"x").unwrap();
            assert!(s.delete(&p, S, b"gone").unwrap());
            assert!(!s.delete(&p, S, b"gone").unwrap());
        }
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&p, S, b"k"), Some(&b"v2"[..]));
        assert_eq!(s.get(&p, S, b"gone"), None);
        assert_eq!(s.keys(&p, S), vec![&b"k"[..]]);
    }

    #[test]
    fn quota_errors_not_panics() {
        let d = TempDir::new("quota");
        let mut s = Store::open(d.path()).unwrap();
        let p = pk("a.example", "cdn.example");
        let big = vec![7u8; MAX_VALUE_BYTES + 1];
        assert!(matches!(
            s.put(&p, S, b"k", &big),
            Err(StoreError::Quota(QuotaError::ValueTooLarge { .. }))
        ));
        for i in 0..3u8 {
            s.put(&p, S, &[i], &vec![1u8; MAX_VALUE_BYTES]).unwrap();
        }
        assert!(matches!(
            s.put(&p, S, &[9], &vec![1u8; MAX_VALUE_BYTES]),
            Err(StoreError::Quota(QuotaError::PartitionFull { .. }))
        ));
        // The refused write left nothing behind, in memory or on disk.
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&p, S, &[9]), None);
        assert_eq!(s.keys(&p, S).len(), 3);
    }

    /// Truncate the real log file at every byte offset. Each time the store
    /// must open, expose exactly the operations wholly written before the cut,
    /// truncate the torn tail away, and accept new writes afterwards.
    #[test]
    fn torn_log_at_every_offset_opens_with_consistent_prefix() {
        let src = TempDir::new("torn-src");
        let p = pk("a.example", "cdn.example");
        let q = pk("b.example", "cdn.example");
        type DiskOp<'a> = (bool, &'a [u8], Option<&'a [u8]>);
        let ops: Vec<DiskOp<'_>> = vec![
            (true, b"a", Some(b"alpha")),
            (false, b"a", Some(b"other-partition")),
            (true, b"a", Some(b"alpha-2")),
            (true, b"b", Some(b"")),
            (true, b"a", None),
            (true, b"c", Some(b"a longer value to span several bytes")),
        ];
        let mut ends = vec![16u64];
        {
            let mut s = Store::open(src.path()).unwrap();
            for (first, k, v) in &ops {
                let part = if *first { &p } else { &q };
                match v {
                    Some(v) => s.put(part, S, k, v).unwrap(),
                    None => {
                        s.delete(part, S, k).unwrap();
                    }
                }
                ends.push(fs::metadata(src.path().join(LOG_NAME)).unwrap().len());
            }
        }
        let log = fs::read(src.path().join(LOG_NAME)).unwrap();
        assert_eq!(*ends.last().unwrap(), log.len() as u64);

        for cut in 0..=log.len() {
            let d = TempDir::new("torn");
            fs::write(d.path().join(LOG_NAME), &log[..cut]).unwrap();
            let mut s = Store::open(d.path()).unwrap_or_else(|e| panic!("cut {cut}: {e}"));

            let applied = ends
                .iter()
                .filter(|&&e| e as usize <= cut)
                .count()
                .saturating_sub(1);
            let mut want: BTreeMap<(bool, Vec<u8>), Vec<u8>> = BTreeMap::new();
            for (first, k, v) in ops.iter().take(applied) {
                match v {
                    Some(v) => want.insert((*first, k.to_vec()), v.to_vec()),
                    None => want.remove(&(*first, k.to_vec())),
                };
            }
            for first in [true, false] {
                let part = if first { &p } else { &q };
                for k in [&b"a"[..], b"b", b"c"] {
                    assert_eq!(
                        s.get(part, S, k).map(<[u8]>::to_vec),
                        want.get(&(first, k.to_vec())).cloned(),
                        "cut {cut} key {k:?}"
                    );
                }
            }
            // The torn tail is gone from disk and the store is writable.
            let len = fs::metadata(d.path().join(LOG_NAME)).unwrap().len();
            let expect_len = if cut < 16 { 16 } else { ends[applied] };
            assert_eq!(len, expect_len, "cut {cut}");
            s.put(&p, S, b"after", b"ok").unwrap();
            drop(s);
            let s = Store::open(d.path()).unwrap();
            assert_eq!(s.get(&p, S, b"after"), Some(&b"ok"[..]), "cut {cut}");
        }
    }

    #[test]
    fn newer_format_and_foreign_files_are_refused_and_left_alone() {
        let d = TempDir::new("refuse");
        let path = d.path().join(LOG_NAME);
        let mut newer = b"PXSL".to_vec();
        newer.extend_from_slice(&2u32.to_le_bytes());
        newer.extend_from_slice(&0u32.to_le_bytes());
        let crc = px_store::checksum::crc32(&newer);
        newer.extend_from_slice(&crc.to_le_bytes());
        newer.extend_from_slice(b"records from the future");
        fs::write(&path, &newer).unwrap();
        assert!(matches!(
            Store::open(d.path()),
            Err(StoreError::Format(FormatError::UnsupportedVersion {
                version: 2,
                ..
            }))
        ));
        assert_eq!(
            fs::read(&path).unwrap(),
            newer,
            "refusal must not modify the file"
        );

        fs::write(&path, b"SQLite format 3\0 and more").unwrap();
        assert!(matches!(
            Store::open(d.path()),
            Err(StoreError::Format(FormatError::NotAStore))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"SQLite format 3\0 and more");
    }

    #[test]
    fn leftover_temporary_from_a_crashed_replacement_is_ignored() {
        let d = TempDir::new("tmp");
        let p = pk("a.example", "cdn.example");
        {
            let mut s = Store::open(d.path()).unwrap();
            s.put(&p, S, b"k", b"real").unwrap();
        }
        fs::write(d.path().join(TMP_NAME), b"half a snapshot").unwrap();
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&p, S, b"k"), Some(&b"real"[..]));
        assert!(!d.path().join(TMP_NAME).exists());
    }

    #[test]
    fn clear_partition_removes_the_bytes_from_disk() {
        let d = TempDir::new("clear");
        let a = pk("a.example", "cdn.example");
        let b = pk("b.example", "cdn.example");
        let mut s = Store::open(d.path()).unwrap();
        s.put(&a, S, b"session", b"SECRET-TOKEN-A").unwrap();
        s.put(&a, S, b"session", b"SECRET-TOKEN-A-v2").unwrap(); // overwritten value too
        s.put(&a, S, b"deleted", b"DELETED-VALUE-A").unwrap();
        s.delete(&a, S, b"deleted").unwrap();
        s.put(&b, S, b"session", b"KEEP-THIS-B").unwrap();
        let before = fs::read(d.path().join(LOG_NAME)).unwrap();
        assert!(contains(&before, b"SECRET-TOKEN-A"));
        assert!(
            contains(&before, b"DELETED-VALUE-A"),
            "delete alone leaves the bytes"
        );

        assert_eq!(s.clear_partition(&a).unwrap(), 1);
        let after = fs::read(d.path().join(LOG_NAME)).unwrap();
        assert!(!contains(&after, b"SECRET-TOKEN-A"));
        assert!(!contains(&after, b"DELETED-VALUE-A"));
        assert!(contains(&after, b"KEEP-THIS-B"));
        // And no other file in the directory holds them.
        for e in fs::read_dir(d.path()).unwrap() {
            let bytes = fs::read(e.unwrap().path()).unwrap();
            assert!(!contains(&bytes, b"SECRET-TOKEN-A"));
        }
        assert_eq!(s.get(&a, S, b"session"), None);
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&a, S, b"session"), None);
        assert_eq!(s.get(&b, S, b"session"), Some(&b"KEEP-THIS-B"[..]));
    }

    #[test]
    fn clear_partition_of_an_emptied_partition_still_scrubs_deleted_values() {
        let d = TempDir::new("clear-empty");
        let a = pk("a.example", "cdn.example");
        let mut s = Store::open(d.path()).unwrap();
        s.put(&a, S, b"k", b"ONLY-VALUE-IN-A").unwrap();
        s.delete(&a, S, b"k").unwrap();
        assert_eq!(s.partition_count(), 0);
        assert_eq!(s.clear_partition(&a).unwrap(), 0);
        let after = fs::read(d.path().join(LOG_NAME)).unwrap();
        assert!(!contains(&after, b"ONLY-VALUE-IN-A"));
    }

    #[test]
    fn clear_site_removes_every_origin_embedded_under_it() {
        let d = TempDir::new("clear-site");
        let a_self = pk("a.example", "a.example");
        let a_cdn = pk("a.example", "cdn.example");
        let b_cdn = pk("b.example", "cdn.example");
        let mut s = Store::open(d.path()).unwrap();
        s.put(&a_self, S, b"k", b"SITE-A-FIRST-PARTY").unwrap();
        s.put(&a_cdn, S, b"k", b"SITE-A-THIRD-PARTY").unwrap();
        s.put(&b_cdn, S, b"k", b"SITE-B-THIRD-PARTY").unwrap();
        assert_eq!(s.clear_site(&a_cdn).unwrap(), 2);
        let bytes = fs::read(d.path().join(LOG_NAME)).unwrap();
        assert!(!contains(&bytes, b"SITE-A-FIRST-PARTY"));
        assert!(!contains(&bytes, b"SITE-A-THIRD-PARTY"));
        assert!(contains(&bytes, b"SITE-B-THIRD-PARTY"));
        assert_eq!(s.get(&b_cdn, S, b"k"), Some(&b"SITE-B-THIRD-PARTY"[..]));
        assert_eq!(s.partition_count(), 1);
    }

    #[test]
    fn overwrites_trigger_compaction_so_the_log_stays_bounded() {
        let d = TempDir::new("autocompact");
        let a = pk("a.example", "cdn.example");
        let mut s = Store::open(d.path()).unwrap();
        let value = vec![5u8; 100_000];
        for _ in 0..60 {
            s.put(&a, S, b"k", &value).unwrap();
        }
        let len = fs::metadata(d.path().join(LOG_NAME)).unwrap().len();
        // Live data is ~100 KB; without compaction the log would be ~6 MB.
        // Bound: 2 * live + 1 MiB slack + one record.
        assert!(
            len < 2 * 100_100 + (1 << 20) + 100_100,
            "log is {len} bytes"
        );
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&a, S, b"k"), Some(&value[..]));
    }
}
