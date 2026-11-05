//! Pictures through the shell: a local page's relative images, a web page's
//! images from a local server (which requests were made, what policy stopped,
//! how a failure degrades to `alt`), caps, and what is drawn.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::path::PathBuf;

use px_shell::nav::Location;
use px_shell::page::{self, Resources};
use px_shell::web::Net;

const RED: [u8; 4] = [255, 0, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const WHITE: [u8; 4] = [255, 255, 255, 255];

/// A 4x4 PNG: left half red, right half blue.
fn png_rb() -> Vec<u8> {
    let mut rgba = Vec::new();
    for _y in 0..4 {
        for x in 0..4 {
            rgba.extend_from_slice(if x < 2 { &RED } else { &BLUE });
        }
    }
    px_paint::png::encode(4, 4, &rgba).unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("px-shell-img-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn draw(source: &str, res: &Resources, w: u32, h: u32) -> px_paint::Canvas {
    page::layout(source, res, w, h)
        .unwrap()
        .render(0, w, h)
        .unwrap()
}

#[test]
fn a_local_page_shows_the_pictures_next_to_it() {
    let dir = temp_dir("local");
    std::fs::write(dir.join("a.png"), png_rb()).unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/b.png"), png_rb()).unwrap();
    std::fs::write(
        dir.join("s.css"),
        "div.bg { width: 8px; height: 4px; background: url(sub/b.png) no-repeat; \
         background-size: 8px 4px; image-rendering: pixelated }",
    )
    .unwrap();
    std::fs::write(
        dir.join("p.html"),
        "<!doctype html><link rel=stylesheet href=s.css><body style='margin:0'>\
         <img src='a.png' style='display:block; width:8px; image-rendering:pixelated'>\
         <div class=bg></div>\
         <img src='missing.png' alt='hi'>",
    )
    .unwrap();
    let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
    assert!(!loaded.failed);
    assert_eq!(
        loaded.res.images.len(),
        2,
        "a.png and sub/b.png; missing is skipped"
    );
    let cv = draw(&loaded.source, &loaded.res, 40, 40);
    // The img: 4x4 scaled to 8x8, red then blue columns of 4.
    assert_eq!(cv.pixel(0, 0), Some(RED));
    assert_eq!(cv.pixel(3, 7), Some(RED));
    assert_eq!(cv.pixel(4, 0), Some(BLUE));
    assert_eq!(cv.pixel(8, 0), Some(WHITE));
    // The background of the div below it (y 8..12).
    assert_eq!(cv.pixel(0, 8), Some(RED));
    assert_eq!(cv.pixel(7, 11), Some(BLUE));
    // The missing image's alt text drew glyphs (non-white ink) on the next line.
    let ink = (12..40).any(|y| (0..40).any(|x| cv.pixel(x, y) != Some(WHITE)));
    assert!(ink, "alt text is shown for a picture that is not there");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_local_page_cannot_reach_outside_its_directory_or_the_web() {
    let dir = temp_dir("escape");
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    std::fs::write(dir.join("secret.png"), png_rb()).unwrap();
    std::fs::write(
        dir.join("pages/p.html"),
        "<img src='../secret.png'><img src='/etc/hostname'><img src='http://127.0.0.1:1/x.png'>\
         <img src='file:///etc/passwd'>",
    )
    .unwrap();
    let loaded = Location::File(dir.join("pages/p.html")).load(&mut Net::default());
    assert!(loaded.res.images.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_oversized_or_corrupt_local_file_is_skipped() {
    let dir = temp_dir("bad");
    let mut huge = png_rb();
    huge.resize(px_net::browse::MAX_IMAGE_BYTES + 1, 0);
    std::fs::write(dir.join("huge.png"), huge).unwrap();
    std::fs::write(dir.join("junk.png"), b"\x89PNG\r\n\x1a\nnot really").unwrap();
    std::fs::write(dir.join("text.png"), b"hello").unwrap();
    // A PNG header that declares 60000 x 60000: over the pixel cap, refused
    // before anything is allocated.
    let mut bomb = png_rb();
    bomb[16..20].copy_from_slice(&60000u32.to_be_bytes());
    bomb[20..24].copy_from_slice(&60000u32.to_be_bytes());
    std::fs::write(dir.join("bomb.png"), bomb).unwrap();
    std::fs::write(
        dir.join("p.html"),
        "<img src=huge.png alt=a><img src=junk.png alt=b><img src=text.png alt=c><img src=bomb.png alt=d>",
    )
    .unwrap();
    let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
    assert!(loaded.res.images.is_empty());
    // And the page still lays out, with the alt texts.
    let laid = page::layout(&loaded.source, &loaded.res, 200, 100).unwrap();
    assert!(laid.height > 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_a_bounded_number_of_images_is_requested() {
    let dir = temp_dir("many");
    std::fs::write(dir.join("a.png"), png_rb()).unwrap();
    let mut html = String::new();
    for i in 0..(px_shell::images::MAX_IMAGES + 20) {
        // Distinct URLs for one file (a query string is ignored locally, so
        // use distinct files instead).
        std::fs::write(dir.join(format!("i{i}.png")), png_rb()).unwrap();
        html.push_str(&format!("<img src=i{i}.png>"));
    }
    std::fs::write(dir.join("p.html"), html).unwrap();
    let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
    assert_eq!(loaded.res.images.len(), px_shell::images::MAX_IMAGES);
    std::fs::remove_dir_all(&dir).unwrap();
}

// -- The web, from a local server (the `testing` feature's `connect_to`) -------

#[cfg(feature = "testing")]
mod web {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::{Arc, Mutex};
    // -- The web, from a local server ---------------------------------------------

    struct Route {
        path: &'static str,
        headers: String,
        body: Vec<u8>,
    }

    fn route(path: &'static str, headers: &str, body: &[u8]) -> Route {
        Route {
            path,
            headers: headers.to_string(),
            body: body.to_vec(),
        }
    }

    /// Serve `routes` on loopback until the listener is dropped with the process;
    /// returns the base URL and the list of request paths seen, in order.
    fn serve(routes: Vec<Route>) -> (SocketAddr, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut buf = vec![0u8; 8192];
                let len = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..len]).into_owned();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                log.lock().unwrap().push(path.clone());
                let (status, headers, body) = routes
                    .iter()
                    .find(|r| r.path == path)
                    .map_or(("404 Not Found", "", &[][..]), |r| {
                        ("200 OK", r.headers.as_str(), &r.body[..])
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

    fn site(addr: SocketAddr) -> Net {
        let mut net = Net::default();
        net.connect_to = Some(addr);
        net
    }

    const HTML: &str = "Content-Type: text/html\r\n";
    const PNGH: &str = "Content-Type: image/png\r\n";

    #[test]
    fn a_web_page_fetches_its_chosen_pictures_and_only_those() {
        let page_html = "<!doctype html><body style='margin:0'>\
            <img src=fallback.png srcset='small.png 400w, big.png 2000w' sizes='100vw' \
                 style='display:block;width:8px;image-rendering:pixelated'>\
            <picture><source srcset='nope.webp' type='image/webp'>\
                     <source srcset='pic.png' media='(min-width: 100px)'>\
                     <img src=pfallback.png alt=x style='display:block'></picture>\
            <img src=bad.png alt='broken' style='display:block'>\
            <div style=\"width:4px;height:4px;background:url('/bg.png')\"></div>";
        let png = png_rb();
        let (addr, seen) = serve(vec![
            route("/p.html", HTML, page_html.as_bytes()),
            route("/small.png", PNGH, &png),
            route("/big.png", PNGH, &png),
            route("/pic.png", PNGH, &png),
            route("/bg.png", PNGH, &png),
            route(
                "/bad.png",
                "Content-Type: text/html\r\n",
                b"<html>no</html>",
            ),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(!loaded.failed, "{}", loaded.source);
        assert_eq!(loaded.res.images.len(), 3, "pic, big (width 1024), bg");
        let seen = seen.lock().unwrap().clone();
        // The navigation, then the chosen candidates in document order: the
        // 2000w one for a 1024 plan width, the picture's source, the refused
        // `bad.png`, and the background. Never small.png, fallback.png, nope.webp,
        // pfallback.png.
        assert_eq!(seen[0], "/p.html");
        for want in ["/big.png", "/pic.png", "/bad.png", "/bg.png"] {
            assert!(seen.iter().any(|p| p == want), "{want} in {seen:?}");
        }
        for never in [
            "/small.png",
            "/fallback.png",
            "/nope.webp",
            "/pfallback.png",
        ] {
            assert!(!seen.iter().any(|p| p == never), "{never} in {seen:?}");
        }
        let cv = draw(&loaded.source, &loaded.res, 50, 50);
        // First image: 4x4 -> 8x8 (width attr style), red | blue columns.
        assert_eq!(cv.pixel(0, 0), Some(RED));
        assert_eq!(cv.pixel(7, 7), Some(BLUE));
    }

    #[test]
    fn a_csp_img_src_policy_stops_every_picture_and_the_page_still_shows_alt() {
        let (addr, seen) = serve(vec![
            route(
                "/p.html",
                "Content-Type: text/html\r\nContent-Security-Policy: img-src 'none'\r\n",
                b"<body style='margin:0'><img src=a.png alt='nothing here'>\
                  <div style='height:4px;background:url(b.png)'></div>",
            ),
            route("/a.png", PNGH, &png_rb()),
            route("/b.png", PNGH, &png_rb()),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(!loaded.failed, "{}", loaded.source);
        assert!(loaded.res.images.is_empty());
        assert_eq!(seen.lock().unwrap().as_slice(), ["/p.html"]);
        let cv = draw(&loaded.source, &loaded.res, 200, 60);
        assert!((0..60).any(|y| (0..200).any(|x| cv.pixel(x, y) != Some(WHITE))));
    }

    #[test]
    fn the_image_request_carries_the_documents_referrer() {
        // The server records raw requests via the path only; check through the
        // header by serving the image only when the referrer is right.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let referers = Arc::new(Mutex::new(Vec::new()));
        let log = referers.clone();
        let png = png_rb();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { return };
                let mut buf = vec![0u8; 8192];
                let len = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..len]).into_owned();
                let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                let referer = req
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("referer:"))
                    .map(str::to_string);
                log.lock().unwrap().push((path.clone(), referer));
                let (headers, body): (&str, &[u8]) = if path == "/p.html" {
                    (HTML, b"<img src=a.png>")
                } else {
                    (PNGH, &png)
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\n{headers}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = s.write_all(head.as_bytes());
                let _ = s.write_all(body);
            }
        });
        let loaded = site(addr).load("http://site.test/p.html");
        assert_eq!(loaded.res.images.len(), 1);
        let log = referers.lock().unwrap().clone();
        assert_eq!(log[0].1, None, "a navigation sends no referrer");
        assert_eq!(
            log[1].1.as_deref(),
            Some("Referer: http://site.test/p.html")
        );
    }
}
