//! DNS messages, for DoH (ADR 014).
//!
//! # This is a trust boundary
//!
//! A DNS response is bytes from a resolver, and a resolver is a third party —
//! the whole reason ADR 014 makes the choice of one explicit. A hostile or
//! compromised one chooses these bytes exactly, so the parser is held to the
//! same rules as the HTTP one: no panic, no unbounded allocation, no trust in
//! a declared length.
//!
//! # Compression pointers, which are the interesting part
//!
//! DNS names are compressed: a name can end in a pointer to an earlier offset
//! in the message, so `www.example.com` and `mail.example.com` share the
//! `example.com` suffix. Two pointers can point at each other, or one at
//! itself, and a parser that follows them naively loops forever on a response
//! an attacker can write in twenty bytes. That is the classic DNS
//! denial-of-service and it is what [`MAX_JUMPS`] exists for.
//!
//! Pointers are also only allowed to point *backwards*. A forward pointer
//! cannot be part of a well-formed message, and permitting one is how a parser
//! is made to walk memory it has not validated.
//!
//! # Scope
//!
//! Enough to ask "what addresses does this name have" over DoH, and no more.
//! No DNSSEC — validating it needs a signing-key chain and a clock this
//! project does not yet have, and pretending otherwise would be worse than
//! being clear that DoH here buys confidentiality from the network, not
//! authenticity from the resolver.

use std::net::{Ipv4Addr, Ipv6Addr};

/// Longest DNS message accepted, in bytes.
///
/// DoH has no 512-byte UDP limit, but a bound is still needed: this one is a
/// response size, not a protocol constant.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;

/// Longest encoded name, in bytes. RFC 1035.
const MAX_NAME_BYTES: usize = 255;

/// Longest single label, in bytes. RFC 1035.
const MAX_LABEL_BYTES: usize = 63;

/// How many compression pointers one name may follow.
///
/// The bound that makes a pointer loop terminate. Any small number works;
/// this one is larger than any legitimate message needs and small enough that
/// a malicious one costs nothing.
const MAX_JUMPS: usize = 16;

/// Most records read from one message.
const MAX_RECORDS: usize = 64;

/// Record types this resolver asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordType {
    /// IPv4 address.
    A,
    /// IPv6 address.
    Aaaa,
}

impl RecordType {
    fn code(self) -> u16 {
        match self {
            Self::A => 1,
            Self::Aaaa => 28,
        }
    }
}

/// What can be wrong with a DNS message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DnsError {
    /// The message ended before the structure did.
    Truncated,
    /// A name, label or record broke a documented bound.
    TooLarge(&'static str),
    /// A compression pointer looped, or pointed forwards.
    BadPointer,
    /// The message was structurally invalid.
    Malformed(&'static str),
    /// The response did not answer the question that was asked.
    ///
    /// Checked rather than assumed: accepting an answer for a name nobody
    /// asked about is how a resolver poisons a cache.
    WrongQuestion,
    /// The server reported an error code.
    ServerFailure(u8),
}

impl std::fmt::Display for DnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "the DNS message ended mid-structure"),
            Self::TooLarge(what) => write!(f, "{what} exceeded its limit"),
            Self::BadPointer => write!(f, "a compression pointer looped or pointed forwards"),
            Self::Malformed(why) => write!(f, "malformed DNS message: {why}"),
            Self::WrongQuestion => write!(f, "the response answers a different question"),
            Self::ServerFailure(code) => write!(f, "the resolver returned rcode {code}"),
        }
    }
}

impl std::error::Error for DnsError {}

/// An address a name resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Address {
    /// IPv4.
    V4(Ipv4Addr),
    /// IPv6.
    V6(Ipv6Addr),
}

/// Build a query for `name`.
///
/// The transaction id is fixed at zero, deliberately. Over DoH the response is
/// carried by the HTTPS request that asked for it, so there is no off-path
/// spoofing window for an id to defend against — and RFC 8484 recommends zero
/// precisely so the message is cacheable. Over UDP this would be a serious
/// bug; there is no UDP path here.
pub fn build_query(name: &str, record: RecordType) -> Result<Vec<u8>, DnsError> {
    let mut out = Vec::with_capacity(32);

    out.extend_from_slice(&0u16.to_be_bytes()); // id
    out.extend_from_slice(&0x0100u16.to_be_bytes()); // recursion desired
    out.extend_from_slice(&1u16.to_be_bytes()); // one question
    out.extend_from_slice(&0u16.to_be_bytes()); // no answers
    out.extend_from_slice(&0u16.to_be_bytes()); // no authority
    out.extend_from_slice(&0u16.to_be_bytes()); // no additional

    encode_name(name, &mut out)?;
    out.extend_from_slice(&record.code().to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // class IN
    Ok(out)
}

/// Write a name in wire form: length-prefixed labels, then a zero.
fn encode_name(name: &str, out: &mut Vec<u8>) -> Result<(), DnsError> {
    let name = name.trim_end_matches('.');
    if name.is_empty() {
        return Err(DnsError::Malformed("empty name"));
    }
    if !name.is_ascii() {
        // A Unicode name means the IDNA step was skipped. Refusing fails
        // closed; encoding it would put a name on the wire that does not mean
        // what the caller thinks.
        return Err(DnsError::Malformed("name is not ASCII"));
    }

    let mut encoded = 1usize;
    for label in name.split('.') {
        if label.is_empty() {
            return Err(DnsError::Malformed("empty label"));
        }
        if label.len() > MAX_LABEL_BYTES {
            return Err(DnsError::TooLarge("label"));
        }
        encoded = encoded
            .checked_add(label.len().saturating_add(1))
            .ok_or(DnsError::TooLarge("name"))?;
        if encoded > MAX_NAME_BYTES {
            return Err(DnsError::TooLarge("name"));
        }
        let length = u8::try_from(label.len()).map_err(|_| DnsError::TooLarge("label"))?;
        out.push(length);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
    Ok(())
}

/// Parse a response, returning the addresses it carries for `expected_name`.
pub fn parse_response(
    message: &[u8],
    expected_name: &str,
    expected_type: RecordType,
) -> Result<Vec<Address>, DnsError> {
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(DnsError::TooLarge("message"));
    }
    let header = message.get(..12).ok_or(DnsError::Truncated)?;

    let flags = be16(header, 2)?;
    let rcode = (flags & 0x000f) as u8;
    if rcode != 0 {
        return Err(DnsError::ServerFailure(rcode));
    }

    let questions = be16(header, 4)?;
    let answers = be16(header, 6)?;
    if questions != 1 {
        return Err(DnsError::Malformed("expected exactly one question"));
    }
    if usize::from(answers) > MAX_RECORDS {
        return Err(DnsError::TooLarge("answer count"));
    }

    // The question section, which is echoed back. Verifying it is not
    // ceremony: accepting an answer for a name nobody asked about is how a
    // resolver poisons a cache.
    let mut offset = 12usize;
    let (question_name, next) = read_name(message, offset)?;
    offset = next;
    if !question_name.eq_ignore_ascii_case(expected_name.trim_end_matches('.')) {
        return Err(DnsError::WrongQuestion);
    }
    let qtype = be16(message, offset)?;
    if qtype != expected_type.code() {
        return Err(DnsError::WrongQuestion);
    }
    offset = offset.checked_add(4).ok_or(DnsError::Truncated)?;

    let mut out = Vec::new();
    for _ in 0..answers {
        let (_name, next) = read_name(message, offset)?;
        offset = next;

        let rtype = be16(message, offset)?;
        let rdlength = usize::from(be16(
            message,
            offset.checked_add(8).ok_or(DnsError::Truncated)?,
        )?);
        offset = offset.checked_add(10).ok_or(DnsError::Truncated)?;

        let data = message
            .get(offset..offset.checked_add(rdlength).ok_or(DnsError::Truncated)?)
            .ok_or(DnsError::Truncated)?;

        // A record whose type this does not handle is skipped rather than
        // rejected: a CNAME in the answer chain is ordinary, and refusing the
        // whole response because of one would break most real lookups.
        if rtype == RecordType::A.code() && rdlength == 4 {
            out.push(Address::V4(Ipv4Addr::new(
                *data.first().unwrap_or(&0),
                *data.get(1).unwrap_or(&0),
                *data.get(2).unwrap_or(&0),
                *data.get(3).unwrap_or(&0),
            )));
        } else if rtype == RecordType::Aaaa.code() && rdlength == 16 {
            let mut octets = [0u8; 16];
            for (index, slot) in octets.iter_mut().enumerate() {
                *slot = *data.get(index).unwrap_or(&0);
            }
            out.push(Address::V6(Ipv6Addr::from(octets)));
        }

        offset = offset.checked_add(rdlength).ok_or(DnsError::Truncated)?;
    }
    Ok(out)
}

fn be16(bytes: &[u8], at: usize) -> Result<u16, DnsError> {
    let hi = *bytes.get(at).ok_or(DnsError::Truncated)?;
    let lo = *bytes
        .get(at.checked_add(1).ok_or(DnsError::Truncated)?)
        .ok_or(DnsError::Truncated)?;
    Ok(u16::from(hi) << 8 | u16::from(lo))
}

/// Read a possibly-compressed name, returning it and the offset just past the
/// name *in the original position* — not past whatever a pointer led to.
///
/// The two properties that matter:
///
/// - a bounded number of jumps, so a pointer loop terminates;
/// - pointers must go backwards, because a forward pointer cannot appear in a
///   well-formed message and allowing one is how a parser is walked forward
///   through bytes it has not checked.
fn read_name(message: &[u8], start: usize) -> Result<(String, usize), DnsError> {
    let mut labels: Vec<String> = Vec::new();
    let mut offset = start;
    let mut jumps = 0usize;
    let mut after: Option<usize> = None;
    let mut encoded = 0usize;

    loop {
        let length = *message.get(offset).ok_or(DnsError::Truncated)?;

        // Top two bits set marks a pointer; the remaining 14 are the offset.
        if length & 0xc0 == 0xc0 {
            let lo = *message
                .get(offset.checked_add(1).ok_or(DnsError::Truncated)?)
                .ok_or(DnsError::Truncated)?;
            let target = (usize::from(length & 0x3f) << 8) | usize::from(lo);

            if target >= offset {
                return Err(DnsError::BadPointer);
            }
            jumps = jumps.checked_add(1).ok_or(DnsError::BadPointer)?;
            if jumps > MAX_JUMPS {
                return Err(DnsError::BadPointer);
            }
            if after.is_none() {
                after = Some(offset.checked_add(2).ok_or(DnsError::Truncated)?);
            }
            offset = target;
            continue;
        }

        if length & 0xc0 != 0 {
            return Err(DnsError::Malformed("reserved label bits set"));
        }

        let length = usize::from(length);
        if length == 0 {
            offset = offset.checked_add(1).ok_or(DnsError::Truncated)?;
            break;
        }
        if length > MAX_LABEL_BYTES {
            return Err(DnsError::TooLarge("label"));
        }

        encoded = encoded
            .checked_add(length.saturating_add(1))
            .ok_or(DnsError::TooLarge("name"))?;
        if encoded > MAX_NAME_BYTES {
            return Err(DnsError::TooLarge("name"));
        }

        let from = offset.checked_add(1).ok_or(DnsError::Truncated)?;
        let to = from.checked_add(length).ok_or(DnsError::Truncated)?;
        let label = message.get(from..to).ok_or(DnsError::Truncated)?;
        labels.push(String::from_utf8_lossy(label).into_owned());
        offset = to;
    }

    Ok((labels.join("."), after.unwrap_or(offset)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_round_trips_its_name() {
        let query = build_query("example.com", RecordType::A).expect("a valid query");
        let (name, _) = read_name(&query, 12).expect("the name is at offset 12");
        assert_eq!(name, "example.com");
    }

    #[test]
    fn a_name_that_is_too_long_is_refused() {
        let long = format!("{}.com", "a".repeat(64));
        assert_eq!(
            build_query(&long, RecordType::A),
            Err(DnsError::TooLarge("label"))
        );
        let many = vec!["ab"; 100].join(".");
        assert_eq!(
            build_query(&many, RecordType::A),
            Err(DnsError::TooLarge("name"))
        );
    }

    #[test]
    fn a_unicode_name_is_refused_rather_than_encoded() {
        assert!(build_query("exämple.com", RecordType::A).is_err());
    }

    /// The classic denial of service: a pointer to itself. A naive parser
    /// loops here forever on twenty bytes an attacker writes.
    #[test]
    fn a_self_referential_pointer_terminates() {
        // Offset 12 holds a pointer to offset 12.
        let mut message = vec![0u8; 12];
        message.extend_from_slice(&[0xc0, 0x0c]);
        assert_eq!(read_name(&message, 12), Err(DnsError::BadPointer));
    }

    #[test]
    fn a_forward_pointer_is_refused() {
        let mut message = vec![0u8; 12];
        // At offset 12, a pointer forwards to 20.
        message.extend_from_slice(&[0xc0, 0x14]);
        message.extend_from_slice(&[0u8; 8]);
        assert_eq!(read_name(&message, 12), Err(DnsError::BadPointer));
    }

    #[test]
    fn a_chain_of_pointers_is_bounded() {
        // A ladder of backwards pointers, each legal on its own.
        let mut message = vec![0u8; 4];
        for index in 0..64 {
            let target = 2 + index * 2;
            message.extend_from_slice(&[0xc0 | ((target >> 8) as u8 & 0x3f), target as u8]);
        }
        let start = message.len() - 2;
        assert_eq!(read_name(&message, start), Err(DnsError::BadPointer));
    }

    #[test]
    fn a_truncated_message_is_refused_rather_than_read_past() {
        assert_eq!(
            parse_response(&[], "example.com", RecordType::A),
            Err(DnsError::Truncated)
        );
        assert_eq!(
            parse_response(&[0u8; 5], "example.com", RecordType::A),
            Err(DnsError::Truncated)
        );
    }

    /// Accepting an answer for a name nobody asked about is how a resolver
    /// poisons a cache.
    #[test]
    fn a_response_answering_a_different_question_is_refused() {
        let mut message = build_query("evil.example", RecordType::A).expect("query");
        // Mark it as a response with one answer.
        if let Some(slot) = message.get_mut(2) {
            *slot = 0x81;
        }
        assert_eq!(
            parse_response(&message, "good.example", RecordType::A),
            Err(DnsError::WrongQuestion)
        );
    }

    #[test]
    fn a_server_failure_is_reported_rather_than_read_as_empty() {
        let mut message = build_query("example.com", RecordType::A).expect("query");
        if let Some(slot) = message.get_mut(3) {
            *slot = 0x03; // NXDOMAIN
        }
        assert_eq!(
            parse_response(&message, "example.com", RecordType::A),
            Err(DnsError::ServerFailure(3))
        );
    }

    #[test]
    fn an_a_record_is_read() {
        let mut message = build_query("example.com", RecordType::A).expect("query");
        if let Some(slot) = message.get_mut(2) {
            *slot = 0x81;
        }
        if let Some(slot) = message.get_mut(7) {
            *slot = 1; // one answer
        }
        // Answer: pointer to the question name, type A, class IN, ttl, 4 bytes.
        message.extend_from_slice(&[0xc0, 0x0c]);
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&300u32.to_be_bytes());
        message.extend_from_slice(&4u16.to_be_bytes());
        message.extend_from_slice(&[192, 0, 2, 1]);

        let addresses = parse_response(&message, "example.com", RecordType::A).expect("parsed");
        assert_eq!(addresses, vec![Address::V4(Ipv4Addr::new(192, 0, 2, 1))]);
    }
}
