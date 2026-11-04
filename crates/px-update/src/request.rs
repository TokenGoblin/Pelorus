//! The exact bytes of every request this crate makes.
//!
//! Invariant 4 (build-spec section 1): an update check carries no identifier
//! of any kind. The strongest way to hold that is for this crate to build the
//! complete request, byte for byte, and hand the bytes to the transport to
//! send verbatim over a fresh connection that shares nothing with browsing
//! state (no cookie jar, no cache, no connection pool, no session tickets, no
//! proxy credentials). The transport cannot add a header it was not given.
//!
//! The request is:
//!
//! ```text
//! GET <base>/<resource> HTTP/1.1\r\n
//! Host: <host>[:<port>]\r\n
//! User-Agent: update/1\r\n
//! Accept: */*\r\n
//! Connection: close\r\n
//! \r\n
//! ```
//!
//! No query string, no cookie, no `Referer`, no `Accept-Language`, no
//! conditional-request header (`If-None-Match` echoes a server-chosen token
//! back and so doubles as a cookie), no `Range`.
//!
//! `User-Agent` is a constant, `update/1`. It names neither the product, the
//! version, the OS nor the architecture, so every installed copy sends the
//! same bytes. It is present at all only because a number of CDN and WAF
//! configurations reject a request without one, and an update check that is
//! silently refused is an update that is never delivered. The `1` is the
//! request-shape version, not the program version; it changes only if this
//! allowlist changes.
//!
//! [`audit`] re-checks any request against the allowlist and is run on every
//! request before it reaches the transport, so a future edit to the builder
//! that adds a header fails closed instead of leaking.

/// The only header names that may appear, in the order they are sent.
pub const ALLOWED_HEADERS: [&str; 4] = ["Host", "User-Agent", "Accept", "Connection"];
/// The constant `User-Agent` value. See the module documentation.
pub const USER_AGENT: &str = "update/1";
const MAX_REQUEST_BYTES: usize = 1024;
const MAX_PATH_BYTES: usize = 256;

/// A mirror: where to connect and which path prefix its tree lives under.
/// The update URL and mirror list are brand-independent and permanent
/// (build-spec 14.1); they are configured here, never in px-brand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mirror {
    /// Lowercase DNS name (no IP literals, no userinfo, no port).
    pub host: String,
    pub port: u16,
    /// Empty, or `/segment[/segment...]` with no trailing slash.
    pub base: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestError(pub &'static str);

/// Complete HTTP/1.1 request bytes, to be sent verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub bytes: Vec<u8>,
}

fn path_chars_ok(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'-'))
}

fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

impl Mirror {
    pub fn validate(&self) -> Result<(), RequestError> {
        if !valid_host(&self.host) {
            return Err(RequestError(
                "mirror host is not a plain lowercase DNS name",
            ));
        }
        if self.port == 0 {
            return Err(RequestError("mirror port is zero"));
        }
        let b = &self.base;
        if !b.is_empty()
            && (!b.starts_with('/')
                || b.ends_with('/')
                || b.contains("//")
                || b.len() > 64
                || !path_chars_ok(b)
                || b.split('/')
                    .skip(1)
                    .any(|s| s.is_empty() || s.starts_with('.')))
        {
            return Err(RequestError("mirror base path is not a plain path prefix"));
        }
        Ok(())
    }
}

/// Build the request for `resource` (a relative path such as `manifest.txt`)
/// on `mirror`. The result has passed [`audit`].
pub fn build_get(mirror: &Mirror, resource: &str) -> Result<Request, RequestError> {
    mirror.validate()?;
    if resource.is_empty()
        || resource.len() > 128
        || !path_chars_ok(resource)
        || resource.starts_with('/')
        || resource
            .split('/')
            .any(|s| s.is_empty() || s.starts_with('.'))
    {
        return Err(RequestError("resource is not a plain relative path"));
    }
    let host = if mirror.port == 443 {
        mirror.host.clone()
    } else {
        format!("{}:{}", mirror.host, mirror.port)
    };
    let text = format!(
        "GET {}/{} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        mirror.base, resource, host, USER_AGENT
    );
    let req = Request {
        bytes: text.into_bytes(),
    };
    audit(&req.bytes)?;
    Ok(req)
}

/// Resource name of the manifest on every mirror.
pub const MANIFEST_RESOURCE: &str = "manifest.txt";
/// Resource name of the detached signature file.
pub const SIGNATURE_RESOURCE: &str = "manifest.sig";

/// The update check itself: `GET <base>/manifest.txt` on `mirror`.
pub fn build_check_request(mirror: &Mirror) -> Result<Request, RequestError> {
    build_get(mirror, MANIFEST_RESOURCE)
}

/// Check request bytes against the allowlist: method GET, a path with no
/// query or fragment, exactly the allowed headers in order with the fixed
/// values, CRLF framing, no body.
pub fn audit(bytes: &[u8]) -> Result<(), RequestError> {
    let bad = RequestError;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err(bad("request too long"));
    }
    if !bytes
        .iter()
        .all(|b| *b == b'\r' || *b == b'\n' || (0x20..=0x7e).contains(b))
    {
        return Err(bad("non-ASCII byte in request"));
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Err(bad("not ASCII"));
    };
    let Some(head) = text.strip_suffix("\r\n\r\n") else {
        return Err(bad(
            "request does not end with an empty line, or has a body",
        ));
    };
    if head.contains("\n\r") || head.replace("\r\n", "").contains(['\r', '\n']) {
        return Err(bad("bare CR or LF"));
    }
    let mut lines = head.split("\r\n");
    let Some(request_line) = lines.next() else {
        return Err(bad("empty request"));
    };
    let parts: Vec<&str> = request_line.split(' ').collect();
    let ["GET", target, "HTTP/1.1"] = parts.as_slice() else {
        return Err(bad("request line is not `GET <path> HTTP/1.1`"));
    };
    if target.contains(['?', '#', '%', ':', '@', '\\'])
        || !target.starts_with('/')
        || target.len() > MAX_PATH_BYTES
        || target.contains("//")
        || target.contains("/.")
    {
        return Err(bad(
            "request path has a query, fragment, escape or dot segment",
        ));
    }
    let mut seen = Vec::new();
    for line in lines {
        let Some((name, value)) = line.split_once(": ") else {
            return Err(bad("malformed header"));
        };
        if !ALLOWED_HEADERS.contains(&name) {
            return Err(bad("header outside the allowlist"));
        }
        let fixed_ok = match name {
            "User-Agent" => value == USER_AGENT,
            "Accept" => value == "*/*",
            "Connection" => value == "close",
            // Host: a validated DNS name with an optional numeric port.
            _ => {
                let (h, p) = value
                    .split_once(':')
                    .map_or((value, None), |(h, p)| (h, Some(p)));
                valid_host(h)
                    && p.is_none_or(|p| {
                        !p.is_empty() && p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit())
                    })
            }
        };
        if !fixed_ok {
            return Err(bad("header value is not the fixed value"));
        }
        seen.push(name);
    }
    if seen != ALLOWED_HEADERS {
        return Err(bad("headers are not exactly the allowlist, in order"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mirror() -> Mirror {
        Mirror {
            host: "mirror.example.net".into(),
            port: 443,
            base: "/v1".into(),
        }
    }

    #[test]
    fn exact_request_bytes() {
        let r = build_check_request(&mirror()).unwrap();
        assert_eq!(
            r.bytes,
            b"GET /v1/manifest.txt HTTP/1.1\r\n\
Host: mirror.example.net\r\n\
User-Agent: update/1\r\n\
Accept: */*\r\n\
Connection: close\r\n\
\r\n"
                .to_vec()
        );
    }

    #[test]
    fn exact_request_bytes_nondefault_port_and_empty_base() {
        let m = Mirror {
            host: "m2.example.org".into(),
            port: 8443,
            base: String::new(),
        };
        let r = build_get(&m, "releases/1.2.3/a.zip").unwrap();
        assert_eq!(
            String::from_utf8(r.bytes).unwrap(),
            "GET /releases/1.2.3/a.zip HTTP/1.1\r\nHost: m2.example.org:8443\r\nUser-Agent: update/1\r\nAccept: */*\r\nConnection: close\r\n\r\n"
        );
    }

    /// The gate: "packet capture of an update check shows no identifier of
    /// any kind". Every request the builder can produce is scanned for every
    /// kind of identifier carrier: cookies, query strings, anything outside
    /// the header allowlist, and any digit-bearing User-Agent.
    #[test]
    fn no_request_carries_an_identifier() {
        for resource in [
            "manifest.txt",
            "manifest.sig",
            "rotation-2.txt",
            "rotation-2.sig",
            "releases/1.2.3/app-linux-x86_64.tar.zst",
            "data/psl-20260909.dat",
        ] {
            for m in [
                mirror(),
                Mirror {
                    host: "a.b".into(),
                    port: 8080,
                    base: String::new(),
                },
            ] {
                let r = build_get(&m, resource).unwrap();
                let text = String::from_utf8(r.bytes.clone()).unwrap();
                assert!(!text.contains('?'), "query string in {text:?}");
                assert!(!text.to_ascii_lowercase().contains("cookie"));
                assert!(!text.contains("Referer") && !text.contains("If-None-Match"));
                for line in text.split("\r\n").skip(1).filter(|l| !l.is_empty()) {
                    let name = line.split(": ").next().unwrap();
                    assert!(ALLOWED_HEADERS.contains(&name), "header {name} not allowed");
                }
                assert_eq!(audit(&r.bytes), Ok(()));
            }
        }
        // The UA is one constant with no version, OS or product detail.
        assert_eq!(USER_AGENT, "update/1");
    }

    #[test]
    fn audit_fails_on_anything_outside_the_allowlist() {
        let good = build_get(&mirror(), "manifest.txt").unwrap().bytes;
        let text = String::from_utf8(good.clone()).unwrap();
        let bads = [
            text.replace("manifest.txt", "manifest.txt?v=1"),
            text.replace("manifest.txt", "manifest.txt#x"),
            text.replace("Accept: */*\r\n", "Accept: */*\r\nCookie: a=b\r\n"),
            text.replace("Accept: */*\r\n", "Accept: */*\r\nX-Request-Id: 1\r\n"),
            text.replace("Accept: */*\r\n", "Accept: */*\r\nAccept-Language: en\r\n"),
            text.replace("Accept: */*\r\n", "Accept: */*\r\nIf-None-Match: \"x\"\r\n"),
            text.replace("Accept: */*\r\n", "Accept: */*\r\nReferer: https://a/\r\n"),
            text.replace("update/1", "update/1.2.3 (Linux x86_64)"),
            text.replace("update/1", "Mozilla/5.0"),
            text.replace("Connection: close", "Connection: keep-alive"),
            text.replace("GET", "POST"),
            text.replace("HTTP/1.1", "HTTP/1.0"),
            text.replace("/v1/manifest.txt", "/v1/../manifest.txt"),
            text.replace("/v1/manifest.txt", "http://x/manifest.txt"),
            text.replace("\r\n\r\n", "\r\n\r\nbody"),
            text.replace("\r\n\r\n", "\n\n"),
            text.replace("Host: mirror.example.net\r\n", ""),
            text.replace("Host: mirror.example.net", "Host: user@mirror.example.net"),
            // Reordered headers.
            text.replace(
                "User-Agent: update/1\r\nAccept: */*\r\n",
                "Accept: */*\r\nUser-Agent: update/1\r\n",
            ),
            text.replace("Host:", "host:"),
        ];
        for (i, bad) in bads.iter().enumerate() {
            assert!(audit(bad.as_bytes()).is_err(), "case {i} passed: {bad:?}");
        }
        assert_eq!(audit(&good), Ok(()));
    }

    #[test]
    fn builder_rejects_bad_mirrors_and_resources() {
        for host in [
            "",
            "Upper.example",
            "a b",
            "a..b",
            "-a.b",
            "a@b.c",
            "a:80",
            "1.2.3.4:5",
            "ex_ample.com",
        ] {
            let m = Mirror {
                host: host.into(),
                port: 443,
                base: String::new(),
            };
            assert!(build_get(&m, "x").is_err(), "{host}");
        }
        for base in [
            "v1", "/v1/", "/v1//x", "/v1/../x", "/v1?x=1", "/.hidden", "/a b",
        ] {
            let m = Mirror {
                base: base.into(),
                ..mirror()
            };
            assert!(build_get(&m, "x").is_err(), "{base}");
        }
        for res in [
            "", "/abs", "a//b", "../x", "a/./b", "x?y=1", "x#f", "a b", "a%20b",
        ] {
            assert!(build_get(&mirror(), res).is_err(), "{res}");
        }
        let m = Mirror {
            port: 0,
            ..mirror()
        };
        assert!(build_get(&m, "x").is_err());
    }
}
