//! Phase 12 integration: the fetch path calls px-websec at every decision.
//!
//! Every scenario drives a REAL fetch (TCP, HTTP/1.1, the production
//! pipeline in `px_net::secure`) against a local server that records what it
//! receives. Each scenario names the px-websec enforcement point it must
//! reach, and the runner checks the per-thread trace for it, so a check that
//! exists but is not on the fetch path fails here and not in review.
//!
//! The last test enumerates every `ENFORCEMENT_POINTS` of every px-websec
//! module and requires that each is either reached through px-net by some
//! scenario or listed in `NOT_WIRED` with the reason. `NOT_WIRED` is checked
//! in both directions: a point that is listed but reached fails too, so the
//! list cannot rot into a lie.
//!
//! Hosts such as `app.test` do not resolve; the `testing` feature's
//! `connect_to` sends every connection to the local server, which is how a
//! non-loopback name (needed for mixed-content and cross-site scenarios,
//! because loopback is "potentially trustworthy") reaches a loopback socket.

#![cfg(feature = "testing")]

// Everything below is in a `cfg(test)` module so that the workspace's
// `allow-*-in-tests` clippy settings cover the helpers as well as the
// `#[test]` functions. No lint is waived.
#[cfg(test)]
mod scenarios {
    use std::collections::BTreeSet;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use px_net::cookie::SameSite;
    use px_net::hsts::PreloadList;
    use px_net::partition::{Origin as NetOrigin, PartitionKey};
    use px_net::psl::PublicSuffixList;
    use px_net::secure::{Destination, FetchContext, FetchOutcome, SecureFetcher, ViolationKind};
    use px_websec::cors::{self, CorsError, CredentialsMode, Header, RequestMode, Tainting};
    use px_websec::csp::{PolicySet, RequestKind as CspKind};
    use px_websec::origin::{Origin as WOrigin, Url as WUrl, trace};
    use px_websec::referrer::ReferrerPolicy;
    use px_websec::sri::SriError;
    use px_websec::{coop, frame, mixed, referrer, sri};

    // ------------------------------------------------------------------ server

    #[derive(Clone, Debug)]
    struct Req {
        method: String,
        target: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl Req {
        fn header(&self, name: &str) -> Option<&str> {
            self.headers
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.as_str())
        }
    }

    struct Resp {
        status: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        raw: Option<Vec<u8>>,
    }

    impl Resp {
        fn new(status: u16, body: &[u8]) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.to_vec(),
                raw: None,
            }
        }
        fn ok(body: &[u8]) -> Self {
            Self::new(200, body)
        }
        fn h(mut self, n: &str, v: &str) -> Self {
            self.headers.push((n.to_string(), v.to_string()));
            self
        }
        fn redirect(location: &str) -> Self {
            Self::new(302, b"").h("Location", location)
        }
    }

    struct Srv {
        port: u16,
        log: Arc<Mutex<Vec<Req>>>,
        conns: Arc<AtomicUsize>,
    }

    impl Srv {
        fn addr(&self) -> SocketAddr {
            SocketAddr::from(([127, 0, 0, 1], self.port))
        }
        fn requests(&self) -> Vec<Req> {
            self.log.lock().unwrap().clone()
        }
        fn count(&self, method: &str) -> usize {
            self.requests()
                .iter()
                .filter(|r| r.method == method)
                .count()
        }
        fn clear(&self) {
            self.log.lock().unwrap().clear();
        }
        fn conns(&self) -> usize {
            self.conns.load(Ordering::SeqCst)
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<Req> {
        let mut raw = Vec::new();
        let mut buf = [0u8; 4096];
        let head_end = loop {
            let n = stream.read(&mut buf).ok()?;
            if n == 0 {
                return None;
            }
            if raw.is_empty() && buf.first().is_some_and(|b| !b.is_ascii_alphabetic()) {
                // Not HTTP: a TLS ClientHello starts 0x16. Record and stop.
                return Some(Req {
                    method: format!("NOT-HTTP-{:#04x}", buf.first().copied().unwrap_or(0)),
                    target: String::new(),
                    headers: Vec::new(),
                    body: Vec::new(),
                });
            }
            raw.extend_from_slice(buf.get(..n)?);
            if let Some(p) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
                break p;
            }
        };
        let head = String::from_utf8_lossy(raw.get(..head_end)?).into_owned();
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let method = first.next()?.to_string();
        let target = first.next()?.to_string();
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        let want: usize = headers
            .iter()
            .find(|(n, _)| n == "content-length")
            .and_then(|(_, v)| v.parse().ok())
            .unwrap_or(0);
        let mut body = raw.get(head_end + 4..)?.to_vec();
        while body.len() < want {
            let n = stream.read(&mut buf).ok()?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(buf.get(..n)?);
        }
        Some(Req {
            method,
            target,
            headers,
            body,
        })
    }

    fn start<F>(handler: F) -> Srv
    where
        F: Fn(&Req) -> Resp + Send + Sync + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let conns = Arc::new(AtomicUsize::new(0));
        let (l, c) = (Arc::clone(&log), Arc::clone(&conns));
        let handler = Arc::new(handler);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                c.fetch_add(1, Ordering::SeqCst);
                let (l, handler) = (Arc::clone(&l), Arc::clone(&handler));
                std::thread::spawn(move || {
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let Some(req) = read_request(&mut stream) else {
                        return;
                    };
                    l.lock().unwrap().push(req.clone());
                    if req.method.starts_with("NOT-HTTP") {
                        return;
                    }
                    let resp = handler(&req);
                    let out = match resp.raw {
                        Some(raw) => raw,
                        None => {
                            let mut o = format!("HTTP/1.1 {} X\r\n", resp.status).into_bytes();
                            for (n, v) in &resp.headers {
                                o.extend_from_slice(format!("{n}: {v}\r\n").as_bytes());
                            }
                            o.extend_from_slice(
                                format!("Content-Length: {}\r\n\r\n", resp.body.len()).as_bytes(),
                            );
                            o.extend_from_slice(&resp.body);
                            o
                        }
                    };
                    let _ = stream.write_all(&out);
                    let _ = stream.flush();
                });
            }
        });
        Srv { port, log, conns }
    }

    // ------------------------------------------------------------------ helpers

    fn psl() -> PublicSuffixList {
        PublicSuffixList::parse("test\n").unwrap()
    }

    fn fetcher<'a>(psl: &'a PublicSuffixList, srv: &Srv) -> SecureFetcher<'a> {
        let mut f = SecureFetcher::new(psl, None);
        f.connect_to = Some(srv.addr());
        f
    }

    fn ctx(doc: &str, dest: Destination) -> FetchContext {
        FetchContext::top_level_document(doc, dest).unwrap()
    }

    fn u(s: &str) -> WUrl {
        WUrl::parse(s).unwrap()
    }

    fn origin_of(s: &str) -> WOrigin {
        WOrigin::from_url(&u(s))
    }

    fn csp_set(enforce: &[&str], report_only: &[&str]) -> PolicySet {
        let e: Vec<&[u8]> = enforce.iter().map(|s| s.as_bytes()).collect();
        let r: Vec<&[u8]> = report_only.iter().map(|s| s.as_bytes()).collect();
        PolicySet::from_headers_fail_closed(&e, &r)
    }

    fn enforced_kinds(o: &FetchOutcome) -> Vec<ViolationKind> {
        o.enforced().map(|v| v.kind.clone()).collect()
    }

    fn acao_for(req: &Req) -> String {
        req.header("origin").unwrap_or("").to_string()
    }

    fn json_get(srv_port: u16, host: &str, path: &str) -> String {
        format!("http://{host}:{srv_port}{path}")
    }

    // --------------------------------------------------------------- scenarios

    fn same_origin_allowed() {
        let srv = start(|_| Resp::ok(b"hello").h("X-Plain", "1"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let doc = json_get(srv.port, "app.test", "/page");
        let mut c = ctx(&doc, Destination::Fetch);
        c.mode = RequestMode::Cors;
        let o = f.fetch(&c, &json_get(srv.port, "app.test", "/data"));
        assert!(o.allowed(), "{:?}", o.violations);
        assert_eq!(o.tainting, Some(Tainting::Basic));
        let r = o.response.unwrap();
        assert_eq!(r.script_body(), Some(&b"hello"[..]));
        assert!(r.headers.iter().any(|h| h.name == "x-plain"));
        // A same-origin GET carries no Origin header (Fetch).
        assert_eq!(srv.requests()[0].header("origin"), None);
    }

    fn no_cors_cross_origin_is_opaque() {
        let srv = start(|_| Resp::ok(b"PNGDATA").h("X-Secret", "s"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Image);
        let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/i.png"));
        assert!(o.allowed(), "{:?}", o.violations);
        assert_eq!(o.tainting, Some(Tainting::Opaque));
        let r = o.response.unwrap();
        assert!(r.is_opaque());
        assert_eq!(r.status, 0);
        assert!(r.headers.is_empty());
        assert_eq!(r.script_body(), None);
        assert_eq!(
            r.embedder_body(),
            b"PNGDATA",
            "the decoder still gets bytes"
        );
    }

    fn cors_allowed_with_acao() {
        let srv = start(|r| {
            Resp::ok(b"{}")
                .h("Access-Control-Allow-Origin", &acao_for(r))
                .h("Access-Control-Expose-Headers", "X-Exposed")
                .h("X-Exposed", "yes")
                .h("X-Hidden", "no")
                .h("Content-Type", "application/json")
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Fetch);
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/data"));
        assert!(o.allowed(), "{:?}", o.violations);
        assert_eq!(o.tainting, Some(Tainting::Cors));
        let r = o.response.unwrap();
        assert_eq!(r.script_body(), Some(&b"{}"[..]));
        let names: Vec<&str> = r.headers.iter().map(|h| h.name.as_str()).collect();
        assert!(names.contains(&"x-exposed") && names.contains(&"content-type"));
        assert!(!names.contains(&"x-hidden"), "not exposed: {names:?}");
        assert_eq!(
            srv.requests()[0].header("origin"),
            Some("http://app.test"),
            "a cross-origin CORS request carries Origin"
        );
        assert_eq!(
            srv.count("OPTIONS"),
            0,
            "a simple request is not preflighted"
        );
    }

    fn cors_blocked_without_acao() {
        let srv = start(|_| Resp::ok(b"secret"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Fetch);
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/data"));
        assert!(!o.allowed());
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Cors(CorsError::MissingAllowOrigin)]
        );
        assert_eq!(
            srv.count("GET"),
            1,
            "the request is sent; the response is withheld"
        );
    }

    fn credentialed_wildcard_blocked() {
        let srv = start(|_| Resp::ok(b"x").h("Access-Control-Allow-Origin", "*"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.credentials = CredentialsMode::Include;
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/data"));
        assert!(!o.allowed());
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Cors(CorsError::OriginMismatch)]
        );
        // And the same server with credentials omitted is fine.
        let mut c2 = ctx("http://app.test/", Destination::Fetch);
        c2.credentials = CredentialsMode::Omit;
        assert!(
            f.fetch(&c2, &json_get(srv.port, "api.test", "/data"))
                .allowed()
        );
    }

    fn credentialed_cors_needs_allow_credentials() {
        let srv = start(|r| {
            let mut resp = Resp::ok(b"x").h("Access-Control-Allow-Origin", &acao_for(r));
            if r.target == "/with" {
                resp = resp.h("Access-Control-Allow-Credentials", "true");
            }
            resp
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.credentials = CredentialsMode::Include;
        assert!(
            f.fetch(&c, &json_get(srv.port, "api.test", "/with"))
                .allowed()
        );
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/without"));
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Cors(CorsError::CredentialsNotAllowed)]
        );
    }

    fn preflight_server(methods: &'static str, headers: &'static str) -> Srv {
        start(move |r| {
            let acao = acao_for(r);
            if r.method == "OPTIONS" {
                Resp::new(204, b"")
                    .h("Access-Control-Allow-Origin", &acao)
                    .h("Access-Control-Allow-Methods", methods)
                    .h("Access-Control-Allow-Headers", headers)
                    .h("Access-Control-Max-Age", "60")
            } else {
                Resp::ok(b"done").h("Access-Control-Allow-Origin", &acao)
            }
        })
    }

    fn preflight_for_custom_header() {
        let srv = preflight_server("GET", "x-custom");
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.headers = vec![Header::new("X-Custom", "1")];
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/r"));
        assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
        assert_eq!(o.preflights_sent, 1);
        let log = srv.requests();
        let methods: Vec<&str> = log.iter().map(|r| r.method.as_str()).collect();
        assert_eq!(
            methods,
            ["OPTIONS", "GET"],
            "preflight first, then the request"
        );
        let pre = &log[0];
        assert_eq!(
            pre.header("access-control-request-headers"),
            Some("x-custom")
        );
        assert_eq!(pre.header("access-control-request-method"), Some("GET"));
        assert_eq!(pre.header("origin"), Some("http://app.test"));
        assert_eq!(
            pre.header("cookie"),
            None,
            "preflights carry no credentials"
        );
        assert_eq!(log[1].header("x-custom"), Some("1"));
    }

    fn preflight_for_put() {
        let srv = preflight_server("PUT", "");
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.method = "put".to_string();
        c.body = b"payload".to_vec();
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/r"));
        assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
        assert_eq!(o.preflights_sent, 1);
        let log = srv.requests();
        assert_eq!(log[0].method, "OPTIONS");
        assert_eq!(log[0].header("access-control-request-method"), Some("PUT"));
        assert_eq!(log[1].method, "PUT");
        assert_eq!(log[1].body, b"payload");
    }

    fn preflight_refusal_stops_the_request() {
        let srv = preflight_server("GET", "x-other");
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.headers = vec![Header::new("X-Custom", "1")];
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/r"));
        assert!(!o.allowed());
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Preflight(CorsError::HeaderNotAllowed(
                "x-custom".to_string()
            ))]
        );
        assert_eq!(srv.count("GET"), 0, "the actual request is never sent");
        assert_eq!(f.preflight_cache_len(), 0, "a refusal is not cached");
    }

    fn preflight_cache_hit_on_second_request() {
        let srv = preflight_server("GET", "x-custom");
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.headers = vec![Header::new("X-Custom", "1")];
        let url = json_get(srv.port, "api.test", "/r");
        let first = f.fetch(&c, &url);
        assert!(first.allowed());
        assert_eq!((first.preflights_sent, first.preflight_cache_hits), (1, 0));
        let second = f.fetch(&c, &url);
        assert!(second.allowed());
        assert_eq!(
            (second.preflights_sent, second.preflight_cache_hits),
            (0, 1)
        );
        assert_eq!(srv.count("OPTIONS"), 1, "one preflight on the wire");
        assert_eq!(srv.count("GET"), 2);
    }

    fn preflight_cache_is_partitioned() {
        let srv = preflight_server("GET", "x-custom");
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        // The same initiating origin (a frame of http://frame.test), embedded
        // under two different top-level sites: the only thing that differs is
        // the partition key.
        let mk = |top: &str| {
            let mut c = ctx("http://frame.test/", Destination::Fetch);
            c.top_level = origin_of(top);
            c.headers = vec![Header::new("X-Custom", "1")];
            c
        };
        let (a, b) = (mk("http://a.test/"), mk("http://b.test/"));
        let url = json_get(srv.port, "api.test", "/r");
        assert_eq!(f.fetch(&a, &url).preflights_sent, 1);
        assert_eq!(
            f.fetch(&a, &url).preflight_cache_hits,
            1,
            "same partition: hit"
        );
        let other = f.fetch(&b, &url);
        assert!(other.allowed());
        assert_eq!(
            (other.preflights_sent, other.preflight_cache_hits),
            (1, 0),
            "another top-level site must not reuse the grant"
        );
        assert_eq!(srv.count("OPTIONS"), 2);
    }

    fn secure_doc(dest: Destination) -> FetchContext {
        ctx("https://secure.test/", dest)
    }

    fn mixed_content_blocks_script() {
        let srv = start(|_| Resp::ok(b"alert(1)"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let o = f.fetch(
            &secure_doc(Destination::Script),
            &json_get(srv.port, "plain.test", "/x.js"),
        );
        assert!(!o.allowed());
        let v = o.enforced().next().unwrap();
        assert_eq!(v.kind, ViolationKind::MixedContent);
        assert_eq!(v.point, "mixed::check_request");
        assert_eq!(srv.conns(), 0, "nothing leaves the machine");
    }

    fn mixed_content_upgrades_image() {
        let srv = start(|_| Resp::ok(b"img"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let from = json_get(srv.port, "plain.test", "/i.png");
        let o = f.fetch(&secure_doc(Destination::Image), &from);
        assert_eq!(
            o.upgraded_from.as_ref().map(|x| x.serialize(true)),
            Some(from)
        );
        assert_eq!(
            o.urls.last().map(|x| x.scheme().to_string()),
            Some("https".to_string())
        );
        // The upgraded request spoke TLS to the plaintext server and failed;
        // there is no fallback to the original http URL.
        assert!(!o.allowed());
        assert!(o.error.is_some(), "{:?}", o.violations);
        let log = srv.requests();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].method, "NOT-HTTP-0x16", "a TLS ClientHello, not GET");
    }

    fn mixed_content_survives_redirects() {
        // A trustworthy http origin (localhost) redirects a script load to an
        // untrustworthy one: the check runs again on the second hop.
        let port_cell = Arc::new(AtomicUsize::new(0));
        let pc = Arc::clone(&port_cell);
        let srv = start(move |r| {
            if r.target == "/start" {
                let p = pc.load(Ordering::SeqCst);
                Resp::redirect(&format!("http://plain.test:{p}/second"))
            } else {
                Resp::ok(b"x")
            }
        });
        port_cell.store(srv.port as usize, Ordering::SeqCst);
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let o = f.fetch(
            &secure_doc(Destination::Script),
            &json_get(srv.port, "localhost", "/start"),
        );
        assert!(!o.allowed());
        assert_eq!(o.urls.len(), 2);
        let v = o.enforced().next().unwrap();
        assert_eq!(
            (v.kind.clone(), v.point),
            (ViolationKind::MixedContent, "mixed::check_request")
        );
        assert_eq!(
            srv.requests().len(),
            1,
            "the second hop was never requested"
        );
    }

    fn hsts_runs_before_mixed_content() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let list = PreloadList::parse("hsts.test\n").unwrap();
        let mut f = SecureFetcher::new(&psl, Some(&list));
        f.connect_to = Some(srv.addr());
        let from = json_get(srv.port, "hsts.test", "/x.js");
        // A script is blockable mixed content. Because the host is preloaded it
        // is upgraded first, so the mixed-content check sees https and allows.
        let o = f.fetch(&secure_doc(Destination::Script), &from);
        assert!(
            !o.enforced().any(|v| v.kind == ViolationKind::MixedContent),
            "{:?}",
            o.violations
        );
        assert_eq!(
            o.upgraded_from.map(|x| x.serialize(true)),
            Some(from.clone())
        );
        assert_eq!(srv.requests()[0].method, "NOT-HTTP-0x16");
        // The same load from a host that is not preloaded is blocked.
        let o = fetcher(&psl, &srv).fetch(
            &secure_doc(Destination::Script),
            &json_get(srv.port, "other.test", "/x.js"),
        );
        assert_eq!(enforced_kinds(&o), vec![ViolationKind::MixedContent]);
    }

    fn csp_script_src_blocks_script() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx(&json_get(srv.port, "app.test", "/"), Destination::Script);
        c.csp = csp_set(&["script-src 'self'"], &[]);
        let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/x.js"));
        assert!(!o.allowed());
        let v = o.enforced().next().unwrap();
        assert_eq!(
            (v.kind.clone(), v.point),
            (ViolationKind::Csp, "script-src-elem")
        );
        assert_eq!(srv.conns(), 0);
        // 'self' is allowed.
        let ok = f.fetch(&c, &json_get(srv.port, "app.test", "/x.js"));
        assert!(ok.allowed(), "{:?}", ok.violations);
    }

    fn csp_connect_src_blocks_fetch() {
        let srv = start(|_| Resp::ok(b"x").h("Access-Control-Allow-Origin", "*"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Fetch);
        c.csp = csp_set(&["connect-src 'self'"], &[]);
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/d"));
        let v = o.enforced().next().unwrap();
        assert_eq!(
            (v.kind.clone(), v.point),
            (ViolationKind::Csp, "connect-src")
        );
        assert_eq!(srv.conns(), 0);
    }

    fn csp_report_only_records_but_allows() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Script);
        c.csp = csp_set(&[], &["script-src 'none'"]);
        let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/x.js"));
        assert!(o.allowed());
        assert_eq!(o.violations.len(), 1);
        let v = &o.violations[0];
        assert!(!v.enforced);
        assert_eq!(
            (v.kind.clone(), v.point),
            (ViolationKind::Csp, "script-src-elem")
        );
        assert_eq!(srv.count("GET"), 1);
    }

    fn csp_governs_every_destination() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut seen = BTreeSet::new();
        for dest in Destination::ALL {
            let Some(kind) = dest.csp_kind() else {
                continue;
            };
            let mut c = ctx("http://app.test/", dest);
            c.csp = csp_set(&["default-src 'none'"], &[]);
            let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/r"));
            let v = o
                .enforced()
                .next()
                .unwrap_or_else(|| panic!("{dest:?} was not blocked"));
            assert_eq!(v.kind, ViolationKind::Csp);
            assert_eq!(v.point, kind.effective_directive(), "{dest:?}");
            seen.insert(v.point);
        }
        assert_eq!(srv.conns(), 0);
        for d in [
            "connect-src",
            "script-src-elem",
            "worker-src",
            "style-src-elem",
            "img-src",
            "font-src",
            "media-src",
            "object-src",
            "frame-src",
            "manifest-src",
            "prefetch-src",
        ] {
            assert!(seen.contains(d), "no fetch exercised {d}");
        }
        let _ = CspKind::ALL;
    }

    fn csp_upgrade_insecure_requests_and_block_all_mixed() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx("http://app.test/", Destination::Image);
        c.csp = csp_set(&["upgrade-insecure-requests"], &[]);
        let from = json_get(srv.port, "cdn.test", "/i.png");
        let o = f.fetch(&c, &from);
        assert_eq!(o.upgraded_from.map(|x| x.serialize(true)), Some(from));
        assert_eq!(srv.requests()[0].method, "NOT-HTTP-0x16");

        let srv2 = start(|_| Resp::ok(b"x"));
        let mut f2 = fetcher(&psl, &srv2);
        let mut c2 = secure_doc(Destination::Fetch);
        c2.csp = csp_set(&["block-all-mixed-content"], &[]);
        // localhost is trustworthy, so mixed content allows it; the CSP
        // directive is stricter and also allows trustworthy URLs.
        let o2 = f2.fetch(&c2, &json_get(srv2.port, "localhost", "/x"));
        assert!(
            !o2.enforced()
                .any(|v| v.point == "csp::block-all-mixed-content"),
            "{:?}",
            o2.violations
        );
        // A plain http URL is stopped before the network by one of the two.
        let before = srv2.conns();
        let o3 = f2.fetch(&c2, &json_get(srv2.port, "plain.test", "/x"));
        assert!(!o3.allowed());
        assert_eq!(srv2.conns(), before);
    }

    // Referrer table: what the server actually received.

    fn expected_referrer(
        policy: ReferrerPolicy,
        case: char,
        full: &str,
        origin: &str,
    ) -> Option<String> {
        use ReferrerPolicy::*;
        let (same, downgrade) = (case == 'A', case == 'C');
        let f = Some(full.to_string());
        let o = Some(origin.to_string());
        match policy {
            NoReferrer => None,
            NoReferrerWhenDowngrade => {
                if downgrade {
                    None
                } else {
                    f
                }
            }
            SameOrigin => {
                if same {
                    f
                } else {
                    None
                }
            }
            Origin => o,
            StrictOrigin => {
                if downgrade {
                    None
                } else {
                    o
                }
            }
            OriginWhenCrossOrigin => {
                if same {
                    f
                } else {
                    o
                }
            }
            StrictOriginWhenCrossOrigin => {
                if same {
                    f
                } else if downgrade {
                    None
                } else {
                    o
                }
            }
            UnsafeUrl => f,
        }
    }

    fn referrer_policies_on_the_wire() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let p = srv.port;
        let target = json_get(p, "plain.test", "/res");
        let cases = [
            // A: same-origin (credentials in the document URL must not leak).
            (
                'A',
                format!("http://u:pw@plain.test:{p}/dir/page?q=1#frag"),
                format!("http://plain.test:{p}/dir/page?q=1"),
                format!("http://plain.test:{p}/"),
            ),
            // B: cross-origin, neither side trustworthy: not a downgrade.
            (
                'B',
                "http://page.test/dir/page?q=1#frag".to_string(),
                "http://page.test/dir/page?q=1".to_string(),
                "http://page.test/".to_string(),
            ),
            // C: https document navigating to http: a downgrade.
            (
                'C',
                "https://secure.test/dir/page?q=1#frag".to_string(),
                "https://secure.test/dir/page?q=1".to_string(),
                "https://secure.test/".to_string(),
            ),
        ];
        let all = [
            ReferrerPolicy::NoReferrer,
            ReferrerPolicy::NoReferrerWhenDowngrade,
            ReferrerPolicy::SameOrigin,
            ReferrerPolicy::Origin,
            ReferrerPolicy::StrictOrigin,
            ReferrerPolicy::OriginWhenCrossOrigin,
            ReferrerPolicy::StrictOriginWhenCrossOrigin,
            ReferrerPolicy::UnsafeUrl,
        ];
        for (case, doc, full, origin) in &cases {
            for policy in all {
                srv.clear();
                let mut c = ctx(doc, Destination::Navigation);
                c.document_referrer_policy = Some(policy);
                let mut f = fetcher(&psl, &srv);
                let o = f.fetch(&c, &target);
                assert!(
                    o.allowed(),
                    "{case} {policy:?}: {:?} {:?}",
                    o.violations,
                    o.error
                );
                let got = srv.requests()[0].header("referer").map(str::to_string);
                assert_eq!(
                    got,
                    expected_referrer(policy, *case, full, origin),
                    "case {case}, policy {}",
                    policy.as_str()
                );
            }
        }
        // The default (no policy anywhere) is strict-origin-when-cross-origin,
        // and a request-level policy beats the document's.
        srv.clear();
        let mut c = ctx("http://page.test/dir/page?q=1", Destination::Navigation);
        c.document_referrer_policy = Some(ReferrerPolicy::NoReferrer);
        c.request_referrer_policy = Some(ReferrerPolicy::UnsafeUrl);
        assert!(fetcher(&psl, &srv).fetch(&c, &target).allowed());
        assert_eq!(
            srv.requests()[0].header("referer"),
            Some("http://page.test/dir/page?q=1")
        );
        srv.clear();
        let c = ctx("http://page.test/dir/page?q=1", Destination::Navigation);
        assert!(fetcher(&psl, &srv).fetch(&c, &target).allowed());
        assert_eq!(
            srv.requests()[0].header("referer"),
            Some("http://page.test/")
        );
        let _ = referrer::MAX_REFERRER_BYTES;
    }

    fn referrer_policy_header_on_redirect_applies_to_next_hop() {
        let port = Arc::new(AtomicUsize::new(0));
        let pc = Arc::clone(&port);
        let srv = start(move |r| {
            if r.target == "/r1" {
                let p = pc.load(Ordering::SeqCst);
                Resp::redirect(&format!("http://plain.test:{p}/r2"))
                    .h("Referrer-Policy", "no-referrer")
            } else {
                Resp::ok(b"x")
            }
        });
        port.store(srv.port as usize, Ordering::SeqCst);
        let psl = psl();
        let c = ctx("http://page.test/dir/page?q=1", Destination::Navigation);
        let o = fetcher(&psl, &srv).fetch(&c, &json_get(srv.port, "plain.test", "/r1"));
        assert!(o.allowed());
        let log = srv.requests();
        assert!(log[0].header("referer").is_some());
        assert_eq!(log[1].header("referer"), None);
    }

    const SRI_ABC: &str = "sha256-ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0=";

    fn sri_pass_and_fail() {
        let srv = start(|r| {
            let acao = acao_for(r);
            let mut resp = Resp::ok(b"abc");
            if !acao.is_empty() {
                resp = resp.h("Access-Control-Allow-Origin", &acao);
            }
            resp
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let url = json_get(srv.port, "app.test", "/s.js");
        let mut c = ctx(&json_get(srv.port, "app.test", "/"), Destination::Script);
        c.integrity = Some(SRI_ABC.to_string());
        assert!(f.fetch(&c, &url).allowed());

        c.integrity = Some(format!("sha256-{}", "A".repeat(43) + "="));
        let bad = f.fetch(&c, &url);
        assert_eq!(
            enforced_kinds(&bad),
            vec![ViolationKind::Sri(SriError::Mismatch)]
        );
        assert!(
            bad.response.is_none(),
            "a mismatching body is never handed over"
        );

        // Cross-origin, no-cors: opaque, so not eligible for integrity.
        let mut x = ctx("http://other.test/", Destination::Script);
        x.integrity = Some(SRI_ABC.to_string());
        let o = f.fetch(&x, &url);
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Sri(SriError::NotEligible)]
        );
        // Cross-origin with crossorigin (cors mode) and ACAO: eligible, passes.
        x.mode = RequestMode::Cors;
        assert!(f.fetch(&x, &url).allowed());

        // An integrity attribute padded past the bound denies; it does not turn
        // enforcement off.
        c.integrity = Some(format!("{SRI_ABC} {}", "x ".repeat(9000)));
        let big = f.fetch(&c, &url);
        assert_eq!(
            enforced_kinds(&big),
            vec![ViolationKind::Sri(SriError::TooLarge)]
        );
        // Unknown algorithms only: no metadata, loads (SRI says so).
        c.integrity = Some("md5-AAAA".to_string());
        assert!(f.fetch(&c, &url).allowed());
    }

    fn corp_blocks_and_coep_requires() {
        let srv = start(|r| {
            let mut resp = Resp::ok(b"img");
            match r.target.as_str() {
                "/same-origin" => resp = resp.h("Cross-Origin-Resource-Policy", "same-origin"),
                "/same-site" => resp = resp.h("Cross-Origin-Resource-Policy", "same-site"),
                "/cross-origin" => resp = resp.h("Cross-Origin-Resource-Policy", "cross-origin"),
                _ => {}
            }
            resp
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Image);
        let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/same-origin"));
        let v = o.enforced().next().unwrap();
        assert_eq!(
            (v.kind.clone(), v.point),
            (ViolationKind::Corp, "coop::corp_check")
        );
        assert!(
            f.fetch(&c, &json_get(srv.port, "cdn.test", "/cross-origin"))
                .allowed()
        );
        // same-site: cdn.app.test is same-site with app.test, cdn.test is not.
        assert!(
            f.fetch(&c, &json_get(srv.port, "cdn.app.test", "/same-site"))
                .allowed()
        );
        let o = f.fetch(&c, &json_get(srv.port, "cdn.test", "/same-site"));
        assert_eq!(enforced_kinds(&o), vec![ViolationKind::Corp]);
        // A same-origin response is not subject to its own CORP.
        let own = ctx(&json_get(srv.port, "app.test", "/"), Destination::Image);
        assert!(
            f.fetch(&own, &json_get(srv.port, "app.test", "/same-origin"))
                .allowed()
        );
        // COEP require-corp: a cross-origin no-cors load with no CORP is refused.
        let mut e = ctx("http://app.test/", Destination::Image);
        e.embedder_policy = coop::Coep::RequireCorp;
        let o = f.fetch(&e, &json_get(srv.port, "cdn.test", "/plain"));
        assert_eq!(enforced_kinds(&o), vec![ViolationKind::Corp]);
        assert!(
            f.fetch(&e, &json_get(srv.port, "cdn.test", "/cross-origin"))
                .allowed()
        );
    }

    fn frame_document(top: &str) -> FetchContext {
        ctx(top, Destination::Iframe)
    }

    fn framing_controls() {
        let srv = start(|r| {
            let mut resp = Resp::ok(b"<p>");
            match r.target.as_str() {
                "/deny" => resp = resp.h("X-Frame-Options", "DENY"),
                "/sameorigin" => resp = resp.h("X-Frame-Options", "SAMEORIGIN"),
                "/conflict" => {
                    resp = resp
                        .h("X-Frame-Options", "DENY")
                        .h("X-Frame-Options", "SAMEORIGIN");
                }
                "/ancestors-none" => {
                    resp = resp.h("Content-Security-Policy", "frame-ancestors 'none'")
                }
                "/ancestors-self" => {
                    resp = resp.h("Content-Security-Policy", "frame-ancestors 'self'")
                }
                "/ancestors-app" => {
                    resp = resp
                        .h("Content-Security-Policy", "frame-ancestors http://app.test")
                        .h("X-Frame-Options", "DENY");
                }
                "/too-many-policies" => {
                    let many = vec!["frame-ancestors *"; 200].join(", ");
                    resp = resp.h("Content-Security-Policy", &many);
                }
                _ => {}
            }
            resp
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let top = "http://app.test/";
        let cross = |p: &str| json_get(srv.port, "other.test", p);
        let blocked = |o: &FetchOutcome, kind: ViolationKind, point: &str| {
            assert!(!o.allowed(), "{:?}", o.violations);
            let v = o.enforced().next().unwrap();
            assert_eq!(
                (v.kind.clone(), v.point),
                (kind, point),
                "{:?}",
                o.violations
            );
        };
        assert!(f.fetch(&frame_document(top), &cross("/open")).allowed());
        blocked(
            &f.fetch(&frame_document(top), &cross("/deny")),
            ViolationKind::FrameOptions,
            "frame::xfo",
        );
        blocked(
            &f.fetch(&frame_document(top), &cross("/sameorigin")),
            ViolationKind::FrameOptions,
            "frame::xfo",
        );
        blocked(
            &f.fetch(&frame_document(top), &cross("/conflict")),
            ViolationKind::FrameOptions,
            "frame::xfo",
        );
        blocked(
            &f.fetch(&frame_document(top), &cross("/ancestors-none")),
            ViolationKind::FrameAncestors,
            "csp::frame-ancestors",
        );
        blocked(
            &f.fetch(&frame_document(top), &cross("/ancestors-self")),
            ViolationKind::FrameAncestors,
            "csp::frame-ancestors",
        );
        // SAMEORIGIN is satisfied by a same-origin embedding.
        let same = frame_document(&json_get(srv.port, "app.test", "/"));
        assert!(
            f.fetch(&same, &json_get(srv.port, "app.test", "/sameorigin"))
                .allowed()
        );
        // frame-ancestors overrides X-Frame-Options, in both directions.
        assert!(
            f.fetch(&frame_document(top), &cross("/ancestors-app"))
                .allowed()
        );
        // A policy list that cannot be parsed is deny-all, which denies framing.
        blocked(
            &f.fetch(&frame_document(top), &cross("/too-many-policies")),
            ViolationKind::FrameAncestors,
            "csp::frame-ancestors",
        );
        // Only a SUBFRAME is subject to framing controls: a top-level
        // navigation to a DENY page works.
        let nav = ctx(top, Destination::Navigation);
        assert!(f.fetch(&nav, &cross("/deny")).allowed());
        // Ancestors beyond the parent count: a SAMEORIGIN page framed by a
        // same-origin parent that is itself inside a cross-origin top fails.
        let mut nested = frame_document(&json_get(srv.port, "other.test", "/mid"));
        nested.document.ancestors = vec![mixed::ContextNode {
            url: u("http://app.test/"),
            origin: origin_of("http://app.test/"),
        }];
        blocked(
            &f.fetch(&nested, &json_get(srv.port, "other.test", "/sameorigin")),
            ViolationKind::FrameOptions,
            "frame::xfo",
        );
    }

    fn navigation_response_establishes_document_policies() {
        let srv = start(|_| {
            Resp::ok(b"<p>")
                .h("Referrer-Policy", "bogus, no-referrer")
                .h("Cross-Origin-Embedder-Policy", "require-corp")
                .h("Cross-Origin-Opener-Policy", "same-origin")
                .h(
                    "Content-Security-Policy",
                    "sandbox allow-scripts; default-src 'self'",
                )
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Navigation);
        let o = f.fetch(&c, &json_get(srv.port, "other.test", "/"));
        let d = o.document.unwrap();
        assert_eq!(d.referrer_policy, Some(ReferrerPolicy::NoReferrer));
        assert_eq!(d.embedder_policy.value, coop::Coep::RequireCorp);
        assert_eq!(d.opener_policy.value, coop::Coop::SameOriginPlusCoep);
        assert!(
            d.origin.is_opaque(),
            "a CSP sandbox without allow-same-origin: opaque origin"
        );
        assert!(d.sandbox.is_some());
        assert_eq!(d.csp.policies().len(), 1);

        // iframe sandbox attribute, no CSP: allow-same-origin keeps the origin.
        let plain = start(|_| Resp::ok(b"<p>"));
        let mut fr = ctx("http://app.test/", Destination::Iframe);
        fr.iframe_sandbox = Some("allow-scripts".to_string());
        let o = fetcher(&psl, &plain).fetch(&fr, &json_get(plain.port, "other.test", "/"));
        assert!(o.document.as_ref().unwrap().origin.is_opaque());
        fr.iframe_sandbox = Some("allow-scripts allow-same-origin".to_string());
        let o = fetcher(&psl, &plain).fetch(&fr, &json_get(plain.port, "other.test", "/"));
        assert!(
            o.document
                .as_ref()
                .unwrap()
                .origin
                .same_origin(&origin_of(&json_get(plain.port, "other.test", "/")))
        );
        let _ = frame::MAX_HEADER_BYTES;
    }

    fn five_hop_chain_where_hop_three_violates() {
        let port = Arc::new(AtomicUsize::new(0));
        let pc = Arc::clone(&port);
        let srv = start(move |r| {
            let p = pc.load(Ordering::SeqCst);
            let next = |h: &str, n: u32| Resp::redirect(&format!("http://{h}:{p}/r{n}"));
            match r.target.as_str() {
                "/r1" => next("b.test", 2),
                "/r2" => next("c.test", 3),
                "/r3" => next("d.test", 4),
                "/r4" => next("e.test", 5),
                "/r5" => next("f.test", 6),
                _ => Resp::ok(b"end"),
            }
        });
        port.store(srv.port as usize, Ordering::SeqCst);
        let psl = psl();
        let p = srv.port;
        let start_url = json_get(p, "a.test", "/r1");

        // Hop 3 (c.test) is not allowed by img-src.
        let mut c = ctx("http://app.test/", Destination::Image);
        c.csp = csp_set(
            &[&format!(
                "img-src http://a.test:{p} http://b.test:{p} http://d.test:{p} http://e.test:{p} http://f.test:{p}"
            )],
            &[],
        );
        let o = fetcher(&psl, &srv).fetch(&c, &start_url);
        assert!(!o.allowed());
        assert_eq!(o.urls.len(), 3, "stopped at hop three: {:?}", o.urls);
        let v = o.enforced().next().unwrap();
        assert_eq!((v.kind.clone(), v.point), (ViolationKind::Csp, "img-src"));
        assert!(v.url.contains("c.test"));
        let seen: Vec<String> = srv.requests().iter().map(|r| r.target.clone()).collect();
        assert_eq!(seen, ["/r1", "/r2"], "hop three never reached the network");

        // Without the violation the whole chain (5 redirects, 6 requests) loads.
        srv.clear();
        let mut ok = ctx("http://app.test/", Destination::Image);
        ok.csp = csp_set(&["img-src http:"], &[]);
        let o = fetcher(&psl, &srv).fetch(&ok, &start_url);
        assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
        assert_eq!(o.urls.len(), 6);
        assert_eq!(srv.requests().len(), 6);
    }

    fn redirects_taint_the_origin_and_are_capped() {
        let port = Arc::new(AtomicUsize::new(0));
        let pc = Arc::clone(&port);
        let srv = start(move |r| {
            let p = pc.load(Ordering::SeqCst);
            let mut resp = match r.target.as_str() {
                "/r1" => Resp::redirect(&format!("http://y.test:{p}/r2")),
                "/r2" => Resp::redirect(&format!("http://z.test:{p}/r3")),
                t if t.starts_with("/loop") => {
                    let n: u32 = t.trim_start_matches("/loop").parse().unwrap_or(0);
                    Resp::redirect(&format!("http://x.test:{p}/loop{}", n + 1))
                }
                _ => Resp::ok(b"end"),
            };
            resp = resp.h("Access-Control-Allow-Origin", "*");
            resp
        });
        port.store(srv.port as usize, Ordering::SeqCst);
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let c = ctx("http://app.test/", Destination::Fetch);
        let o = f.fetch(&c, &json_get(srv.port, "x.test", "/r1"));
        assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
        let reqs = srv.requests();
        let origins: Vec<Option<&str>> = reqs.iter().map(|r| r.header("origin")).collect();
        assert_eq!(
            origins,
            [Some("http://app.test"), Some("null"), Some("null")],
            "Fetch: a cross-origin redirect of a cross-origin response taints the origin for every later hop"
        );
        assert_eq!(o.tainting, Some(Tainting::Cors));

        // A redirect from a CORS response without ACAO is refused at that hop.
        let noacao = start(|_| Resp::redirect("/elsewhere"));
        let o = fetcher(&psl, &noacao).fetch(&c, &json_get(noacao.port, "x.test", "/r"));
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Cors(CorsError::MissingAllowOrigin)]
        );

        // A same-origin-mode request cannot be redirected cross-origin.
        let mut so = ctx(&json_get(srv.port, "x.test", "/"), Destination::Fetch);
        so.mode = RequestMode::SameOrigin;
        let o = fetcher(&psl, &srv).fetch(&so, &json_get(srv.port, "x.test", "/r1"));
        assert_eq!(
            enforced_kinds(&o),
            vec![ViolationKind::Cors(CorsError::CrossOriginForbidden)]
        );

        // An endless chain is cut by px-net's own hop cap.
        let mut nocors = ctx("http://app.test/", Destination::Image);
        nocors.csp = csp_set(&[], &[]);
        let o = fetcher(&psl, &srv).fetch(&nocors, &json_get(srv.port, "x.test", "/loop0"));
        assert!(!o.allowed());
        assert!(matches!(
            enforced_kinds(&o).first(),
            Some(ViolationKind::Redirect(_))
        ));
        assert!(o.urls.len() <= px_net::redirect::MAX_HOPS + 1);
    }

    fn redirect_rewrites_post_and_strips_authorization() {
        let port = Arc::new(AtomicUsize::new(0));
        let pc = Arc::clone(&port);
        let srv = start(move |r| {
            let p = pc.load(Ordering::SeqCst);
            match r.target.as_str() {
                "/r" => Resp::new(302, b"").h("Location", &format!("http://other.test:{p}/to")),
                _ => Resp::ok(b"x"),
            }
            .h("Access-Control-Allow-Origin", "*")
            .h("Access-Control-Allow-Headers", "authorization")
            .h("Access-Control-Allow-Methods", "POST")
        });
        port.store(srv.port as usize, Ordering::SeqCst);
        let psl = psl();
        let mut c = ctx(&json_get(srv.port, "app.test", "/"), Destination::Fetch);
        c.method = "POST".to_string();
        c.body = b"data".to_vec();
        c.headers = vec![Header::new("Authorization", "Bearer t")];
        let o = fetcher(&psl, &srv).fetch(&c, &json_get(srv.port, "app.test", "/r"));
        assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
        let log = srv.requests();
        let last = log.last().unwrap();
        assert_eq!(last.method, "GET");
        assert!(last.body.is_empty());
        assert_eq!(last.header("authorization"), None);
        assert_eq!(log[0].header("authorization"), Some("Bearer t"));
    }

    fn cookies_use_cross_site_for_cookies() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let p = srv.port;
        let dest = NetOrigin::new("http", "api.test", p).unwrap();
        for (top_url, top_host, expect_cookie) in [
            // Same site (api.test): SameSite=Lax cookie travels.
            ("http://www.api.test/", "www.api.test", true),
            // A frame of api.test inside app.test: cross-site, Lax withheld.
            ("http://app.test/", "app.test", false),
        ] {
            let mut f = fetcher(&psl, &srv);
            let key = PartitionKey::new(&psl, top_host, dest.clone()).unwrap();
            f.jar
                .store(&psl, &key, "sid=1; SameSite=Lax; Path=/")
                .unwrap();
            assert_eq!(f.jar.len(), 1);
            let mut c = ctx(&json_get(p, "api.test", "/"), Destination::Fetch);
            c.top_level = origin_of(top_url);
            c.credentials = CredentialsMode::Include;
            c.mode = RequestMode::Cors;
            srv.clear();
            let o = f.fetch(&c, &json_get(p, "api.test", "/r"));
            assert!(o.allowed(), "{:?} {:?}", o.violations, o.error);
            assert_eq!(
                srv.requests()[0].header("cookie"),
                expect_cookie.then_some("sid=1"),
                "top level {top_url}"
            );
            // credentials: omit never sends them.
            c.credentials = CredentialsMode::Omit;
            srv.clear();
            assert!(f.fetch(&c, &json_get(p, "api.test", "/r")).allowed());
            assert_eq!(srv.requests()[0].header("cookie"), None);
        }
        let _ = SameSite::Lax;
    }

    fn set_cookie_is_stored_only_when_credentials_allow() {
        let srv = start(|_| {
            Resp::ok(b"x")
                .h("Set-Cookie", "k=v; Path=/")
                .h("Access-Control-Allow-Origin", "*")
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let mut c = ctx(&json_get(srv.port, "api.test", "/"), Destination::Fetch);
        c.credentials = CredentialsMode::Omit;
        assert!(f.fetch(&c, &json_get(srv.port, "api.test", "/a")).allowed());
        assert!(f.jar.is_empty());
        c.credentials = CredentialsMode::SameOrigin;
        let o = f.fetch(&c, &json_get(srv.port, "api.test", "/a"));
        assert!(o.allowed());
        assert_eq!(f.jar.len(), 1);
        // Set-Cookie is a forbidden response header: script never sees it.
        assert!(
            !o.response
                .unwrap()
                .headers
                .iter()
                .any(|h| h.name == "set-cookie")
        );
    }

    // Hostile input.

    fn hostile_headers_fail_closed() {
        let srv = start(|r| match r.target.as_str() {
            "/dup-acao" => Resp::ok(b"x")
                .h("Access-Control-Allow-Origin", "*")
                .h("Access-Control-Allow-Origin", "*"),
            "/two-origins" => Resp::ok(b"x")
                .h("Access-Control-Allow-Origin", "http://app.test")
                .h("Access-Control-Allow-Origin", "http://evil.test"),
            "/nul" => {
                let mut raw = b"HTTP/1.1 200 X\r\nAccess-Control-Allow-Origin: *\x00\r\nContent-Length: 1\r\n\r\nx".to_vec();
                raw.shrink_to_fit();
                Resp {
                    status: 200,
                    headers: vec![],
                    body: vec![],
                    raw: Some(raw),
                }
            }
            "/huge" => Resp::ok(b"x")
                .h("Access-Control-Allow-Origin", "*")
                .h("X-Pad", &"a".repeat(100_000)),
            "/dup-xfo" => Resp::ok(b"x")
                .h("X-Frame-Options", "SAMEORIGIN")
                .h("X-Frame-Options", "DENY"),
            "/huge-xfo" => Resp::ok(b"x").h(
                "X-Frame-Options",
                &format!("ALLOWALL, {}", "a".repeat(5000)),
            ),
            "/bad-csp" => Resp::ok(b"x").h(
                "Content-Security-Policy",
                "frame-ancestors http://app.test \u{7f}\u{1}",
            ),
            "/dup-corp" => Resp::ok(b"x")
                .h("Cross-Origin-Resource-Policy", "same-origin")
                .h("Cross-Origin-Resource-Policy", "same-origin"),
            _ => Resp::ok(b"x"),
        });
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let fetch_c = ctx("http://app.test/", Destination::Fetch);
        for path in ["/dup-acao", "/two-origins"] {
            let o = f.fetch(&fetch_c, &json_get(srv.port, "api.test", path));
            assert!(!o.allowed(), "{path} must be denied");
            assert!(matches!(
                enforced_kinds(&o).first(),
                Some(ViolationKind::Cors(_))
            ));
        }
        for path in ["/nul", "/huge"] {
            let o = f.fetch(&fetch_c, &json_get(srv.port, "api.test", path));
            assert!(!o.allowed(), "{path} must be denied");
            assert!(
                o.error.is_some(),
                "a malformed response is an error, not a pass"
            );
        }
        let top = "http://app.test/";
        for path in ["/dup-xfo", "/huge-xfo"] {
            let o = f.fetch(
                &frame_document(top),
                &json_get(srv.port, "other.test", path),
            );
            assert!(!o.allowed(), "{path} must be denied: {:?}", o.violations);
            assert_eq!(enforced_kinds(&o), vec![ViolationKind::FrameOptions]);
        }
        // A CSP header with garbage: whatever the parser makes of it, the verdict
        // is never "allowed because we could not read it" for a frame that the
        // policy names (http://app.test) -- either it parses and allows exactly
        // that ancestor, or it is deny-all.
        let o = f.fetch(
            &frame_document("http://evil.test/"),
            &json_get(srv.port, "other.test", "/bad-csp"),
        );
        assert!(!o.allowed());
        // Duplicate CORP lines do not match anything (Fetch): documented limit,
        // pinned so a change is deliberate.
        let img = ctx("http://app.test/", Destination::Image);
        assert!(
            f.fetch(&img, &json_get(srv.port, "cdn.test", "/dup-corp"))
                .allowed()
        );
    }

    fn hostile_requests_fail_closed() {
        let srv = start(|_| Resp::ok(b"x"));
        let psl = psl();
        let mut f = fetcher(&psl, &srv);
        let url = json_get(srv.port, "app.test", "/r");
        let base = ctx(&json_get(srv.port, "app.test", "/"), Destination::Fetch);

        let mut c = base.clone();
        c.method = "TRACE".to_string();
        let o = f.fetch(&c, &url);
        assert_eq!(o.violations[0].point, "cors::forbidden_method");

        for (n, v) in [
            ("Cookie", "a=b"),
            ("Host", "evil"),
            ("Sec-Fetch-Site", "same-origin"),
            ("Origin", "http://evil.test"),
        ] {
            let mut c = base.clone();
            c.headers = vec![Header::new(n, v)];
            let o = f.fetch(&c, &url);
            assert!(!o.allowed(), "author header {n} must be refused");
            assert_eq!(o.violations[0].point, "cors::forbidden_request_header");
        }

        // CRLF injection in an allowed header name's value: refused by the
        // transport, nothing sent.
        let mut c = base.clone();
        c.headers = vec![Header::new("X-A", "1\r\nInjected: 1")];
        let o = f.fetch(&c, &url);
        assert!(!o.allowed());
        assert!(o.error.is_some());
        // Non-UTF-8 header value.
        let mut c = base.clone();
        c.headers = vec![Header {
            name: "X-A".to_string(),
            value: vec![0xff, 0xfe],
        }];
        let o = f.fetch(&c, &url);
        assert!(!o.allowed());
        // Oversized author header.
        let mut c = base.clone();
        c.headers = vec![Header::new("X-A", &"a".repeat(20_000))];
        assert!(!f.fetch(&c, &url).allowed());
        assert_eq!(srv.conns(), 0, "none of those reached the network");

        // Unparsable URL, wrong scheme, credentials in the URL, NUL in the host.
        for bad in [
            "",
            "http://",
            "ftp://app.test/x",
            "http://u:p@app.test/x",
            "http://app.test\0/x",
            "javascript:alert(1)",
            "data:text/html,x",
        ] {
            let o = f.fetch(&base, bad);
            assert!(!o.allowed(), "{bad:?}");
        }
        assert_eq!(srv.conns(), 0);

        // No partition key (IP-literal top level): denied, not unpartitioned.
        let mut c = base.clone();
        c.top_level = origin_of("http://127.0.0.1/");
        let o = f.fetch(&c, &url);
        assert!(!o.allowed());
        assert_eq!(o.violations[0].point, "net::partition");
        assert_eq!(srv.conns(), 0);
    }

    // ----------------------------------------------------------------- the table

    #[test]
    fn net_secure_public_suffix_list_resolves_sites_for_websec() {
        use px_websec::origin::{same_site, schemelessly_same_site};
        let psl = PublicSuffixList::parse("test\nco.test\n").unwrap();
        let o = origin_of;
        assert!(same_site(
            &o("http://a.shop.test/"),
            &o("http://b.shop.test:81/"),
            &psl
        ));
        assert!(!same_site(
            &o("http://a.shop.test/"),
            &o("http://other.test/"),
            &psl
        ));
        assert!(!same_site(
            &o("http://a.co.test/"),
            &o("http://b.co.test/"),
            &psl
        ));
        assert!(schemelessly_same_site(
            &o("https://a.shop.test/"),
            &o("http://b.shop.test/"),
            &psl
        ));
        assert!(!same_site(
            &o("https://a.shop.test/"),
            &o("http://b.shop.test/"),
            &psl
        ));
    }

    struct Scenario {
        name: &'static str,
        /// A px-websec enforcement point this scenario must reach.
        point: &'static str,
        run: fn(),
    }

    const SCENARIOS: &[Scenario] = &[
        Scenario {
            name: "same-origin allowed",
            point: "origin::same_origin",
            run: same_origin_allowed,
        },
        Scenario {
            name: "no-cors cross-origin is opaque",
            point: "cors::filter_response",
            run: no_cors_cross_origin_is_opaque,
        },
        Scenario {
            name: "cors allowed with ACAO",
            point: "cors::validate_response",
            run: cors_allowed_with_acao,
        },
        Scenario {
            name: "cors blocked without ACAO",
            point: "cors::validate_response",
            run: cors_blocked_without_acao,
        },
        Scenario {
            name: "credentialed + wildcard blocked",
            point: "cors::validate_response",
            run: credentialed_wildcard_blocked,
        },
        Scenario {
            name: "credentialed needs ACAC",
            point: "cors::validate_response",
            run: credentialed_cors_needs_allow_credentials,
        },
        Scenario {
            name: "preflight custom header",
            point: "cors::preflight_request",
            run: preflight_for_custom_header,
        },
        Scenario {
            name: "preflight PUT",
            point: "cors::validate_preflight",
            run: preflight_for_put,
        },
        Scenario {
            name: "preflight refusal stops the request",
            point: "cors::validate_preflight",
            run: preflight_refusal_stops_the_request,
        },
        Scenario {
            name: "preflight cache hit",
            point: "cors::cache_lookup",
            run: preflight_cache_hit_on_second_request,
        },
        Scenario {
            name: "preflight cache partitioned",
            point: "cors::cache_lookup",
            run: preflight_cache_is_partitioned,
        },
        Scenario {
            name: "mixed content blocks script",
            point: "mixed::check_request",
            run: mixed_content_blocks_script,
        },
        Scenario {
            name: "mixed content upgrades image",
            point: "mixed::upgrade",
            run: mixed_content_upgrades_image,
        },
        Scenario {
            name: "mixed content survives redirects",
            point: "mixed::check_request",
            run: mixed_content_survives_redirects,
        },
        Scenario {
            name: "HSTS before mixed content",
            point: "mixed::check_request",
            run: hsts_runs_before_mixed_content,
        },
        Scenario {
            name: "CSP script-src blocks script",
            point: "script-src-elem",
            run: csp_script_src_blocks_script,
        },
        Scenario {
            name: "CSP connect-src blocks fetch",
            point: "connect-src",
            run: csp_connect_src_blocks_fetch,
        },
        Scenario {
            name: "CSP report-only records, allows",
            point: "script-src-elem",
            run: csp_report_only_records_but_allows,
        },
        Scenario {
            name: "CSP governs every destination",
            point: "worker-src",
            run: csp_governs_every_destination,
        },
        Scenario {
            name: "CSP upgrade-insecure / block-all-mixed",
            point: "upgrade-insecure-requests",
            run: csp_upgrade_insecure_requests_and_block_all_mixed,
        },
        Scenario {
            name: "referrer policies on the wire",
            point: "referrer::compute",
            run: referrer_policies_on_the_wire,
        },
        Scenario {
            name: "referrer policy on a redirect",
            point: "referrer::parse_header",
            run: referrer_policy_header_on_redirect_applies_to_next_hop,
        },
        Scenario {
            name: "SRI pass and fail",
            point: "sri::verify",
            run: sri_pass_and_fail,
        },
        Scenario {
            name: "CORP and COEP",
            point: "coop::corp_check",
            run: corp_blocks_and_coep_requires,
        },
        Scenario {
            name: "frame controls",
            point: "frame::combine",
            run: framing_controls,
        },
        Scenario {
            name: "navigation establishes document policies",
            point: "frame::sandboxed_origin",
            run: navigation_response_establishes_document_policies,
        },
        Scenario {
            name: "5-hop chain, hop 3 violates",
            point: "img-src",
            run: five_hop_chain_where_hop_three_violates,
        },
        Scenario {
            name: "redirects taint origin, are capped",
            point: "cors::taint",
            run: redirects_taint_the_origin_and_are_capped,
        },
        Scenario {
            name: "redirect rewrites POST, strips Authorization",
            point: "cors::taint",
            run: redirect_rewrites_post_and_strips_authorization,
        },
        Scenario {
            name: "cookies use cross_site_for_cookies",
            point: "origin::cross_site_for_cookies",
            run: cookies_use_cross_site_for_cookies,
        },
        Scenario {
            name: "Set-Cookie stored per credentials mode",
            point: "origin::cross_site_for_cookies",
            run: set_cookie_is_stored_only_when_credentials_allow,
        },
        Scenario {
            name: "hostile response headers fail closed",
            point: "frame::xfo",
            run: hostile_headers_fail_closed,
        },
        Scenario {
            name: "hostile requests fail closed",
            point: "cors::forbidden_request_header",
            run: hostile_requests_fail_closed,
        },
    ];

    /// Run every scenario, checking each reached its named point; return the
    /// union of every point reached.
    fn run_all() -> BTreeSet<&'static str> {
        let mut reached = BTreeSet::new();
        for s in SCENARIOS {
            trace::clear();
            (s.run)();
            let hits = trace::snapshot();
            assert!(
                hits.contains(&s.point),
                "scenario {:?} never reached {} (reached: {:?})",
                s.name,
                s.point,
                hits.iter().collect::<BTreeSet<_>>()
            );
            reached.extend(hits);
        }
        reached
    }

    #[test]
    fn net_secure_scenarios_reach_their_enforcement_points() {
        let _ = run_all();
    }

    /// Points that cannot be reached through a fetch, and why. Checked both ways.
    const NOT_WIRED: &[(&str, &str)] = &[
        (
            "referrer::parse_attribute",
            "the `referrerpolicy` attribute is parsed by the HTML layer; the fetch takes the parsed policy",
        ),
        (
            "coop::group_switch",
            "browsing-context-group switching is a navigation decision (content process / Phase 13); the fetch only parses COOP",
        ),
        (
            "inline:script",
            "inline script is decided by the HTML layer, there is no request",
        ),
        ("inline:script-attr", "inline event handlers: HTML layer"),
        ("inline:style", "inline <style>: style system"),
        ("inline:style-attr", "style attributes: style system"),
        (
            "inline:navigation",
            "javascript: URL navigation: navigation layer",
        ),
        ("unsafe-eval", "eval/Function: script engine"),
        ("wasm-unsafe-eval", "Wasm compilation: script engine"),
        ("form-action", "form submission: navigation layer"),
        ("base-uri", "<base> processing: HTML layer"),
    ];

    #[test]
    fn net_secure_every_enforcement_point_is_reached_through_a_fetch() {
        let reached = run_all();

        let mut all: Vec<&'static str> = Vec::new();
        all.extend(px_websec::origin::ENFORCEMENT_POINTS);
        all.extend(cors::ENFORCEMENT_POINTS);
        all.extend(mixed::ENFORCEMENT_POINTS);
        all.extend(referrer::ENFORCEMENT_POINTS);
        all.extend(sri::ENFORCEMENT_POINTS);
        all.extend(frame::ENFORCEMENT_POINTS);
        all.extend(coop::ENFORCEMENT_POINTS);
        all.extend(px_websec::csp::ENFORCEMENT_POINTS.iter().map(|p| p.name));

        let unwired: BTreeSet<&str> = NOT_WIRED.iter().map(|(p, _)| *p).collect();
        let mut missing = Vec::new();
        for p in &all {
            let hit = reached.contains(p);
            match (hit, unwired.contains(p)) {
                (true, false) | (false, true) => {}
                (false, false) => missing.push(format!(
                    "{p}: not reached through px-net and not listed in NOT_WIRED"
                )),
                (true, true) => missing.push(format!(
                    "{p}: listed in NOT_WIRED but IS reached; delete the entry"
                )),
            }
        }
        for p in &unwired {
            if !all.contains(p) {
                missing.push(format!(
                    "{p}: NOT_WIRED names a point px-websec does not list"
                ));
            }
        }
        for p in &reached {
            if !all.contains(p) {
                missing.push(format!("{p}: reached but not in any ENFORCEMENT_POINTS"));
            }
        }
        assert!(missing.is_empty(), "\n{}", missing.join("\n"));
        eprintln!(
            "{} points: {} reached through px-net, {} not wired",
            all.len(),
            all.len() - unwired.len(),
            unwired.len()
        );
    }
}
