//! Fetching a single resource: connect, request, parse, close.
//!
//! # Every request is partitioned
//!
//! [`fetch`] takes a [`PartitionKey`], not a URL and not a host. There is no
//! overload that omits it. The connection this opens, and the pool entry it
//! may later become, belong to that key and to nothing else — see
//! [`crate::partition`] for why the *pair* rather than the destination alone.
//!
//! # What this deliberately does not do
//!
//! - **No redirects.** A redirect is a policy decision — whether the target is
//!   in the same partition, whether a downgrade to `http` is permitted,
//!   whether credentials survive — and following one silently inside a fetch
//!   primitive is how a request ends up somewhere the caller never approved.
//!   [`Response::redirect_target`] surfaces the location and the caller
//!   decides.
//! - **No cookies, no cache, no authentication.** Those are partitioned state,
//!   and this is the layer beneath them.
//! - **No HTTP/2.** ADR 015. ALPN offers `http/1.1` only.
//!
//! # Invariant 4
//!
//! One connection is opened, to the origin named in the key, and nothing else.
//! No preconnect, no DNS prefetch, no OCSP — ADR 013 — and no "helpful"
//! parallel connection. `crates/px-net/tests/connections.rs` asserts it by
//! watching what the process actually opens.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::Arc;
use std::time::Duration;

use crate::http1::{self, Framing, Http1Error, ResponseHead};
use crate::partition::PartitionKey;
use crate::tls;

/// How long to wait for the connection and for the response.
///
/// A server that accepts a connection and never answers is otherwise a way to
/// hold a slot open indefinitely, which is the same wedge Phase 1's
/// adversarial session found in the IPC layer wearing different clothes.
const TIMEOUT: Duration = Duration::from_secs(30);

/// What can go wrong fetching a resource.
#[derive(Debug)]
pub enum FetchError {
    /// The name did not resolve, or the connection was refused.
    Connect(std::io::Error),
    /// The TLS handshake failed — a bad certificate, an untrusted chain, a
    /// name mismatch. Never downgraded to plaintext: invariant 8.
    Tls(String),
    /// The response was not a well-formed HTTP/1.1 message.
    Protocol(Http1Error),
    /// The transport failed mid-exchange.
    Io(std::io::Error),
    /// The request could not be built — a path with a control character, a
    /// header value that would split the request.
    BadRequest(&'static str),
}

impl std::fmt::Display for FetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Connect(error) => write!(f, "could not connect: {error}"),
            Self::Tls(detail) => write!(f, "TLS failed: {detail}"),
            Self::Protocol(error) => write!(f, "malformed response: {error}"),
            Self::Io(error) => write!(f, "transport error: {error}"),
            Self::BadRequest(detail) => write!(f, "cannot build the request: {detail}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// A fetched resource.
#[derive(Debug, Clone)]
pub struct Response {
    /// The parsed head.
    pub head: ResponseHead,
    /// The body, decoded if it was chunked.
    pub body: Vec<u8>,
}

impl Response {
    /// The `Location` of a redirect, if this is one.
    ///
    /// Returned rather than followed. See the module documentation.
    pub fn redirect_target(&self) -> Option<&str> {
        match self.head.status {
            301 | 302 | 303 | 307 | 308 => self.head.header("location"),
            _ => None,
        }
    }
}

/// Fetch `path` from the origin named by `key`.
///
/// The path must be an origin-relative request target — `/index.html`. A
/// caller holding a full URL resolves it to a key and a path first, which is
/// what forces the partition decision to be made rather than inferred.
pub fn fetch(key: &PartitionKey, path: &str) -> Result<Response, FetchError> {
    let origin = key.origin();
    let request = build_request(origin.host(), path)?;

    let address = format!("{}:{}", origin.host(), origin.port());
    let stream = TcpStream::connect(&address).map_err(FetchError::Connect)?;
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(FetchError::Io)?;
    stream
        .set_write_timeout(Some(TIMEOUT))
        .map_err(FetchError::Io)?;

    if origin.is_secure() {
        let mut session = tls::connect(origin.host(), stream)?;
        exchange(&mut session, &request)
    } else {
        // Plaintext is reachable only for an `http` origin, which
        // `Origin::new` allows and higher layers will restrict. There is no
        // path from `https` to here: a TLS failure is an error, never a
        // fallback (invariant 8).
        let mut stream = stream;
        exchange(&mut stream, &request)
    }
}

/// Build the request bytes, refusing anything that could split the request.
fn build_request(host: &str, path: &str) -> Result<Vec<u8>, FetchError> {
    if path.is_empty() || !path.starts_with('/') {
        return Err(FetchError::BadRequest("the path must be origin-relative"));
    }
    // A control character in the target or the host is request splitting: it
    // terminates the line early and the rest is read as another header, or
    // another request.
    if path.bytes().any(|byte| byte < 0x21 || byte == 0x7f) {
        return Err(FetchError::BadRequest(
            "the path contains a control character or a space",
        ));
    }
    if host.bytes().any(|byte| byte < 0x21 || byte == 0x7f) {
        return Err(FetchError::BadRequest(
            "the host contains a control character",
        ));
    }

    // Deliberately minimal. Every header sent is a bit of entropy, and
    // invariant 4's "no identifiers" is easier to keep by not adding them in
    // the first place: no User-Agent, no Accept-Language, no Accept-Encoding.
    // Connection: close because pooling is the caller's business and this
    // primitive does one exchange.
    let mut request = Vec::new();
    request.extend_from_slice(b"GET ");
    request.extend_from_slice(path.as_bytes());
    request.extend_from_slice(b" HTTP/1.1\r\nHost: ");
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    Ok(request)
}

/// Write the request, read until the message is complete, parse it.
fn exchange<S: Read + Write>(stream: &mut S, request: &[u8]) -> Result<Response, FetchError> {
    stream.write_all(request).map_err(FetchError::Io)?;
    stream.flush().map_err(FetchError::Io)?;

    let mut raw = Vec::new();
    let mut buffer = [0u8; 16 * 1024];

    // Read until the head parses, then until the body is complete. The head
    // has to arrive before its own length limit can be applied, so the read
    // is bounded by the head limit until the head is known.
    let (head, head_len) = loop {
        match http1::parse_response_head(&raw) {
            Ok(parsed) => break parsed,
            Err(Http1Error::Truncated) => {}
            Err(error) => return Err(FetchError::Protocol(error)),
        }
        if raw.len() > http1::MAX_BODY_BYTES {
            return Err(FetchError::Protocol(Http1Error::TooLarge {
                what: "response head",
            }));
        }
        let read = stream.read(&mut buffer).map_err(FetchError::Io)?;
        if read == 0 {
            return Err(FetchError::Protocol(Http1Error::Truncated));
        }
        raw.extend_from_slice(buffer.get(..read).unwrap_or_default());
    };

    let body = read_body(stream, &mut raw, head_len, head.framing, &mut buffer)?;
    Ok(Response { head, body })
}

/// Read the body according to the framing the head declared.
fn read_body<S: Read>(
    stream: &mut S,
    raw: &mut Vec<u8>,
    head_len: usize,
    framing: Framing,
    buffer: &mut [u8],
) -> Result<Vec<u8>, FetchError> {
    let mut fill = |raw: &mut Vec<u8>| -> Result<bool, FetchError> {
        let read = stream.read(buffer).map_err(FetchError::Io)?;
        if read == 0 {
            return Ok(false);
        }
        if raw.len().saturating_add(read) > http1::MAX_BODY_BYTES {
            return Err(FetchError::Protocol(Http1Error::TooLarge { what: "body" }));
        }
        raw.extend_from_slice(buffer.get(..read).unwrap_or_default());
        Ok(true)
    };

    match framing {
        Framing::Empty => Ok(Vec::new()),
        Framing::Length(length) => {
            while raw.len().saturating_sub(head_len) < length {
                if !fill(raw)? {
                    return Err(FetchError::Protocol(Http1Error::Truncated));
                }
            }
            let start = head_len;
            let end = start.saturating_add(length);
            Ok(raw.get(start..end).unwrap_or_default().to_vec())
        }
        Framing::Chunked => loop {
            let rest = raw.get(head_len..).unwrap_or_default();
            match http1::decode_chunked(rest) {
                Ok((body, _)) => return Ok(body),
                Err(Http1Error::Truncated) => {
                    if !fill(raw)? {
                        return Err(FetchError::Protocol(Http1Error::Truncated));
                    }
                }
                Err(error) => return Err(FetchError::Protocol(error)),
            }
        },
        Framing::UntilClose => {
            while fill(raw)? {}
            Ok(raw.get(head_len..).unwrap_or_default().to_vec())
        }
    }
}

/// A configured fetcher, holding the TLS configuration so it is built once.
///
/// Building a `ClientConfig` parses the whole root store, which is expensive
/// enough that doing it per request would be noticeable.
#[derive(Clone)]
pub struct Fetcher {
    _config: Arc<rustls::ClientConfig>,
}

impl Fetcher {
    /// Build a fetcher with the trust anchors ADR 012 specifies.
    pub fn new() -> Result<Self, FetchError> {
        Ok(Self {
            _config: tls::client_config()?,
        })
    }
}
