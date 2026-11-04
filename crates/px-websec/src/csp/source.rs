//! Source expressions and source lists (CSP3 §2.3, §6.7.2, §6.7.3).
//!
//! A source list is the value of a fetch directive: `'self' https://a.com
//! 'nonce-abc'`. [`SourceList::parse`] turns the tokens into typed
//! [`SourceExpr`]s (tokens that are not valid expressions become
//! [`SourceExpr::Unrecognized`], which never matches anything), and the
//! matching methods answer "does this URL / nonce / inline text match".
//!
//! Case rules, from the spec: keywords, scheme and host are ASCII
//! case-insensitive; the nonce value, the hash value and the path are
//! case-sensitive.

use super::sha2;
use super::url::{CspUrl, Origin, default_port, is_ip_literal};

/// A hash algorithm CSP allows in hash-source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlg {
    /// SHA-256.
    Sha256,
    /// SHA-384.
    Sha384,
    /// SHA-512.
    Sha512,
}

impl HashAlg {
    /// Parse the algorithm label (`sha256`, case-insensitive).
    pub fn parse(s: &str) -> Option<HashAlg> {
        if s.eq_ignore_ascii_case("sha256") {
            Some(HashAlg::Sha256)
        } else if s.eq_ignore_ascii_case("sha384") {
            Some(HashAlg::Sha384)
        } else if s.eq_ignore_ascii_case("sha512") {
            Some(HashAlg::Sha512)
        } else {
            None
        }
    }

    /// Lowercase label.
    pub fn name(self) -> &'static str {
        match self {
            HashAlg::Sha256 => "sha256",
            HashAlg::Sha384 => "sha384",
            HashAlg::Sha512 => "sha512",
        }
    }

    /// Digest length in bytes.
    pub fn digest_len(self) -> usize {
        match self {
            HashAlg::Sha256 => 32,
            HashAlg::Sha384 => 48,
            HashAlg::Sha512 => 64,
        }
    }

    /// Digest of `data`.
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            HashAlg::Sha256 => sha2::sha256(data).to_vec(),
            HashAlg::Sha384 => sha2::sha384(data).to_vec(),
            HashAlg::Sha512 => sha2::sha512(data).to_vec(),
        }
    }
}

/// A digest named by an integrity attribute or a hash-source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashValue {
    /// Algorithm.
    pub alg: HashAlg,
    /// Raw digest bytes (decoded from base64 / base64url).
    pub digest: Vec<u8>,
}

impl HashValue {
    /// Parse `sha256-<base64>` (the form used by both hash-source and SRI
    /// integrity metadata, without quotes).
    pub fn parse(s: &str) -> Option<HashValue> {
        let (alg, value) = s.split_once('-')?;
        let alg = HashAlg::parse(alg)?;
        let digest = decode_base64(value)?;
        if digest.len() != alg.digest_len() {
            return None;
        }
        Some(HashValue { alg, digest })
    }
}

/// Decode base64 in either the standard or the URL-safe alphabet, with
/// optional trailing `=` (at most two). `None` on any other byte.
pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let trimmed = s
        .strip_suffix("==")
        .or_else(|| s.strip_suffix('='))
        .unwrap_or(s);
    if trimmed.contains('=') || trimmed.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(trimmed.len() / 4 * 3 + 3);
    let mut acc: u32 = 0;
    let mut bits: u32 = 0;
    for b in trimmed.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Keyword sources (quoted, case-insensitive).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    /// `'none'`
    None,
    /// `'self'`
    SelfOrigin,
    /// `'unsafe-inline'`
    UnsafeInline,
    /// `'unsafe-eval'`
    UnsafeEval,
    /// `'strict-dynamic'`
    StrictDynamic,
    /// `'unsafe-hashes'`
    UnsafeHashes,
    /// `'report-sample'`
    ReportSample,
    /// `'unsafe-allow-redirects'` (parsed; not acted on, see crate docs)
    UnsafeAllowRedirects,
    /// `'wasm-unsafe-eval'`
    WasmUnsafeEval,
    /// `'inline-speculation-rules'` (parsed; not acted on)
    InlineSpeculationRules,
}

fn keyword(inner: &str) -> Option<Keyword> {
    const TABLE: [(&str, Keyword); 10] = [
        ("none", Keyword::None),
        ("self", Keyword::SelfOrigin),
        ("unsafe-inline", Keyword::UnsafeInline),
        ("unsafe-eval", Keyword::UnsafeEval),
        ("strict-dynamic", Keyword::StrictDynamic),
        ("unsafe-hashes", Keyword::UnsafeHashes),
        ("report-sample", Keyword::ReportSample),
        ("unsafe-allow-redirects", Keyword::UnsafeAllowRedirects),
        ("wasm-unsafe-eval", Keyword::WasmUnsafeEval),
        ("inline-speculation-rules", Keyword::InlineSpeculationRules),
    ];
    TABLE
        .iter()
        .find(|(n, _)| inner.eq_ignore_ascii_case(n))
        .map(|&(_, k)| k)
}

/// Port part of a host-source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortPart {
    /// No port written.
    Absent,
    /// `:*`
    Any,
    /// `:443`
    Num(u16),
}

/// A host-source expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSource {
    /// Lowercased scheme-part, if written.
    pub scheme: Option<String>,
    /// Lowercased host-part: `*`, `*.example.com` or `example.com`.
    pub host: String,
    /// Port-part.
    pub port: PortPart,
    /// Path-part (case-sensitive, starts with `/`), if written.
    pub path: Option<String>,
}

/// One parsed source expression.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceExpr {
    /// A quoted keyword.
    Keyword(Keyword),
    /// `'nonce-…'`, value case-sensitive.
    Nonce(String),
    /// `'sha256-…'` etc.
    Hash(HashValue),
    /// `https:` (lowercased, without the colon).
    Scheme(String),
    /// A host-source.
    Host(HostSource),
    /// Anything that is not a valid expression. Never matches.
    Unrecognized,
}

fn is_scheme(s: &str) -> bool {
    let mut it = s.chars();
    match it.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    it.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn is_base64_value(s: &str) -> bool {
    let body = s.trim_end_matches('=');
    let pad = s.len().saturating_sub(body.len());
    !body.is_empty()
        && pad <= 2
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'-' | b'_'))
}

fn valid_host_part(h: &str) -> bool {
    if h == "*" {
        return true;
    }
    let body = h.strip_prefix("*.").unwrap_or(h);
    !body.is_empty()
        && body
            .split('.')
            .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

/// Parse one token of a source list.
pub fn parse_source(token: &str) -> SourceExpr {
    if let Some(inner) = token.strip_prefix('\'') {
        let Some(inner) = inner.strip_suffix('\'') else {
            return SourceExpr::Unrecognized;
        };
        if let Some(k) = keyword(inner) {
            return SourceExpr::Keyword(k);
        }
        if inner
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("nonce-"))
        {
            let value = inner.get(6..).unwrap_or("");
            return if is_base64_value(value) {
                SourceExpr::Nonce(value.to_string())
            } else {
                SourceExpr::Unrecognized
            };
        }
        return HashValue::parse(inner).map_or(SourceExpr::Unrecognized, SourceExpr::Hash);
    }
    if let Some(s) = token.strip_suffix(':') {
        return if is_scheme(s) {
            SourceExpr::Scheme(s.to_ascii_lowercase())
        } else {
            SourceExpr::Unrecognized
        };
    }
    parse_host_source(token).map_or(SourceExpr::Unrecognized, SourceExpr::Host)
}

fn parse_host_source(token: &str) -> Option<HostSource> {
    if token.contains(['?', '#']) {
        return None;
    }
    let (scheme, rest) = match token.split_once("://") {
        Some((s, r)) if is_scheme(s) => (Some(s.to_ascii_lowercase()), r),
        Some(_) => return None,
        None => (None, token),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => {
            let (a, p) = rest.split_at(i);
            (a, Some(p.to_string()))
        }
        None => (rest, None),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, "*")) => (h, PortPart::Any),
        Some((h, p)) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            (h, PortPart::Num(p.parse::<u16>().ok()?))
        }
        None => (authority, PortPart::Absent),
    };
    if !valid_host_part(host) {
        return None;
    }
    Some(HostSource {
        scheme,
        host: host.to_ascii_lowercase(),
        port,
        path,
    })
}

/// CSP3 §6.7.2.5: does the scheme in a policy match the scheme of a URL?
/// Allows the secure upgrade `http`→`https`, `ws`→`wss`/`http`/`https`,
/// `wss`→`https`.
pub fn scheme_part_matches(policy_scheme: &str, url_scheme: &str) -> bool {
    policy_scheme == url_scheme
        || (policy_scheme == "http" && url_scheme == "https")
        || (policy_scheme == "ws" && matches!(url_scheme, "wss" | "http" | "https"))
        || (policy_scheme == "wss" && url_scheme == "https")
}

fn is_network_scheme(s: &str) -> bool {
    matches!(s, "ftp" | "http" | "https" | "ws" | "wss")
}

fn percent_decode(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0usize;
    while let Some(&c) = b.get(i) {
        if c == b'%' {
            let hi = b.get(i + 1).and_then(|&h| (h as char).to_digit(16));
            let lo = b.get(i + 2).and_then(|&l| (l as char).to_digit(16));
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(c);
        i += 1;
    }
    out
}

/// CSP3 §6.7.2.12 path-part matching. A policy path ending in `/` is a
/// prefix match on whole segments; otherwise it is exact. Segments are
/// compared after percent-decoding (the split happens first, so `%2F` does
/// not act as a separator).
pub fn path_part_matches(policy_path: &str, url_path: &str) -> bool {
    if policy_path.is_empty() {
        return true;
    }
    let a_rest = policy_path.strip_prefix('/').unwrap_or(policy_path);
    let b_rest = url_path.strip_prefix('/').unwrap_or(url_path);
    let exact = !policy_path.ends_with('/');
    let mut a: Vec<&str> = a_rest.split('/').collect();
    let b: Vec<&str> = b_rest.split('/').collect();
    if a.len() > b.len() {
        return false;
    }
    if exact && a.len() != b.len() {
        return false;
    }
    if !exact {
        a.pop();
    }
    a.iter()
        .zip(b.iter())
        .all(|(x, y)| percent_decode(x) == percent_decode(y))
}

/// CSP3 §6.7.2.9 host-part matching. A wildcard never matches an IP
/// literal; `*.example.com` does not match `example.com` itself.
pub fn host_part_matches(policy_host: &str, url_host: &str) -> bool {
    if policy_host == "*" {
        return true;
    }
    if let Some(remaining) = policy_host.strip_prefix('*') {
        // remaining starts with '.', guaranteed by the grammar.
        return !is_ip_literal(url_host)
            && url_host.len() > remaining.len()
            && url_host.to_ascii_lowercase().ends_with(remaining);
    }
    policy_host == url_host.to_ascii_lowercase()
}

/// `'self'` matching (CSP3 §6.7.2.7): same origin, or the same host with the
/// scheme upgraded (`http`→`https`/`ws`/`wss`, `https`→`wss`) and each port
/// either identical or the scheme default. Opaque origins match nothing.
pub fn self_matches(url: &CspUrl, origin: &Origin) -> bool {
    let Origin::Tuple {
        scheme: o_scheme,
        host: o_host,
        port: o_port,
    } = origin
    else {
        return false;
    };
    if origin.same_origin(&url.origin()) {
        return true;
    }
    let Some(u_host) = url.host() else {
        return false;
    };
    if u_host != o_host {
        return false;
    }
    let u_scheme = url.scheme();
    let upgraded = (o_scheme == "http" && matches!(u_scheme, "https" | "ws" | "wss"))
        || (o_scheme == "https" && u_scheme == "wss");
    if !upgraded {
        return false;
    }
    let Some(u_port) = url.effective_port() else {
        return false;
    };
    *o_port == u_port
        || (default_port(o_scheme) == Some(*o_port) && default_port(u_scheme) == Some(u_port))
}

/// The inline-behaviour categories of CSP3 §6.7.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InlineType {
    /// `<script>` element.
    Script,
    /// Event-handler content attribute.
    ScriptAttribute,
    /// `<style>` element.
    Style,
    /// `style=""` attribute.
    StyleAttribute,
    /// `javascript:` navigation.
    Navigation,
}

/// A parsed source list.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceList {
    exprs: Vec<SourceExpr>,
}

impl SourceList {
    /// Parse the value tokens of a directive.
    pub fn parse<S: AsRef<str>>(tokens: &[S]) -> SourceList {
        SourceList {
            exprs: tokens.iter().map(|t| parse_source(t.as_ref())).collect(),
        }
    }

    /// The parsed expressions.
    pub fn exprs(&self) -> &[SourceExpr] {
        &self.exprs
    }

    /// Empty, or exactly `'none'`: matches nothing (CSP3 §6.7.2.6).
    pub fn is_none(&self) -> bool {
        matches!(
            self.exprs.as_slice(),
            [] | [SourceExpr::Keyword(Keyword::None)]
        )
    }

    /// Whether the keyword occurs.
    pub fn has_keyword(&self, k: Keyword) -> bool {
        self.exprs.contains(&SourceExpr::Keyword(k))
    }

    /// Whether any nonce-source or hash-source occurs.
    pub fn has_nonce_or_hash(&self) -> bool {
        self.exprs
            .iter()
            .any(|e| matches!(e, SourceExpr::Nonce(_) | SourceExpr::Hash(_)))
    }

    /// Whether any hash-source occurs.
    pub fn has_hash(&self) -> bool {
        self.exprs.iter().any(|e| matches!(e, SourceExpr::Hash(_)))
    }

    /// CSP3 §6.7.3.2: a non-empty nonce equal (case-sensitively) to a
    /// nonce-source.
    pub fn nonce_matches(&self, nonce: &str) -> bool {
        !nonce.is_empty()
            && self
                .exprs
                .iter()
                .any(|e| matches!(e, SourceExpr::Nonce(n) if n == nonce))
    }

    /// Whether a hash-source with this algorithm and digest occurs.
    pub fn contains_hash(&self, h: &HashValue) -> bool {
        self.exprs
            .iter()
            .any(|e| matches!(e, SourceExpr::Hash(x) if x == h))
    }

    /// Whether the SHA-2 digest of `text` (UTF-8) equals any hash-source.
    pub fn hash_matches_text(&self, text: &str) -> bool {
        if !self.has_hash() {
            return false;
        }
        [HashAlg::Sha256, HashAlg::Sha384, HashAlg::Sha512]
            .into_iter()
            .any(|alg| {
                let wanted = self
                    .exprs
                    .iter()
                    .any(|e| matches!(e, SourceExpr::Hash(h) if h.alg == alg));
                wanted
                    && self.contains_hash(&HashValue {
                        alg,
                        digest: alg.digest(text.as_bytes()),
                    })
            })
    }

    /// CSP3 §6.7.2.7 "Does url match source list in origin with redirect
    /// count". `redirect_count > 0` makes path-parts irrelevant.
    pub fn matches_url(&self, url: &CspUrl, origin: &Origin, redirect_count: u32) -> bool {
        if self.is_none() {
            return false;
        }
        self.exprs
            .iter()
            .any(|e| expr_matches_url(e, url, origin, redirect_count))
    }

    /// CSP3 §6.7.3.3 "Does a source list allow all inline behavior for
    /// type". A nonce or hash anywhere disables `'unsafe-inline'`; for
    /// script-like types `'strict-dynamic'` does too.
    pub fn allows_all_inline(&self, ty: InlineType) -> bool {
        let script_like = matches!(
            ty,
            InlineType::Script | InlineType::ScriptAttribute | InlineType::Navigation
        );
        let mut allow = false;
        for e in &self.exprs {
            match e {
                SourceExpr::Nonce(_) | SourceExpr::Hash(_) => return false,
                SourceExpr::Keyword(Keyword::StrictDynamic) if script_like => return false,
                SourceExpr::Keyword(Keyword::UnsafeInline) => allow = true,
                _ => {}
            }
        }
        allow
    }
}

fn expr_matches_url(e: &SourceExpr, url: &CspUrl, origin: &Origin, redirects: u32) -> bool {
    match e {
        SourceExpr::Keyword(Keyword::SelfOrigin) => self_matches(url, origin),
        SourceExpr::Keyword(_) | SourceExpr::Nonce(_) | SourceExpr::Hash(_) => false,
        SourceExpr::Unrecognized => false,
        SourceExpr::Scheme(s) => scheme_part_matches(s, url.scheme()),
        SourceExpr::Host(h) => host_source_matches(h, url, origin, redirects),
    }
}

fn host_source_matches(h: &HostSource, url: &CspUrl, origin: &Origin, redirects: u32) -> bool {
    // The bare `*`: any network-scheme URL, or a URL with the page's own
    // scheme (so `*` does not cover data:/blob:/javascript:).
    if h.host == "*" && h.scheme.is_none() && h.port == PortPart::Absent && h.path.is_none() {
        return is_network_scheme(url.scheme()) || origin.scheme() == Some(url.scheme());
    }
    let scheme_ok = match &h.scheme {
        Some(s) => scheme_part_matches(s, url.scheme()),
        None => origin
            .scheme()
            .is_some_and(|os| scheme_part_matches(os, url.scheme())),
    };
    if !scheme_ok {
        return false;
    }
    let Some(host) = url.host() else {
        return false;
    };
    if !host_part_matches(&h.host, host) {
        return false;
    }
    let port_ok = match h.port {
        PortPart::Any => true,
        PortPart::Num(n) => url.effective_port() == Some(n),
        PortPart::Absent => url.port().is_none(),
    };
    if !port_ok {
        return false;
    }
    match &h.path {
        Some(p) if redirects == 0 => path_part_matches(p, url.path()),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> CspUrl {
        CspUrl::parse(s).unwrap()
    }
    fn list(s: &str) -> SourceList {
        SourceList::parse(&s.split_ascii_whitespace().collect::<Vec<_>>())
    }
    fn page() -> Origin {
        Origin::from_url_str("https://page.example/")
    }
    fn m(l: &str, u: &str) -> bool {
        list(l).matches_url(&url(u), &page(), 0)
    }

    #[test]
    fn keyword_sources_are_case_insensitive() {
        for k in ["'SELF'", "'Self'", "'self'"] {
            assert_eq!(
                parse_source(k),
                SourceExpr::Keyword(Keyword::SelfOrigin),
                "{k}"
            );
        }
        assert_eq!(
            parse_source("self"),
            SourceExpr::Host(HostSource {
                scheme: None,
                host: "self".into(),
                port: PortPart::Absent,
                path: None
            })
        );
        assert_eq!(parse_source("'selfx'"), SourceExpr::Unrecognized);
        assert_eq!(parse_source("'self"), SourceExpr::Unrecognized);
    }

    #[test]
    fn nonce_and_hash_grammar() {
        assert_eq!(
            parse_source("'nonce-AbC+/=='"),
            SourceExpr::Nonce("AbC+/==".into())
        );
        assert_eq!(
            parse_source("'NONCE-abc_-'"),
            SourceExpr::Nonce("abc_-".into())
        );
        assert_eq!(parse_source("'nonce-'"), SourceExpr::Unrecognized);
        assert_eq!(parse_source("'nonce-a b'"), SourceExpr::Unrecognized);
        assert_eq!(parse_source("'nonce-a===' "), SourceExpr::Unrecognized);
        // hash of "" in SHA-256, base64.
        let h = "'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='";
        assert!(matches!(parse_source(h), SourceExpr::Hash(_)));
        // wrong digest length for the algorithm
        assert_eq!(
            parse_source("'sha512-47DEQpj8HBSa='"),
            SourceExpr::Unrecognized
        );
        assert_eq!(parse_source("'md5-AAAA'"), SourceExpr::Unrecognized);
        // base64url alphabet and missing padding decode to the same digest
        let a = parse_source("'sha256-47DEQpj8HBSa-_TImW-5JCeuQeRkm5NMpJWZG3hSuFU'");
        let b = parse_source("'sha256-47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU='");
        assert_eq!(a, b);
    }

    #[test]
    fn scheme_source() {
        assert_eq!(parse_source("HTTPS:"), SourceExpr::Scheme("https".into()));
        assert_eq!(parse_source("1x:"), SourceExpr::Unrecognized);
        assert_eq!(parse_source(":"), SourceExpr::Unrecognized);
        assert!(m("https:", "https://anything.test/x"));
        assert!(!m("https:", "http://anything.test/x"));
        assert!(
            m("http:", "https://anything.test/x"),
            "http: allows upgrade"
        );
        assert!(m("ws:", "wss://a.test/"));
        assert!(m("ws:", "https://a.test/"));
        assert!(!m("wss:", "http://a.test/"));
        assert!(m("data:", "data:text/plain,x"));
        assert!(!m("data:", "javascript:alert(1)"));
    }

    #[test]
    fn host_source_grammar_rejects_garbage() {
        for bad in [
            "exa mple.com",
            "*.",
            "a.*.com",
            "**",
            "*example.com",
            "a..b",
            "a.com:99999",
            "a.com:8x",
            "a.com?x",
            "a.com#x",
            "'weird'",
            "http://",
            "://a.com",
            "ex_ample.com",
        ] {
            assert_eq!(parse_source(bad), SourceExpr::Unrecognized, "{bad}");
        }
    }

    #[test]
    fn trailing_colon_is_a_scheme_source_even_for_dotted_names() {
        // "a.com:" is a valid scheme-source (scheme chars include '.'): the
        // grammar is ambiguous and the scheme reading wins. It matches only
        // URLs whose scheme is literally "a.com", i.e. nothing real.
        assert_eq!(parse_source("a.com:"), SourceExpr::Scheme("a.com".into()));
        assert!(!m("a.com:", "https://a.com/"));
    }

    #[test]
    fn none_matches_nothing() {
        assert!(!m("'none'", "https://page.example/"));
        assert!(!m("", "https://page.example/"));
        // 'none' alongside other sources is simply ignored.
        assert!(m("'none' https://a.test", "https://a.test/"));
        assert!(list("'NONE'").is_none());
        assert!(list("").is_none());
        assert!(!list("'none' 'none'").is_none());
    }

    #[test]
    fn self_matching() {
        assert!(m("'self'", "https://page.example/x"));
        assert!(m("'self'", "https://page.example:443/x"));
        assert!(!m("'self'", "https://page.example:8443/x"));
        assert!(!m("'self'", "http://page.example/x"), "downgrade");
        assert!(!m("'self'", "https://sub.page.example/"));
        assert!(m("'self'", "wss://page.example/sock"));
        let http_page = Origin::from_url_str("http://page.example/");
        let l = list("'self'");
        assert!(l.matches_url(&url("https://page.example/"), &http_page, 0));
        assert!(l.matches_url(&url("ws://page.example/"), &http_page, 0));
        assert!(l.matches_url(&url("wss://page.example/"), &http_page, 0));
        let p8080 = Origin::from_url_str("http://page.example:8080/");
        assert!(!l.matches_url(&url("https://page.example/"), &p8080, 0));
        assert!(l.matches_url(&url("http://page.example:8080/z"), &p8080, 0));
        assert!(!l.matches_url(&url("data:text/plain,x"), &page(), 0));
        assert!(!l.matches_url(&url("https://page.example/"), &Origin::Opaque, 0));
    }

    #[test]
    fn host_source_basic_and_wildcards() {
        assert!(m("a.test", "https://a.test/x"));
        assert!(m("A.TEST", "https://a.test/x"));
        assert!(!m("a.test", "https://b.test/x"));
        assert!(!m("a.test", "https://sub.a.test/x"));
        assert!(m("*.a.test", "https://sub.a.test/x"));
        assert!(m("*.a.test", "https://x.y.a.test/x"));
        assert!(!m("*.a.test", "https://a.test/x"), "wildcard needs a label");
        assert!(!m("*.a.test", "https://evila.test/x"));
        assert!(!m("*.a.test", "https://a.test.evil.test/x"));
        // the page's scheme is the default scheme-part
        assert!(!m("a.test", "http://a.test/x"), "https page, http url");
        assert!(!m("a.test", "ws://a.test/x"));
        assert!(m("http://a.test", "https://a.test/x"));
        assert!(!m("https://a.test", "http://a.test/x"));
        assert!(m("a.test", "https://A.Test/x"));
    }

    #[test]
    fn wildcards_never_match_ip_literals() {
        assert!(!m("*.0.0.1", "https://127.0.0.1/"));
        assert!(!m("*.0.1", "https://127.0.0.1/"));
        assert!(m("127.0.0.1", "https://127.0.0.1/"));
        assert!(m("*", "https://127.0.0.1/"));
        assert!(!m("*.1", "https://[::1]/"));
    }

    #[test]
    fn star_source() {
        assert!(m("*", "https://a.test/"));
        assert!(m("*", "http://a.test/"));
        assert!(m("*", "ws://a.test/"));
        assert!(m("*", "ftp://a.test/"));
        assert!(!m("*", "data:text/plain,x"));
        assert!(!m("*", "javascript:alert(1)"));
        assert!(!m("*", "blob:https://a.test/uuid"));
        let data_page = Origin::from_url_str("data:text/html,x");
        assert!(!list("*").matches_url(&url("data:text/plain,x"), &data_page, 0));
    }

    #[test]
    fn port_matching() {
        assert!(m("a.test", "https://a.test/"));
        assert!(m("a.test", "https://a.test:443/"));
        assert!(!m("a.test", "https://a.test:8443/"));
        assert!(m("a.test:8443", "https://a.test:8443/"));
        assert!(!m("a.test:8443", "https://a.test/"));
        assert!(m("a.test:443", "https://a.test/"), "443 is the default");
        assert!(m("a.test:*", "https://a.test:1234/"));
        assert!(m("a.test:*", "https://a.test/"));
        assert!(!m("a.test:80", "https://a.test/"));
    }

    // CSP3 §6.7.2.12 examples: "/subdirectory/" is a prefix, "/file.js" exact.
    #[test]
    fn path_matching() {
        assert!(m("a.test/", "https://a.test/anything/at/all"));
        assert!(m("a.test/", "https://a.test/"));
        assert!(m("a.test/scripts/", "https://a.test/scripts/x.js"));
        assert!(m("a.test/scripts/", "https://a.test/scripts/d/x.js"));
        assert!(!m("a.test/scripts/", "https://a.test/scriptsx/x.js"));
        assert!(!m("a.test/scripts/", "https://a.test/other/x.js"));
        assert!(m("a.test/x.js", "https://a.test/x.js"));
        assert!(!m("a.test/x.js", "https://a.test/x.js/more"));
        assert!(!m("a.test/x.js", "https://a.test/x.jsx"));
        assert!(
            !m("a.test/x.js", "https://a.test/X.js"),
            "path is case-sensitive"
        );
        assert!(
            m("a.test/x.js", "https://a.test/x.js?q=1#f"),
            "query ignored"
        );
        assert!(!m("a.test/x.js", "https://a.test/"));
        // exact path with trailing slash on the url does not match
        assert!(!m("a.test/x", "https://a.test/x/"));
        // dot segments are resolved by the url parser before matching
        assert!(!m(
            "a.test/scripts/",
            "https://a.test/scripts/../secret/x.js"
        ));
    }

    #[test]
    fn path_percent_decoding() {
        assert!(m("a.test/a%20b", "https://a.test/a%20b"));
        assert!(m(
            "a.test/a b".replace(' ', "%20").as_str(),
            "https://a.test/a%20b"
        ));
        assert!(m("a.test/%7Euser/", "https://a.test/~user/x"));
        assert!(m("a.test/~user/", "https://a.test/%7euser/x"));
        // %2F does not act as a separator
        assert!(!m("a.test/a/b", "https://a.test/a%2Fb"));
        assert!(m("a.test/a%2Fb", "https://a.test/a%2fb"));
        // malformed escapes compare literally
        assert!(m("a.test/%zz", "https://a.test/%zz"));
        assert!(!m("a.test/%zz", "https://a.test/%7a"));
    }

    #[test]
    fn redirects_ignore_paths() {
        let l = list("a.test/only/this.js");
        assert!(!l.matches_url(&url("https://a.test/other.js"), &page(), 0));
        assert!(l.matches_url(&url("https://a.test/other.js"), &page(), 1));
        assert!(!l.matches_url(&url("https://b.test/other.js"), &page(), 1));
    }

    #[test]
    fn unrecognized_tokens_never_match() {
        assert!(!m("bogus://", "https://a.test/"));
        assert!(!m("'unsafe-inline'", "https://page.example/"));
        assert!(!m("'nonce-abc'", "https://page.example/"));
    }

    #[test]
    fn nonce_comparison_is_case_sensitive_and_nonempty() {
        let l = list("'nonce-AbC'");
        assert!(l.nonce_matches("AbC"));
        assert!(!l.nonce_matches("abc"));
        assert!(!l.nonce_matches(""));
        assert!(!l.nonce_matches("AbC "));
        assert!(!list("'nonce-abc'").nonce_matches(""));
    }

    #[test]
    fn hash_matching() {
        // FIPS 180-4 "abc"
        let l = list("'sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='");
        assert!(l.hash_matches_text("abc"));
        assert!(!l.hash_matches_text("abd"));
        assert!(!l.hash_matches_text(""));
        // the same digest written in base64url without padding
        let l = list("'sha256-ungWv48Bz-pBQUDeXa4iI7ADYaOWF3qctBD_YfIAFa0'");
        assert!(l.hash_matches_text("abc"));
        // algorithm label must agree: a sha512 expression does not match
        // a sha256 digest
        let l = list(
            "'sha512-3a81oZNherrMQXNJriBBMRLm+k6JqX6iCp7u5ktV05ohkpkqJ0/BqDa6PCOj/uu9RU1EI2Q86A4qmslPpUyknw=='",
        );
        assert!(l.hash_matches_text("abc"));
        assert!(!l.hash_matches_text("abcd"));
        // case: the algorithm label is case-insensitive, the value is not
        let l = list("'SHA256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='");
        assert!(l.hash_matches_text("abc"));
    }

    #[test]
    fn allow_all_inline_rules() {
        let a = |s: &str, t| list(s).allows_all_inline(t);
        use InlineType::*;
        assert!(a("'unsafe-inline'", Script));
        assert!(a("'unsafe-inline'", Style));
        assert!(!a("", Script));
        assert!(!a("'self'", Script));
        // a nonce or hash switches 'unsafe-inline' off, for every type
        for t in [Script, ScriptAttribute, Style, StyleAttribute, Navigation] {
            assert!(!a("'unsafe-inline' 'nonce-abc'", t), "{t:?}");
            assert!(
                !a(
                    "'unsafe-inline' 'sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0='",
                    t
                ),
                "{t:?}"
            );
        }
        // strict-dynamic switches it off for script-like types only
        for t in [Script, ScriptAttribute, Navigation] {
            assert!(!a("'unsafe-inline' 'strict-dynamic'", t), "{t:?}");
        }
        assert!(a("'unsafe-inline' 'strict-dynamic'", Style));
        assert!(a("'unsafe-inline' 'strict-dynamic'", StyleAttribute));
        // an invalid nonce expression does not disable unsafe-inline
        assert!(a("'unsafe-inline' 'nonce-'", Script));
    }

    #[test]
    fn base64_decoder_edges() {
        assert_eq!(decode_base64("").unwrap(), Vec::<u8>::new());
        assert_eq!(decode_base64("YQ==").unwrap(), b"a");
        assert_eq!(decode_base64("YQ").unwrap(), b"a");
        assert_eq!(decode_base64("YWI=").unwrap(), b"ab");
        assert!(decode_base64("Y").is_none());
        assert!(decode_base64("Y===").is_none());
        assert!(decode_base64("YQ=a").is_none());
        assert!(decode_base64("YQ\0=").is_none());
        assert!(decode_base64("Y Q=").is_none());
    }
}
