//! Phase 3 gate item: the PSL version is asserted, and a stale PSL fails.
//!
//! # Why a stale list gets its own test
//!
//! Invariant 2 partitions by eTLD+1. A list that is out of date does not
//! degrade privacy gently — a suffix added upstream and missing here means two
//! sites this browser believes are one, sharing cookies and storage. Nothing
//! observable breaks. There is no error, no warning, and no symptom a user
//! could report.
//!
//! That is the entire argument for asserting freshness mechanically: it is the
//! class of defect that cannot be found by using the product.

use std::path::{Path, PathBuf};

use px_net::psl::PublicSuffixList;

fn data_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("data")
}

/// Read one of the data files.
///
/// Returns the error rather than panicking inside a helper: clippy's
/// `allow-expect-in-tests` covers `#[test]` functions and not the helpers they
/// call, and the workspace denies `expect` because this crate parses hostile
/// bytes. Each test does its own `expect`, which also puts the failure message
/// where the failure is.
fn read_data(name: &str) -> std::io::Result<String> {
    std::fs::read_to_string(data_dir().join(name))
}

/// Read a `key = "value"` line out of the version file text.
fn version_field(text: &str, name: &str) -> String {
    text.lines()
        .find_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix(name)?.trim_start().strip_prefix('=')?;
            Some(rest.trim().trim_matches('"').to_owned())
        })
        .unwrap_or_default()
}

/// The version file must describe the list that is actually present.
///
/// This is the assertion §9 Phase 3 asks for. It is not a checksum for its own
/// sake: the version file is the only thing that makes "stale" a question with
/// an answer, and a version that does not match the data it names is worse than
/// no version at all, because it reads as though someone checked.
#[test]
fn psl_version_matches_the_list_on_disk() {
    let version = read_data("public_suffix_list.version").expect("the version file must exist");
    let list = read_data("public_suffix_list.dat").expect("the list must exist");
    let recorded = version_field(&version, "sha256");
    assert!(
        !recorded.is_empty(),
        "the version file records no sha256; staleness cannot be assessed"
    );

    let actual = sha256_hex(list.as_bytes());
    assert_eq!(
        actual, recorded,
        "data/public_suffix_list.dat does not match the sha256 in its version \
         file. Either the list was edited by hand, or it was refreshed without \
         ci/update-psl.sh rewriting both files together."
    );
}

#[test]
fn psl_version_records_where_the_list_came_from() {
    let version = read_data("public_suffix_list.version").expect("the version file must exist");
    assert_eq!(
        version_field(&version, "source"),
        "https://publicsuffix.org/list/public_suffix_list.dat",
        "the upstream URL is part of the version: a list from anywhere else is \
         not the list this project claims to ship"
    );
    assert!(
        !version_field(&version, "fetched").is_empty(),
        "the fetch date is what makes age visible"
    );
}

/// The stale-PSL test §9 Phase 3 requires: a list that is out of date must
/// **fail**, not warn.
///
/// Staleness is defined as the recorded fetch date being older than a year.
/// Any threshold is a judgement; a year is chosen because the list changes
/// continuously and a year-old list has certainly missed suffixes, while a
/// shorter window would turn an ordinary clone into a red gate for a project
/// that is nowhere near shipping.
#[test]
fn psl_version_is_not_stale() {
    let version = read_data("public_suffix_list.version").expect("the version file must exist");
    let fetched = version_field(&version, "fetched");
    assert!(!fetched.is_empty(), "no fetch date recorded");

    let year: i32 = fetched
        .split('-')
        .next()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    assert!(year >= 2016, "unparseable fetch date: {fetched:?}");

    // Against the clock, not a constant. A hardcoded "current year" is a
    // second thing that goes stale, and it goes stale silently: the check
    // would keep passing for a list that is five years old because nobody
    // edited the constant. An average-length year is close enough to answer
    // "older than a year" and needs no date library.
    let seconds_since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let now_year: i32 = 1970 + (seconds_since_epoch / 31_556_952) as i32;
    assert!(
        now_year >= 2026,
        "the system clock reports {now_year}, which cannot be right"
    );
    assert!(
        now_year - year <= 1,
        "the PSL was fetched in {year} and is more than a year old. \
         Partition boundaries are drawn from it; refresh with ci/update-psl.sh."
    );
}

/// A list that fails to parse must be refused, not accepted as empty.
///
/// An empty list makes every host its own registrable domain, which merges
/// every subdomain of every site into a single partition. That is failing
/// open, silently, in the one direction that matters.
#[test]
fn psl_version_an_empty_list_is_refused_rather_than_accepted() {
    assert!(
        PublicSuffixList::parse("// only a comment\n\n").is_err(),
        "a list with no rules must be an error; accepting it would widen every \
         partition boundary at once"
    );
}

/// The real list must answer the cases partitioning depends on.
#[test]
fn psl_version_the_shipped_list_draws_the_boundaries_correctly() {
    let list = read_data("public_suffix_list.dat").expect("the list must exist");
    let psl = PublicSuffixList::parse(&list).expect("the shipped list must parse");
    assert!(
        psl.len() > 6000,
        "the shipped list has only {} rules, which is not the real list",
        psl.len()
    );

    // Ordinary registry suffixes.
    assert_eq!(
        psl.registrable_domain("www.example.com").as_deref(),
        Some("example.com")
    );
    assert_eq!(
        psl.registrable_domain("a.b.c.example.co.uk").as_deref(),
        Some("example.co.uk"),
        "co.uk is a two-label suffix; treating uk as the suffix would merge \
         every .co.uk site into one partition"
    );

    // The private section, which is the case this list exists for: these are
    // different people, and a list without github.io would let them read each
    // other's cookies.
    assert_eq!(
        psl.registrable_domain("alice.github.io").as_deref(),
        Some("alice.github.io")
    );
    assert_ne!(
        psl.registrable_domain("alice.github.io"),
        psl.registrable_domain("bob.github.io"),
        "two users' pages must not share a partition"
    );

    // A public suffix by itself has nothing registrable below it.
    assert_eq!(psl.registrable_domain("com"), None);
    assert_eq!(psl.registrable_domain("co.uk"), None);

    // IP literals are not partitioned by name.
    assert_eq!(psl.registrable_domain("127.0.0.1"), None);
    assert_eq!(psl.registrable_domain("::1"), None);

    // Malformed input yields None rather than a guess.
    assert_eq!(psl.registrable_domain(""), None);
    assert_eq!(psl.registrable_domain("."), None);
    assert_eq!(psl.registrable_domain("a..b"), None);
}

/// Wildcard and exception rules, which are where implementations go wrong.
#[test]
fn psl_version_wildcards_and_exceptions_follow_the_algorithm() {
    let psl = PublicSuffixList::parse("com\n*.ck\n!www.ck\n").expect("valid list");

    // A plain rule.
    assert_eq!(
        psl.registrable_domain("a.example.com").as_deref(),
        Some("example.com")
    );

    // `*.ck` makes `foo.ck` a suffix, so the registrable domain needs a third
    // label.
    assert_eq!(
        psl.registrable_domain("bar.foo.ck").as_deref(),
        Some("bar.foo.ck")
    );
    assert_eq!(psl.registrable_domain("foo.ck"), None);

    // `!www.ck` removes the wildcard's match, so `www` is registrable.
    assert_eq!(psl.registrable_domain("www.ck").as_deref(), Some("www.ck"));

    // An unknown TLD is treated as `*`, giving two labels — the narrow
    // direction, more partitions rather than fewer.
    assert_eq!(
        psl.registrable_domain("a.b.unknowntld").as_deref(),
        Some("b.unknowntld")
    );
}

/// Minimal SHA-256, so the freshness assertion does not need a dependency.
///
/// FIPS 180-4. Written here rather than taken because this crate's whole
/// dependency budget is spent on rustls, and a hash used only to compare a
/// file against a recorded digest is not where a supply-chain risk is worth
/// taking.
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
