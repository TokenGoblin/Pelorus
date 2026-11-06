//! Web fonts through the shell: a local page's `@font-face` files, a web page's
//! fonts from a local server (CORS, CSP, size cap), hostile font files, and the
//! generic families resolved from an in-memory font directory.

use std::path::PathBuf;

use px_shell::nav::Location;
use px_shell::page::{self, Resources};
use px_shell::web::Net;

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fonts");
    std::fs::read(p.join(name)).unwrap()
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("px-shell-font-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The width in CSS px of the link box around the text of `<a>` in `html`.
fn link_width(source: &str, res: &Resources) -> i32 {
    let laid = page::layout(source, res, 800, 200).unwrap();
    laid.links
        .iter()
        .map(|l| l.w)
        .max()
        .expect("the page has a link")
}

const AAAA: &str = "<a href=x style=\"font-size:20px\">AAAA</a>";

fn page_with(css: &str, family: &str) -> String {
    format!(
        "<!doctype html><style>{css} body{{margin:0}} a{{font-family:{family};text-decoration:none}}</style>{AAAA}"
    )
}

#[test]
fn a_local_page_uses_the_fonts_it_declares_and_measures_with_them() {
    let dir = temp_dir("local");
    std::fs::write(dir.join("t.woff2"), fixture("PxText-Regular.woff2")).unwrap();
    std::fs::write(dir.join("b.woff2"), fixture("PxText-Bold.woff2")).unwrap();
    std::fs::write(dir.join("i.ttf"), fixture("PxIcons-Regular.ttf")).unwrap();
    let css = "@font-face{font-family:'Px T';src:url(t.woff2)} \
               @font-face{font-family:'Px T';font-weight:700;src:url(b.woff2)} \
               @font-face{font-family:'Px I';src:url(i.ttf)}";
    let html = |family: &str, weight: &str| {
        format!(
            "{}<style>a{{font-weight:{weight}}}</style>",
            page_with(css, family)
        )
    };
    let load = |family: &str, weight: &str| {
        let src = html(family, weight);
        std::fs::write(dir.join("p.html"), &src).unwrap();
        let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
        assert!(!loaded.failed);
        link_width(&loaded.source, &loaded.res)
    };
    // Px Text Regular: 600/1000 em per letter at 20px; Bold: 700/1000.
    assert_eq!(load("'Px T'", "400"), 4 * 12);
    assert_eq!(load("'Px T'", "700"), 4 * 14);
    // 'A' is not in the icon font: the bundled Noto Sans draws it, so the width
    // is neither of the above.
    let noto = load("'Px I', 'Noto Sans'", "400");
    assert!(noto != 48 && noto != 56 && noto > 0, "{noto}");
    // An undeclared family is the same as no font at all.
    assert_eq!(load("'Px T', 'Noto Sans'", "400"), 48);
    assert_eq!(load("'Nope', 'Noto Sans'", "400"), noto);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_local_page_cannot_reach_fonts_outside_its_directory_or_the_web() {
    let dir = temp_dir("escape");
    std::fs::create_dir_all(dir.join("pages")).unwrap();
    std::fs::write(dir.join("secret.ttf"), fixture("PxText-Regular.ttf")).unwrap();
    let css = "@font-face{font-family:'Px T';src:url(../secret.ttf)} \
               @font-face{font-family:'Px T2';src:url(http://127.0.0.1:1/x.ttf)} \
               @font-face{font-family:'Px T3';src:url(file:///etc/passwd)}";
    std::fs::write(dir.join("pages/p.html"), page_with(css, "'Px T'")).unwrap();
    let loaded = Location::File(dir.join("pages/p.html")).load(&mut Net::default());
    assert!(loaded.res.fonts.faces.is_empty());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hostile_font_files_are_skipped_and_the_page_still_lays_out() {
    let dir = temp_dir("hostile");
    let good = fixture("PxIcons-Regular.woff2");
    let mut cut = good.clone();
    cut.truncate(good.len() / 2);
    let mut flipped = good.clone();
    for i in (40..flipped.len()).step_by(7) {
        flipped[i] ^= 0x5a;
    }
    let mut otto = fixture("PxIcons-Regular.ttf");
    otto[..4].copy_from_slice(b"OTTO");
    let mut huge = fixture("PxIcons-Regular.ttf");
    huge.resize(px_net::browse::MAX_FONT_BYTES + 1, 0);
    for (name, bytes) in [
        ("cut.woff2", cut),
        ("flipped.woff2", flipped),
        ("otto.otf", otto),
        ("huge.ttf", huge),
        ("text.ttf", b"hello world".to_vec()),
        ("empty.ttf", Vec::new()),
    ] {
        std::fs::write(dir.join(name), bytes).unwrap();
    }
    let css = "@font-face{font-family:A;src:url(cut.woff2)} @font-face{font-family:B;src:url(flipped.woff2)} \
               @font-face{font-family:C;src:url(otto.otf)} @font-face{font-family:D;src:url(huge.ttf)} \
               @font-face{font-family:E;src:url(text.ttf)} @font-face{font-family:F;src:url(empty.ttf)} \
               @font-face{font-family:G;src:url(missing.ttf)}";
    let src = page_with(css, "A, B, C, D, E, F, G, 'Noto Sans'");
    std::fs::write(dir.join("p.html"), &src).unwrap();
    let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
    assert!(!loaded.failed);
    assert!(
        loaded.res.fonts.faces.is_empty(),
        "{:?}",
        loaded.res.fonts.faces
    );
    // Text falls through the whole list to Noto Sans.
    assert!(link_width(&loaded.source, &loaded.res) > 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn only_a_bounded_number_of_font_rules_is_loaded() {
    let dir = temp_dir("many");
    let mut css = String::new();
    for i in 0..(px_layout::fontset::MAX_FACE_RULES + 8) {
        std::fs::write(dir.join(format!("f{i}.ttf")), {
            // Distinct bytes per file, so none is a duplicate registration.
            let mut b = fixture("PxIcons-Regular.ttf");
            let n = b.len();
            b.extend_from_slice(&[i as u8; 4]);
            assert!(b.len() > n);
            b
        })
        .unwrap();
        css.push_str(&format!("@font-face{{font-family:F{i};src:url(f{i}.ttf)}}"));
    }
    std::fs::write(dir.join("p.html"), page_with(&css, "F0")).unwrap();
    let loaded = Location::File(dir.join("p.html")).load(&mut Net::default());
    assert_eq!(
        loaded.res.fonts.faces.len(),
        px_layout::fontset::MAX_FACE_RULES
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

// -- Generic families from an in-memory font directory --------------------------

#[test]
fn generic_families_resolve_to_distinct_faces_from_the_system_set() {
    use px_text::sysfonts::{FakeFs, GenericFamily, ScanLimits, SystemFonts};
    let mut fs = FakeFs::new();
    fs.file("/f/PxSerif-Regular.ttf", fixture("PxSerif-Regular.ttf"));
    fs.file("/f/PxMono-Regular.ttf", fixture("PxMono-Regular.ttf"));
    fs.file("/f/PxText-Regular.ttf", fixture("PxText-Regular.ttf"));
    fs.file("/f/broken.ttf", b"nope".to_vec());
    let sys = SystemFonts::scan(&fs, &["/f".into()], &ScanLimits::default());
    assert_eq!(sys.len(), 3, "the broken file is not a face");
    let read = |f: &px_text::sysfonts::SystemFace| {
        use px_text::sysfonts::DirReader;
        fs.read_bytes(&f.path, 1 << 24).ok()
    };
    // Preference lists of our own for the test: the first name is not installed,
    // so the second is taken (the walk, not luck, picks a face).
    let prefs = |g: GenericFamily| -> Vec<String> {
        match g {
            GenericFamily::Serif => vec!["Not Installed".into(), "Px Serif".into()],
            GenericFamily::Monospace => vec!["Px Mono".into()],
            GenericFamily::SansSerif | GenericFamily::SystemUi => vec!["Px Text".into()],
            _ => vec!["Missing".into()],
        }
    };
    let got = px_shell::fonts::resolve_generics(&sys, &read, &prefs);
    let of = |g| got.iter().find(|(k, _)| *k == g).map(|(_, id)| *id);
    let (serif, mono, sans) = (
        of(GenericFamily::Serif).unwrap(),
        of(GenericFamily::Monospace).unwrap(),
        of(GenericFamily::SansSerif).unwrap(),
    );
    assert!(serif != mono && mono != sans && serif != sans, "distinct");
    assert_eq!(of(GenericFamily::SystemUi), Some(sans), "same file, same id");
    assert_eq!(of(GenericFamily::Cursive), None, "nothing installed: no entry");
    // Deterministic.
    assert_eq!(got, px_shell::fonts::resolve_generics(&sys, &read, &prefs));
    // And the faces really are the three fixtures.
    let adv = |id| {
        let f = px_layout::text::font_of(id).unwrap();
        f.advance(f.glyph_index('A').unwrap()).unwrap()
    };
    assert_eq!((adv(serif), adv(mono), adv(sans)), (650, 500, 600));
}

#[test]
fn a_page_override_beats_the_installed_generic_and_unmapped_ones_fall_back() {
    // `sans-serif` mapped for this page only; the page's list then continues to
    // the bundled face.
    let id = px_layout::fontset::register_font(&fixture("PxMono-Regular.ttf")).unwrap();
    let mut fonts = px_layout::fontset::PageFonts::default();
    fonts.set_generic(px_text::sysfonts::GenericFamily::SansSerif, id);
    let res = Resources {
        fonts: std::sync::Arc::new(fonts),
        ..Resources::default()
    };
    let w = |family: &str| {
        link_width(
            &format!(
                "<style>body{{margin:0}}a{{font-family:{family};font-size:20px;text-decoration:none}}</style>{AAAA}"
            ),
            &res,
        )
    };
    assert_eq!(w("sans-serif, 'Noto Sans'"), 4 * 10, "mono advance 500 at 20px");
    let fallback = w("serif, 'Noto Sans'");
    assert_eq!(fallback, w("'Noto Sans'"), "unmapped generic is skipped");
}

// -- The web, from a local server (the `testing` feature's `connect_to`) ---------

#[cfg(feature = "testing")]
mod web {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::{Arc, Mutex};

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
    const FONT: &str = "Content-Type: font/woff2\r\n";

    fn families(loaded: &px_shell::nav::Loaded) -> Vec<String> {
        loaded
            .res
            .fonts
            .faces
            .iter()
            .map(|f| f.family.clone())
            .collect()
    }

    #[test]
    fn a_same_origin_font_loads_and_a_cross_origin_one_needs_cors() {
        let woff2 = fixture("PxText-Regular.woff2");
        let page = "<!doctype html><style>\
            @font-face{font-family:same;src:url(/same.woff2)}\
            @font-face{font-family:cors;src:url(http://other.test/cors.woff2)}\
            @font-face{font-family:nocors;src:url(http://other.test/nocors.woff2)}\
            @font-face{font-family:notfound;src:url(/gone.woff2)}\
            </style><body>x";
        let (addr, seen) = serve(vec![
            route("/p.html", HTML, page.as_bytes()),
            route("/same.woff2", FONT, &woff2),
            route(
                "/cors.woff2",
                &format!("{FONT}Access-Control-Allow-Origin: *\r\n"),
                &woff2,
            ),
            route("/nocors.woff2", FONT, &woff2),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(!loaded.failed, "{}", loaded.source);
        assert_eq!(families(&loaded), ["same", "cors"]);
        let seen = seen.lock().unwrap().clone();
        assert!(seen.contains(&"/nocors.woff2".to_string()), "{seen:?}");
    }

    #[test]
    fn csp_font_src_and_the_byte_cap_stop_a_font() {
        let woff2 = fixture("PxText-Regular.woff2");
        let page = "<!doctype html><style>@font-face{font-family:f;src:url(/f.woff2)}</style><body>x";
        let (addr, seen) = serve(vec![
            route(
                "/p.html",
                "Content-Type: text/html\r\nContent-Security-Policy: font-src 'none'\r\n",
                page.as_bytes(),
            ),
            route("/f.woff2", FONT, &woff2),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(loaded.res.fonts.faces.is_empty());
        assert_eq!(seen.lock().unwrap().as_slice(), ["/p.html"]);

        let mut big = woff2.clone();
        big.resize(px_net::browse::MAX_FONT_BYTES + 1, 0);
        let (addr, _) = serve(vec![
            route("/p.html", HTML, page.as_bytes()),
            route("/f.woff2", FONT, &big),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(!loaded.failed);
        assert!(loaded.res.fonts.faces.is_empty());
    }

    #[test]
    fn a_web_page_lays_out_with_its_font_and_a_second_source_is_a_fallback() {
        let woff2 = fixture("PxText-Bold.woff2");
        let page = "<!doctype html><style>\
            @font-face{font-family:'Px B';src:url(/missing.woff2) format('woff2'),url(/b.woff2) format('woff2')}\
            body{margin:0}a{font:20px 'Px B';text-decoration:none}</style><a href=x>AAAA</a>";
        let (addr, seen) = serve(vec![
            route("/p.html", HTML, page.as_bytes()),
            route("/b.woff2", FONT, &woff2),
        ]);
        let loaded = site(addr).load("http://site.test/p.html");
        assert!(!loaded.failed, "{}", loaded.source);
        assert_eq!(families(&loaded), ["px b"]);
        assert_eq!(link_width(&loaded.source, &loaded.res), 4 * 14);
        let seen = seen.lock().unwrap().clone();
        assert!(seen.iter().any(|p| p == "/missing.woff2"));
    }
}
