//! Squircle windows against a real X server (Xvfb): the SHAPE-extension fallback
//! with no compositor, the 32-bit ARGB path under `xcompmgr` (skipped when it is
//! not installed), pointer hit-testing in the transparent corners, and popup /
//! menu / tooltip windows. Set `PX_SQUIRCLE_SHOTS=<dir>` to also write the
//! screenshots (`ui-squircle-*.png`, committed under `docs/brand/`).

#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use px_display::x11::conn::Conn;
use px_display::x11::discover::Auth;
use px_display::x11::{ShapeMode, X11Window, proto};
use px_display::{Display, DisplayEvent, Mask, WindowKind, WindowOptions, WindowShape};
use px_paint::raster::Canvas;
use px_ui::geom::Rect;
use px_ui::input::{Event, PointerKind};
use px_ui::platform::PlatformWindow;

static NEXT: AtomicU32 = AtomicU32::new(0);

struct Server {
    child: Child,
    number: u32,
    extra: Vec<Child>,
}

impl Drop for Server {
    fn drop(&mut self) {
        for c in &mut self.extra {
            let _ = c.kill();
            let _ = c.wait();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.number));
        let _ = std::fs::remove_file(format!("/tmp/.X{}-lock", self.number));
    }
}

fn start() -> Option<Server> {
    let number = 1300 + (std::process::id() % 400) + NEXT.fetch_add(1, Ordering::SeqCst) * 400;
    let child = Command::new("Xvfb")
        .arg(format!(":{number}"))
        .args(["-screen", "0", "800x600x24", "-nolisten", "tcp"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let server = Server {
        child,
        number,
        extra: Vec::new(),
    };
    let sock = format!("/tmp/.X11-unix/X{number}");
    let t = Instant::now();
    while !Path::new(&sock).exists() {
        if t.elapsed() > Duration::from_secs(10) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(server)
}

macro_rules! server {
    () => {
        match start() {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED: Xvfb is not available");
                return;
            }
        }
    };
}

fn raw(srv: &Server) -> Conn {
    let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{}", srv.number)).unwrap();
    Conn::connect(stream, &Auth::default()).unwrap()
}

fn open(srv: &Server, opts: &WindowOptions) -> X11Window {
    let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{}", srv.number)).unwrap();
    X11Window::connect(stream, &Auth::default(), opts).unwrap()
}

/// Start `xcompmgr` and wait until it owns `_NET_WM_CM_S0`. `None` if it is not
/// installed.
fn compositor(srv: &mut Server) -> Option<()> {
    let child = Command::new("xcompmgr")
        .env("DISPLAY", format!(":{}", srv.number))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    srv.extra.push(child);
    let mut c = raw(srv);
    let atom = c
        .round_trip(proto::intern_atom("_NET_WM_CM_S0"))
        .unwrap()
        .u32_at(8)
        .unwrap();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(5) {
        let owner = c
            .round_trip(proto::get_selection_owner(atom))
            .unwrap()
            .u32_at(8)
            .unwrap();
        if owner != 0 {
            return Some(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

// -- painting ---------------------------------------------------------------

fn backdrop_rgb(x: u32, y: u32) -> [u8; 3] {
    // Diagonal stripes: the squircle's edge pixels blend visibly against them.
    if ((x + y) / 14) % 2 == 0 {
        [196, 214, 236]
    } else {
        [236, 224, 196]
    }
}

fn paint(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Canvas {
    let mut c = Canvas::new(w, h).unwrap();
    for y in 0..h {
        for x in 0..w {
            let [r, g, b] = f(x, y);
            let i = ((y * w + x) * 4) as usize;
            c.pixels[i..i + 4].copy_from_slice(&[r, g, b, 255]);
        }
    }
    c
}

fn body(x: u32, y: u32, base: [u8; 3]) -> [u8; 3] {
    // A flat body with a slightly darker 3 px top band and a 1 px inner outline,
    // so the outline's own shape is visible against the cut-out edge.
    let _ = x;
    if y < 3 {
        [base[0] / 2, base[1] / 2, base[2] / 2]
    } else {
        base
    }
}

fn bgr(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i + 2], img[i + 1], img[i], img[i + 3]] // R, G, B, A/X
}

fn wait_close_events(win: &mut X11Window) {
    let _ = win.wait_events(Some(Duration::from_millis(50)));
}

/// Four windows on a striped backdrop: the main window, a dialog, a menu and a
/// tooltip. Returns the screenshot of the screen region `(0, 0, 640, 420)`.
fn scene(srv: &Server) -> (Canvas, Vec<X11Window>) {
    let mut wins = Vec::new();
    let mut back = open(srv, &WindowOptions::new("backdrop", 640, 420).square());
    back.present(
        &paint(640, 420, backdrop_rgb),
        Rect::new(0, 0, 640, 420),
    )
    .unwrap();
    back.sync().unwrap();
    wins.push(back);

    let mut put = |kind: WindowKind, w: u32, h: u32, x: i32, y: i32, base: [u8; 3]| {
        let mut o = WindowOptions::transient(kind, w, h, x, y);
        if kind == WindowKind::Main || kind == WindowKind::Dialog {
            o = WindowOptions::of_kind(kind, "w", w, h);
            o.position = Some((x, y));
        }
        let mut win = open(srv, &o);
        win.present(
            &paint(w, h, |px, py| body(px, py, base)),
            Rect::new(0, 0, w as i32, h as i32),
        )
        .unwrap();
        win.sync().unwrap();
        assert!(win.take_errors().is_empty());
        wins.push(win);
    };
    put(WindowKind::Main, 360, 240, 24, 24, [250, 250, 252]);
    put(WindowKind::Dialog, 220, 130, 400, 270, [240, 246, 255]);
    put(WindowKind::Menu, 130, 150, 410, 40, [232, 238, 232]);
    put(WindowKind::Tooltip, 110, 34, 400, 220, [60, 60, 70]);
    std::thread::sleep(Duration::from_millis(300));
    let shot = screenshot(&mut wins[0], 640, 420);
    (shot, wins)
}

fn screenshot(win: &mut X11Window, w: u32, h: u32) -> Canvas {
    let img = win
        .get_root_image(Rect::new(0, 0, w as i32, h as i32))
        .unwrap();
    paint(w, h, |x, y| {
        let p = bgr(&img, w, x, y);
        [p[0], p[1], p[2]]
    })
}

fn save(name: &str, c: &Canvas) {
    if let Ok(dir) = std::env::var("PX_SQUIRCLE_SHOTS") {
        std::fs::write(Path::new(&dir).join(name), c.to_png().unwrap()).unwrap();
    }
}

fn px(c: &Canvas, x: u32, y: u32) -> [u8; 3] {
    let i = ((y * c.width + x) * 4) as usize;
    [c.pixels[i], c.pixels[i + 1], c.pixels[i + 2]]
}

// -- tests ------------------------------------------------------------------

#[test]
fn without_a_compositor_the_shape_extension_clips_the_corners() {
    let srv = server!();
    let (shot, wins) = scene(&srv);
    assert_eq!(wins[1].shape_mode(), ShapeMode::ShapeExt);
    for w in &wins[1..] {
        assert_eq!(w.shape_mode(), ShapeMode::ShapeExt);
    }
    // Main window at (24, 24), 360x240. The corner shows the backdrop through.
    assert_eq!(px(&shot, 24, 24), backdrop_rgb(24, 24));
    assert_eq!(px(&shot, 24 + 359, 24 + 239), backdrop_rgb(383, 263));
    // The middle shows the window; the top-edge midpoint is inside too.
    assert_eq!(px(&shot, 24 + 180, 24 + 120), [250, 250, 252]);
    assert_eq!(px(&shot, 24 + 180, 24 + 5), [250, 250, 252]);
    // Exactly the thresholded mask: every pixel is window or backdrop.
    let mask = WindowShape::for_kind(WindowKind::Main).mask(360, 240);
    for y in 0..240u32 {
        for x in 0..360u32 {
            let got = px(&shot, 24 + x, 24 + y);
            let want = if mask.hit(x as i32, y as i32) {
                body(x, y, [250, 250, 252])
            } else {
                backdrop_rgb(24 + x, 24 + y)
            };
            assert_eq!(got, want, "pixel ({x},{y})");
        }
    }
    save("ui-squircle-shape-fallback.png", &shot);
    save("ui-squircle-shape-fallback-zoom.png", &zoom(&shot, 20, 20, 32, 12));
}

#[test]
fn argb_windows_are_anti_aliased_under_a_compositor() {
    let mut srv = server!();
    if compositor(&mut srv).is_none() {
        eprintln!("SKIPPED: xcompmgr is not available");
        return;
    }
    let (_, mut wins) = scene(&srv);
    if wins[1].shape_mode() != ShapeMode::Argb {
        eprintln!("SKIPPED: this X server has no 32-bit ARGB visual");
        return;
    }
    for w in &wins[1..] {
        assert_eq!(w.shape_mode(), ShapeMode::Argb);
    }
    // The window's own pixels are the premultiplied mask, exactly.
    let mask = WindowShape::for_kind(WindowKind::Main).mask(360, 240);
    let img = wins[1].get_image(Rect::new(0, 0, 360, 240)).unwrap();
    let mut partial = 0;
    for y in 0..240u32 {
        for x in 0..360u32 {
            let src = body(x, y, [250, 250, 252]);
            let mut want = Vec::new();
            mask.premultiply_row(y as i32, x as i32, &[[src[0], src[1], src[2], 255]], &mut want);
            let got = bgr(&img, 360, x, y);
            assert_eq!([want[2], want[1], want[0], want[3]], got, "pixel ({x},{y})");
            if got[3] != 0 && got[3] != 255 {
                partial += 1;
            }
        }
    }
    assert!(partial > 40, "only {partial} anti-aliased pixels");
    assert_eq!(bgr(&img, 360, 0, 0)[3], 0);

    // The composited screen: poll until the compositor has painted the window.
    let t = Instant::now();
    let shot = loop {
        let s = screenshot(&mut wins[0], 640, 420);
        if px(&s, 24 + 180, 24 + 120) == [250, 250, 252] || t.elapsed() > Duration::from_secs(5) {
            break s;
        }
        wait_close_events(&mut wins[1]);
    };
    assert_eq!(px(&shot, 24 + 180, 24 + 120), [250, 250, 252]);
    assert_eq!(px(&shot, 24, 24), backdrop_rgb(24, 24));
    // An edge pixel is a blend: neither the backdrop nor the window.
    let mut blends = 0;
    for y in 0..22u32 {
        for x in 0..22u32 {
            let p = px(&shot, 24 + x, 24 + y);
            if p != backdrop_rgb(24 + x, 24 + y) && p != body(x, y, [250, 250, 252]) {
                blends += 1;
            }
        }
    }
    assert!(blends > 15, "only {blends} blended edge pixels on screen");
    save("ui-squircle-argb.png", &shot);
    save("ui-squircle-argb-zoom.png", &zoom(&shot, 20, 20, 32, 12));
}

#[test]
fn transparent_corners_are_outside_for_the_pointer() {
    let srv = server!();
    let mut win = open(&srv, &WindowOptions::new("hit", 200, 120));
    assert!(!win.hit(0, 0) && !win.hit(199, 119));
    assert!(win.hit(100, 60) && win.hit(0, 60) && win.hit(100, 0));

    // A press injected at the corner (SendEvent ignores the input region) is
    // dropped; one inside arrives.
    let mut c = raw(&srv);
    let wid = win.window_id();
    let input = |code: u8, x: i16, y: i16| {
        let mut e = [0u8; 32];
        e[0] = code;
        e[1] = 1;
        e[24..26].copy_from_slice(&x.to_le_bytes());
        e[26..28].copy_from_slice(&y.to_le_bytes());
        e
    };
    for (x, y) in [(1, 1), (100, 60)] {
        c.send(proto::send_event(
            wid,
            proto::MASK_BUTTON_PRESS,
            &input(proto::EV_BUTTON_PRESS, x, y),
        ))
        .unwrap();
    }
    c.sync().unwrap();
    let mut downs = Vec::new();
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(500) {
        for e in win.wait_events(Some(Duration::from_millis(50))).unwrap() {
            if let DisplayEvent::Ui(Event::Pointer(p)) = e
                && matches!(p.kind, PointerKind::Down(_))
            {
                downs.push((p.pos.x, p.pos.y));
            }
        }
    }
    assert_eq!(downs, vec![(100, 60)]);
}

#[test]
fn resize_reshapes_and_popups_are_unmanaged() {
    let srv = server!();
    let mut win = open(&srv, &WindowOptions::new("r", 200, 120));
    let mut c = raw(&srv);
    c.send(
        proto::Req::new(proto::OP_CONFIGURE_WINDOW, 0)
            .u32(win.window_id())
            .u16(0x4 | 0x8)
            .zeros(2)
            .u32(300)
            .u32(240),
    )
    .unwrap();
    c.sync().unwrap();
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(2) && win.size().w != 300 {
        let _ = win.wait_events(Some(Duration::from_millis(50))).unwrap();
    }
    assert_eq!((win.size().w, win.size().h), (300, 240));
    assert!(!win.hit(299, 239) && win.hit(150, 120));
    // A tooltip ignores the pointer entirely and is a smaller squircle.
    let tip = open(
        &srv,
        &WindowOptions::transient(WindowKind::Tooltip, 100, 30, 10, 10),
    );
    assert!(!tip.hit(0, 0) && tip.hit(50, 15));
    assert!(WindowShape::for_kind(WindowKind::Tooltip).radius < 22);
    let _ = Mask::full(1, 1);
}

/// The top-left `n` x `n` pixels at (`x0`, `y0`), magnified `k` times with no
/// smoothing, to show the corner's edge pixels.
fn zoom(c: &Canvas, x0: u32, y0: u32, n: u32, k: u32) -> Canvas {
    paint(n * k, n * k, |x, y| px(c, x0 + x / k, y0 + y / k))
}
