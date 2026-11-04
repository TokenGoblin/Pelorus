//! The cross-site leak harness (Phase 13 gate: "cross-site leak harness finds
//! no shared state").
//!
//! The scenario is the one partitioning exists for: `cdn.example` is embedded
//! on `a.example` and on `b.example`, and writes the same logical keys on
//! both. Nothing it stored on one may be visible, by any API, while it is
//! embedded in the other, including after the store is reopened from disk.
#![cfg(test)]

mod common;

mod leak {
    use super::common::fx::{TempDir, psl};
    use px_net::cookie::Jar;
    use px_net::partition::{Origin, PartitionKey};
    use px_store::cookies::{load, save};
    use px_store::{Namespace, Store};
    use std::time::Instant;

    const NAMESPACES: [Namespace; 2] = [Namespace::Storage, Namespace::Cookies];
    type Seen = (u8, Vec<u8>, Option<Vec<u8>>);
    const KEYS: [&[u8]; 4] = [b"uid", b"session", b"", b"k\0with-nul"];

    fn key(top: &str, scheme: &str, host: &str, port: u16) -> PartitionKey {
        PartitionKey::new(&psl(), top, Origin::new(scheme, host, port).unwrap()).unwrap()
    }

    fn a() -> PartitionKey {
        key("a.example", "https", "cdn.example", 443)
    }
    fn b() -> PartitionKey {
        key("b.example", "https", "cdn.example", 443)
    }

    /// Everything observable about one partition through the public API.
    fn observe(s: &Store, p: &PartitionKey) -> Vec<Seen> {
        let mut seen = Vec::new();
        for (i, ns) in NAMESPACES.iter().enumerate() {
            for k in s.keys(p, *ns) {
                seen.push((i as u8, k.to_vec(), s.get(p, *ns, k).map(<[u8]>::to_vec)));
            }
            for k in KEYS {
                if let Some(v) = s.get(p, *ns, k) {
                    seen.push((i as u8, k.to_vec(), Some(v.to_vec())));
                }
            }
        }
        seen.sort();
        seen.dedup();
        seen
    }

    fn assert_never_contains(
        seen: &[Seen],
        needle: &[u8],
        context: &str,
    ) {
        for (_, _, v) in seen {
            let v = v.as_deref().unwrap_or_default();
            assert!(
                !v.windows(needle.len()).any(|w| w == needle),
                "{context}: saw {needle:?}"
            );
        }
    }

    #[test]
    fn same_logical_keys_in_two_embeddings_share_nothing() {
        let d = TempDir::new("leak");
        for reopen in [false, true] {
            // Fresh directory for each pass so the first pass's data cannot
            // mask a second-pass leak.
            let dir = d.path().join(if reopen { "reopened" } else { "live" });
            let mut s = Store::open(&dir).unwrap();
            for ns in NAMESPACES {
                for k in KEYS {
                    s.put(&a(), ns, k, b"VALUE-FROM-A").unwrap();
                }
            }
            // Only A has written. B must observe a completely empty world.
            if reopen {
                drop(s);
                s = Store::open(&dir).unwrap();
            }
            assert!(observe(&s, &b()).is_empty(), "B saw A's data (reopen={reopen})");
            assert_eq!(s.partition_usage(&b()), 0);
            for ns in NAMESPACES {
                assert!(s.keys(&b(), ns).is_empty());
                for k in KEYS {
                    assert_eq!(s.get(&b(), ns, k), None);
                }
            }

            // Now B writes the same keys with different values.
            for ns in NAMESPACES {
                for k in KEYS {
                    s.put(&b(), ns, k, b"VALUE-FROM-B").unwrap();
                }
            }
            if reopen {
                drop(s);
                s = Store::open(&dir).unwrap();
            }
            assert_never_contains(&observe(&s, &a()), b"FROM-B", "A observing B");
            assert_never_contains(&observe(&s, &b()), b"FROM-A", "B observing A");
            assert_eq!(observe(&s, &a()).len(), 8);
            assert_eq!(observe(&s, &b()).len(), 8);

            // Usage is per partition and sums to the whole: no hidden shared pool.
            assert_eq!(s.partition_usage(&a()) + s.partition_usage(&b()), s.total_usage());
            assert_eq!(s.partition_count(), 2);

            // Mutating B does not touch A.
            s.delete(&b(), Namespace::Storage, b"uid").unwrap();
            s.put(&b(), Namespace::Storage, b"session", b"B-CHANGED").unwrap();
            assert_eq!(s.get(&a(), Namespace::Storage, b"uid"), Some(&b"VALUE-FROM-A"[..]));
            assert_eq!(s.get(&a(), Namespace::Storage, b"session"), Some(&b"VALUE-FROM-A"[..]));

            // Forgetting A leaves B whole, and the reverse.
            s.clear_partition(&a()).unwrap();
            assert!(observe(&s, &a()).is_empty());
            assert_eq!(s.get(&b(), Namespace::Storage, b"session"), Some(&b"B-CHANGED"[..]));
            drop(s);
            let s = Store::open(&dir).unwrap();
            assert!(observe(&s, &a()).is_empty());
            assert_eq!(s.get(&b(), Namespace::Storage, b"session"), Some(&b"B-CHANGED"[..]));
        }
    }

    /// Partitions that differ in exactly one component are different
    /// partitions: scheme, port, origin host, or top-level site.
    #[test]
    fn near_identical_keys_are_distinct_partitions() {
        let d = TempDir::new("leak-near");
        let base = key("a.example", "https", "cdn.example", 443);
        let variants = [
            key("a.example", "http", "cdn.example", 443),
            key("a.example", "https", "cdn.example", 8443),
            key("a.example", "https", "cdn2.example", 443),
            key("a.example", "https", "www.cdn.example", 443),
            key("b.example", "https", "cdn.example", 443),
            key("aa.example", "https", "cdn.example", 443),
            key("a.example", "https", "a.example", 443),
        ];
        let mut s = Store::open(d.path()).unwrap();
        s.put(&base, Namespace::Storage, b"k", b"BASE").unwrap();
        for (i, v) in variants.iter().enumerate() {
            assert_eq!(s.get(v, Namespace::Storage, b"k"), None, "variant {i} read base");
            s.put(v, Namespace::Storage, b"k", format!("V{i}").as_bytes()).unwrap();
        }
        drop(s);
        let s = Store::open(d.path()).unwrap();
        assert_eq!(s.get(&base, Namespace::Storage, b"k"), Some(&b"BASE"[..]));
        for (i, v) in variants.iter().enumerate() {
            assert_eq!(
                s.get(v, Namespace::Storage, b"k"),
                Some(format!("V{i}").as_bytes()),
                "variant {i}"
            );
        }
        assert_eq!(s.partition_count(), variants.len() + 1);
    }

    /// Cookies persisted for the same cookie name on both embeddings load only
    /// into their own partition's jar.
    #[test]
    fn persisted_cookies_do_not_cross_partitions() {
        let d = TempDir::new("leak-cookies");
        let psl = psl();
        let (pa, pb) = (key("a.example", "https", "www.cdn.example", 443), key("b.example", "https", "www.cdn.example", 443));
        let mut jar = Jar::new();
        jar.store(&psl, &pa, "uid=UID-FROM-A; Path=/; Max-Age=1000").unwrap();
        jar.store(&psl, &pb, "uid=UID-FROM-B; Path=/; Max-Age=1000").unwrap();
        {
            let mut s = Store::open(d.path()).unwrap();
            let now = Instant::now();
            save(&mut s, &pa, &jar.cookies_for(&pa, "/", false), now, 0).unwrap();
            save(&mut s, &pb, &jar.cookies_for(&pb, "/", false), now, 0).unwrap();
        }
        let s = Store::open(d.path()).unwrap();
        for (mine, other, want, not) in [
            (&pa, &pb, "UID-FROM-A", "UID-FROM-B"),
            (&pb, &pa, "UID-FROM-B", "UID-FROM-A"),
        ] {
            let mut j = Jar::new();
            assert_eq!(load(&s, &psl, mine, &mut j, 10, true).loaded, 1);
            let seen = j.cookies_for(mine, "/", false);
            assert_eq!(seen.len(), 1);
            assert_eq!(seen[0].value, want);
            assert!(j.cookies_for(other, "/", false).is_empty());
            assert!(seen.iter().all(|c| c.value != not));
        }
    }

    /// No partition can be reached without a key: the surface test in
    /// `store.rs` enforces it for methods; this checks the one aggregate pair
    /// leaks no per-partition content (only counts).
    #[test]
    fn aggregate_methods_return_numbers_only() {
        let d = TempDir::new("leak-agg");
        let mut s = Store::open(d.path()).unwrap();
        s.put(&a(), Namespace::Storage, b"k", b"v").unwrap();
        let _: u64 = s.total_usage();
        let _: usize = s.partition_count();
    }
}
