//! The minimal URL and origin types the CSP engine needs.
//!
//! # Why this is not `px_net::partition::Origin`
//!
//! `px-net`'s `Origin` was considered first and rejected, for three reasons:
//!
//! 1. A path dependency from `px-websec` on `px-net` is cycle-free today, but
//!    it drags `rustls`, `ring` and `webpki-roots` into the crate that parses
//!    attacker-controlled headers. `px-websec`'s dependency set is empty on
//!    purpose (its `Cargo.toml` says so) and adding that closure needs an ADR.
//! 2. `px-net`'s `Origin::new` refuses every scheme except `http` and `https`
//!    and always carries an explicit port. CSP needs `ws`, `wss`, `ftp`, and
//!    to *represent* opaque origins (`data:`, sandboxed documents) so that
//!    `'self'` can fail closed against them.
//! 3. CSP matching needs a URL's path (for host-source path-parts), which an
//!    origin does not carry.
//!
//! The types here are therefore deliberately small. [`CspUrl::parse`] is
//! **not** a WHATWG URL parser: it accepts only absolute URLs that are already
//! in canonical form (ASCII, IDNA already applied, no whitespace) and returns
//! `None` for anything else. The network layer is expected to hand over URLs
//! it has already parsed; a `None` here means "do not load".

/// Upper bound on the URL length accepted by [`CspUrl::parse`].
pub const MAX_URL_BYTES: usize = 64 * 1024;

/// The default port of a scheme CSP knows about.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// Schemes whose URLs have a host and a hierarchical path.
pub fn is_special_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp")
}

/// A parsed absolute URL, reduced to what CSP looks at.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CspUrl {
    scheme: String,
    host: Option<String>,
    /// Explicit port, `None` when absent or equal to the scheme default.
    port: Option<u16>,
    /// Path as written (dot-segments removed for special schemes). Empty only
    /// for URLs without a hierarchical path.
    path: String,
}

fn is_scheme_char(c: char, first: bool) -> bool {
    if first {
        c.is_ascii_alphabetic()
    } else {
        c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')
    }
}

fn is_dot_segment(s: &str) -> bool {
    s == "." || s.eq_ignore_ascii_case("%2e")
}

fn is_double_dot_segment(s: &str) -> bool {
    s == ".."
        || s.eq_ignore_ascii_case(".%2e")
        || s.eq_ignore_ascii_case("%2e.")
        || s.eq_ignore_ascii_case("%2e%2e")
}

fn normalize_path(path: &str) -> String {
    let rest = path.strip_prefix('/').unwrap_or(path);
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = rest.split('/').collect();
    let last = segments.len().saturating_sub(1);
    for (i, seg) in segments.iter().enumerate() {
        if is_dot_segment(seg) {
            if i == last {
                out.push("");
            }
        } else if is_double_dot_segment(seg) {
            out.pop();
            if i == last {
                out.push("");
            }
        } else {
            out.push(seg);
        }
    }
    let mut s = String::with_capacity(path.len().saturating_add(1));
    s.push('/');
    s.push_str(&out.join("/"));
    s
}

fn valid_special_host(host: &str) -> bool {
    !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}

fn valid_ipv6_literal(host: &str) -> bool {
    host.len() >= 3
        && host.starts_with('[')
        && host.ends_with(']')
        && host
            .bytes()
            .skip(1)
            .take(host.len().saturating_sub(2))
            .all(|b| b.is_ascii_hexdigit() || matches!(b, b':' | b'.'))
        && host.contains(':')
}

impl CspUrl {
    /// Parse an absolute, already-canonical URL. `None` on anything else.
    pub fn parse(input: &str) -> Option<CspUrl> {
        if input.len() > MAX_URL_BYTES {
            return None;
        }
        // Reject controls, space and non-ASCII outright: a real URL parser
        // would strip or percent-encode these, and two parsers disagreeing
        // about them is a classic bypass. Callers canonicalise first.
        if input.bytes().any(|b| b <= 0x20 || b >= 0x7f) {
            return None;
        }
        let colon = input.find(':')?;
        let (scheme_raw, rest) = input.split_at(colon);
        let rest = rest.strip_prefix(':')?;
        let mut chars = scheme_raw.chars();
        let first = chars.next()?;
        if !is_scheme_char(first, true) || !chars.all(|c| is_scheme_char(c, false)) {
            return None;
        }
        let scheme = scheme_raw.to_ascii_lowercase();
        // The fragment never matters to CSP.
        let rest = rest.split('#').next().unwrap_or("");

        if is_special_scheme(&scheme) {
            // Backslash is a slash for special schemes.
            let rest: String = rest.replace('\\', "/");
            let after = rest.strip_prefix("//")?;
            let end = after.find(['/', '?']).unwrap_or(after.len());
            let (authority, tail) = after.split_at(end);
            // Userinfo is dropped (and never consulted by CSP).
            let hostport = authority.rsplit('@').next().unwrap_or("");
            let (host, port_str) = if hostport.starts_with('[') {
                let close = hostport.find(']')?;
                let (h, after_h) = hostport.split_at(close.saturating_add(1));
                let p = match after_h {
                    "" => None,
                    other => Some(other.strip_prefix(':')?),
                };
                (h, p)
            } else {
                match hostport.rfind(':') {
                    Some(i) => {
                        let (h, p) = hostport.split_at(i);
                        (h, p.strip_prefix(':'))
                    }
                    None => (hostport, None),
                }
            };
            let host = host.to_ascii_lowercase();
            if host.starts_with('[') {
                if !valid_ipv6_literal(&host) {
                    return None;
                }
            } else if !valid_special_host(&host) {
                return None;
            }
            let port = match port_str {
                None | Some("") => None,
                Some(p) => {
                    if p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    Some(p.parse::<u16>().ok()?)
                }
            };
            let port = match (port, default_port(&scheme)) {
                (Some(p), Some(d)) if p == d => None,
                (p, _) => p,
            };
            let path_part = tail.split('?').next().unwrap_or("");
            let path = if path_part.is_empty() {
                "/".to_string()
            } else {
                normalize_path(path_part)
            };
            return Some(CspUrl {
                scheme,
                host: Some(host),
                port,
                path,
            });
        }

        // Non-special: `scheme://authority/path` is kept as host-less (no
        // network origin); everything else is an opaque path.
        let path = rest.split('?').next().unwrap_or("").to_string();
        Some(CspUrl {
            scheme,
            host: None,
            port: None,
            path,
        })
    }

    /// Build from parts (used by tests and by callers that already hold a
    /// parsed URL). Returns `None` if the parts are not a valid special URL.
    pub fn from_parts(scheme: &str, host: &str, port: Option<u16>, path: &str) -> Option<CspUrl> {
        let p = match port {
            Some(p) => format!(":{p}"),
            None => String::new(),
        };
        CspUrl::parse(&format!("{scheme}://{host}{p}{path}"))
    }

    /// Lowercased scheme.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    /// Lowercased host, `None` for host-less URLs (`data:`, `javascript:`).
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }

    /// Explicit non-default port.
    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Port actually used: explicit, else the scheme default.
    pub fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }

    /// The path (starts with `/` for special schemes).
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The origin of this URL (opaque for host-less schemes).
    pub fn origin(&self) -> Origin {
        match (&self.host, is_special_scheme(&self.scheme)) {
            (Some(host), true) => match self.effective_port() {
                Some(port) => Origin::Tuple {
                    scheme: self.scheme.clone(),
                    host: host.clone(),
                    port,
                },
                None => Origin::Opaque,
            },
            _ => Origin::Opaque,
        }
    }

    /// Serialization without fragment, query and credentials, as used by
    /// violation reports for network URLs.
    pub fn serialize_stripped(&self) -> String {
        match &self.host {
            Some(h) => {
                let p = match self.port {
                    Some(p) => format!(":{p}"),
                    None => String::new(),
                };
                format!("{}://{}{}{}", self.scheme, h, p, self.path)
            }
            None => format!("{}:{}", self.scheme, self.path),
        }
    }
}

/// A web origin: a tuple, or opaque.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Origin {
    /// No network identity. Never same-origin with anything, itself included.
    Opaque,
    /// scheme, host, effective port.
    Tuple {
        /// Lowercased scheme.
        scheme: String,
        /// Lowercased host.
        host: String,
        /// Effective port.
        port: u16,
    },
}

impl Origin {
    /// Build a tuple origin from a URL string; opaque if unparsable.
    pub fn from_url_str(s: &str) -> Origin {
        CspUrl::parse(s).map_or(Origin::Opaque, |u| u.origin())
    }

    /// Same-origin: equal tuples. Opaque origins are never same-origin,
    /// which makes `'self'` fail closed for them.
    pub fn same_origin(&self, other: &Origin) -> bool {
        matches!((self, other), (Origin::Tuple { .. }, Origin::Tuple { .. })) && self == other
    }

    /// Scheme of a tuple origin.
    pub fn scheme(&self) -> Option<&str> {
        match self {
            Origin::Tuple { scheme, .. } => Some(scheme),
            Origin::Opaque => None,
        }
    }
}

/// Whether a host string is an IP-address literal (IPv6 in brackets, or
/// dotted-quad IPv4). Wildcard host-sources never match these.
pub fn is_ip_literal(host: &str) -> bool {
    if host.starts_with('[') {
        return true;
    }
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `host` is a loopback name or address (`localhost`,
/// `*.localhost`, `127.0.0.0/8`, `[::1]`).
pub fn is_loopback_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host == "[::1]" {
        return true;
    }
    is_ip_literal(host) && host.starts_with("127.")
}

/// Mixed Content's "potentially trustworthy URL", reduced to what a
/// no-network crate can decide: secure schemes, loopback, and the
/// non-network schemes (`data:`, `blob:`, `about:`, `file:`).
pub fn is_potentially_trustworthy(url: &CspUrl) -> bool {
    match url.scheme() {
        "https" | "wss" | "data" | "blob" | "about" | "file" | "filesystem" => true,
        "http" | "ws" => url.host().is_some_and(is_loopback_host),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_urls() {
        let u = CspUrl::parse("HTTPS://Example.COM:443/a/b?q=1#f").unwrap();
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.host(), Some("example.com"));
        assert_eq!(u.port(), None, "default port is normalised away");
        assert_eq!(u.effective_port(), Some(443));
        assert_eq!(u.path(), "/a/b");
        let u = CspUrl::parse("http://a.test:8080").unwrap();
        assert_eq!(u.path(), "/");
        assert_eq!(u.port(), Some(8080));
    }

    #[test]
    fn rejects_what_it_cannot_canonicalise() {
        for bad in [
            "",
            "example.com",
            "http//x",
            "http://",
            "http:///x",
            "http://a b/",
            "http://a\0b/",
            "http://exämple.com/",
            "http://a.com:99999/",
            "http://a.com:8x/",
            "http://[::1/",
            "http://[zz]/",
            "1http://a/",
            "http://a%2fb/",
        ] {
            assert!(CspUrl::parse(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn dot_segments_and_backslashes_are_resolved() {
        let u = CspUrl::parse("https://a.com/x/./y/../z/%2e%2E/w").unwrap();
        assert_eq!(u.path(), "/x/w");
        let u = CspUrl::parse("https://a.com\\p\\q").unwrap();
        assert_eq!(u.host(), Some("a.com"));
        assert_eq!(u.path(), "/p/q");
        let u = CspUrl::parse("https://a.com/..").unwrap();
        assert_eq!(u.path(), "/");
    }

    #[test]
    fn userinfo_does_not_become_the_host() {
        let u = CspUrl::parse("https://good.com@evil.com/").unwrap();
        assert_eq!(u.host(), Some("evil.com"));
        let u = CspUrl::parse("https://evil.com@good.com:444/").unwrap();
        assert_eq!(u.host(), Some("good.com"));
        assert_eq!(u.port(), Some(444));
    }

    #[test]
    fn host_less_schemes_are_opaque() {
        let u = CspUrl::parse("data:text/plain,hi").unwrap();
        assert_eq!(u.host(), None);
        assert_eq!(u.origin(), Origin::Opaque);
        assert!(!Origin::Opaque.same_origin(&Origin::Opaque));
        let u = CspUrl::parse("javascript:alert(1)").unwrap();
        assert_eq!(u.scheme(), "javascript");
    }

    #[test]
    fn origins_compare_by_tuple() {
        let a = CspUrl::parse("https://a.com/x").unwrap().origin();
        let b = CspUrl::parse("https://a.com:443/y").unwrap().origin();
        let c = CspUrl::parse("https://a.com:444/y").unwrap().origin();
        let d = CspUrl::parse("http://a.com/y").unwrap().origin();
        assert!(a.same_origin(&b));
        assert!(!a.same_origin(&c));
        assert!(!a.same_origin(&d));
    }

    #[test]
    fn trustworthiness() {
        let t = |s: &str| is_potentially_trustworthy(&CspUrl::parse(s).unwrap());
        assert!(t("https://a.com/"));
        assert!(t("http://localhost/"));
        assert!(t("http://127.0.0.1:8000/"));
        assert!(t("http://[::1]/"));
        assert!(t("wss://a.com/"));
        assert!(!t("http://a.com/"));
        assert!(!t("ws://a.com/"));
        assert!(!t("http://127.0.0.1.evil.com/"));
        assert!(!t("ftp://a.com/"));
    }

    #[test]
    fn ip_literals() {
        assert!(is_ip_literal("127.0.0.1"));
        assert!(is_ip_literal("[::1]"));
        assert!(!is_ip_literal("a.b.c.d"));
        assert!(!is_ip_literal("1.2.3"));
    }
}
