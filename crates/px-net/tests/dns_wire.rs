//! DNS wire format (RFC 1035) and the partitioned resolver cache, through the
//! public API only. Messages are assembled byte by byte so the expected
//! structure comes from the RFC, not from `build_query`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use px_net::dns::{Address, DnsError, MAX_MESSAGE_BYTES, RecordType, build_query, parse_response};
use px_net::partition::{Origin, PartitionKey};
use px_net::psl::PublicSuffixList;
use px_net::resolver::{Cache, ResolveError, parse_providers, resolve_system};

/// Header for a response: id 0, QR+RD+RA, one question, `answers` answers.
fn header(rcode: u8, questions: u16, answers: u16) -> Vec<u8> {
    let mut out = vec![0, 0, 0x81, 0x80 | rcode];
    out.extend_from_slice(&questions.to_be_bytes());
    out.extend_from_slice(&answers.to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out
}

fn wire_name(name: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for label in name.split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    out
}

fn question(name: &str, qtype: u16) -> Vec<u8> {
    let mut out = wire_name(name);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out
}

/// One resource record whose owner name is a pointer to offset 12.
fn record(rtype: u16, rdata: &[u8]) -> Vec<u8> {
    let mut out = vec![0xc0, 0x0c];
    out.extend_from_slice(&rtype.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes());
    out.extend_from_slice(&60u32.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
    out
}

fn response(name: &str, qtype: u16, records: &[Vec<u8>]) -> Vec<u8> {
    let mut out = header(0, 1, records.len() as u16);
    out.extend(question(name, qtype));
    for r in records {
        out.extend_from_slice(r);
    }
    out
}

// ---- build_query ---------------------------------------------------------

#[test]
fn net_dns_query_matches_the_rfc_1035_layout_byte_for_byte() {
    let query = build_query("www.example.com", RecordType::A).expect("query");
    let mut expected = vec![
        0x00, 0x00, // id
        0x01, 0x00, // RD
        0x00, 0x01, // QDCOUNT
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // AN, NS, AR
    ];
    expected.extend_from_slice(b"\x03www\x07example\x03com\x00");
    expected.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // A, IN
    assert_eq!(query, expected);
}

#[test]
fn net_dns_aaaa_query_uses_type_28() {
    let query = build_query("a.b", RecordType::Aaaa).expect("query");
    assert_eq!(
        query.get(query.len() - 4..),
        Some(&[0x00, 28, 0x00, 0x01][..])
    );
}

#[test]
fn net_dns_query_ignores_one_trailing_dot_but_refuses_empty_labels() {
    let with_dot = build_query("example.com.", RecordType::A).expect("query");
    let without = build_query("example.com", RecordType::A).expect("query");
    assert_eq!(with_dot, without);

    for bad in ["", ".", "a..b", ".a"] {
        assert!(
            matches!(build_query(bad, RecordType::A), Err(DnsError::Malformed(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn net_dns_label_limit_is_63_and_name_limit_is_255_encoded_bytes() {
    let l63 = "a".repeat(63);
    assert!(build_query(&format!("{l63}.com"), RecordType::A).is_ok());
    assert_eq!(
        build_query(&format!("{}.com", "a".repeat(64)), RecordType::A),
        Err(DnsError::TooLarge("label"))
    );

    // Four 63-byte labels encode to 4*64 + 1 = 257 > 255.
    let four = vec![l63.as_str(); 4].join(".");
    assert_eq!(
        build_query(&four, RecordType::A),
        Err(DnsError::TooLarge("name"))
    );
    // Three 63-byte labels plus a 61-byte label encode to 3*64 + 62 + 1 = 255.
    let l61 = "b".repeat(61);
    let exact = format!("{l63}.{l63}.{l63}.{l61}");
    assert!(build_query(&exact, RecordType::A).is_ok());
    let l62 = "b".repeat(62);
    let over = format!("{l63}.{l63}.{l63}.{l62}");
    assert_eq!(
        build_query(&over, RecordType::A),
        Err(DnsError::TooLarge("name"))
    );
}

// ---- parse_response ------------------------------------------------------

#[test]
fn net_dns_parses_multiple_a_and_aaaa_answers_in_order() {
    let v6: Ipv6Addr = "2001:db8::1".parse().expect("v6");
    let msg = response(
        "example.com",
        28,
        &[record(28, &v6.octets()), record(28, &[0u8; 16])],
    );
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::Aaaa),
        Ok(vec![Address::V6(v6), Address::V6(Ipv6Addr::UNSPECIFIED)])
    );

    let msg = response(
        "example.com",
        1,
        &[record(1, &[192, 0, 2, 1]), record(1, &[198, 51, 100, 7])],
    );
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Ok(vec![
            Address::V4(Ipv4Addr::new(192, 0, 2, 1)),
            Address::V4(Ipv4Addr::new(198, 51, 100, 7)),
        ])
    );
}

#[test]
fn net_dns_a_cname_in_the_answer_chain_is_skipped_not_fatal() {
    // CNAME rdata is a name; here simply wire_name("alias.example.com").
    let cname = record(5, &wire_name("alias.example.com"));
    let msg = response("example.com", 1, &[cname, record(1, &[10, 0, 0, 9])]);
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Ok(vec![Address::V4(Ipv4Addr::new(10, 0, 0, 9))])
    );
}

#[test]
fn net_dns_records_with_the_wrong_rdlength_are_ignored() {
    // An A record of 5 bytes and an AAAA record of 4 bytes carry no address.
    let msg = response(
        "example.com",
        1,
        &[record(1, &[1, 2, 3, 4, 5]), record(28, &[1, 2, 3, 4])],
    );
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Ok(vec![])
    );
}

#[test]
fn net_dns_question_name_comparison_ignores_case_and_a_trailing_dot() {
    let msg = response("ExAmPlE.CoM", 1, &[record(1, &[1, 1, 1, 1])]);
    assert!(parse_response(&msg, "example.com.", RecordType::A).is_ok());
    assert_eq!(
        parse_response(&msg, "example.org", RecordType::A),
        Err(DnsError::WrongQuestion)
    );
    // A subdomain or parent of the asked name is a different question.
    assert_eq!(
        parse_response(&msg, "www.example.com", RecordType::A),
        Err(DnsError::WrongQuestion)
    );
    assert_eq!(
        parse_response(&msg, "com", RecordType::A),
        Err(DnsError::WrongQuestion)
    );
}

#[test]
fn net_dns_question_type_must_match_the_type_asked() {
    let msg = response("example.com", 28, &[]);
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::WrongQuestion)
    );
}

#[test]
fn net_dns_every_nonzero_rcode_is_a_failure() {
    for rcode in 1..=15u8 {
        let mut msg = header(rcode, 1, 0);
        msg.extend(question("example.com", 1));
        assert_eq!(
            parse_response(&msg, "example.com", RecordType::A),
            Err(DnsError::ServerFailure(rcode)),
            "rcode {rcode}"
        );
    }
}

#[test]
fn net_dns_exactly_one_question_is_required() {
    for questions in [0u16, 2, 0xffff] {
        let mut msg = header(0, questions, 0);
        msg.extend(question("example.com", 1));
        assert!(
            matches!(
                parse_response(&msg, "example.com", RecordType::A),
                Err(DnsError::Malformed(_))
            ),
            "{questions} questions"
        );
    }
}

#[test]
fn net_dns_answer_count_is_bounded_before_any_record_is_read() {
    // Claims 65 answers (limit 64) but carries none: refused on the claim.
    let mut msg = header(0, 1, 65);
    msg.extend(question("example.com", 1));
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::TooLarge("answer count"))
    );
    // Claims 64 but carries none: truncated, not a panic or an empty success.
    let mut msg = header(0, 1, 64);
    msg.extend(question("example.com", 1));
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::Truncated)
    );
}

#[test]
fn net_dns_oversized_messages_are_refused_up_front() {
    let msg = vec![0u8; MAX_MESSAGE_BYTES + 1];
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::TooLarge("message"))
    );
}

#[test]
fn net_dns_every_truncation_of_a_valid_response_is_an_error_not_a_panic() {
    let msg = response("example.com", 1, &[record(1, &[192, 0, 2, 1])]);
    assert!(parse_response(&msg, "example.com", RecordType::A).is_ok());
    for cut in 0..msg.len() {
        let result = parse_response(
            msg.get(..cut).unwrap_or_default(),
            "example.com",
            RecordType::A,
        );
        assert!(result.is_err(), "a message cut at {cut} must not parse");
    }
}

#[test]
fn net_dns_rdlength_running_past_the_message_is_truncated() {
    let mut rec = record(1, &[1, 2, 3, 4]);
    // Claim 200 bytes of rdata.
    let len_at = rec.len() - 4 - 2;
    if let Some(slot) = rec.get_mut(len_at + 1) {
        *slot = 200;
    }
    let msg = response("example.com", 1, &[rec]);
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::Truncated)
    );
}

#[test]
fn net_dns_reserved_label_bits_are_refused() {
    // 0x40 and 0x80 prefixes are reserved (RFC 1035 4.1.4 / RFC 6891).
    for first in [0x40u8, 0x80] {
        let mut msg = header(0, 1, 0);
        msg.push(first);
        msg.extend_from_slice(b"abc\x00\x00\x01\x00\x01");
        assert!(
            matches!(
                parse_response(&msg, "example.com", RecordType::A),
                Err(DnsError::Malformed(_))
            ),
            "{first:#x}"
        );
    }
}

#[test]
fn net_dns_a_pointer_in_the_question_cannot_point_at_itself_or_forwards() {
    // The question name is a pointer to offset 12, which is itself.
    let mut msg = header(0, 1, 0);
    msg.extend_from_slice(&[0xc0, 0x0c, 0, 1, 0, 1]);
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Err(DnsError::BadPointer)
    );
}

#[test]
fn net_dns_answer_names_may_use_a_compression_pointer_to_the_question() {
    // This is what `record` does; additionally verify a label followed by a
    // pointer (www + ->example.com) is accepted as an owner name.
    let mut rec = vec![3, b'w', b'w', b'w', 0xc0, 0x0c];
    rec.extend_from_slice(&1u16.to_be_bytes());
    rec.extend_from_slice(&1u16.to_be_bytes());
    rec.extend_from_slice(&60u32.to_be_bytes());
    rec.extend_from_slice(&4u16.to_be_bytes());
    rec.extend_from_slice(&[9, 9, 9, 9]);
    let msg = response("example.com", 1, &[rec]);
    assert_eq!(
        parse_response(&msg, "example.com", RecordType::A),
        Ok(vec![Address::V4(Ipv4Addr::new(9, 9, 9, 9))])
    );
}

// ---- resolver cache ------------------------------------------------------

fn psl() -> Option<PublicSuffixList> {
    PublicSuffixList::parse("com\nnet\n").ok()
}

fn key(psl: &PublicSuffixList, top: &str, dest: &str) -> Option<PartitionKey> {
    PartitionKey::new(psl, top, Origin::new("https", dest, 443)?)
}

fn ip(last: u8) -> IpAddr {
    IpAddr::V4(Ipv4Addr::new(192, 0, 2, last))
}

#[test]
fn net_resolver_cache_is_partitioned() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "cdn.net").expect("key");
    let b = key(&psl, "b.com", "cdn.net").expect("key");
    let mut cache = Cache::new();
    cache.put(&a, "cdn.net", vec![ip(1)]);
    assert_eq!(cache.get(&a, "cdn.net"), Some(&[ip(1)][..]));
    assert_eq!(
        cache.get(&b, "cdn.net"),
        None,
        "a hit would tell b.com the user has been to a.com"
    );
}

#[test]
fn net_resolver_cache_names_are_case_insensitive() {
    let psl = psl().expect("psl");
    let k = key(&psl, "a.com", "a.com").expect("key");
    let mut cache = Cache::new();
    cache.put(&k, "Example.COM", vec![ip(2)]);
    assert_eq!(cache.get(&k, "example.com"), Some(&[ip(2)][..]));
    cache.put(&k, "EXAMPLE.com", vec![ip(3)]);
    assert_eq!(cache.len(), 1, "same name, one entry");
    assert_eq!(cache.get(&k, "example.COM"), Some(&[ip(3)][..]));
}

#[test]
fn net_resolver_cache_forget_is_scoped_to_one_partition() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "a.com").expect("key");
    let b = key(&psl, "b.com", "b.com").expect("key");
    let mut cache = Cache::new();
    cache.put(&a, "x.com", vec![ip(1)]);
    cache.put(&b, "x.com", vec![ip(1)]);
    cache.forget(&a);
    assert_eq!(cache.get(&a, "x.com"), None);
    assert!(cache.get(&b, "x.com").is_some());
    assert_eq!(cache.len(), 1);
    cache.forget(&b);
    assert!(cache.is_empty());
}

#[test]
fn net_resolver_cache_holds_at_most_64_names_per_partition() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "a.com").expect("key");
    let b = key(&psl, "b.com", "b.com").expect("key");
    let mut cache = Cache::new();
    for i in 0..200u8 {
        cache.put(&a, &format!("h{i}.com"), vec![ip(i)]);
    }
    assert_eq!(cache.len(), 64);
    // The cap is per partition: b is unaffected by a being full.
    cache.put(&b, "h.com", vec![ip(1)]);
    assert_eq!(cache.len(), 65);
}

/// Refreshing a name that is already cached is not an addition, so a full
/// partition must still accept the new answer (a stale address would
/// otherwise be served until its TTL ran out).
#[test]
fn net_resolver_cache_refreshing_an_existing_name_works_when_full() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "a.com").expect("key");
    let mut cache = Cache::new();
    for i in 0..64u8 {
        cache.put(&a, &format!("h{i}.com"), vec![ip(1)]);
    }
    cache.put(&a, "h10.com", vec![ip(99)]);
    assert_eq!(cache.get(&a, "h10.com"), Some(&[ip(99)][..]));
    assert_eq!(cache.len(), 64);
}

#[test]
fn net_resolver_cache_miss_on_an_unknown_name() {
    let psl = psl().expect("psl");
    let a = key(&psl, "a.com", "a.com").expect("key");
    let cache = Cache::new();
    assert_eq!(cache.get(&a, "nothing.com"), None);
}

// ---- providers and system resolution -------------------------------------

#[test]
fn net_resolver_providers_reject_each_malformed_shape() {
    let text = "\
Good\thost.example/dns-query\tNL\tpolicy
NoPath\thost.example\tNL\tpolicy
ShortFields\thost.example/q\tNL
\thost.example/q\tNL\tnameless
Nameless host\t/dns-query\tNL\tno host
   \t\t\t
# \tcommented\tout\tline
Last\th2.example/a/b\tUS\tp
";
    let providers = parse_providers(text);
    let names: Vec<&str> = providers.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["Good", "Last"]);
    assert_eq!(
        providers.get(1).map(|p| p.host.as_str()),
        Some("h2.example")
    );
    assert_eq!(providers.get(1).map(|p| p.path.as_str()), Some("/a/b"));
    assert_eq!(
        providers.get(1).map(|p| p.jurisdiction.as_str()),
        Some("US")
    );
}

#[test]
fn net_resolver_providers_empty_input_gives_empty_list() {
    assert!(parse_providers("").is_empty());
    assert!(parse_providers("\n\n# only comments\n").is_empty());
}

#[test]
fn net_resolver_system_lookup_refuses_unusable_names_without_touching_the_network() {
    assert!(matches!(
        resolve_system("", 443),
        Err(ResolveError::BadName(_))
    ));
    assert!(matches!(
        resolve_system("b\u{fc}cher.example", 443),
        Err(ResolveError::BadName(_))
    ));
}
