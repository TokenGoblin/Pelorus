//! HTTP/1.1 response parsing, written here rather than taken (ADR 015).
//!
//! # This is a trust boundary
//!
//! Every byte parsed here was written by a server, which for our purposes is
//! an attacker: a page the user visited can be served by anyone, and a
//! compromised or hostile origin gets to choose exactly these bytes. The
//! module is held to the panic lints §4.3 specifies — no `unwrap`, no
//! `expect`, no indexing — because a panic here is a denial of service
//! reachable by any site.
//!
//! # Framing is the security property
//!
//! Almost every serious HTTP/1.1 vulnerability is a framing disagreement:
//! two parties reading the same bytes as different numbers of messages. The
//! classic is request smuggling, where a proxy and an origin disagree about
//! where a message ends and an attacker writes the difference. A browser is
//! not a proxy, so it does not smuggle requests — but it does sit downstream
//! of caches and CDNs, and a client that resolves an ambiguous framing
//! *differently from them* is the other half of a response-splitting bug.
//!
//! So this parser does not resolve ambiguity. It refuses it. Where the
//! specification says "prefer this one", the safer reading is that a message
//! which needs the tiebreak was not written by anyone honest:
//!
//! - `Content-Length` **and** `Transfer-Encoding` together — rejected, not
//!   resolved in favour of `Transfer-Encoding` as RFC 9112 §6.1 permits.
//! - Two `Content-Length` headers that disagree — rejected. Two that agree are
//!   also rejected, because nothing legitimate emits them.
//! - A `Content-Length` that is not purely digits — no `+`, no whitespace, no
//!   hex — rejected before it reaches an integer parse.
//! - `Transfer-Encoding` whose final coding is not `chunked` — rejected. This
//!   is the shape that makes a body's end undefined.
//!
//! # Bounds
//!
//! Every length that comes off the wire is checked against a limit before it
//! sizes anything. A declared chunk size is not an allocation.

use std::fmt;

/// Longest status line accepted, in bytes.
///
/// A status line is a version, a three-digit code and a reason phrase. Anything
/// approaching this is not one.
const MAX_STATUS_LINE: usize = 8 * 1024;

/// Longest single header line accepted, in bytes.
const MAX_HEADER_LINE: usize = 16 * 1024;

/// Most header fields accepted in one response.
///
/// A bound on count as well as on size: ten thousand one-byte headers cost
/// little on the wire and a great deal in a map.
const MAX_HEADERS: usize = 128;

/// Largest body this parser will accumulate, in bytes.
///
/// Not a protocol limit — a bound on what one response can make us hold. The
/// streaming path that replaces it belongs with the phase that has a consumer
/// for a partial body; until then, refusing is better than an allocation a
/// server chooses.
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Largest single chunk accepted in a chunked body, in bytes.
const MAX_CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// What can be wrong with a response.
///
/// Deliberately not one `Malformed`. When a fetch fails, the difference
/// between "the server framed this ambiguously" and "the header block is too
/// large" is the difference between a bug worth chasing and a limit worth
/// raising.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Http1Error {
    /// The status line was not a status line.
    BadStatusLine,
    /// A header field was malformed.
    BadHeader,
    /// The message framing was ambiguous, and ambiguity is refused rather than
    /// resolved. Carries which ambiguity, because they have different causes.
    AmbiguousFraming(Ambiguity),
    /// A declared length or count exceeded its bound.
    TooLarge {
        /// What was too large.
        what: &'static str,
    },
    /// The peer closed before the message was complete.
    Truncated,
}

/// The specific framing ambiguity a response contained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ambiguity {
    /// Both `Content-Length` and `Transfer-Encoding` were present.
    LengthAndTransferEncoding,
    /// More than one `Content-Length` header.
    RepeatedContentLength,
    /// A `Content-Length` that was not purely decimal digits.
    MalformedContentLength,
    /// A `Transfer-Encoding` whose final coding was not `chunked`.
    TransferEncodingNotChunked,
    /// A chunk size that was not valid hexadecimal.
    MalformedChunkSize,
}

impl fmt::Display for Http1Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadStatusLine => write!(f, "malformed status line"),
            Self::BadHeader => write!(f, "malformed header field"),
            Self::AmbiguousFraming(which) => {
                write!(f, "ambiguous message framing: {which:?}")
            }
            Self::TooLarge { what } => write!(f, "{what} exceeded its limit"),
            Self::Truncated => write!(f, "the peer closed mid-message"),
        }
    }
}

impl std::error::Error for Http1Error {}

/// How the body's end is determined.
///
/// Exactly one of these applies to any response this parser accepts. That is
/// the point of refusing ambiguity: there is never a question of which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// A `Content-Length` was present and unambiguous.
    Length(usize),
    /// `Transfer-Encoding: chunked`.
    Chunked,
    /// Neither, so the body runs until the connection closes.
    ///
    /// Legal, and a liability: it cannot be distinguished from a truncated
    /// response, so a connection dropped mid-body looks like a complete one.
    /// The caller is told which framing it got so it can decline to cache or
    /// trust a body delimited this way.
    UntilClose,
    /// The status forbids a body, whatever the headers say.
    Empty,
}

/// One header field, as received.
///
/// The name is lowercased at parse time because HTTP field names are
/// case-insensitive and comparing them any other way is a bug waiting for a
/// server that capitalises differently. The value keeps its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Lowercased field name.
    pub name: String,
    /// Field value, trimmed of surrounding whitespace.
    pub value: String,
}

/// A parsed response head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseHead {
    /// The three-digit status code.
    pub status: u16,
    /// Header fields in the order received.
    pub headers: Vec<Header>,
    /// How this message's body is delimited.
    pub framing: Framing,
}

impl ResponseHead {
    /// First value for a field name, which must already be lowercase.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.name == name)
            .map(|header| header.value.as_str())
    }
}

/// Parse a response head from `bytes`.
///
/// Returns the head and the number of bytes consumed, so the caller knows
/// where the body starts. `None` is not a variant: a caller that cannot tell
/// "incomplete" from "invalid" will retry forever on a malformed response, so
/// an incomplete head is [`Http1Error::Truncated`] and the caller reads more
/// only when it knows the connection is still open.
pub fn parse_response_head(bytes: &[u8]) -> Result<(ResponseHead, usize), Http1Error> {
    let (line, mut offset) = read_line(bytes, MAX_STATUS_LINE, "status line")?;
    let status = parse_status_line(line)?;

    let mut headers: Vec<Header> = Vec::new();
    loop {
        let (line, consumed) = read_line(
            bytes.get(offset..).ok_or(Http1Error::Truncated)?,
            MAX_HEADER_LINE,
            "header line",
        )?;
        offset = offset.checked_add(consumed).ok_or(Http1Error::TooLarge {
            what: "header block",
        })?;

        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(Http1Error::TooLarge {
                what: "header count",
            });
        }
        headers.push(parse_header(line)?);
    }

    let framing = determine_framing(status, &headers)?;
    Ok((
        ResponseHead {
            status,
            headers,
            framing,
        },
        offset,
    ))
}

/// Read one CRLF-terminated line, returning it without the terminator.
///
/// A bare LF is accepted as a terminator, because a great deal of the web
/// emits it and refusing would break more than it protects. A bare CR is not a
/// terminator: treating it as one is how a header value smuggles a second
/// header past a parser that disagrees.
fn read_line<'a>(
    bytes: &'a [u8],
    limit: usize,
    what: &'static str,
) -> Result<(&'a [u8], usize), Http1Error> {
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .ok_or(Http1Error::Truncated)?;
    if end > limit {
        return Err(Http1Error::TooLarge { what });
    }
    let line = bytes.get(..end).ok_or(Http1Error::Truncated)?;
    let line = match line.last() {
        Some(b'\r') => line.get(..end.saturating_sub(1)).unwrap_or_default(),
        _ => line,
    };
    Ok((line, end.saturating_add(1)))
}

/// `HTTP/1.x SP status SP reason`.
fn parse_status_line(line: &[u8]) -> Result<u16, Http1Error> {
    let mut parts = line.splitn(3, |byte| *byte == b' ');
    let version = parts.next().ok_or(Http1Error::BadStatusLine)?;
    let code = parts.next().ok_or(Http1Error::BadStatusLine)?;

    // Only HTTP/1.x. A response claiming HTTP/2 over a connection negotiated
    // as HTTP/1.1 is not something to interpret.
    if !matches!(version, b"HTTP/1.1" | b"HTTP/1.0") {
        return Err(Http1Error::BadStatusLine);
    }

    // Exactly three digits. `parse` would accept "+404" and " 404".
    if code.len() != 3 || !code.iter().all(u8::is_ascii_digit) {
        return Err(Http1Error::BadStatusLine);
    }
    let mut status: u16 = 0;
    for digit in code {
        status = status
            .checked_mul(10)
            .and_then(|value| value.checked_add(u16::from(digit.saturating_sub(b'0'))))
            .ok_or(Http1Error::BadStatusLine)?;
    }
    Ok(status)
}

/// `name: value`, with the name lowercased and the value trimmed.
fn parse_header(line: &[u8]) -> Result<Header, Http1Error> {
    // Obsolete line folding: a header continued onto the next line by leading
    // whitespace. RFC 9112 deprecates it, and it is a classic way to disagree
    // with an upstream parser about where a field ends.
    if matches!(line.first(), Some(b' ' | b'\t')) {
        return Err(Http1Error::BadHeader);
    }

    let colon = line
        .iter()
        .position(|byte| *byte == b':')
        .ok_or(Http1Error::BadHeader)?;
    let name = line.get(..colon).ok_or(Http1Error::BadHeader)?;
    let value = line
        .get(colon.saturating_add(1)..)
        .ok_or(Http1Error::BadHeader)?;

    if name.is_empty() || !name.iter().all(|byte| is_token_byte(*byte)) {
        return Err(Http1Error::BadHeader);
    }

    // A value may not contain a control character. NUL, CR and LF inside a
    // value are the response-splitting primitive.
    if value
        .iter()
        .any(|byte| *byte < 0x20 && *byte != b'\t' || *byte == 0x7f)
    {
        return Err(Http1Error::BadHeader);
    }

    let name = String::from_utf8(name.to_ascii_lowercase()).map_err(|_| Http1Error::BadHeader)?;
    let value = String::from_utf8(trim(value).to_vec()).map_err(|_| Http1Error::BadHeader)?;
    Ok(Header { name, value })
}

/// RFC 9110 token characters. Anything else in a field name is not a name.
fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b'!' | b'#'
                | b'$'
                | b'%'
                | b'&'
                | b'\''
                | b'*'
                | b'+'
                | b'-'
                | b'.'
                | b'^'
                | b'_'
                | b'`'
                | b'|'
                | b'~'
        )
}

fn trim(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index.saturating_add(1));
    bytes.get(start..end).unwrap_or_default()
}

/// Decide how the body ends, refusing anything ambiguous.
fn determine_framing(status: u16, headers: &[Header]) -> Result<Framing, Http1Error> {
    // These statuses never carry a body, whatever the headers claim. A
    // Content-Length on a 204 is a framing disagreement waiting to happen: an
    // upstream that honours it and a client that does not have desynchronised.
    if status == 204 || status == 304 || (100..200).contains(&status) {
        return Ok(Framing::Empty);
    }

    let lengths: Vec<&Header> = headers
        .iter()
        .filter(|header| header.name == "content-length")
        .collect();
    let encodings: Vec<&Header> = headers
        .iter()
        .filter(|header| header.name == "transfer-encoding")
        .collect();

    if !lengths.is_empty() && !encodings.is_empty() {
        return Err(Http1Error::AmbiguousFraming(
            Ambiguity::LengthAndTransferEncoding,
        ));
    }

    if !encodings.is_empty() {
        // The final coding must be chunked, or the body has no defined end.
        let last = encodings.last().ok_or(Http1Error::AmbiguousFraming(
            Ambiguity::TransferEncodingNotChunked,
        ))?;
        let final_coding = last
            .value
            .rsplit(',')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        if final_coding != "chunked" {
            return Err(Http1Error::AmbiguousFraming(
                Ambiguity::TransferEncodingNotChunked,
            ));
        }
        return Ok(Framing::Chunked);
    }

    match lengths.len() {
        0 => Ok(Framing::UntilClose),
        1 => {
            let value = lengths
                .first()
                .ok_or(Http1Error::AmbiguousFraming(
                    Ambiguity::MalformedContentLength,
                ))?
                .value
                .as_str();
            // Purely digits. `str::parse` accepts a leading `+`, and a value
            // that differs from what an upstream cache read is the whole bug.
            if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(Http1Error::AmbiguousFraming(
                    Ambiguity::MalformedContentLength,
                ));
            }
            let length: usize = value.parse().map_err(|_| Http1Error::TooLarge {
                what: "content-length",
            })?;
            if length > MAX_BODY_BYTES {
                return Err(Http1Error::TooLarge {
                    what: "content-length",
                });
            }
            Ok(Framing::Length(length))
        }
        _ => Err(Http1Error::AmbiguousFraming(
            Ambiguity::RepeatedContentLength,
        )),
    }
}

/// Decode a chunked body.
///
/// Returns the decoded bytes and how many input bytes were consumed. The
/// trailer section is parsed and discarded: a trailer that redefined framing
/// would be a way to disagree with an upstream, so nothing in it is honoured.
pub fn decode_chunked(bytes: &[u8]) -> Result<(Vec<u8>, usize), Http1Error> {
    let mut body: Vec<u8> = Vec::new();
    let mut offset = 0usize;

    loop {
        let remaining = bytes.get(offset..).ok_or(Http1Error::Truncated)?;
        let (line, consumed) = read_line(remaining, MAX_HEADER_LINE, "chunk size line")?;
        offset = offset.checked_add(consumed).ok_or(Http1Error::TooLarge {
            what: "chunked body",
        })?;

        // A chunk extension is permitted after a semicolon and is ignored, but
        // the size itself must be clean hexadecimal — no sign, no whitespace,
        // no `0x`.
        let size_field = line.split(|byte| *byte == b';').next().unwrap_or_default();
        let size = parse_chunk_size(size_field)?;

        if size == 0 {
            // Trailers run to the blank line. Read and discard them.
            loop {
                let remaining = bytes.get(offset..).ok_or(Http1Error::Truncated)?;
                let (trailer, consumed) = read_line(remaining, MAX_HEADER_LINE, "trailer")?;
                offset = offset.checked_add(consumed).ok_or(Http1Error::TooLarge {
                    what: "chunked body",
                })?;
                if trailer.is_empty() {
                    return Ok((body, offset));
                }
            }
        }

        if size > MAX_CHUNK_BYTES {
            return Err(Http1Error::TooLarge { what: "chunk" });
        }
        if body.len().saturating_add(size) > MAX_BODY_BYTES {
            return Err(Http1Error::TooLarge { what: "body" });
        }

        let chunk = bytes
            .get(offset..offset.saturating_add(size))
            .ok_or(Http1Error::Truncated)?;
        body.extend_from_slice(chunk);
        offset = offset.checked_add(size).ok_or(Http1Error::TooLarge {
            what: "chunked body",
        })?;

        // Each chunk is followed by CRLF, which must be there and must be
        // empty. A non-empty line here means the declared size was wrong.
        let remaining = bytes.get(offset..).ok_or(Http1Error::Truncated)?;
        let (terminator, consumed) = read_line(remaining, 2, "chunk terminator")?;
        if !terminator.is_empty() {
            return Err(Http1Error::AmbiguousFraming(Ambiguity::MalformedChunkSize));
        }
        offset = offset.checked_add(consumed).ok_or(Http1Error::TooLarge {
            what: "chunked body",
        })?;
    }
}

/// Hexadecimal chunk size, with no tolerance for decoration.
fn parse_chunk_size(field: &[u8]) -> Result<usize, Http1Error> {
    let field = trim(field);
    if field.is_empty() || field.len() > 16 {
        return Err(Http1Error::AmbiguousFraming(Ambiguity::MalformedChunkSize));
    }
    let mut size: usize = 0;
    for byte in field {
        let digit = match byte {
            b'0'..=b'9' => byte.saturating_sub(b'0'),
            b'a'..=b'f' => byte.saturating_sub(b'a').saturating_add(10),
            b'A'..=b'F' => byte.saturating_sub(b'A').saturating_add(10),
            _ => return Err(Http1Error::AmbiguousFraming(Ambiguity::MalformedChunkSize)),
        };
        size = size
            .checked_mul(16)
            .and_then(|value| value.checked_add(usize::from(digit)))
            .ok_or(Http1Error::TooLarge { what: "chunk size" })?;
    }
    Ok(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(raw: &str) -> Result<ResponseHead, Http1Error> {
        parse_response_head(raw.as_bytes()).map(|(head, _)| head)
    }

    #[test]
    fn an_ordinary_response_parses() {
        let parsed = head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nX-A: b\r\n\r\n")
            .expect("a well-formed response");
        assert_eq!(parsed.status, 200);
        assert_eq!(parsed.framing, Framing::Length(5));
        assert_eq!(parsed.header("x-a"), Some("b"));
    }

    /// Field names are case-insensitive, and a lookup that misses because a
    /// server capitalised differently is a bug that shows up only on some
    /// servers — the worst kind to find later.
    #[test]
    fn field_names_are_matched_case_insensitively() {
        let parsed = head("HTTP/1.1 200 OK\r\nCONTENT-length: 0\r\n\r\n").expect("valid");
        assert_eq!(parsed.framing, Framing::Length(0));
        assert_eq!(parsed.header("content-length"), Some("0"));
    }

    // ---- framing ambiguity is refused, not resolved -----------------------

    #[test]
    fn content_length_with_transfer_encoding_is_refused() {
        let error =
            head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nTransfer-Encoding: chunked\r\n\r\n")
                .expect_err("both framings present");
        assert_eq!(
            error,
            Http1Error::AmbiguousFraming(Ambiguity::LengthAndTransferEncoding),
            "RFC 9112 permits preferring Transfer-Encoding; an upstream that \
             prefers differently is the other half of a desync"
        );
    }

    #[test]
    fn repeated_content_length_is_refused_even_when_the_values_agree() {
        let error = head("HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 5\r\n\r\n")
            .expect_err("two content-lengths");
        assert_eq!(
            error,
            Http1Error::AmbiguousFraming(Ambiguity::RepeatedContentLength),
            "nothing legitimate emits two, so agreement is not evidence of honesty"
        );
    }

    /// `str::parse` accepts a leading `+`. An upstream using a stricter parser
    /// reads a different length, which is the whole bug.
    ///
    /// Note what is *not* here: `" 5"` and `"5 "`. Surrounding whitespace is
    /// optional whitespace the field grammar allows and RFC 9110 §5.5 requires
    /// to be excluded from the value, so every conformant parser sees `5` and
    /// there is nothing to disagree about. An earlier version of this test
    /// demanded they be refused, which would have broken ordinary servers to
    /// defend against a disagreement that cannot occur. Internal whitespace is
    /// a different matter and is covered below.
    #[test]
    fn a_decorated_content_length_is_refused() {
        for value in ["+5", "0x5", "five", "", "5 5", "5	5", "٥"] {
            let raw = format!("HTTP/1.1 200 OK\r\nContent-Length: {value}\r\n\r\n");
            let result = head(&raw);
            assert!(
                result.is_err(),
                "content-length {value:?} must be refused, got {result:?}"
            );
        }
    }

    #[test]
    fn transfer_encoding_not_ending_in_chunked_is_refused() {
        let error = head("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked, gzip\r\n\r\n")
            .expect_err("chunked is not final");
        assert_eq!(
            error,
            Http1Error::AmbiguousFraming(Ambiguity::TransferEncodingNotChunked)
        );
    }

    #[test]
    fn transfer_encoding_ending_in_chunked_is_accepted() {
        let parsed =
            head("HTTP/1.1 200 OK\r\nTransfer-Encoding: gzip, chunked\r\n\r\n").expect("valid");
        assert_eq!(parsed.framing, Framing::Chunked);
    }

    /// A body on these statuses is a desync primitive: an upstream that
    /// honours the length and a client that does not are no longer agreed on
    /// where the next response starts.
    #[test]
    fn statuses_that_forbid_a_body_ignore_any_declared_length() {
        for status in [204u16, 304, 100, 199] {
            let raw = format!("HTTP/1.1 {status} X\r\nContent-Length: 99\r\n\r\n");
            let parsed = head(&raw).expect("valid head");
            assert_eq!(
                parsed.framing,
                Framing::Empty,
                "status {status} must carry no body"
            );
        }
    }

    #[test]
    fn no_length_and_no_encoding_runs_until_close() {
        let parsed = head("HTTP/1.1 200 OK\r\nX: y\r\n\r\n").expect("valid");
        assert_eq!(parsed.framing, Framing::UntilClose);
    }

    // ---- header and status line hygiene -----------------------------------

    /// Obsolete line folding lets a field continue onto the next line, which
    /// is a way to disagree with an upstream about where a field ends.
    #[test]
    fn folded_headers_are_refused() {
        let error =
            head("HTTP/1.1 200 OK\r\nX-A: b\r\n  continued\r\n\r\n").expect_err("folded header");
        assert_eq!(error, Http1Error::BadHeader);
    }

    /// A control character in a value is the response-splitting primitive.
    #[test]
    fn control_characters_in_a_header_value_are_refused() {
        for raw in [
            "HTTP/1.1 200 OK\r\nX-A: b\u{0}c\r\n\r\n",
            "HTTP/1.1 200 OK\r\nX-A: b\rc\r\n\r\n",
            "HTTP/1.1 200 OK\r\nX-A: b\u{7f}c\r\n\r\n",
        ] {
            assert_eq!(
                head(raw),
                Err(Http1Error::BadHeader),
                "a control character in a value must be refused"
            );
        }
    }

    #[test]
    fn a_header_name_outside_the_token_set_is_refused() {
        for raw in [
            "HTTP/1.1 200 OK\r\nX A: b\r\n\r\n",
            "HTTP/1.1 200 OK\r\n: b\r\n\r\n",
            "HTTP/1.1 200 OK\r\nX(A): b\r\n\r\n",
        ] {
            assert_eq!(head(raw), Err(Http1Error::BadHeader));
        }
    }

    #[test]
    fn a_malformed_status_line_is_refused() {
        for raw in [
            "HTTP/2 200 OK\r\n\r\n",
            "HTTP/1.1 20 OK\r\n\r\n",
            "HTTP/1.1 2000 OK\r\n\r\n",
            "HTTP/1.1 +04 OK\r\n\r\n",
            "HTTP/1.1\r\n\r\n",
            "garbage\r\n\r\n",
        ] {
            assert_eq!(
                head(raw),
                Err(Http1Error::BadStatusLine),
                "status line {raw:?} must be refused"
            );
        }
    }

    /// Incomplete must be distinguishable from invalid, or a caller retries
    /// forever on a malformed response.
    #[test]
    fn an_incomplete_head_is_truncated_rather_than_malformed() {
        assert_eq!(head("HTTP/1.1 200 OK\r\nX: y"), Err(Http1Error::Truncated));
        assert_eq!(head(""), Err(Http1Error::Truncated));
    }

    #[test]
    fn an_oversized_header_block_is_refused_rather_than_accumulated() {
        let mut raw = String::from("HTTP/1.1 200 OK\r\n");
        for index in 0..(MAX_HEADERS + 10) {
            raw.push_str(&format!("X-{index}: v\r\n"));
        }
        raw.push_str("\r\n");
        assert_eq!(
            head(&raw),
            Err(Http1Error::TooLarge {
                what: "header count"
            })
        );
    }

    // ---- chunked decoding -------------------------------------------------

    #[test]
    fn a_chunked_body_decodes() {
        let (body, consumed) =
            decode_chunked(b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n").expect("valid chunked");
        assert_eq!(body, b"hello world");
        assert_eq!(
            consumed, 26,
            "every byte including the terminating chunk must be consumed, or the              next response is parsed starting from the wrong offset"
        );
    }

    #[test]
    fn a_chunk_size_that_is_not_hex_is_refused() {
        for raw in ["z\r\nx\r\n0\r\n\r\n", "-1\r\n0\r\n\r\n", "\r\n0\r\n\r\n"] {
            let result = decode_chunked(raw.as_bytes());
            assert!(result.is_err(), "chunk size in {raw:?} must be refused");
        }
    }

    #[test]
    fn a_chunk_size_larger_than_its_data_is_truncated_not_over_read() {
        assert_eq!(
            decode_chunked(b"10\r\nshort\r\n0\r\n\r\n"),
            Err(Http1Error::Truncated),
            "a declared size beyond the buffer must not read past it"
        );
    }

    /// Eight bytes on the wire must not buy an allocation.
    #[test]
    fn an_enormous_declared_chunk_is_refused_before_allocating() {
        let error = decode_chunked(b"ffffffff\r\n").expect_err("declared 4 GiB");
        assert!(
            matches!(error, Http1Error::TooLarge { .. } | Http1Error::Truncated),
            "got {error:?}"
        );
    }

    #[test]
    fn chunk_extensions_are_ignored_but_the_size_is_not() {
        let (body, _) =
            decode_chunked(b"5;name=value\r\nhello\r\n0\r\n\r\n").expect("extension is ignored");
        assert_eq!(body, b"hello");
    }

    #[test]
    fn trailers_are_consumed_and_discarded() {
        let (body, consumed) =
            decode_chunked(b"5\r\nhello\r\n0\r\nX-Trailer: v\r\n\r\n").expect("valid");
        assert_eq!(body, b"hello");
        assert!(
            consumed > 12,
            "the trailer must be consumed, not left for the next parse"
        );
    }
}

/// An incremental chunked-body decoder.
///
/// # Why this exists rather than calling [`decode_chunked`] again
///
/// The obvious read loop — accumulate bytes, try to decode the whole buffer,
/// read more if it is incomplete — is quadratic in the body's length, because
/// every read re-scans everything that arrived before it and rebuilds the
/// output from scratch. Measured on the real path: 1 MB took 10 ms, 2 MB took
/// 38 ms and 4 MB took 182 ms, which is roughly a quadrupling per doubling.
/// Extrapolated to [`MAX_BODY_BYTES`] that is about twelve seconds of CPU for
/// one response, chosen entirely by the server.
///
/// That is a denial of service rather than a performance note: a page with
/// several such subresources multiplies it, and nothing about sending a large
/// body slowly looks hostile.
///
/// This decoder keeps its position, so each call costs only the bytes that are
/// new.
#[derive(Debug, Default)]
pub struct ChunkedDecoder {
    body: Vec<u8>,
    /// How much of the input has been decoded and will never be re-scanned.
    consumed: usize,
    complete: bool,
}

impl ChunkedDecoder {
    /// A decoder positioned at the start of a body.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many input bytes have been consumed.
    pub fn consumed(&self) -> usize {
        self.consumed
    }

    /// Whether the terminating zero-length chunk has been seen.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Decode whatever is newly available in `input`.
    ///
    /// `input` is the whole body region received so far; the decoder reads
    /// from its own offset, so passing a growing buffer is correct and costs
    /// only the new bytes. Returns whether the body is complete.
    pub fn push(&mut self, input: &[u8]) -> Result<bool, Http1Error> {
        if self.complete {
            return Ok(true);
        }

        loop {
            let rest = input.get(self.consumed..).unwrap_or_default();
            if rest.is_empty() {
                return Ok(false);
            }

            // A chunk size line that has not fully arrived is not an error —
            // it means read more. Distinguishing that from a malformed one is
            // the whole reason read_line reports Truncated separately.
            let (line, header_len) = match read_line(rest, MAX_HEADER_LINE, "chunk size line") {
                Ok(pair) => pair,
                Err(Http1Error::Truncated) => return Ok(false),
                Err(error) => return Err(error),
            };

            let size_field = line.split(|byte| *byte == b';').next().unwrap_or_default();
            let size = parse_chunk_size(size_field)?;

            if size == 0 {
                // Trailers, to the blank line. Parsed and discarded: a trailer
                // that redefined framing would be a way to disagree with an
                // upstream, so nothing in it is honoured.
                let mut offset = self.consumed.saturating_add(header_len);
                loop {
                    let rest = input.get(offset..).unwrap_or_default();
                    let (trailer, used) = match read_line(rest, MAX_HEADER_LINE, "trailer") {
                        Ok(pair) => pair,
                        Err(Http1Error::Truncated) => return Ok(false),
                        Err(error) => return Err(error),
                    };
                    offset = offset.checked_add(used).ok_or(Http1Error::TooLarge {
                        what: "chunked body",
                    })?;
                    if trailer.is_empty() {
                        self.consumed = offset;
                        self.complete = true;
                        return Ok(true);
                    }
                }
            }

            if size > MAX_CHUNK_BYTES {
                return Err(Http1Error::TooLarge { what: "chunk" });
            }
            if self.body.len().saturating_add(size) > MAX_BODY_BYTES {
                return Err(Http1Error::TooLarge { what: "body" });
            }

            // The chunk, its trailing CRLF, and the size line must all be
            // present before anything is consumed — otherwise a partially
            // arrived chunk would be counted twice.
            let data_at = self.consumed.saturating_add(header_len);
            let data_end = data_at.checked_add(size).ok_or(Http1Error::TooLarge {
                what: "chunked body",
            })?;
            let Some(chunk) = input.get(data_at..data_end) else {
                return Ok(false);
            };
            let after = input.get(data_end..).unwrap_or_default();
            let (terminator, term_len) = match read_line(after, 2, "chunk terminator") {
                Ok(pair) => pair,
                Err(Http1Error::Truncated) => return Ok(false),
                Err(error) => return Err(error),
            };
            if !terminator.is_empty() {
                return Err(Http1Error::AmbiguousFraming(Ambiguity::MalformedChunkSize));
            }

            self.body.extend_from_slice(chunk);
            self.consumed = data_end.checked_add(term_len).ok_or(Http1Error::TooLarge {
                what: "chunked body",
            })?;
        }
    }

    /// The decoded body.
    pub fn into_body(self) -> Vec<u8> {
        self.body
    }
}

#[cfg(test)]
mod incremental_tests {
    use super::*;

    /// Feeding a growing buffer must never re-scan what it already consumed.
    ///
    /// This is the contract that makes the read loop linear rather than
    /// quadratic. It is asserted structurally rather than by timing, because a
    /// timing assertion in CI is a flake waiting to happen — but the property
    /// it stands in for is a denial of service, not a slow path.
    #[test]
    fn the_decoder_consumes_forward_only() {
        let message = b"5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let mut decoder = ChunkedDecoder::new();
        let mut last = 0usize;

        // Feed one byte at a time, as a slow server would.
        for end in 1..=message.len() {
            let prefix = message.get(..end).unwrap_or_default();
            let complete = decoder.push(prefix).expect("valid chunked");
            assert!(
                decoder.consumed() >= last,
                "consumed went backwards: {} then {}",
                last,
                decoder.consumed()
            );
            last = decoder.consumed();
            if complete {
                assert_eq!(end, message.len(), "completed early");
            }
        }
        assert!(decoder.is_complete());
        assert_eq!(decoder.into_body(), b"hello world");
    }

    /// The incremental decoder and the one-shot one must agree. If they ever
    /// disagree, one of them is wrong about framing and the fuzz target is
    /// attacking the wrong one.
    #[test]
    fn the_two_decoders_agree() {
        for message in [
            &b"5\r\nhello\r\n0\r\n\r\n"[..],
            &b"5;ext=1\r\nhello\r\n0\r\n\r\n"[..],
            &b"5\r\nhello\r\n0\r\nX-T: v\r\n\r\n"[..],
            &b"1\r\na\r\n1\r\nb\r\n1\r\nc\r\n0\r\n\r\n"[..],
        ] {
            let mut decoder = ChunkedDecoder::new();
            let complete = decoder.push(message).expect("incremental decode");
            let (one_shot, _) = decode_chunked(message).expect("one-shot decode");
            assert!(complete, "incremental must complete on {message:?}");
            assert_eq!(
                decoder.into_body(),
                one_shot,
                "decoders disagree on {message:?}"
            );
        }
    }

    /// A malformed size must be an error at any feed boundary, not a stall.
    #[test]
    fn a_malformed_chunk_size_is_an_error_not_a_stall() {
        let mut decoder = ChunkedDecoder::new();
        assert!(decoder.push(b"zz\r\n").is_err());
    }

    /// A declared chunk larger than the limit is refused before the bytes are
    /// accumulated, not after.
    #[test]
    fn an_enormous_declared_chunk_is_refused_before_accumulating() {
        let mut decoder = ChunkedDecoder::new();
        let error = decoder.push(b"ffffffff\r\n").expect_err("declared 4 GiB");
        assert!(
            matches!(error, Http1Error::TooLarge { .. }),
            "got {error:?}"
        );
    }
}
