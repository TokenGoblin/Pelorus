//! Bounds and lifecycle of the partitioned connection pool.
//!
//! `partition.rs` covers the isolation property. This file covers the other
//! half of the pool's contract: it must stay bounded no matter what a remote
//! party causes, and clearing a site's state must really drop its connections.

use px_net::partition::{Origin, PartitionKey};
use px_net::pool::{ConnectionPool, Protocol};
use px_net::psl::PublicSuffixList;

fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("com\nnet\n").ok()
}

fn key(psl: &PublicSuffixList, top: &str, dest: &str) -> Option<PartitionKey> {
    PartitionKey::new(psl, top, Origin::new("https", dest, 443)?)
}

#[test]
fn net_pool_starts_empty() {
    let pool: ConnectionPool<u32> = ConnectionPool::default();
    assert!(pool.is_empty());
    assert_eq!(pool.len(), 0);
    assert_eq!(pool.key_count(), 0);
}

#[test]
fn net_pool_take_removes_the_connection() {
    let psl = psl().expect("psl");
    let k = key(&psl, "a.com", "cdn.com").expect("key");
    let mut pool = ConnectionPool::new();
    pool.put(k.clone(), Protocol::Http11, 1u32);
    assert_eq!(pool.take(&k, Protocol::Http11), Some(1));
    assert_eq!(pool.take(&k, Protocol::Http11), None);
}

#[test]
fn net_pool_reuses_the_most_recently_returned_connection_first() {
    let psl = psl().expect("psl");
    let k = key(&psl, "a.com", "cdn.com").expect("key");
    let mut pool = ConnectionPool::new();
    pool.put(k.clone(), Protocol::Http11, 1u32);
    pool.put(k.clone(), Protocol::Http11, 2);
    assert_eq!(pool.take(&k, Protocol::Http11), Some(2));
    assert_eq!(pool.take(&k, Protocol::Http11), Some(1));
}

#[test]
fn net_pool_caps_idle_connections_per_key() {
    let psl = psl().expect("psl");
    let k = key(&psl, "a.com", "cdn.com").expect("key");
    let mut pool = ConnectionPool::new();
    for i in 0..100u32 {
        pool.put(k.clone(), Protocol::Http11, i);
    }
    assert_eq!(pool.len(), 6, "surplus connections are dropped, not stored");
    assert_eq!(pool.key_count(), 1);
    // The ones kept are the first six offered.
    assert_eq!(pool.take(&k, Protocol::Http11), Some(5));
}

#[test]
fn net_pool_bounds_the_number_of_keys_and_evicts_the_oldest() {
    let psl = psl().expect("psl");
    let mut pool = ConnectionPool::new();
    let first = key(&psl, "site0.com", "cdn.com").expect("key");
    pool.put(first.clone(), Protocol::Http11, 0u32);
    for i in 1..400u32 {
        let k = key(&psl, &format!("site{i}.com"), "cdn.com").expect("key");
        pool.put(k, Protocol::Http11, i);
    }
    assert_eq!(pool.key_count(), 256);
    assert_eq!(pool.len(), 256);
    assert_eq!(pool.take(&first, Protocol::Http11), None, "oldest evicted");
    let newest = key(&psl, "site399.com", "cdn.com").expect("key");
    assert_eq!(pool.take(&newest, Protocol::Http11), Some(399));
}

#[test]
fn net_pool_forget_drops_only_that_key() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "cdn.com").expect("key");
    let b = key(&psl, "b.com", "cdn.com").expect("key");
    let mut pool = ConnectionPool::new();
    pool.put(a.clone(), Protocol::Http11, 1u32);
    pool.put(a.clone(), Protocol::Http11, 2);
    pool.put(b.clone(), Protocol::Http11, 3);
    pool.forget(&a);
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.key_count(), 1);
    assert_eq!(pool.take(&a, Protocol::Http11), None);
    assert_eq!(pool.take(&b, Protocol::Http11), Some(3));
}

#[test]
fn net_pool_forgetting_an_unknown_key_is_harmless() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "cdn.com").expect("key");
    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    pool.forget(&a);
    assert!(pool.is_empty());
}

#[test]
fn net_pool_expire_idle_keeps_fresh_connections() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "cdn.com").expect("key");
    let mut pool = ConnectionPool::new();
    pool.put(a.clone(), Protocol::Http11, 1u32);
    pool.expire_idle();
    assert_eq!(pool.len(), 1);
    assert_eq!(pool.take(&a, Protocol::Http11), Some(1));
}

#[test]
fn net_pool_slot_is_reusable_after_forget_and_eviction_bookkeeping_stays_consistent() {
    let psl = psl().expect("psl");
    let mut pool = ConnectionPool::new();
    // Fill, forget half, then overfill again: the order list must not retain
    // forgotten keys, or eviction would remove nothing and the pool would grow.
    for i in 0..256u32 {
        let k = key(&psl, &format!("s{i}.com"), "cdn.com").expect("key");
        pool.put(k, Protocol::Http11, i);
    }
    for i in 0..128u32 {
        let k = key(&psl, &format!("s{i}.com"), "cdn.com").expect("key");
        pool.forget(&k);
    }
    for i in 1000..1300u32 {
        let k = key(&psl, &format!("s{i}.com"), "cdn.com").expect("key");
        pool.put(k, Protocol::Http11, i);
    }
    assert!(pool.key_count() <= 256);
}
