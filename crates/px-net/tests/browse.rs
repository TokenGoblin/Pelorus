//! The embedder's loader against a local server: a navigation with a redirect,
//! then a linked stylesheet, and what the server saw of each request.
#![cfg(feature = "testing")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use px_net::browse::{LoadError, WebSession};

/// Serve `routes` (path -> (status line, extra headers, body)) on loopback,
/// recording each request's text. Handles `n` connections, then stops.
fn serve(
    routes: Vec<(&'static str, &'static str, &'static str, &'static str)>,
    n: usize,
) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for _ in 0..n {
            let Ok((mut s, _)) = listener.accept() else {
                return;
            };
            let mut buf = vec![0u8; 8192];
            let len = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..len]).into_owned();
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            log.lock().unwrap().push(req);
            let (status, headers, body) = routes
                .iter()
                .find(|r| r.0 == path)
                .map_or(("404 Not Found", "", ""), |r| (r.1, r.2, r.3));
            let resp = format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (addr, seen)
}

#[test]
fn redirect_then_stylesheet_with_policy_applied() {
    let (addr, seen) = serve(
        vec![
            ("/", "301 Moved", "Location: /home\r\n", ""),
            (
                "/home",
                "200 OK",
                "Content-Type: text/html; charset=utf-8\r\nContent-Security-Policy: style-src 'self'\r\n",
                "<link rel=stylesheet href=s.css>",
            ),
            (
                "/s.css",
                "200 OK",
                "Content-Type: text/css\r\n",
                "p{color:red}",
            ),
        ],
        3,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    let doc = loader.navigate("http://site.test/").unwrap();
    assert_eq!(doc.url, "http://site.test/home");
    assert_eq!(doc.status, 200);
    assert!(doc.body.starts_with(b"<link"));
    // The CSP the page sent is enforced on inline style, fail closed.
    assert!(!loader.allows_inline_style("p{}"));
    let css = loader.stylesheet("s.css").unwrap();
    assert_eq!(css, b"p{color:red}");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for req in seen.iter() {
        let lower = req.to_ascii_lowercase();
        assert!(!lower.contains("cookie:"), "no cookie: {req}");
        assert!(!lower.contains("user-agent"), "no user agent: {req}");
    }
    assert!(
        !seen[0].to_ascii_lowercase().contains("referer:"),
        "a typed navigation sends no referrer"
    );
    assert!(
        seen[2]
            .to_ascii_lowercase()
            .contains("referer: http://site.test/home")
    );
}

#[test]
fn a_dead_server_is_an_error_not_a_page() {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    let e = loader.navigate("http://site.test/").unwrap_err();
    assert!(matches!(e, LoadError::Network(_)));
    assert!(!e.reason().is_empty());
}

#[test]
fn only_http_and_https_are_navigable() {
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    assert!(matches!(
        loader.navigate("file:///etc/passwd"),
        Err(LoadError::BadAddress)
    ));
    assert!(matches!(
        loader.navigate("javascript:alert(1)"),
        Err(LoadError::BadAddress)
    ));
}
