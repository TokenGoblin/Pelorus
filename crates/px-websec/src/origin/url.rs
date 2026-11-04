//! A deliberately small absolute-URL type.
//!
//! This is *not* the browser's URL parser. It exists so the policy modules can
//! be written and tested against something typed, without a dependency. The
//! real parser (when it lands) should produce this type or replace it.
//!
//! Deviations from the WHATWG URL Standard, all fail-closed (`None`):
//! - only absolute URLs; no base resolution;
//! - hosts must already be ASCII (IDNA/punycode is the caller's job);
//! - the path is kept as written: no dot-segment removal, no re-encoding;
//! - userinfo is kept as written.
//!
//! Input is bounded by [`MAX_URL_BYTES`].

/// Longest URL accepted. Longer input is refused, not truncated.
pub const MAX_URL_BYTES: usize = 8192;

/// A parsed host.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Host {
    /// A lowercase ASCII domain.
    Domain(String),
    /// IPv4 address.
    Ipv4([u8; 4]),
    /// IPv6 address.
    Ipv6([u16; 8]),
    /// Host of a non-special URL: validated, not interpreted.
    Opaque(String),
    /// The empty host (`file:///x`).
    Empty,
}

impl Host {
    /// Serialisation as in a URL (IPv6 in brackets).
    pub fn serialize(&self) -> String {
        match self {
            Host::Domain(d) | Host::Opaque(d) => d.clone(),
            Host::Ipv4(a) => format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3]),
            Host::Ipv6(p) => format!("[{}]", serialize_ipv6(p)),
            Host::Empty => String::new(),
        }
    }
}

fn serialize_ipv6(p: &[u16; 8]) -> String {
    // Longest run (>= 2) of zero pieces, first on ties.
    let mut best: Option<(usize, usize)> = None;
    let mut i = 0;
    while i < 8 {
        if p.get(i) == Some(&0) {
            let mut j = i;
            while p.get(j) == Some(&0) {
                j += 1;
            }
            let len = j - i;
            if len >= 2 && best.is_none_or(|(_, bl)| len > bl) {
                best = Some((i, len));
            }
            i = j;
        } else {
            i += 1;
        }
    }
    let mut out = String::new();
    let mut i = 0;
    while i < 8 {
        if let Some((s, l)) = best
            && s == i
        {
            out.push_str("::");
            i += l;
            continue;
        }
        if let Some(v) = p.get(i) {
            out.push_str(&format!("{v:x}"));
        }
        i += 1;
        if i < 8 && best.is_none_or(|(s, _)| s != i) {
            out.push(':');
        }
    }
    out
}

/// A parsed absolute URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    scheme: String,
    username: String,
    password: String,
    host: Option<Host>,
    port: Option<u16>,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

/// Default port of a special scheme.
pub fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        "ftp" => Some(21),
        _ => None,
    }
}

/// Whether `scheme` is a WHATWG "special" scheme.
pub fn is_special_scheme(scheme: &str) -> bool {
    matches!(scheme, "http" | "https" | "ws" | "wss" | "ftp" | "file")
}

impl Url {
    /// Parse an absolute URL; `None` for anything this type cannot represent.
    pub fn parse(input: &str) -> Option<Url> {
        if input.len() > MAX_URL_BYTES {
            return None;
        }
        let trimmed = input.trim_matches(|c: char| c <= ' ');
        let cleaned: String = trimmed
            .chars()
            .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
            .collect();
        let (scheme, rest) = split_scheme(&cleaned)?;
        let (rest, fragment) = match rest.split_once('#') {
            Some((a, f)) => (a, Some(f.to_string())),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q.to_string())),
            None => (rest, None),
        };
        let special = is_special_scheme(&scheme);
        let is_slash = |c: char| c == '/' || (special && c == '\\');

        let mut url = Url {
            scheme: scheme.clone(),
            username: String::new(),
            password: String::new(),
            host: None,
            port: None,
            path: String::new(),
            query,
            fragment,
        };

        let after_auth: &str;
        if scheme == "file" {
            let mut it = rest.chars();
            let a = it.next();
            let b = it.next();
            if a.is_some_and(is_slash) && b.is_some_and(is_slash) {
                let body = rest.get(2..)?;
                let end = body.find(is_slash).unwrap_or(body.len());
                let (auth, tail) = body.split_at_checked(end)?;
                url.host = Some(if auth.is_empty() {
                    Host::Empty
                } else {
                    match parse_host(auth, true)? {
                        Host::Domain(d) if d == "localhost" => Host::Empty,
                        h => h,
                    }
                });
                after_auth = tail;
            } else {
                url.host = Some(Host::Empty);
                after_auth = rest;
            }
        } else if special {
            let body = rest.trim_start_matches(is_slash);
            let end = body.find(is_slash).unwrap_or(body.len());
            let (auth, tail) = body.split_at_checked(end)?;
            parse_authority(&mut url, auth, true)?;
            after_auth = tail;
        } else if let Some(body) = rest.strip_prefix("//") {
            let end = body.find('/').unwrap_or(body.len());
            let (auth, tail) = body.split_at_checked(end)?;
            parse_authority(&mut url, auth, false)?;
            after_auth = tail;
        } else {
            after_auth = rest;
        }

        url.path = if special {
            let p: String = after_auth
                .chars()
                .map(|c| if c == '\\' { '/' } else { c })
                .collect();
            if p.is_empty() { "/".to_string() } else { p }
        } else {
            after_auth.to_string()
        };
        if let (Some(p), Some(d)) = (url.port, default_port(&url.scheme))
            && p == d
        {
            url.port = None;
        }
        Some(url)
    }

    /// Lowercase scheme, no colon.
    pub fn scheme(&self) -> &str {
        &self.scheme
    }
    /// Username as written (may be empty).
    pub fn username(&self) -> &str {
        &self.username
    }
    /// Password as written (may be empty).
    pub fn password(&self) -> &str {
        &self.password
    }
    /// The host, if the URL has an authority.
    pub fn host(&self) -> Option<&Host> {
        self.host.as_ref()
    }
    /// The port, `None` when absent or the scheme's default.
    pub fn port(&self) -> Option<u16> {
        self.port
    }
    /// The effective port: explicit or the scheme default.
    pub fn effective_port(&self) -> Option<u16> {
        self.port.or_else(|| default_port(&self.scheme))
    }
    /// Path as written.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// Query without `?`.
    pub fn query(&self) -> Option<&str> {
        self.query.as_deref()
    }
    /// Fragment without `#`.
    pub fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }
    /// Whether the scheme is special.
    pub fn is_special(&self) -> bool {
        is_special_scheme(&self.scheme)
    }
    /// Whether username or password is non-empty.
    pub fn has_credentials(&self) -> bool {
        !self.username.is_empty() || !self.password.is_empty()
    }
    /// Remove username and password.
    pub fn strip_credentials(&mut self) {
        self.username.clear();
        self.password.clear();
    }
    /// Remove the fragment.
    pub fn strip_fragment(&mut self) {
        self.fragment = None;
    }
    /// `http` -> `https` (port 80 becomes the default); `None` for any other
    /// scheme. Mixed Content "upgrade a request to a potentially trustworthy
    /// URL".
    pub fn upgraded_to_https(&self) -> Option<Url> {
        if self.scheme != "http" {
            return None;
        }
        let mut u = self.clone();
        u.scheme = "https".to_string();
        if u.port == Some(80) {
            u.port = None;
        }
        if u.port == Some(443) {
            u.port = None;
        }
        Some(u)
    }
    /// Remove the path (set to `/`) and query: the "origin-only" form.
    pub fn set_origin_only(&mut self) {
        self.path = "/".to_string();
        self.query = None;
    }

    /// Serialise. `exclude_fragment` omits `#...`.
    pub fn serialize(&self, exclude_fragment: bool) -> String {
        let mut s = format!("{}:", self.scheme);
        if let Some(h) = &self.host {
            s.push_str("//");
            if self.has_credentials() {
                s.push_str(&self.username);
                if !self.password.is_empty() {
                    s.push(':');
                    s.push_str(&self.password);
                }
                s.push('@');
            }
            s.push_str(&h.serialize());
            if let Some(p) = self.port {
                s.push_str(&format!(":{p}"));
            }
        }
        s.push_str(&self.path);
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        if !exclude_fragment && let Some(f) = &self.fragment {
            s.push('#');
            s.push_str(f);
        }
        s
    }
}

fn split_scheme(s: &str) -> Option<(String, &str)> {
    let colon = s.find(':')?;
    let (scheme, rest) = s.split_at_checked(colon)?;
    let rest = rest.get(1..)?;
    let mut chars = scheme.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    Some((scheme.to_ascii_lowercase(), rest))
}

fn parse_authority(url: &mut Url, auth: &str, special: bool) -> Option<()> {
    let (userinfo, hostport) = match auth.rfind('@') {
        Some(i) => (auth.get(..i)?, auth.get(i + 1..)?),
        None => ("", auth),
    };
    if let Some((u, p)) = userinfo.split_once(':') {
        url.username = u.to_string();
        url.password = p.to_string();
    } else {
        url.username = userinfo.to_string();
    }
    let (host_s, port_s) = if hostport.starts_with('[') {
        let close = hostport.find(']')?;
        let h = hostport.get(..=close)?;
        let rest = hostport.get(close + 1..)?;
        match rest.strip_prefix(':') {
            Some(p) => (h, Some(p)),
            None if rest.is_empty() => (h, None),
            None => return None,
        }
    } else {
        match hostport.rfind(':') {
            Some(i) => (hostport.get(..i)?, hostport.get(i + 1..)),
            None => (hostport, None),
        }
    };
    if host_s.is_empty() {
        if special || !userinfo.is_empty() || port_s.is_some() {
            return None;
        }
        url.host = Some(Host::Empty);
        return Some(());
    }
    url.host = Some(parse_host(host_s, special)?);
    if let Some(p) = port_s
        && !p.is_empty()
    {
        if !p.bytes().all(|b| b.is_ascii_digit()) || p.len() > 5 {
            return None;
        }
        url.port = Some(p.parse::<u16>().ok()?);
    }
    Some(())
}

fn forbidden_host_cp(c: char) -> bool {
    c <= ' '
        || c == '\u{7f}'
        || matches!(
            c,
            '#' | '/' | ':' | '<' | '>' | '?' | '@' | '[' | '\\' | ']' | '^' | '|'
        )
}

/// Host parser. `special` selects domain/IP interpretation.
pub fn parse_host(input: &str, special: bool) -> Option<Host> {
    if let Some(inner) = input.strip_prefix('[') {
        let inner = inner.strip_suffix(']')?;
        return parse_ipv6(inner).map(Host::Ipv6);
    }
    if !special {
        if input.is_empty() || input.chars().any(|c| forbidden_host_cp(c) || !c.is_ascii()) {
            return None;
        }
        return Some(Host::Opaque(input.to_string()));
    }
    let decoded = percent_decode_ascii(input)?;
    let lower = decoded.to_ascii_lowercase();
    if lower.is_empty() || lower.chars().any(|c| forbidden_host_cp(c) || c == '%') {
        return None;
    }
    if ends_in_number(&lower) {
        return parse_ipv4(&lower).map(Host::Ipv4);
    }
    Some(Host::Domain(lower))
}

fn percent_decode_ascii(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'%'
            && let (Some(&h), Some(&l)) = (b.get(i + 1), b.get(i + 2))
            && let (Some(h), Some(l)) = (
                (h as char).to_digit(16),
                (l as char).to_digit(16),
            )
        {
            out.push((h * 16 + l) as u8);
            i += 3;
            continue;
        }
        out.push(c);
        i += 1;
    }
    if !out.is_ascii() {
        return None;
    }
    String::from_utf8(out).ok()
}

/// Outcome of parsing one IPv4 number: `Err(())` is failure, overflow included.
fn parse_ipv4_number(s: &str) -> Result<u64, ()> {
    if s.is_empty() {
        return Err(());
    }
    let (digits, radix) = if let Some(r) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (r, 16)
    } else if s.len() > 1 && s.starts_with('0') {
        (s.get(1..).unwrap_or(""), 8)
    } else {
        (s, 10)
    };
    if digits.is_empty() {
        return Ok(0);
    }
    let mut v: u64 = 0;
    for c in digits.chars() {
        let d = c.to_digit(radix).ok_or(())?;
        v = v
            .checked_mul(u64::from(radix))
            .and_then(|x| x.checked_add(u64::from(d)))
            .ok_or(())?;
    }
    Ok(v)
}

fn ends_in_number(s: &str) -> bool {
    let mut parts: Vec<&str> = s.split('.').collect();
    if parts.last() == Some(&"") {
        if parts.len() == 1 {
            return false;
        }
        parts.pop();
    }
    let Some(last) = parts.last() else {
        return false;
    };
    if !last.is_empty() && last.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    // Hex forms such as `0x7f`; an overflow still counts as "a number".
    last.len() >= 2 && (last.starts_with("0x") || last.starts_with("0X"))
        && last.get(2..).is_some_and(|r| r.bytes().all(|b| b.is_ascii_hexdigit()))
}

fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let mut parts: Vec<&str> = s.split('.').collect();
    if parts.last() == Some(&"") && parts.len() > 1 {
        parts.pop();
    }
    if parts.len() > 4 {
        return None;
    }
    let mut nums = Vec::with_capacity(4);
    for p in &parts {
        nums.push(parse_ipv4_number(p).ok()?);
    }
    let (last, init) = nums.split_last()?;
    if init.iter().any(|&n| n > 255) {
        return None;
    }
    let exp = 5u32.checked_sub(nums.len() as u32)?;
    let limit = 256u64.checked_pow(exp)?;
    if *last >= limit {
        return None;
    }
    let mut ip = *last;
    for (i, n) in init.iter().enumerate() {
        let shift = 3u32.checked_sub(i as u32)?;
        ip += n << (8 * shift);
    }
    let b = (ip as u32).to_be_bytes();
    Some(b)
}

fn parse_ipv6(s: &str) -> Option<[u16; 8]> {
    fn groups(part: &str, allow_v4_tail: bool, out: &mut Vec<u16>) -> Option<()> {
        if part.is_empty() {
            return Some(());
        }
        let items: Vec<&str> = part.split(':').collect();
        let n = items.len();
        for (i, g) in items.iter().enumerate() {
            if allow_v4_tail && i + 1 == n && g.contains('.') {
                let oct: Vec<&str> = g.split('.').collect();
                if oct.len() != 4 {
                    return None;
                }
                let mut v = [0u8; 4];
                for (slot, o) in v.iter_mut().zip(&oct) {
                    if o.is_empty()
                        || o.len() > 3
                        || (o.len() > 1 && o.starts_with('0'))
                        || !o.bytes().all(|b| b.is_ascii_digit())
                    {
                        return None;
                    }
                    *slot = o.parse::<u8>().ok()?;
                }
                out.push(u16::from_be_bytes([v[0], v[1]]));
                out.push(u16::from_be_bytes([v[2], v[3]]));
            } else {
                if g.is_empty() || g.len() > 4 || !g.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return None;
                }
                out.push(u16::from_str_radix(g, 16).ok()?);
            }
        }
        Some(())
    }
    if s.len() > 45 {
        return None;
    }
    let mut result = [0u16; 8];
    match s.split_once("::") {
        None => {
            let mut g = Vec::new();
            groups(s, true, &mut g)?;
            if g.len() != 8 {
                return None;
            }
            result.copy_from_slice(&g);
        }
        Some((head, tail)) => {
            if tail.contains("::") {
                return None;
            }
            let mut h = Vec::new();
            let mut t = Vec::new();
            groups(head, false, &mut h)?;
            groups(tail, true, &mut t)?;
            if h.len() + t.len() > 7 {
                return None;
            }
            for (slot, v) in result.iter_mut().zip(&h) {
                *slot = *v;
            }
            let start = 8 - t.len();
            for (slot, v) in result.iter_mut().skip(start).zip(&t) {
                *slot = *v;
            }
        }
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_normalises() {
        let u = Url::parse("HTTPS://User:pw@Example.COM:443/a/b?q=1#frag").unwrap();
        assert_eq!(u.scheme(), "https");
        assert_eq!(u.host(), Some(&Host::Domain("example.com".into())));
        assert_eq!(u.port(), None, "default port elided");
        assert_eq!(u.effective_port(), Some(443));
        assert_eq!(u.username(), "User");
        assert_eq!(u.password(), "pw");
        assert_eq!(u.query(), Some("q=1"));
        assert_eq!(u.fragment(), Some("frag"));
        assert_eq!(
            u.serialize(true),
            "https://User:pw@example.com/a/b?q=1"
        );
    }

    #[test]
    fn rejects_malformed() {
        for bad in [
            "",
            "example.com",
            "http://",
            "http://exa mple.com/",
            "http://a:99999/",
            "http://a:x/",
            "http://[::1/",
            "1http://a/",
            "http://ex%00ample/",
            "http://bücher.example/",
            "http://256.1.1.1/",
            "http://1.2.3.4.5/",
        ] {
            assert!(Url::parse(bad).is_none(), "{bad}");
        }
        assert!(Url::parse(&format!("http://a/{}", "x".repeat(MAX_URL_BYTES))).is_none());
    }

    #[test]
    fn ipv4_forms() {
        let h = |s: &str| Url::parse(s).and_then(|u| u.host().cloned());
        assert_eq!(h("http://127.0.0.1/"), Some(Host::Ipv4([127, 0, 0, 1])));
        assert_eq!(h("http://127.1/"), Some(Host::Ipv4([127, 0, 0, 1])));
        assert_eq!(h("http://0x7f.1/"), Some(Host::Ipv4([127, 0, 0, 1])));
        assert_eq!(h("http://2130706433/"), Some(Host::Ipv4([127, 0, 0, 1])));
        assert_eq!(h("http://017700000001/"), Some(Host::Ipv4([127, 0, 0, 1])));
        assert_eq!(h("http://1.2.3.4./"), Some(Host::Ipv4([1, 2, 3, 4])));
        assert_eq!(h("http://example.1/"), None, "ends in a number but invalid");
        assert!(matches!(h("http://1.example/"), Some(Host::Domain(_))));
    }

    #[test]
    fn ipv6_forms() {
        let h = |s: &str| Url::parse(s).and_then(|u| u.host().cloned());
        assert_eq!(h("http://[::1]/"), Some(Host::Ipv6([0, 0, 0, 0, 0, 0, 0, 1])));
        assert_eq!(
            h("http://[::ffff:127.0.0.1]/"),
            Some(Host::Ipv6([0, 0, 0, 0, 0, 0xffff, 0x7f00, 1]))
        );
        assert_eq!(
            Url::parse("http://[2001:db8:0:0:1:0:0:1]:8080/").map(|u| u.serialize(true)),
            Some("http://[2001:db8::1:0:0:1]:8080/".to_string())
        );
        assert_eq!(
            Url::parse("http://[::1]/").map(|u| u.serialize(true)),
            Some("http://[::1]/".to_string())
        );
        assert!(h("http://[1:2:3:4:5:6:7:8:9]/").is_none());
        assert!(h("http://[1::2::3]/").is_none());
        assert!(h("http://[1:2:3:4:5:6:7::8]/").is_none());
    }

    #[test]
    fn non_special_and_file() {
        let u = Url::parse("data:text/plain,hi").unwrap();
        assert_eq!(u.host(), None);
        assert_eq!(u.path(), "text/plain,hi");
        let f = Url::parse("file:///etc/passwd").unwrap();
        assert_eq!(f.host(), Some(&Host::Empty));
        assert_eq!(f.serialize(true), "file:///etc/passwd");
        let a = Url::parse("about:blank").unwrap();
        assert_eq!(a.path(), "blank");
        let c = Url::parse("custom://Host:81/x").unwrap();
        assert_eq!(c.host(), Some(&Host::Opaque("Host".into())));
        assert_eq!(c.port(), Some(81));
    }

    #[test]
    fn strips_whitespace_controls() {
        let u = Url::parse("  ht\ttp://exa\nmple.com/ ").unwrap();
        assert_eq!(u.serialize(true), "http://example.com/");
    }
}
