//! The CORS protocol (Fetch Standard, "CORS protocol" and "HTTP fetch").
//!
//! Pure decision functions. The caller (px-net's fetch) supplies the request
//! and response as data and obeys the verdicts; nothing here performs I/O or
//! reads a clock (the preflight cache takes `now` in seconds).
//!
//! # Deviations
//! - `Access-Control-Max-Age` is capped at [`MAX_AGE_CAP_SECS`] (the Fetch
//!   "imposed limit" is implementation-defined); an unparsable value is 5
//!   seconds, per spec.
//! - The preflight cache is bounded at [`MAX_CACHE_ENTRIES`] and evicts the
//!   oldest entry when full. Eviction only ever causes an extra preflight.
//! - Header values are bytes; list-valued headers are split on commas outside
//!   quoted strings ("get, decode, and split").
//! - `Range` safelisting accepts `bytes=N-` and `bytes=N-M` only, as the spec
//!   does (suffix ranges are not safelisted).
//! - Redirect handling, `Timing-Allow-Origin` and service workers are not part
//!   of this module.

use crate::origin::{Origin, Url, trace};

/// Enforcement points of this module.
pub const ENFORCEMENT_POINTS: &[&str] = &[
    "cors::classify",
    "cors::preflight_request",
    "cors::validate_preflight",
    "cors::cache_lookup",
    "cors::validate_response",
    "cors::taint",
    "cors::filter_response",
    "cors::forbidden_request_header",
    "cors::forbidden_method",
];

/// Cap on `Access-Control-Max-Age`, seconds (two hours).
pub const MAX_AGE_CAP_SECS: u64 = 7200;
/// Default max-age when absent or unparsable, seconds (Fetch).
pub const DEFAULT_MAX_AGE_SECS: u64 = 5;
/// Preflight cache capacity.
pub const MAX_CACHE_ENTRIES: usize = 512;
/// Largest header value length considered when listing names; longer values
/// are unsafe for the safelist anyway (limit is 128).
pub const SAFELIST_VALUE_MAX: usize = 128;
/// Combined safelisted value size above which all become unsafe.
pub const SAFELIST_TOTAL_MAX: usize = 1024;

/// Request credentials mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CredentialsMode {
    /// Never send credentials.
    Omit,
    /// Credentials for same-origin requests only.
    SameOrigin,
    /// Credentials always.
    Include,
}

/// Request mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestMode {
    /// Cross-origin allowed with CORS.
    Cors,
    /// Cross-origin allowed, response opaque.
    NoCors,
    /// Cross-origin forbidden.
    SameOrigin,
    /// Navigation.
    Navigate,
}

/// One header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Name, any case.
    pub name: String,
    /// Value bytes.
    pub value: Vec<u8>,
}

impl Header {
    /// Convenience constructor from strings.
    pub fn new(name: &str, value: &str) -> Header {
        Header {
            name: name.to_string(),
            value: value.as_bytes().to_vec(),
        }
    }
}

/// The parts of a request the CORS protocol looks at.
#[derive(Debug, Clone)]
pub struct CorsRequest {
    /// Method, already normalised ([`normalize_method`]).
    pub method: String,
    /// Current URL.
    pub url: Url,
    /// Request origin (the initiator's).
    pub origin: Origin,
    /// Author-supplied headers.
    pub headers: Vec<Header>,
    /// Credentials mode.
    pub credentials: CredentialsMode,
    /// Mode.
    pub mode: RequestMode,
    /// Fetch's use-CORS-preflight flag (e.g. upload listeners registered).
    pub use_cors_preflight_flag: bool,
    /// Fetch's origin-tainted flag (set by a cross-origin redirect).
    pub origin_tainted: bool,
}

/// The parts of a response the CORS protocol looks at.
#[derive(Debug, Clone)]
pub struct CorsResponse {
    /// Status code.
    pub status: u16,
    /// Headers in order.
    pub headers: Vec<Header>,
}

/// Why CORS refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorsError {
    /// No `Access-Control-Allow-Origin`.
    MissingAllowOrigin,
    /// ACAO is neither `*` (allowed) nor the request origin.
    OriginMismatch,
    /// Credentialed request but `Access-Control-Allow-Credentials` is not
    /// exactly `true`.
    CredentialsNotAllowed,
    /// Preflight status not 2xx.
    PreflightStatus,
    /// Method not granted.
    MethodNotAllowed,
    /// Header not granted.
    HeaderNotAllowed(String),
    /// Mode `same-origin` and the URL is cross-origin.
    CrossOriginForbidden,
    /// CORS only exists for http(s).
    UnsupportedScheme,
}

/// Simple or preflighted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Classification {
    /// No preflight needed (still subject to the response CORS check).
    Simple,
    /// Preflight required; reasons in [`PreflightReasons`].
    Preflighted(PreflightReasons),
}

/// Why a preflight is required.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PreflightReasons {
    /// The method is not CORS-safelisted.
    pub unsafe_method: bool,
    /// Lowercase sorted CORS-unsafe request-header names.
    pub unsafe_headers: Vec<String>,
    /// use-CORS-preflight flag.
    pub flag: bool,
}

// ------------------------------------------------------------ primitives

/// Normalise a method: the six well-known ones are uppercased, others kept.
pub fn normalize_method(m: &str) -> String {
    for k in ["DELETE", "GET", "HEAD", "OPTIONS", "POST", "PUT"] {
        if m.eq_ignore_ascii_case(k) {
            return k.to_string();
        }
    }
    m.to_string()
}

/// CORS-safelisted method: `GET`, `HEAD`, `POST` (case-sensitive here;
/// normalise first).
pub fn is_safelisted_method(m: &str) -> bool {
    matches!(m, "GET" | "HEAD" | "POST")
}

/// Forbidden method (`CONNECT`, `TRACE`, `TRACK`, case-insensitive). Scripts
/// may not issue these.
pub fn is_forbidden_method(m: &str) -> bool {
    trace::hit("cors::forbidden_method");
    ["CONNECT", "TRACE", "TRACK"]
        .iter()
        .any(|f| m.eq_ignore_ascii_case(f))
}

fn trim_http_ws(b: &[u8]) -> &[u8] {
    let is_ws = |c: &u8| matches!(c, b'\t' | b'\n' | b'\r' | b' ');
    let start = b.iter().position(|c| !is_ws(c)).unwrap_or(b.len());
    let end = b.iter().rposition(|c| !is_ws(c)).map_or(start, |e| e + 1);
    b.get(start..end).unwrap_or(&[])
}

/// Forbidden request-header: scripts may not set it, so the page cannot
/// forge these.
pub fn is_forbidden_request_header(name: &str, value: &[u8]) -> bool {
    trace::hit("cors::forbidden_request_header");
    let n = name.to_ascii_lowercase();
    const LIST: &[&str] = &[
        "accept-charset",
        "accept-encoding",
        "access-control-request-headers",
        "access-control-request-method",
        "connection",
        "content-length",
        "cookie",
        "cookie2",
        "date",
        "dnt",
        "expect",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "set-cookie",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "via",
    ];
    if LIST.contains(&n.as_str()) || n.starts_with("proxy-") || n.starts_with("sec-") {
        return true;
    }
    if matches!(
        n.as_str(),
        "x-http-method" | "x-http-method-override" | "x-method-override"
    ) {
        // Fail closed on a non-UTF-8 value: treat as forbidden.
        let Ok(v) = std::str::from_utf8(value) else {
            return true;
        };
        return split_list(v.as_bytes())
            .iter()
            .any(|m| is_forbidden_method(m));
    }
    false
}

/// Forbidden response-header names (hidden from scripts).
pub fn is_forbidden_response_header(name: &str) -> bool {
    name.eq_ignore_ascii_case("set-cookie") || name.eq_ignore_ascii_case("set-cookie2")
}

fn is_cors_unsafe_byte(b: u8) -> bool {
    (b < 0x20 && b != 0x09)
        || matches!(
            b,
            b'"' | b'('
                | b')'
                | b':'
                | b'<'
                | b'>'
                | b'?'
                | b'@'
                | b'['
                | b'\\'
                | b']'
                | b'{'
                | b'}'
                | 0x7f
        )
}

fn is_token_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
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

/// Essence of a MIME type per "parse a MIME type" (lowercased `type/subtype`)
/// or `None` on failure.
fn mime_essence(value: &[u8]) -> Option<String> {
    let v = trim_http_ws(value);
    let end = v.iter().position(|&b| b == b';').unwrap_or(v.len());
    let essence = trim_http_ws(v.get(..end)?);
    let slash = essence.iter().position(|&b| b == b'/')?;
    let (t, s) = essence.split_at_checked(slash)?;
    let s = s.get(1..)?;
    if t.is_empty() || s.is_empty() || !t.iter().chain(s).all(|&b| is_token_byte(b)) {
        return None;
    }
    Some(String::from_utf8_lossy(essence).to_ascii_lowercase())
}

fn parse_single_range_start(v: &[u8]) -> bool {
    let Ok(s) = std::str::from_utf8(v) else {
        return false;
    };
    let s = s.trim();
    let Some(eq) = s.find('=') else { return false };
    let (unit, rest) = s.split_at(eq);
    if !unit.eq_ignore_ascii_case("bytes") {
        return false;
    }
    let rest = rest.get(1..).unwrap_or("");
    let Some((a, b)) = rest.split_once('-') else {
        return false;
    };
    let digits = |x: &str| !x.is_empty() && x.bytes().all(|c| c.is_ascii_digit());
    if !digits(a) {
        return false;
    }
    if b.is_empty() {
        return true;
    }
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(a), Ok(b)) => a <= b,
        _ => false,
    }
}

/// CORS-safelisted request-header (name, value).
pub fn is_safelisted_request_header(name: &str, value: &[u8]) -> bool {
    if value.len() > SAFELIST_VALUE_MAX {
        return false;
    }
    match name.to_ascii_lowercase().as_str() {
        "accept" => !value.iter().any(|&b| is_cors_unsafe_byte(b)),
        "accept-language" | "content-language" => value.iter().all(|&b| {
            b.is_ascii_alphanumeric() || matches!(b, b' ' | b'*' | b',' | b'-' | b'.' | b';' | b'=')
        }),
        "content-type" => {
            if value.iter().any(|&b| is_cors_unsafe_byte(b)) {
                return false;
            }
            matches!(
                mime_essence(value).as_deref(),
                Some("application/x-www-form-urlencoded" | "multipart/form-data" | "text/plain")
            )
        }
        "range" => parse_single_range_start(value),
        _ => false,
    }
}

/// CORS-unsafe request-header names: lowercase, sorted, deduplicated.
pub fn unsafe_header_names(headers: &[Header]) -> Vec<String> {
    let mut unsafe_names = Vec::new();
    let mut potentially = Vec::new();
    let mut total = 0usize;
    for h in headers {
        let lname = h.name.to_ascii_lowercase();
        if is_safelisted_request_header(&h.name, &h.value) {
            total = total.saturating_add(h.value.len());
            potentially.push(lname);
        } else {
            unsafe_names.push(lname);
        }
    }
    if total > SAFELIST_TOTAL_MAX {
        unsafe_names.append(&mut potentially);
    }
    unsafe_names.sort();
    unsafe_names.dedup();
    unsafe_names
}

/// Split a list-valued header on commas outside quoted strings, trimming
/// HTTP whitespace.
pub fn split_list(value: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut in_q = false;
    let mut esc = false;
    for &b in value {
        if in_q {
            cur.push(b);
            if esc {
                esc = false;
            } else if b == b'\\' {
                esc = true;
            } else if b == b'"' {
                in_q = false;
            }
        } else if b == b'"' {
            in_q = true;
            cur.push(b);
        } else if b == b',' {
            out.push(String::from_utf8_lossy(trim_http_ws(&cur)).into_owned());
            cur.clear();
        } else {
            cur.push(b);
        }
    }
    out.push(String::from_utf8_lossy(trim_http_ws(&cur)).into_owned());
    out
}

/// Fetch "get": all values of `name` joined by `, `; `None` if absent.
fn get_combined(headers: &[Header], name: &str) -> Option<Vec<u8>> {
    let mut found = false;
    let mut out: Vec<u8> = Vec::new();
    for h in headers.iter().filter(|h| h.name.eq_ignore_ascii_case(name)) {
        if found {
            out.extend_from_slice(b", ");
        }
        found = true;
        out.extend_from_slice(trim_http_ws(&h.value));
    }
    found.then_some(out)
}

/// "Extract header list values": `None` when absent, else the split list.
fn extract_list(headers: &[Header], name: &str) -> Option<Vec<String>> {
    get_combined(headers, name).map(|v| split_list(&v))
}

// ------------------------------------------------------------ classification

/// Is `request` simple, or must it be preflighted? Only meaningful for
/// `RequestMode::Cors` cross-origin requests; [`needs_cors_protocol`] says
/// whether it applies.
pub fn classify(request: &CorsRequest) -> Classification {
    trace::hit("cors::classify");
    let unsafe_method = !is_safelisted_method(&request.method);
    let unsafe_headers = unsafe_header_names(&request.headers);
    if !unsafe_method && unsafe_headers.is_empty() && !request.use_cors_preflight_flag {
        Classification::Simple
    } else {
        Classification::Preflighted(PreflightReasons {
            unsafe_method,
            unsafe_headers,
            flag: request.use_cors_preflight_flag,
        })
    }
}

/// Serialise the request origin for the `Origin` header / ACAO comparison.
pub fn serialize_request_origin(request: &CorsRequest) -> String {
    request.origin.serialize()
}

/// Whether the `Origin` header is sent: cross-origin CORS-mode requests and
/// any request that is not GET/HEAD (Fetch "append a request `Origin`
/// header").
pub fn should_send_origin_header(request: &CorsRequest, taint: Tainting) -> bool {
    taint == Tainting::Cors || !matches!(request.method.as_str(), "GET" | "HEAD")
}

/// STUB
pub fn origin_header_value(
    request: &CorsRequest,
    taint: Tainting,
    _p: crate::referrer::ReferrerPolicy,
) -> Option<String> {
    should_send_origin_header(request, taint).then(|| serialize_request_origin(request))
}

// ------------------------------------------------------------ preflight

/// The constructed preflight request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightRequest {
    /// Always `OPTIONS`.
    pub method: String,
    /// The request's URL.
    pub url: Url,
    /// Request headers: `Accept: */*`, `Access-Control-Request-Method`, and
    /// `Access-Control-Request-Headers` when needed. `Origin` is in
    /// [`PreflightRequest::origin_header`].
    pub headers: Vec<Header>,
    /// Value of the `Origin` header.
    pub origin_header: String,
    /// Always `SameOrigin`-mode credentials: preflights never carry cookies.
    pub credentials: CredentialsMode,
}

/// Build the preflight for `request` (Fetch "CORS-preflight fetch").
pub fn preflight_request(request: &CorsRequest) -> PreflightRequest {
    trace::hit("cors::preflight_request");
    let mut headers = vec![
        Header::new("Accept", "*/*"),
        Header::new("Access-Control-Request-Method", &request.method),
    ];
    let names = unsafe_header_names(&request.headers);
    if !names.is_empty() {
        headers.push(Header::new(
            "Access-Control-Request-Headers",
            &names.join(","),
        ));
    }
    PreflightRequest {
        method: "OPTIONS".to_string(),
        url: request.url.clone(),
        headers,
        origin_header: serialize_request_origin(request),
        credentials: CredentialsMode::SameOrigin,
    }
}

/// What a successful preflight granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreflightGrant {
    /// Allowed methods (may contain `*`).
    pub methods: Vec<String>,
    /// Allowed header names, as sent (may contain `*`).
    pub header_names: Vec<String>,
    /// Seconds to cache, capped.
    pub max_age: u64,
}

/// The CORS check (Fetch "CORS check") against `request`'s credentials mode.
pub fn cors_check(request: &CorsRequest, response_headers: &[Header]) -> Result<(), CorsError> {
    let acao = get_combined(response_headers, "Access-Control-Allow-Origin")
        .ok_or(CorsError::MissingAllowOrigin)?;
    let include = request.credentials == CredentialsMode::Include;
    if !include && acao == b"*" {
        return Ok(());
    }
    if acao != serialize_request_origin(request).as_bytes() {
        return Err(CorsError::OriginMismatch);
    }
    if !include {
        return Ok(());
    }
    match get_combined(response_headers, "Access-Control-Allow-Credentials") {
        Some(c) if c == b"true" => Ok(()),
        _ => Err(CorsError::CredentialsNotAllowed),
    }
}

fn header_listed(list: &[String], name: &str) -> bool {
    list.iter().any(|i| i.eq_ignore_ascii_case(name))
}

/// Validate a preflight response (Fetch "CORS-preflight fetch" steps after
/// the network fetch).
pub fn validate_preflight(
    request: &CorsRequest,
    response: &CorsResponse,
) -> Result<PreflightGrant, CorsError> {
    trace::hit("cors::validate_preflight");
    cors_check(request, &response.headers)?;
    if !(200..300).contains(&response.status) {
        return Err(CorsError::PreflightStatus);
    }
    let include = request.credentials == CredentialsMode::Include;
    let mut methods = extract_list(&response.headers, "Access-Control-Allow-Methods");
    let header_names =
        extract_list(&response.headers, "Access-Control-Allow-Headers").unwrap_or_default();
    if methods.is_none() && request.use_cors_preflight_flag {
        methods = Some(vec![request.method.clone()]);
    }
    let methods = methods.unwrap_or_default();

    let method_listed = methods.contains(&request.method);
    let method_star = !include && methods.iter().any(|m| m == "*");
    if !method_listed && !is_safelisted_method(&request.method) && !method_star {
        return Err(CorsError::MethodNotAllowed);
    }
    // `Authorization` is never covered by the wildcard.
    if request
        .headers
        .iter()
        .any(|h| h.name.eq_ignore_ascii_case("authorization"))
        && !header_listed(&header_names, "authorization")
    {
        return Err(CorsError::HeaderNotAllowed("authorization".to_string()));
    }
    let star = !include && header_names.iter().any(|h| h == "*");
    for n in unsafe_header_names(&request.headers) {
        if !header_listed(&header_names, &n) && !star {
            return Err(CorsError::HeaderNotAllowed(n));
        }
    }
    let max_age = match get_combined(&response.headers, "Access-Control-Max-Age") {
        Some(v) => parse_delta_seconds(&v).unwrap_or(DEFAULT_MAX_AGE_SECS),
        None => DEFAULT_MAX_AGE_SECS,
    }
    .min(MAX_AGE_CAP_SECS);
    Ok(PreflightGrant {
        methods,
        header_names,
        max_age,
    })
}

/// Digits only; overflow saturates (it is capped afterwards anyway).
fn parse_delta_seconds(v: &[u8]) -> Option<u64> {
    let v = trim_http_ws(v);
    if v.is_empty() || !v.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let mut n: u64 = 0;
    for &d in v {
        n = n.saturating_mul(10).saturating_add(u64::from(d - b'0'));
    }
    Some(n)
}

// ------------------------------------------------------------ cache

#[derive(Debug, Clone)]
struct CacheEntry {
    partition: String,
    origin: Origin,
    url: String,
    credentials: bool,
    method: Option<String>,
    header: Option<String>,
    expires_at: u64,
}

/// The CORS-preflight cache. Keyed by (partition key, origin, URL,
/// credentials); the partition key is the string form of px-net's
/// `PartitionKey` (invariant 2: nothing is shared across partitions).
#[derive(Debug, Default)]
pub struct PreflightCache {
    entries: Vec<CacheEntry>,
}

impl PreflightCache {
    /// Empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of live entries (including not-yet-purged expired ones).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop everything for `partition` (e.g. storage cleared).
    pub fn forget_partition(&mut self, partition: &str) {
        self.entries.retain(|e| e.partition != partition);
    }

    fn base_match(e: &CacheEntry, partition: &str, request: &CorsRequest, now: u64) -> bool {
        e.expires_at > now
            && e.partition == partition
            && e.origin == request.origin
            && e.url == request.url.serialize(true)
            && (e.credentials || request.credentials != CredentialsMode::Include)
    }

    /// Record a grant for `request`.
    pub fn store(
        &mut self,
        partition: &str,
        request: &CorsRequest,
        grant: &PreflightGrant,
        now: u64,
    ) {
        if grant.max_age == 0 {
            return;
        }
        self.entries.retain(|e| e.expires_at > now);
        let credentials = request.credentials == CredentialsMode::Include;
        let expires_at = now.saturating_add(grant.max_age);
        let url = request.url.serialize(true);
        let mut add = |method: Option<String>, header: Option<String>| {
            // Refresh an equal entry rather than duplicating.
            if let Some(e) = self.entries.iter_mut().find(|e| {
                e.partition == partition
                    && e.origin == request.origin
                    && e.url == url
                    && e.credentials == credentials
                    && e.method == method
                    && e.header == header
            }) {
                e.expires_at = expires_at;
                return;
            }
            if self.entries.len() >= MAX_CACHE_ENTRIES {
                self.entries.remove(0);
            }
            self.entries.push(CacheEntry {
                partition: partition.to_string(),
                origin: request.origin.clone(),
                url: url.clone(),
                credentials,
                method,
                header,
                expires_at,
            });
        };
        for m in &grant.methods {
            add(Some(m.clone()), None);
        }
        for h in &grant.header_names {
            add(None, Some(h.to_ascii_lowercase()));
        }
    }

    /// May the preflight be skipped for `request`? True only when the cache
    /// covers the method and every CORS-unsafe header.
    pub fn covers(&self, partition: &str, request: &CorsRequest, now: u64) -> bool {
        trace::hit("cors::cache_lookup");
        let include = request.credentials == CredentialsMode::Include;
        let live = || {
            self.entries
                .iter()
                .filter(|e| Self::base_match(e, partition, request, now))
        };
        let method_hit = live().any(|e| {
            e.method
                .as_deref()
                .is_some_and(|m| m == request.method || (m == "*" && !include))
        });
        let method_ok = method_hit
            || (is_safelisted_method(&request.method) && !request.use_cors_preflight_flag);
        if !method_ok {
            return false;
        }
        let header_hit = |name: &str| {
            live().any(|e| {
                e.header
                    .as_deref()
                    .is_some_and(|h| h == name || (h == "*" && !include && name != "authorization"))
            })
        };
        if request
            .headers
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("authorization"))
            && !header_hit("authorization")
        {
            return false;
        }
        unsafe_header_names(&request.headers)
            .iter()
            .all(|n| header_hit(n))
    }
}

// ------------------------------------------------------------ response

/// Response tainting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tainting {
    /// Same-origin: fully readable.
    Basic,
    /// CORS-approved: filtered headers.
    Cors,
    /// no-cors cross-origin: nothing readable.
    Opaque,
}

/// Decide the response tainting for `request` (Fetch "main fetch" scheme and
/// tainting step). `Err` means a network error before any request is made.
pub fn taint(request: &CorsRequest) -> Result<Tainting, CorsError> {
    trace::hit("cors::taint");
    if request.url.scheme() == "data" || request.mode == RequestMode::Navigate {
        return Ok(Tainting::Basic);
    }
    let url_origin = Origin::from_url(&request.url);
    if url_origin.same_origin(&request.origin) && !request.origin_tainted {
        return Ok(Tainting::Basic);
    }
    match request.mode {
        RequestMode::SameOrigin => Err(CorsError::CrossOriginForbidden),
        RequestMode::NoCors => Ok(Tainting::Opaque),
        RequestMode::Cors | RequestMode::Navigate => {
            if matches!(request.url.scheme(), "http" | "https") {
                Ok(Tainting::Cors)
            } else {
                Err(CorsError::UnsupportedScheme)
            }
        }
    }
}

/// Validate the actual (non-preflight) response. Required whenever
/// [`taint`] returned `Cors`; trivially fine otherwise.
pub fn validate_response(
    request: &CorsRequest,
    taint: Tainting,
    response: &CorsResponse,
) -> Result<(), CorsError> {
    trace::hit("cors::validate_response");
    match taint {
        Tainting::Cors => cors_check(request, &response.headers),
        Tainting::Basic | Tainting::Opaque => Ok(()),
    }
}

/// What script may see of a response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilteredResponse {
    /// Status (0 when opaque).
    pub status: u16,
    /// Visible headers.
    pub headers: Vec<Header>,
    /// Whether the response is opaque (body unreadable).
    pub opaque: bool,
}

/// Apply the response filter for `taint`.
pub fn filter_response(
    request: &CorsRequest,
    taint: Tainting,
    response: &CorsResponse,
) -> FilteredResponse {
    trace::hit("cors::filter_response");
    match taint {
        Tainting::Opaque => FilteredResponse {
            status: 0,
            headers: Vec::new(),
            opaque: true,
        },
        Tainting::Basic => FilteredResponse {
            status: response.status,
            headers: response
                .headers
                .iter()
                .filter(|h| !is_forbidden_response_header(&h.name))
                .cloned()
                .collect(),
            opaque: false,
        },
        Tainting::Cors => {
            let exposed = extract_list(&response.headers, "Access-Control-Expose-Headers")
                .unwrap_or_default();
            let wildcard =
                request.credentials != CredentialsMode::Include && exposed.iter().any(|e| e == "*");
            const SAFE: &[&str] = &[
                "cache-control",
                "content-language",
                "content-length",
                "content-type",
                "expires",
                "last-modified",
                "pragma",
            ];
            let headers = response
                .headers
                .iter()
                .filter(|h| {
                    !is_forbidden_response_header(&h.name)
                        && (SAFE.contains(&h.name.to_ascii_lowercase().as_str())
                            || wildcard
                            || header_listed(&exposed, &h.name))
                })
                .cloned()
                .collect();
            FilteredResponse {
                status: response.status,
                headers,
                opaque: false,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, url: &str, headers: Vec<Header>, cred: CredentialsMode) -> CorsRequest {
        CorsRequest {
            method: normalize_method(method),
            url: Url::parse(url).unwrap(),
            origin: Origin::from_url(&Url::parse("https://app.example/").unwrap()),
            headers,
            credentials: cred,
            mode: RequestMode::Cors,
            use_cors_preflight_flag: false,
            origin_tainted: false,
        }
    }
    fn simple() -> CorsRequest {
        req(
            "GET",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        )
    }
    fn resp(status: u16, hs: &[(&str, &str)]) -> CorsResponse {
        CorsResponse {
            status,
            headers: hs.iter().map(|(n, v)| Header::new(n, v)).collect(),
        }
    }

    #[test]
    fn classify_methods() {
        for m in ["GET", "HEAD", "POST", "get"] {
            assert_eq!(
                classify(&req(m, "https://a.example/", vec![], CredentialsMode::Omit)),
                Classification::Simple,
                "{m}"
            );
        }
        for m in ["PUT", "DELETE", "PATCH", "OPTIONS", "FOO"] {
            assert!(
                matches!(
                    classify(&req(m, "https://a.example/", vec![], CredentialsMode::Omit)),
                    Classification::Preflighted(PreflightReasons {
                        unsafe_method: true,
                        ..
                    })
                ),
                "{m}"
            );
        }
    }

    #[test]
    fn classify_content_type_safelist() {
        let ct = |v: &str| {
            classify(&req(
                "POST",
                "https://a.example/",
                vec![Header::new("Content-Type", v)],
                CredentialsMode::Omit,
            ))
        };
        assert_eq!(ct("text/plain"), Classification::Simple);
        assert_eq!(ct("TEXT/PLAIN; charset=utf-8"), Classification::Simple);
        assert_eq!(
            ct("multipart/form-data; boundary=x"),
            Classification::Simple
        );
        assert_eq!(
            ct("application/x-www-form-urlencoded"),
            Classification::Simple
        );
        // Not safelisted:
        assert_ne!(ct("application/json"), Classification::Simple);
        assert_ne!(ct("text/plain, application/json"), Classification::Simple); // comma list? essence "text/plain, application/json" is not a token pair
        assert_ne!(ct("text/html"), Classification::Simple);
        assert_ne!(ct("garbage"), Classification::Simple);
        assert_ne!(ct("text/plain\u{1}"), Classification::Simple);
        assert_ne!(ct("text/plain\"x"), Classification::Simple);
    }

    #[test]
    fn classify_other_safelisted_headers() {
        let one = |n: &str, v: &str| unsafe_header_names(&[Header::new(n, v)]);
        assert!(one("Accept", "text/html,*/*;q=0.8").is_empty());
        assert_eq!(one("Accept", "a{b"), vec!["accept"]);
        assert!(one("Accept-Language", "en-US,en;q=0.9").is_empty());
        assert_eq!(one("Accept-Language", "en_US"), vec!["accept-language"]);
        assert!(one("Content-Language", "de").is_empty());
        assert!(one("Range", "bytes=0-99").is_empty());
        assert!(one("Range", "bytes=5-").is_empty());
        assert_eq!(one("Range", "bytes=-500"), vec!["range"]);
        assert_eq!(one("Range", "bytes=9-1"), vec!["range"]);
        assert_eq!(one("Range", "bytes=0-1,5-6"), vec!["range"]);
        assert_eq!(one("X-Custom", "1"), vec!["x-custom"]);
        assert_eq!(one("Authorization", "x"), vec!["authorization"]);
    }

    #[test]
    fn value_length_limits() {
        let long = "a".repeat(129);
        assert_eq!(
            unsafe_header_names(&[Header::new("Accept", &long)]),
            vec!["accept"]
        );
        let ok = "a".repeat(128);
        assert!(unsafe_header_names(&[Header::new("Accept", &ok)]).is_empty());
        // 9 x 128 = 1152 > 1024: every otherwise-safelisted header becomes unsafe.
        let hs: Vec<Header> = ["Accept", "Accept-Language"]
            .iter()
            .cycle()
            .take(9)
            .map(|n| Header::new(n, &ok))
            .collect();
        assert_eq!(unsafe_header_names(&hs), vec!["accept", "accept-language"]);
        // 8 x 128 = 1024, not greater: stays safe.
        let hs8: Vec<Header> = hs.into_iter().take(8).collect();
        assert!(unsafe_header_names(&hs8).is_empty());
    }

    #[test]
    fn unsafe_names_sorted_deduped_lowercase() {
        let hs = vec![
            Header::new("X-B", "1"),
            Header::new("x-a", "1"),
            Header::new("X-B", "2"),
        ];
        assert_eq!(unsafe_header_names(&hs), vec!["x-a", "x-b"]);
    }

    #[test]
    fn upload_flag_forces_preflight() {
        let mut r = simple();
        r.use_cors_preflight_flag = true;
        assert!(matches!(classify(&r), Classification::Preflighted(p) if p.flag));
    }

    #[test]
    fn preflight_request_shape() {
        let r = req(
            "PUT",
            "https://api.example/x?y=1#frag",
            vec![
                Header::new("X-Token", "1"),
                Header::new("Content-Type", "application/json"),
                Header::new("Accept", "*/*"),
            ],
            CredentialsMode::Include,
        );
        let p = preflight_request(&r);
        assert_eq!(p.method, "OPTIONS");
        assert_eq!(p.origin_header, "https://app.example");
        assert_eq!(p.credentials, CredentialsMode::SameOrigin);
        assert_eq!(p.url.serialize(true), "https://api.example/x?y=1");
        assert!(p.headers.contains(&Header::new("Accept", "*/*")));
        assert!(
            p.headers
                .contains(&Header::new("Access-Control-Request-Method", "PUT"))
        );
        assert!(p.headers.contains(&Header::new(
            "Access-Control-Request-Headers",
            "content-type,x-token"
        )));
    }

    #[test]
    fn preflight_request_omits_headers_list_when_empty() {
        let p = preflight_request(&req(
            "DELETE",
            "https://a.example/",
            vec![],
            CredentialsMode::Omit,
        ));
        assert!(
            !p.headers
                .iter()
                .any(|h| h.name == "Access-Control-Request-Headers")
        );
    }

    #[test]
    fn opaque_origin_serialises_null() {
        let mut r = simple();
        r.origin = Origin::new_opaque();
        assert_eq!(preflight_request(&r).origin_header, "null");
        let ok = resp(200, &[("Access-Control-Allow-Origin", "null")]);
        assert!(cors_check(&r, &ok.headers).is_ok());
    }

    #[test]
    fn cors_check_matrix() {
        let none = simple();
        let inc = req(
            "GET",
            "https://api.example/x",
            vec![],
            CredentialsMode::Include,
        );
        let h = |v: &[(&str, &str)]| resp(200, v).headers;
        // Missing.
        assert_eq!(
            cors_check(&none, &h(&[])),
            Err(CorsError::MissingAllowOrigin)
        );
        // Wildcard without credentials.
        assert!(cors_check(&none, &h(&[("Access-Control-Allow-Origin", "*")])).is_ok());
        // Wildcard with credentials: rejected.
        assert_eq!(
            cors_check(
                &inc,
                &h(&[
                    ("Access-Control-Allow-Origin", "*"),
                    ("Access-Control-Allow-Credentials", "true")
                ])
            ),
            Err(CorsError::OriginMismatch)
        );
        // Exact origin.
        assert!(
            cors_check(
                &none,
                &h(&[("access-control-allow-origin", "https://app.example")])
            )
            .is_ok()
        );
        // Wrong origin, trailing slash, case.
        for bad in [
            "https://other.example",
            "https://app.example/",
            "HTTPS://APP.EXAMPLE",
            "null",
            "",
        ] {
            assert_eq!(
                cors_check(&none, &h(&[("Access-Control-Allow-Origin", bad)])),
                Err(CorsError::OriginMismatch),
                "{bad}"
            );
        }
        // Credentials: needs exactly `true`.
        let acao = ("Access-Control-Allow-Origin", "https://app.example");
        assert!(
            cors_check(
                &inc,
                &h(&[acao, ("Access-Control-Allow-Credentials", "true")])
            )
            .is_ok()
        );
        for bad in ["True", "TRUE", "1", "yes", "true, true", " false"] {
            assert_eq!(
                cors_check(&inc, &h(&[acao, ("Access-Control-Allow-Credentials", bad)])),
                Err(CorsError::CredentialsNotAllowed),
                "{bad}"
            );
        }
        assert_eq!(
            cors_check(&inc, &h(&[acao])),
            Err(CorsError::CredentialsNotAllowed)
        );
        // Duplicate ACAO headers combine to "a, b" and fail.
        assert_eq!(
            cors_check(&none, &h(&[acao, acao])),
            Err(CorsError::OriginMismatch)
        );
    }

    fn grant_resp(extra: &[(&str, &str)]) -> CorsResponse {
        let mut v = vec![("Access-Control-Allow-Origin", "https://app.example")];
        v.extend_from_slice(extra);
        resp(204, &v)
    }

    #[test]
    fn preflight_validation_methods() {
        let put = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        );
        assert_eq!(
            validate_preflight(&put, &grant_resp(&[])).unwrap_err(),
            CorsError::MethodNotAllowed
        );
        let g = validate_preflight(
            &put,
            &grant_resp(&[("Access-Control-Allow-Methods", "GET, PUT")]),
        )
        .unwrap();
        assert_eq!(g.methods, vec!["GET", "PUT"]);
        assert_eq!(g.max_age, 5);
        // Methods are case-sensitive.
        assert!(
            validate_preflight(
                &put,
                &grant_resp(&[("Access-Control-Allow-Methods", "put")])
            )
            .is_err()
        );
        // Wildcard works without credentials only.
        assert!(
            validate_preflight(&put, &grant_resp(&[("Access-Control-Allow-Methods", "*")])).is_ok()
        );
        let putc = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Include,
        );
        let r = grant_resp(&[
            ("Access-Control-Allow-Methods", "*"),
            ("Access-Control-Allow-Credentials", "true"),
        ]);
        assert_eq!(
            validate_preflight(&putc, &r).unwrap_err(),
            CorsError::MethodNotAllowed
        );
        // A safelisted method needs no listing (header-only preflight).
        let post = req(
            "POST",
            "https://api.example/x",
            vec![Header::new("X-A", "1")],
            CredentialsMode::Omit,
        );
        assert!(
            validate_preflight(
                &post,
                &grant_resp(&[("Access-Control-Allow-Headers", "x-a")])
            )
            .is_ok()
        );
    }

    #[test]
    fn preflight_validation_status() {
        let put = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        );
        let mut r = grant_resp(&[("Access-Control-Allow-Methods", "PUT")]);
        for s in [301, 403, 404, 500, 199, 0] {
            r.status = s;
            assert_eq!(
                validate_preflight(&put, &r).unwrap_err(),
                CorsError::PreflightStatus,
                "{s}"
            );
        }
        for s in [200, 204, 299] {
            r.status = s;
            assert!(validate_preflight(&put, &r).is_ok(), "{s}");
        }
    }

    #[test]
    fn preflight_validation_headers() {
        let r = req(
            "GET",
            "https://api.example/x",
            vec![Header::new("X-Token", "1"), Header::new("X-Other", "2")],
            CredentialsMode::Omit,
        );
        let only_one = grant_resp(&[("Access-Control-Allow-Headers", "X-Token")]);
        assert_eq!(
            validate_preflight(&r, &only_one).unwrap_err(),
            CorsError::HeaderNotAllowed("x-other".into())
        );
        let both = grant_resp(&[("Access-Control-Allow-Headers", "x-token , X-OTHER")]);
        assert!(validate_preflight(&r, &both).is_ok());
        // Wildcard without credentials.
        let star = grant_resp(&[("Access-Control-Allow-Headers", "*")]);
        assert!(validate_preflight(&r, &star).is_ok());
        // Wildcard with credentials is a literal "*": rejected.
        let rc = req(
            "GET",
            "https://api.example/x",
            vec![Header::new("X-Token", "1")],
            CredentialsMode::Include,
        );
        let star_c = grant_resp(&[
            ("Access-Control-Allow-Headers", "*"),
            ("Access-Control-Allow-Credentials", "true"),
        ]);
        assert!(validate_preflight(&rc, &star_c).is_err());
        // Authorization is never covered by `*`.
        let ra = req(
            "GET",
            "https://api.example/x",
            vec![Header::new("Authorization", "t")],
            CredentialsMode::Omit,
        );
        assert_eq!(
            validate_preflight(&ra, &star).unwrap_err(),
            CorsError::HeaderNotAllowed("authorization".into())
        );
        let named = grant_resp(&[("Access-Control-Allow-Headers", "authorization")]);
        assert!(validate_preflight(&ra, &named).is_ok());
    }

    #[test]
    fn preflight_flag_with_safelisted_method_caches_method() {
        let mut r = simple();
        r.use_cors_preflight_flag = true;
        let g = validate_preflight(&r, &grant_resp(&[])).unwrap();
        assert_eq!(g.methods, vec!["GET"]);
    }

    #[test]
    fn max_age_rules() {
        let put = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        );
        let age = |v: &str| {
            validate_preflight(
                &put,
                &grant_resp(&[
                    ("Access-Control-Allow-Methods", "PUT"),
                    ("Access-Control-Max-Age", v),
                ]),
            )
            .unwrap()
            .max_age
        };
        assert_eq!(age("600"), 600);
        assert_eq!(age("0"), 0);
        assert_eq!(age("86400"), MAX_AGE_CAP_SECS);
        assert_eq!(age("99999999999999999999999999"), MAX_AGE_CAP_SECS);
        assert_eq!(age("-1"), 5);
        assert_eq!(age("abc"), 5);
        assert_eq!(age("1.5"), 5);
        assert_eq!(age(""), 5);
    }

    #[test]
    fn credentialed_preflight_requires_credentials_header() {
        let put = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Include,
        );
        let r = grant_resp(&[("Access-Control-Allow-Methods", "PUT")]);
        assert_eq!(
            validate_preflight(&put, &r).unwrap_err(),
            CorsError::CredentialsNotAllowed
        );
    }

    #[test]
    fn preflight_cache_key_and_expiry() {
        let r = req(
            "PUT",
            "https://api.example/x",
            vec![Header::new("X-T", "1")],
            CredentialsMode::Omit,
        );
        let g = PreflightGrant {
            methods: vec!["PUT".into()],
            header_names: vec!["X-T".into()],
            max_age: 100,
        };
        let mut c = PreflightCache::new();
        assert!(!c.covers("p1", &r, 0));
        c.store("p1", &r, &g, 1000);
        assert!(c.covers("p1", &r, 1000));
        assert!(c.covers("p1", &r, 1099));
        assert!(!c.covers("p1", &r, 1100), "expired at exactly max-age");
        // Different partition.
        assert!(!c.covers("p2", &r, 1000));
        // Different origin.
        let mut r2 = r.clone();
        r2.origin = Origin::from_url(&Url::parse("https://other.example/").unwrap());
        assert!(!c.covers("p1", &r2, 1000));
        // Different URL (query differs).
        let mut r3 = r.clone();
        r3.url = Url::parse("https://api.example/x?q").unwrap();
        assert!(!c.covers("p1", &r3, 1000));
        // Fragment is not part of the key.
        let mut r4 = r.clone();
        r4.url = Url::parse("https://api.example/x#f").unwrap();
        assert!(c.covers("p1", &r4, 1000));
        // Header not in grant.
        let mut r5 = r.clone();
        r5.headers.push(Header::new("X-New", "1"));
        assert!(!c.covers("p1", &r5, 1000));
        // Other method.
        let mut r6 = r.clone();
        r6.method = "DELETE".into();
        assert!(!c.covers("p1", &r6, 1000));
        c.forget_partition("p1");
        assert!(!c.covers("p1", &r, 1000));
    }

    #[test]
    fn preflight_cache_credentials_asymmetry() {
        let plain = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        );
        let cred = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Include,
        );
        let g = PreflightGrant {
            methods: vec!["PUT".into()],
            header_names: vec![],
            max_age: 100,
        };
        let mut c = PreflightCache::new();
        // A non-credentialed grant must not satisfy a credentialed request.
        c.store("p", &plain, &g, 0);
        assert!(c.covers("p", &plain, 1));
        assert!(!c.covers("p", &cred, 1));
        // A credentialed grant also satisfies a non-credentialed request.
        let mut c2 = PreflightCache::new();
        c2.store("p", &cred, &g, 0);
        assert!(c2.covers("p", &cred, 1));
        assert!(c2.covers("p", &plain, 1));
    }

    #[test]
    fn preflight_cache_wildcards() {
        let r = req(
            "PUT",
            "https://api.example/x",
            vec![Header::new("X-T", "1"), Header::new("Authorization", "a")],
            CredentialsMode::Omit,
        );
        let g = PreflightGrant {
            methods: vec!["*".into()],
            header_names: vec!["*".into()],
            max_age: 100,
        };
        let mut c = PreflightCache::new();
        c.store("p", &r, &g, 0);
        assert!(!c.covers("p", &r, 1), "authorization is not wildcarded");
        let r2 = req(
            "PUT",
            "https://api.example/x",
            vec![Header::new("X-T", "1")],
            CredentialsMode::Omit,
        );
        assert!(c.covers("p", &r2, 1));
    }

    #[test]
    fn cache_is_bounded_and_zero_age_not_stored() {
        let mut c = PreflightCache::new();
        let g0 = PreflightGrant {
            methods: vec!["PUT".into()],
            header_names: vec![],
            max_age: 0,
        };
        let r = req(
            "PUT",
            "https://api.example/x",
            vec![],
            CredentialsMode::Omit,
        );
        c.store("p", &r, &g0, 0);
        assert!(c.is_empty());
        for i in 0..(MAX_CACHE_ENTRIES + 50) {
            let r = req(
                "PUT",
                &format!("https://api.example/{i}"),
                vec![],
                CredentialsMode::Omit,
            );
            let g = PreflightGrant {
                methods: vec!["PUT".into()],
                header_names: vec![],
                max_age: 100,
            };
            c.store("p", &r, &g, 0);
        }
        assert_eq!(c.len(), MAX_CACHE_ENTRIES);
    }

    #[test]
    fn tainting() {
        let same = {
            let mut r = simple();
            r.url = Url::parse("https://app.example/data").unwrap();
            r
        };
        assert_eq!(taint(&same), Ok(Tainting::Basic));
        let mut tainted = same.clone();
        tainted.origin_tainted = true;
        assert_eq!(taint(&tainted), Ok(Tainting::Cors));
        assert_eq!(taint(&simple()), Ok(Tainting::Cors));
        let mut nocors = simple();
        nocors.mode = RequestMode::NoCors;
        assert_eq!(taint(&nocors), Ok(Tainting::Opaque));
        let mut so = simple();
        so.mode = RequestMode::SameOrigin;
        assert_eq!(taint(&so), Err(CorsError::CrossOriginForbidden));
        let mut ftp = simple();
        ftp.url = Url::parse("ftp://x.example/").unwrap();
        assert_eq!(taint(&ftp), Err(CorsError::UnsupportedScheme));
        let mut data = simple();
        data.url = Url::parse("data:text/plain,hi").unwrap();
        assert_eq!(taint(&data), Ok(Tainting::Basic));
        let mut nav = simple();
        nav.mode = RequestMode::Navigate;
        assert_eq!(taint(&nav), Ok(Tainting::Basic));
        // http vs https is cross-origin.
        let mut down = same.clone();
        down.url = Url::parse("http://app.example/data").unwrap();
        assert_eq!(taint(&down), Ok(Tainting::Cors));
    }

    #[test]
    fn validate_response_only_enforces_for_cors() {
        let r = simple();
        let bad = resp(200, &[]);
        assert!(validate_response(&r, Tainting::Cors, &bad).is_err());
        assert!(validate_response(&r, Tainting::Basic, &bad).is_ok());
        assert!(validate_response(&r, Tainting::Opaque, &bad).is_ok());
    }

    #[test]
    fn origin_header_rules() {
        let get = simple();
        assert!(should_send_origin_header(&get, Tainting::Cors));
        assert!(!should_send_origin_header(&get, Tainting::Basic));
        let post = req(
            "POST",
            "https://app.example/",
            vec![],
            CredentialsMode::Omit,
        );
        assert!(should_send_origin_header(&post, Tainting::Basic));
    }

    #[test]
    fn filtering() {
        let r = simple();
        let response = resp(
            200,
            &[
                ("Content-Type", "text/plain"),
                ("X-Secret", "s"),
                ("X-Public", "p"),
                ("Set-Cookie", "a=b"),
                ("Access-Control-Expose-Headers", "x-public, set-cookie"),
            ],
        );
        let f = filter_response(&r, Tainting::Cors, &response);
        let names: Vec<&str> = f.headers.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["Content-Type", "X-Public"]);
        // Wildcard (no credentials).
        let w = resp(
            200,
            &[
                ("X-A", "1"),
                ("Set-Cookie", "c"),
                ("Access-Control-Expose-Headers", "*"),
            ],
        );
        let names: Vec<String> = filter_response(&r, Tainting::Cors, &w)
            .headers
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, vec!["X-A", "Access-Control-Expose-Headers"]);
        // Wildcard ignored with credentials.
        let rc = req(
            "GET",
            "https://api.example/x",
            vec![],
            CredentialsMode::Include,
        );
        assert!(filter_response(&rc, Tainting::Cors, &w).headers.is_empty());
        // Basic hides only cookies.
        let b = filter_response(&r, Tainting::Basic, &response);
        assert!(b.headers.iter().any(|h| h.name == "X-Secret"));
        assert!(!b.headers.iter().any(|h| h.name == "Set-Cookie"));
        // Opaque: nothing.
        let o = filter_response(&r, Tainting::Opaque, &response);
        assert!(o.opaque && o.status == 0 && o.headers.is_empty());
    }

    #[test]
    fn forbidden_names_and_methods() {
        for n in [
            "Cookie",
            "HOST",
            "origin",
            "Referer",
            "Sec-Fetch-Mode",
            "proxy-authorization",
            "Transfer-Encoding",
            "Access-Control-Request-Method",
        ] {
            assert!(is_forbidden_request_header(n, b"x"), "{n}");
        }
        for n in [
            "Accept",
            "X-Custom",
            "Authorization",
            "Content-Type",
            "secure",
        ] {
            assert!(!is_forbidden_request_header(n, b"x"), "{n}");
        }
        assert!(is_forbidden_request_header(
            "X-HTTP-Method-Override",
            b"trace"
        ));
        assert!(is_forbidden_request_header(
            "X-Method-Override",
            b"PUT, CONNECT"
        ));
        assert!(!is_forbidden_request_header("X-HTTP-Method", b"PUT"));
        assert!(is_forbidden_request_header("X-HTTP-Method", &[0xff]));
        for m in ["CONNECT", "trace", "Track"] {
            assert!(is_forbidden_method(m));
        }
        assert!(!is_forbidden_method("PUT"));
    }

    #[test]
    fn split_list_quotes() {
        assert_eq!(split_list(b"a, b ,c"), vec!["a", "b", "c"]);
        assert_eq!(split_list(b"\"a,b\", c"), vec!["\"a,b\"", "c"]);
        assert_eq!(split_list(b""), vec![""]);
    }

    #[test]
    fn every_enforcement_point_is_reached() {
        trace::reset();
        let r = req(
            "PUT",
            "https://api.example/x",
            vec![Header::new("X-T", "1")],
            CredentialsMode::Omit,
        );
        let _ = classify(&r);
        let _ = preflight_request(&r);
        let resp = grant_resp(&[
            ("Access-Control-Allow-Methods", "PUT"),
            ("Access-Control-Allow-Headers", "x-t"),
        ]);
        let g = validate_preflight(&r, &resp);
        let mut c = PreflightCache::new();
        if let Ok(g) = g {
            c.store("p", &r, &g, 0);
        }
        let _ = c.covers("p", &r, 1);
        let t = taint(&r).unwrap();
        let _ = validate_response(&r, t, &resp);
        let _ = filter_response(&r, t, &resp);
        let _ = is_forbidden_request_header("cookie", b"");
        let _ = is_forbidden_method("TRACE");
        trace::assert_all_hit(ENFORCEMENT_POINTS);
    }

    #[test]
    fn hostile_headers_never_panic() {
        let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..5000 {
            let mut hs = Vec::new();
            for _ in 0..(next() % 6) {
                let nl = (next() % 12) as usize;
                let vl = (next() % 200) as usize;
                let name: String = (0..nl)
                    .map(|_| (b'a' + (next() % 26) as u8) as char)
                    .collect();
                let value: Vec<u8> = (0..vl).map(|_| (next() & 0xff) as u8).collect();
                hs.push(Header { name, value });
            }
            let mut rq = simple();
            rq.headers = hs.clone();
            rq.credentials = if next() & 1 == 0 {
                CredentialsMode::Include
            } else {
                CredentialsMode::Omit
            };
            let _ = classify(&rq);
            let _ = preflight_request(&rq);
            let rs = CorsResponse {
                status: (next() % 600) as u16,
                headers: hs,
            };
            let _ = validate_preflight(&rq, &rs);
            let _ = cors_check(&rq, &rs.headers);
            let _ = filter_response(&rq, Tainting::Cors, &rs);
        }
    }
}
