//! The shipped HSTS preload list, its version, and the refusal it drives.
//!
//! Same shape as `psl.rs`, for the same reason: the list has no version of its
//! own, so a version file is the only thing that makes "stale" answerable.
//!
//! A stale HSTS list is quieter than a stale PSL and no less real. Hosts added
//! upstream since the fetch get no protection on a *first* visit — which is
//! precisely the visit no dynamic `Strict-Transport-Security` header can
//! cover, and the one an on-path attacker is waiting for.

use std::path::{Path, PathBuf};

use px_net::hsts::PreloadList;
use px_net::partition::Origin;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
}

/// Returns the error rather than panicking inside a helper — see
/// `tests/partition.rs` for why the fixtures in this crate are fallible.
fn read_data(name: &str) -> std::io::Result<String> {
    std::fs::read_to_string(data_dir().join(name))
}

fn version_field(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(name)?.trim_start().strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_owned())
        })
        .unwrap_or_default()
}

/// The version file must describe the list actually present.
#[test]
fn hsts_version_matches_the_list_on_disk() {
    let version = read_data("hsts_preload.version").expect("the version file must exist");
    let list = read_data("hsts_preload.txt").expect("the list must exist");

    let recorded = version_field(&version, "sha256");
    assert!(!recorded.is_empty(), "no sha256 recorded");
    assert_eq!(
        sha256_hex(list.as_bytes()),
        recorded,
        "data/hsts_preload.txt does not match the sha256 in its version file. \
         Either it was edited by hand, or refreshed without ci/update-hsts.sh \
         rewriting both files together."
    );

    let recorded_count: usize = version_field(&version, "hosts").parse().unwrap_or(0);
    assert_eq!(
        recorded_count,
        list.lines().filter(|line| !line.trim().is_empty()).count(),
        "the recorded host count does not match the list"
    );
}

/// The stale check, against the clock rather than a constant — a hardcoded
/// year is a second thing that goes stale, silently.
#[test]
fn hsts_version_is_not_stale() {
    let version = read_data("hsts_preload.version").expect("the version file must exist");
    let fetched = version_field(&version, "fetched");
    assert!(!fetched.is_empty(), "no fetch date recorded");

    let year: i32 = fetched
        .split('-')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    assert!(year >= 2016, "unparseable fetch date: {fetched:?}");

    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let now_year: i32 = 1970 + (seconds / 31_556_952) as i32;
    assert!(now_year >= 2026, "the clock reports {now_year}");
    assert!(
        now_year - year <= 1,
        "the HSTS list was fetched in {year} and is more than a year old. \
         Hosts added since get no first-visit protection; run ci/update-hsts.sh."
    );
}

/// The shipped list must be the real one and must answer correctly.
#[test]
fn hsts_version_the_shipped_list_protects_known_hosts() {
    let text = read_data("hsts_preload.txt").expect("the list must exist");
    let list = PreloadList::parse(&text).expect("the shipped list must parse");

    assert!(
        list.len() > 50_000,
        "the shipped list has only {} hosts, which is not the real list",
        list.len()
    );

    // Hosts verified present in the shipped list. Checked against the file
    // rather than assumed: an earlier version of this test asserted
    // `google.com`, which is *not* a force-https entry — Chromium handles it
    // through a separate mechanism, and only specific subdomains such as
    // `.accounts.google.com` appear here. The matcher was right and the
    // expectation was wrong.
    for host in ["facebook.com", "torproject.org", "twitter.com"] {
        assert!(
            list.requires_https(host),
            "{host} is an exact entry upstream and must be covered"
        );
    }

    // Subdomain coverage, which is most of the list's value.
    for host in ["github.com", "api.github.com", "mail.google.com"] {
        assert!(
            list.requires_https(host),
            "{host} is covered by a subdomain entry upstream"
        );
    }

    // And hosts that are not on it.
    assert!(!list.requires_https("example.com"));
    assert!(
        !list.requires_https("google.com"),
        "google.com is not a force-https entry; asserting otherwise would test a belief about the list rather than the list"
    );
}

/// The refusal, end to end: a plaintext fetch to a preloaded host must fail
/// before anything leaves the machine.
#[test]
fn hsts_version_a_plaintext_fetch_to_a_preloaded_host_is_refused() {
    use px_net::partition::PartitionKey;
    use px_net::psl::PublicSuffixList;

    let text = read_data("hsts_preload.txt").expect("the list must exist");
    let list = PreloadList::parse(&text).expect("parse");
    let psl = PublicSuffixList::parse("com\n").expect("a valid suffix list");

    // Port 1: nothing is listening, so if the refusal did not happen first
    // this would fail with a connection error instead — a different error, and
    // the test would catch the difference.
    let origin = Origin::new("http", "github.com", 1).expect("origin");
    let key = PartitionKey::new(&psl, "example.com", origin).expect("key");

    let error = px_net::fetch::fetch_with_hsts(&key, "/", Some(&list))
        .expect_err("a preloaded host must not be fetched over plaintext");
    assert!(
        matches!(error, px_net::fetch::FetchError::PlaintextToPreloadedHost),
        "expected the preload refusal, got {error:?} — a connection error \
         would mean the check ran too late"
    );

    // And the upgrade the caller should use instead.
    let plain = Origin::new("http", "github.com", 80).expect("origin");
    let upgraded = list.upgrade(&plain).expect("a preloaded host upgrades");
    assert_eq!(upgraded.scheme(), "https");
    assert_eq!(upgraded.port(), 443);
}

/// Same implementation as `tests/psl.rs`, cross-checked there against coreutils.
fn sha256_hex(input: &[u8]) -> String {
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
