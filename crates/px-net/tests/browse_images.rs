//! `Loader::image` against a local server: the same secure path as stylesheets
//! (referrer, CSP `img-src`), plus what only images need (content-type and
//! signature checks, the byte cap).
#![cfg(feature = "testing")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};

use px_net::browse::{ImageKind, LoadError, MAX_IMAGE_BYTES, WebSession};

/// A 1x1 PNG (signature is what the loader looks at; the bytes need not decode).
const PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53,
    0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
    0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E,
    0x44, 0xAE, 0x42, 0x60, 0x82,
];

struct Route {
    path: &'static str,
    headers: &'static str,
    body: Vec<u8>,
}

fn route(path: &'static str, headers: &'static str, body: &[u8]) -> Route {
    Route {
        path,
        headers,
        body: body.to_vec(),
    }
}

fn serve(routes: Vec<Route>, n: usize) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
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
                .find(|r| r.path == path)
                .map_or(("404 Not Found", "", &[][..]), |r| {
                    ("200 OK", r.headers, &r.body[..])
                });
            let head = format!(
                "HTTP/1.1 {status}\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = s.write_all(head.as_bytes());
            let _ = s.write_all(body);
        }
    });
    (addr, seen)
}

const HTML: &str = "Content-Type: text/html\r\n";

#[test]
fn an_image_is_fetched_with_the_documents_referrer_policy() {
    let (addr, seen) = serve(
        vec![
            route("/p/page.html", HTML, b"<img src=a.png>"),
            route("/p/a.png", "Content-Type: image/png\r\n", PNG),
        ],
        2,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    loader.navigate("http://site.test/p/page.html").unwrap();
    let img = loader.image("a.png").unwrap();
    assert_eq!(img.kind, ImageKind::Png);
    assert_eq!(img.url, "http://site.test/p/a.png");
    assert_eq!(img.body, PNG);
    let seen = seen.lock().unwrap();
    let req = seen[1].to_ascii_lowercase();
    assert!(req.starts_with("get /p/a.png"));
    assert!(req.contains("referer: http://site.test/p/page.html"));
    assert!(!req.contains("cookie:") && !req.contains("user-agent"));
}

#[test]
fn csp_img_src_stops_the_request_before_it_is_sent() {
    let (addr, seen) = serve(
        vec![
            route(
                "/",
                "Content-Type: text/html\r\nContent-Security-Policy: img-src 'none'\r\n",
                b"x",
            ),
            route("/a.png", "Content-Type: image/png\r\n", PNG),
        ],
        2,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    loader.navigate("http://site.test/").unwrap();
    let e = loader.image("/a.png").unwrap_err();
    assert!(matches!(e, LoadError::Blocked { .. }), "{e:?}");
    // Only the navigation reached the server.
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn default_src_self_allows_same_origin_and_refuses_cross_origin_images() {
    let (addr, seen) = serve(
        vec![
            route(
                "/",
                "Content-Type: text/html\r\nContent-Security-Policy: default-src 'self'\r\n",
                b"x",
            ),
            route("/a.png", "Content-Type: image/png\r\n", PNG),
        ],
        3,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    loader.navigate("http://site.test/").unwrap();
    assert!(loader.image("/a.png").is_ok());
    let e = loader.image("http://other.test/a.png").unwrap_err();
    assert!(matches!(e, LoadError::Blocked { .. }), "{e:?}");
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[test]
fn what_is_not_a_picture_is_refused_whatever_it_is_called() {
    let (addr, _seen) = serve(
        vec![
            route("/", HTML, b"x"),
            // Honest HTML.
            route("/html", HTML, b"<html>not a picture</html>"),
            // A picture's label on bytes that are not one.
            route(
                "/lie.png",
                "Content-Type: image/png\r\n",
                b"<html>hi</html>",
            ),
            // A picture's bytes under a document's label.
            route("/mislabel", "Content-Type: text/plain\r\n", PNG),
            // SVG is drawn by px-image, which never runs or loads anything in it;
            // an SVG label on a page is still just a page.
            route(
                "/v.svg",
                "Content-Type: image/svg+xml\r\n",
                b"<svg xmlns='http://www.w3.org/2000/svg'/>",
            ),
            route(
                "/fake.svg",
                "Content-Type: image/svg+xml\r\n",
                b"<html><body>not a picture</body></html>",
            ),
            route(
                "/svg-as-text",
                "Content-Type: text/html\r\n",
                b"<svg xmlns='http://www.w3.org/2000/svg'/>",
            ),
            // No label at all is fine when the bytes are right.
            route("/bare", "", PNG),
            route("/octet", "Content-Type: application/octet-stream\r\n", PNG),
        ],
        10,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    loader.navigate("http://site.test/").unwrap();
    for bad in [
        "/html",
        "/lie.png",
        "/mislabel",
        "/fake.svg",
        "/svg-as-text",
        "/missing",
    ] {
        assert!(loader.image(bad).is_err(), "{bad}");
    }
    assert_eq!(loader.image("/v.svg").unwrap().kind, ImageKind::Svg);
    assert!(loader.image("/bare").is_ok());
    assert!(loader.image("/octet").is_ok());
}

#[test]
fn an_oversized_body_is_refused() {
    let mut big = PNG.to_vec();
    big.resize(MAX_IMAGE_BYTES + 1, 0);
    let (addr, _seen) = serve(
        vec![
            route("/", HTML, b"x"),
            route("/big.png", "Content-Type: image/png\r\n", &big),
        ],
        2,
    );
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    loader.connect_to(addr);
    loader.navigate("http://site.test/").unwrap();
    assert!(loader.image("/big.png").is_err());
}

#[test]
fn only_http_images_resolve_and_a_loader_without_a_document_refuses() {
    let session = WebSession::new().unwrap();
    let mut loader = session.loader();
    assert!(matches!(
        loader.image("http://x.test/a.png"),
        Err(LoadError::BadAddress)
    ));
    let (addr, _s) = serve(vec![route("/", HTML, b"x")], 1);
    loader.connect_to(addr);
    loader.navigate("http://site.test/").unwrap();
    for bad in [
        "file:///etc/passwd",
        "data:image/png;base64,AAAA",
        "javascript:1",
    ] {
        assert!(
            matches!(loader.image(bad), Err(LoadError::BadAddress)),
            "{bad}"
        );
    }
}
