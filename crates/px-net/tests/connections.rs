//! Phase 3 gate item: zero connections outside the requested set.
//!
//! # What this can and cannot show
//!
//! §9 Phase 3 words this as a packet capture. A capture is the right
//! instrument for "what left this machine", and it is the wrong one for CI:
//! it needs privileges the runner does not grant, it is different on both
//! operating systems, and a gate that cannot run on a developer's machine is
//! one they learn to argue with.
//!
//! So the observation is moved to where the connections are made instead. A
//! listener on a second port records every accept, and the fetch under test is
//! given no reason to touch it. If `px-net` preconnected, prefetched DNS over
//! TCP, contacted an OCSP responder (ADR 013 says it never does), or opened a
//! speculative parallel connection, the count on one side or the other moves.
//!
//! That is weaker than a capture in one specific way, and it should be said
//! rather than glossed: it cannot see traffic to a host these tests never
//! bind. The other half of invariant 4 is enforced structurally instead — the
//! symbol audit in `ci/gate-network.sh` fails the build if any crate outside
//! `px-net` and `px-update` so much as names a socket type, and that keeps
//! holding as the project grows, where a test only speaks for the code
//! already written.

mod common;

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use px_net::partition::{Origin, PartitionKey};
use px_net::psl::PublicSuffixList;

fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("test\n").ok()
}

/// A listener nothing should ever connect to.
///
/// The control. A test that only counts connections to the server it meant to
/// use cannot distinguish "made one connection" from "made one connection and
/// also called somewhere else".
fn decoy() -> std::io::Result<(u16, Arc<AtomicUsize>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stream.is_ok() {
                counter.fetch_add(1, Ordering::SeqCst);
            }
        }
    });
    Ok((port, hits))
}

/// One fetch opens exactly one connection, and touches nothing else.
#[test]
fn net_connects_only_once_per_fetch_and_nowhere_else() {
    let corpus = common::load_corpus();
    let server = common::start(corpus).expect("the replay server must start");
    let (_decoy_port, decoy_hits) = decoy().expect("the decoy listener must start");

    let psl = psl().expect("a valid list");
    let origin = Origin::new("http", "127.0.0.1", server.port).expect("origin");
    let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");

    px_net::fetch::fetch(&key, "/r/000").expect("the fetch must succeed");

    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        1,
        "one fetch must open exactly one connection; a preconnect or a \
         speculative parallel connection would show as more"
    );
    assert_eq!(
        decoy_hits.load(Ordering::SeqCst),
        0,
        "nothing may connect to a host the fetch was never given"
    );
}

/// Ten fetches open ten connections and no more.
///
/// `Connection: close` is sent deliberately by the fetch primitive — pooling
/// is the caller's business — so the count is exact rather than an upper
/// bound, and a hidden retry would be visible as an eleventh.
#[test]
fn net_connects_only_as_many_times_as_it_was_asked_to() {
    let corpus = common::load_corpus();
    let server = common::start(corpus).expect("server");
    let (_decoy_port, decoy_hits) = decoy().expect("decoy");

    let psl = psl().expect("a valid list");

    for index in 0..10 {
        let origin = Origin::new("http", "127.0.0.1", server.port).expect("origin");
        let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");
        let path = format!("/r/{index:03}");
        px_net::fetch::fetch(&key, &path).expect("fetch");
    }

    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        10,
        "ten requested fetches must be ten connections, with no retries or \
         speculative extras"
    );
    assert_eq!(decoy_hits.load(Ordering::SeqCst), 0);
}

/// A failed fetch must not become traffic somewhere else.
///
/// The shape worth guarding: a client that cannot reach an origin and "helps"
/// by trying a fallback resolver, a proxy, or a different port. Invariant 4
/// permits pages the user requested and signed update manifests. Nothing else,
/// including on the error path.
#[test]
fn net_connects_only_nowhere_when_the_fetch_fails() {
    let (decoy_port, decoy_hits) = decoy().expect("decoy");
    let psl = psl().expect("a valid list");

    // A port with nothing listening. Chosen as the decoy's port plus one and
    // then verified closed, rather than assumed.
    let dead_port = decoy_port.checked_add(1).unwrap_or(1);
    if TcpListener::bind(("127.0.0.1", dead_port)).is_err() {
        // Something is listening there after all; the test would be
        // meaningless rather than wrong, so it declines to assert.
        return;
    }

    let origin = Origin::new("http", "127.0.0.1", dead_port).expect("origin");
    let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");

    let result = px_net::fetch::fetch(&key, "/r/000");
    assert!(result.is_err(), "connecting to a closed port must fail");
    assert_eq!(
        decoy_hits.load(Ordering::SeqCst),
        0,
        "a failed fetch must not fall back to anywhere"
    );
}

/// A request the client refuses to build must not open a socket at all.
///
/// Validation before connection, so a malformed target costs nothing and
/// reveals nothing — a connection opened and then abandoned still tells the
/// destination that somebody tried.
#[test]
fn net_connects_only_after_the_request_is_known_to_be_valid() {
    let corpus = common::load_corpus();
    let server = common::start(corpus).expect("server");

    let psl = psl().expect("a valid list");
    let origin = Origin::new("http", "127.0.0.1", server.port).expect("origin");
    let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");

    for path in ["/a b", "/a\rb", "relative"] {
        let _ = px_net::fetch::fetch(&key, path);
    }

    assert_eq!(
        server.accepted.load(Ordering::SeqCst),
        0,
        "a request that cannot be built must be refused before a socket opens"
    );
}
