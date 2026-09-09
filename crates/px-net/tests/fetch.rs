//! Phase 3 gate item: 200 URLs fetched correctly.
//!
//! Against the recorded corpus and a local replay server, for the reasons in
//! `common/mod.rs`: a gate that needs the internet goes red when somebody
//! else has an outage, and the awkward framings cannot be summoned from the
//! real web on demand.
//!
//! "Correctly" is the load-bearing word. Fetching two hundred URLs and
//! checking each returned *something* would pass against a client that
//! truncated every body, so every response is compared against the SHA-256
//! recorded with it.

mod common;

use px_net::partition::{Origin, PartitionKey};
use px_net::psl::PublicSuffixList;

/// A suffix list that makes `localhost.test` a registrable domain, so a
/// partition key exists for the replay server.
///
/// Note which half of the key needs it. The *top-level site* is looked up in
/// the suffix list, because that is what the partition is named after; the
/// *destination origin* is connected to verbatim, so it is the loopback
/// address. An earlier version of this test used `localhost.test` for both and
/// failed to resolve — the key was fine, the socket had nowhere to go.
fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("test\n").ok()
}

fn sha256_hex(input: &[u8]) -> String {
    // Same implementation as tests/psl.rs, which is cross-checked against
    // coreutils sha256sum there.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut message = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for block in message.chunks(64) {
        let mut w = [0u32; 64];
        for (index, word) in w.iter_mut().enumerate().take(16) {
            let start = index * 4;
            let bytes = block.get(start..start + 4).unwrap_or(&[0, 0, 0, 0]);
            *word = u32::from_be_bytes([
                *bytes.first().unwrap_or(&0),
                *bytes.get(1).unwrap_or(&0),
                *bytes.get(2).unwrap_or(&0),
                *bytes.get(3).unwrap_or(&0),
            ]);
        }
        for index in 16..64 {
            let w15 = *w.get(index - 15).unwrap_or(&0);
            let w2 = *w.get(index - 2).unwrap_or(&0);
            let s0 = w15.rotate_right(7) ^ w15.rotate_right(18) ^ (w15 >> 3);
            let s1 = w2.rotate_right(17) ^ w2.rotate_right(19) ^ (w2 >> 10);
            let value = w
                .get(index - 16)
                .unwrap_or(&0)
                .wrapping_add(s0)
                .wrapping_add(*w.get(index - 7).unwrap_or(&0))
                .wrapping_add(s1);
            if let Some(slot) = w.get_mut(index) {
                *slot = value;
            }
        }
        let (mut a, mut b, mut c, mut d) = (h[0], h[1], h[2], h[3]);
        let (mut e, mut f, mut g, mut hh) = (h[4], h[5], h[6], h[7]);
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(*K.get(index).unwrap_or(&0))
                .wrapping_add(*w.get(index).unwrap_or(&0));
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// The gate item: every recorded URL is fetched, and every body matches the
/// digest recorded with it.
#[test]
fn net_fetches_every_recorded_url_correctly() {
    let corpus = common::load_corpus();
    assert_eq!(
        corpus.len(),
        200,
        "§9 Phase 3 asks for 200 URLs; the corpus holds {}",
        corpus.len()
    );

    let server = common::start(corpus.clone()).expect("the replay server must start");
    let psl = psl().expect("a valid list");

    let mut checked = 0usize;
    for entry in &corpus {
        let origin = Origin::new("http", "127.0.0.1", server.port).expect("a representable origin");
        let key = PartitionKey::new(&psl, "localhost.test", origin).expect("a partition key");

        let response = match px_net::fetch::fetch(&key, &entry.path) {
            Ok(response) => response,
            Err(error) => panic!("fetching {} failed: {error}", entry.path),
        };

        assert_eq!(
            response.head.status, entry.status,
            "wrong status for {}",
            entry.path
        );

        // The digest is the point. Checking only that something came back
        // would pass against a client that truncated every body.
        let expected_empty = matches!(entry.framing.as_str(), "none");
        if expected_empty {
            assert!(
                response.body.is_empty(),
                "{} declared a Content-Length on a bodiless status and it was \
                 not ignored",
                entry.path
            );
        } else {
            assert_eq!(
                sha256_hex(&response.body),
                entry.body_sha256,
                "body mismatch for {} ({} bytes received)",
                entry.path,
                response.body.len()
            );
        }
        checked += 1;
    }

    assert_eq!(checked, 200);
}

/// A redirect is surfaced, not followed. Following one inside the fetch
/// primitive would send a request somewhere the caller never approved, and
/// the partition decision for the target has not been made.
#[test]
fn net_fetches_does_not_follow_redirects_itself() {
    let corpus = common::load_corpus();
    let server = common::start(corpus.clone()).expect("server");
    let psl = psl().expect("a valid list");

    let redirect = corpus
        .iter()
        .find(|entry| entry.status == 301)
        .expect("the corpus contains a 301");

    let origin = Origin::new("http", "127.0.0.1", server.port).expect("origin");
    let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");
    let response = px_net::fetch::fetch(&key, &redirect.path).expect("fetch");

    assert_eq!(response.head.status, 301);
    assert!(
        response.redirect_target().is_some(),
        "the location must be surfaced for the caller to decide about"
    );
}

/// A request target that could split the request is refused before a socket is
/// opened.
#[test]
fn net_fetches_refuses_a_request_target_that_would_split_the_request() {
    let psl = psl().expect("a valid list");
    let origin = Origin::new("http", "127.0.0.1", 1).expect("origin");
    let key = PartitionKey::new(&psl, "localhost.test", origin).expect("key");

    for path in ["/a b", "/a\rb", "/a\nb", "relative", ""] {
        let result = px_net::fetch::fetch(&key, path);
        assert!(
            result.is_err(),
            "request target {path:?} must be refused before connecting"
        );
    }
}
