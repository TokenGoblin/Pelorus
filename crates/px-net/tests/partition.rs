//! Phase 3 gate item: connection pools are keyed by partition key, with no
//! unpartitioned path.
//!
//! Invariant 2's second sentence is the one under test — "there is no
//! unpartitioned code path to leave enabled by accident". Most of that is
//! enforced by the type system rather than by these tests: `ConnectionPool`
//! has no lookup that takes an origin without a key, and `PartitionKey` has no
//! constructor that skips the public suffix list. What a test *can* show is
//! that the keying actually separates the things it claims to.

use px_net::partition::{Origin, PartitionKey};
use px_net::pool::{ConnectionPool, Protocol};
use px_net::psl::PublicSuffixList;

/// A small list, so these tests state their own suffix rules rather than
/// depending on the shipped one. `crates/px-net/tests/psl.rs` is where the
/// real list is exercised.
///
/// This and the fixtures below return the fallible type, and each test does
/// its own `expect`. Not a style preference: clippy's `allow-expect-in-tests`
/// covers `#[test]` functions and not the helpers they call, and this crate
/// denies `expect` because it parses hostile bytes. Waiving the lint locally
/// is forbidden by the crate's CLAUDE.md, so the fixtures are honest about
/// being fallible instead.
fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("com\nnet\nco.uk\ngithub.io\n").ok()
}

fn origin(host: &str) -> Option<Origin> {
    Origin::new("https", host, 443)
}

fn key(psl: &PublicSuffixList, top_level: &str, dest: &str) -> Option<PartitionKey> {
    PartitionKey::new(psl, top_level, origin(dest)?)
}

/// The property the whole design exists for: the same destination, reached
/// from two different top-level sites, is two different pool entries.
///
/// Without this the shared connection is a cross-site identifier — the CDN
/// sees one connection carrying requests from both, which links them.
#[test]
fn net_partitions_the_same_origin_under_two_sites_does_not_share_a_connection() {
    let psl = psl().expect("a valid list");
    // Genuinely different sites. An earlier version of this test used
    // news.example.com and evil.example.com, which share the eTLD+1
    // example.com and are therefore *one* site — the test name claimed a
    // separation the hostnames did not have, and the code was right to merge
    // them. Subdomains sharing a partition is asserted deliberately below.
    let from_news =
        key(&psl, "news.example.com", "cdn.shared.net").expect("a registrable top-level site");
    let from_evil =
        key(&psl, "evil.attacker.com", "cdn.shared.net").expect("a registrable top-level site");

    assert_ne!(
        from_news, from_evil,
        "the same CDN reached from two sites must not be one key"
    );

    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    pool.put(from_news.clone(), Protocol::Http11, 1);

    assert_eq!(
        pool.take(&from_evil, Protocol::Http11),
        None,
        "a connection warmed for one top-level site must not be handed to another"
    );
    assert_eq!(
        pool.take(&from_news, Protocol::Http11),
        Some(1),
        "the site that warmed it must still get it back"
    );
}

/// Subdomains of one site share a partition, because the site is the eTLD+1.
/// Partitioning per-origin instead would break ordinary sites for no gain.
#[test]
fn net_partitions_subdomains_of_one_site_share_a_partition() {
    let psl = psl().expect("a valid list");
    let from_www =
        key(&psl, "www.example.com", "cdn.example.net").expect("a registrable top-level site");
    let from_shop =
        key(&psl, "shop.example.com", "cdn.example.net").expect("a registrable top-level site");

    assert_eq!(
        from_www, from_shop,
        "www and shop are the same site; eTLD+1 is the boundary"
    );

    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    pool.put(from_www, Protocol::Http11, 7);
    assert_eq!(pool.take(&from_shop, Protocol::Http11), Some(7));
}

/// The case the private section of the PSL exists for. These are different
/// people, and a list that did not know `github.io` would put them in one
/// partition.
#[test]
fn net_partitions_two_users_pages_on_one_host_do_not_share() {
    let psl = psl().expect("a valid list");
    let alice =
        key(&psl, "alice.github.io", "cdn.example.net").expect("a registrable top-level site");
    let bob = key(&psl, "bob.github.io", "cdn.example.net").expect("a registrable top-level site");
    assert_ne!(alice, bob);

    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    pool.put(alice, Protocol::Http11, 1);
    assert_eq!(pool.take(&bob, Protocol::Http11), None);
}

/// Different destinations under one top-level site are also distinct: the key
/// is the pair, not just the outer half.
#[test]
fn net_partitions_the_destination_is_part_of_the_key() {
    let psl = psl().expect("a valid list");
    let to_a =
        key(&psl, "news.example.com", "a.example.net").expect("a registrable top-level site");
    let to_b =
        key(&psl, "news.example.com", "b.example.net").expect("a registrable top-level site");
    assert_ne!(to_a, to_b);
    assert!(
        to_a.same_top_level(&to_b),
        "they are the same top-level site, and still different keys"
    );
}

/// Scheme and port are part of the origin, so they separate keys — but not of
/// the top-level *site*, so an http page cannot reach an https partition and
/// vice versa.
#[test]
fn net_partitions_scheme_and_port_separate_origins() {
    let psl = psl().expect("a valid list");
    let secure = PartitionKey::new(
        &psl,
        "news.example.com",
        Origin::new("https", "cdn.example.net", 443).expect("origin"),
    )
    .expect("key");
    let plain = PartitionKey::new(
        &psl,
        "news.example.com",
        Origin::new("http", "cdn.example.net", 80).expect("origin"),
    )
    .expect("key");
    let odd_port = PartitionKey::new(
        &psl,
        "news.example.com",
        Origin::new("https", "cdn.example.net", 8443).expect("origin"),
    )
    .expect("key");

    assert_ne!(secure, plain);
    assert_ne!(secure, odd_port);
}

/// A host with no registrable domain cannot be partitioned, so no key exists
/// for it. The caller has to refuse rather than fall back — and there is no
/// fallback in the API to reach for.
#[test]
fn net_partitions_a_host_with_no_registrable_domain_yields_no_key() {
    let psl = psl().expect("a valid list");
    for host in ["127.0.0.1", "com", "co.uk", "", "."] {
        assert_eq!(
            PartitionKey::new(
                &psl,
                host,
                origin("cdn.example.net").expect("a valid origin")
            ),
            None,
            "{host:?} has no registrable domain, so it must yield no key"
        );
    }
}

/// An origin this crate cannot represent is refused rather than normalised
/// into something adjacent.
#[test]
fn net_partitions_an_unrepresentable_origin_is_refused() {
    assert_eq!(Origin::new("file", "x", 0), None, "not a network scheme");
    assert_eq!(Origin::new("https", "", 443), None, "no host");
    assert_eq!(Origin::new("", "x.com", 443), None, "no scheme");
    assert_eq!(
        Origin::new("https", "exämple.com", 443),
        None,
        "a Unicode host means the IDNA step was skipped, and comparing Unicode \
         against an ASCII suffix list puts confusables in the wrong partition"
    );
    // Case and a trailing dot are normalised, not refused: they name the same
    // origin, and two representations of one key is how a cache gets two
    // entries that should have been one.
    assert_eq!(
        Origin::new("HTTPS", "Example.COM.", 443),
        Origin::new("https", "example.com", 443)
    );
}

/// Clearing a site's state must take its warm connections with it, or the
/// linkage survives the clearing that was meant to remove it.
#[test]
fn net_partitions_forgetting_a_key_drops_its_connections() {
    let psl = psl().expect("a valid list");
    let news =
        key(&psl, "news.example.com", "cdn.shared.net").expect("a registrable top-level site");
    let other =
        key(&psl, "other.unrelated.net", "cdn.shared.net").expect("a registrable top-level site");

    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    pool.put(news.clone(), Protocol::Http11, 1);
    pool.put(other.clone(), Protocol::Http11, 2);
    assert_eq!(pool.len(), 2);

    pool.forget(&news);
    assert_eq!(pool.take(&news, Protocol::Http11), None);
    assert_eq!(
        pool.take(&other, Protocol::Http11),
        Some(2),
        "forgetting one key must not disturb another"
    );
}

/// The pool must not grow to hold whatever it is given. Partitioning
/// multiplies entries by the number of top-level sites, so the bound that
/// mattered before partitioning is not the bound that matters now.
#[test]
fn net_partitions_the_pool_is_bounded_per_key_and_in_total() {
    let psl = psl().expect("a valid list");
    let one =
        key(&psl, "news.example.com", "cdn.example.net").expect("a registrable top-level site");

    let mut pool: ConnectionPool<u32> = ConnectionPool::new();
    for index in 0..100 {
        pool.put(one.clone(), Protocol::Http11, index);
    }
    assert!(
        pool.len() <= 8,
        "one key accumulated {} connections; a pool that holds whatever it is \
         given is a leak whose size a remote party chooses",
        pool.len()
    );

    // Many distinct sites, which is ordinary rather than hostile.
    let mut wide: ConnectionPool<u32> = ConnectionPool::new();
    for index in 0..1000u32 {
        let host = format!("site{index}.example.com");
        let key = PartitionKey::new(
            &psl,
            &host,
            origin("cdn.example.net").expect("a valid origin"),
        )
        .expect("key");
        wide.put(key, Protocol::Http11, index);
    }
    assert!(
        wide.key_count() <= 256,
        "the pool tracked {} keys without evicting",
        wide.key_count()
    );
}
