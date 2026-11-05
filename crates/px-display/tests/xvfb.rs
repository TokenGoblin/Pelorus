//! The X11 backend against a real X server (Xvfb). Skipped, loudly, when Xvfb
//! is not installed; CI installs it. Each test starts its own server so they can
//! run in parallel.

#![cfg(unix)]

use px_display::x11::discover::Auth;
use px_display::x11::proto;
use px_display::{Display, DisplayEvent};
use px_ui::geom::{Rect, Size};
use px_ui::input::{Button, CursorShape, Event, Key, PointerKind, WindowAction};
use px_ui::platform::PlatformWindow;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

// Helpers live in a cfg(test) module so the workspace panic lints treat them like test code (clippy.toml).
#[cfg(test)]
mod h {
    use std::os::unix::net::UnixStream;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    use px_display::x11::X11Window;
    use px_display::x11::discover::Auth;
    use px_display::{Display, DisplayEvent, WindowOptions};
    use px_paint::raster::Canvas;

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct Server {
        child: Child,
        pub number: u32,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_file(format!("/tmp/.X11-unix/X{}", self.number));
            let _ = std::fs::remove_file(format!("/tmp/.X{}-lock", self.number));
        }
    }

    pub fn start() -> Option<Server> {
        let number = 300 + (std::process::id() % 500) + NEXT.fetch_add(1, Ordering::SeqCst) * 500;
        let child = Command::new("Xvfb")
            .arg(format!(":{number}"))
            .args(["-screen", "0", "800x600x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let server = Server { child, number };
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

    pub fn open(s: &Server, w: u32, h: u32) -> X11Window {
        let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{}", s.number)).unwrap();
        X11Window::connect(stream, &Auth::default(), &WindowOptions::new("test", w, h).square()).unwrap()
    }

    /// Poll `win` until `pred` accepts an event, up to two seconds.
    pub fn wait_for(
        win: &mut X11Window,
        mut pred: impl FnMut(&DisplayEvent) -> bool,
    ) -> Vec<DisplayEvent> {
        let t = Instant::now();
        let mut seen = Vec::new();
        while t.elapsed() < Duration::from_secs(2) {
            for e in win.wait_events(Some(Duration::from_millis(50))).unwrap() {
                let hit = pred(&e);
                seen.push(e);
                if hit {
                    return seen;
                }
            }
        }
        panic!("event never arrived; saw {seen:?}");
    }

    pub fn gradient(w: u32, h: u32) -> Canvas {
        let mut c = Canvas::new(w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                c.pixels[i] = (x % 256) as u8;
                c.pixels[i + 1] = (y % 256) as u8;
                c.pixels[i + 2] = 128;
                c.pixels[i + 3] = 255;
            }
        }
        c
    }

    pub fn pixel(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 3] {
        let i = ((y * w + x) * 4) as usize;
        [img[i + 2], img[i + 1], img[i]] // stored B,G,R,X
    }
}

use h::*;

macro_rules! server {
    () => {
        match h::start() {
            Some(s) => s,
            None => {
                eprintln!("SKIPPED: Xvfb is not available");
                return;
            }
        }
    };
}

#[test]
fn presented_pixels_read_back_exactly() {
    let srv = server!();
    let mut win = open(&srv, 320, 200);
    assert_eq!(win.size(), Size::new(320, 200));
    let frame = gradient(320, 200);
    win.present(&frame, Rect::new(0, 0, 320, 200)).unwrap();
    win.sync().unwrap();
    assert!(win.take_errors().is_empty());
    let img = win.get_image(Rect::new(0, 0, 320, 200)).unwrap();
    assert_eq!(img.len(), 320 * 200 * 4);
    assert_eq!(pixel(&img, 320, 10, 20), [10, 20, 128]);
    assert_eq!(pixel(&img, 320, 319, 199), [63, 199, 128]);
}

#[test]
fn damage_changes_only_its_rectangle() {
    let srv = server!();
    let mut win = open(&srv, 100, 100);
    let mut frame = gradient(100, 100);
    win.present(&frame, Rect::new(0, 0, 100, 100)).unwrap();
    // New content everywhere, but only a 10x10 patch declared as damage.
    for p in frame.pixels.as_chunks_mut::<4>().0 {
        *p = [255, 0, 0, 255];
    }
    win.present(&frame, Rect::new(40, 30, 10, 10)).unwrap();
    win.sync().unwrap();
    assert!(win.take_errors().is_empty());
    let img = win.get_image(Rect::new(0, 0, 100, 100)).unwrap();
    assert_eq!(pixel(&img, 100, 45, 35), [255, 0, 0]);
    assert_eq!(pixel(&img, 100, 39, 35), [39, 35, 128]);
    assert_eq!(pixel(&img, 100, 50, 35), [50, 35, 128]);
    // Damage hanging off the frame is clipped, not an error.
    win.present(&frame, Rect::new(90, 90, 50, 50)).unwrap();
    win.present(&frame, Rect::new(-5, -5, 3, 3)).unwrap();
    win.sync().unwrap();
    assert!(win.take_errors().is_empty());
}

#[test]
fn a_large_frame_is_sent_whole_or_in_bands() {
    let srv = server!();
    let mut win = open(&srv, 800, 600);
    let frame = gradient(800, 600); // 1.9 MB, over the 256 KiB plain limit
    win.present(&frame, Rect::new(0, 0, 800, 600)).unwrap();
    win.sync().unwrap();
    assert!(win.take_errors().is_empty());
    let img = win.get_image(Rect::new(0, 590, 800, 10)).unwrap();
    assert_eq!(
        pixel(&img, 800, 700, 9),
        [(700 % 256) as u8, (599 % 256) as u8, 128]
    );
}

#[test]
fn another_client_can_send_keys_pointer_and_close() {
    let srv = server!();
    let mut win = open(&srv, 200, 100);

    // A raw second connection injects events with SendEvent.
    let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{}", srv.number)).unwrap();
    let mut c = px_display::x11::conn::Conn::connect(stream, &Auth::default()).unwrap();
    let wid = win.window_id();
    let send = |c: &mut px_display::x11::conn::Conn, ev: [u8; 32], mask: u32| {
        c.send(proto::send_event(wid, mask, &ev)).unwrap();
        c.sync().unwrap();
    };
    let input = |code: u8, detail: u8, x: i16, y: i16, state: u16| {
        let mut e = [0u8; 32];
        e[0] = code;
        e[1] = detail;
        e[24..26].copy_from_slice(&x.to_le_bytes());
        e[26..28].copy_from_slice(&y.to_le_bytes());
        e[28..30].copy_from_slice(&state.to_le_bytes());
        e
    };

    // keycode 38 is "a" on a standard PC keymap; 36 is Return.
    send(&mut c, input(proto::EV_KEY_PRESS, 38, 0, 0, 0), 1);
    let seen = wait_for(&mut win, |e| matches!(e, DisplayEvent::Ui(Event::Text(_))));
    assert!(
        seen.iter().any(|e| matches!(e,
        DisplayEvent::Ui(Event::Key(k)) if k.key == Key::Char('a'))),
        "{seen:?}"
    );
    assert!(seen.contains(&DisplayEvent::Ui(Event::Text("a".into()))));

    send(&mut c, input(proto::EV_KEY_PRESS, 38, 0, 0, 1), 1); // Shift
    let seen = wait_for(&mut win, |e| matches!(e, DisplayEvent::Ui(Event::Text(_))));
    assert!(seen.contains(&DisplayEvent::Ui(Event::Text("A".into()))));

    // Ctrl+a is a shortcut: a key, no text.
    send(&mut c, input(proto::EV_KEY_PRESS, 38, 0, 0, 4), 1);
    let seen = wait_for(&mut win, |e| matches!(e, DisplayEvent::Ui(Event::Key(_))));
    assert!(
        seen.iter()
            .all(|e| !matches!(e, DisplayEvent::Ui(Event::Text(_))))
    );
    let more = win.wait_events(Some(Duration::from_millis(100))).unwrap();
    assert!(
        more.iter()
            .all(|e| !matches!(e, DisplayEvent::Ui(Event::Text(_))))
    );

    send(&mut c, input(proto::EV_BUTTON_PRESS, 1, 12, 34, 0), 4);
    let seen = wait_for(&mut win, |e| {
        matches!(e, DisplayEvent::Ui(Event::Pointer(_)))
    });
    assert!(seen.iter().any(|e| matches!(e,
        DisplayEvent::Ui(Event::Pointer(p))
            if p.kind == PointerKind::Down(Button::Primary) && p.pos.x == 12 && p.pos.y == 34)));

    // WM_DELETE_WINDOW.
    let atoms = {
        let a = c.round_trip(proto::intern_atom("WM_PROTOCOLS")).unwrap();
        let b = c
            .round_trip(proto::intern_atom("WM_DELETE_WINDOW"))
            .unwrap();
        (a.u32_at(8).unwrap(), b.u32_at(8).unwrap())
    };
    let ev = proto::client_message_event(wid, atoms.0, [atoms.1, 0, 0, 0, 0]);
    send(&mut c, ev, 0);
    wait_for(&mut win, |e| *e == DisplayEvent::Close);
}

#[test]
fn resizing_the_window_reports_a_new_size() {
    let srv = server!();
    let mut win = open(&srv, 200, 100);
    let stream = UnixStream::connect(format!("/tmp/.X11-unix/X{}", srv.number)).unwrap();
    let mut c = px_display::x11::conn::Conn::connect(stream, &Auth::default()).unwrap();
    // ConfigureWindow width/height from another client (no window manager here).
    c.send(
        proto::Req::new(proto::OP_CONFIGURE_WINDOW, 0)
            .u32(win.window_id())
            .u16(0x4 | 0x8)
            .zeros(2)
            .u32(300)
            .u32(150),
    )
    .unwrap();
    c.sync().unwrap();
    let seen = wait_for(&mut win, |e| {
        matches!(e, DisplayEvent::Ui(Event::Resize(_)))
    });
    assert!(seen.contains(&DisplayEvent::Ui(Event::Resize(Size::new(300, 150)))));
    assert_eq!(win.size(), Size::new(300, 150));
}

#[test]
fn clipboard_round_trips_between_two_windows() {
    let srv = server!();
    let mut a = open(&srv, 100, 100);
    let mut b = open(&srv, 100, 100);
    let text = "h\u{e9}llo clipboard \u{4e16}\u{754c}";
    a.clipboard_write(text);
    a.sync().unwrap();
    b.clipboard_request_read();
    // `a` serves the request while `b` waits for the answer.
    let t = Instant::now();
    let mut got = None;
    while got.is_none() && t.elapsed() < Duration::from_secs(3) {
        let _ = a.wait_events(Some(Duration::from_millis(20))).unwrap();
        for e in b.wait_events(Some(Duration::from_millis(20))).unwrap() {
            if let DisplayEvent::Ui(Event::Clipboard(s)) = e {
                got = Some(s);
            }
        }
    }
    assert_eq!(got.as_deref(), Some(text));

    // The owner reads its own selection without the server.
    a.clipboard_request_read();
    let ev = a.wait_events(Some(Duration::from_millis(50))).unwrap();
    assert!(ev.contains(&DisplayEvent::Ui(Event::Clipboard(text.into()))));
}

#[test]
fn losing_the_selection_forgets_the_text() {
    let srv = server!();
    let mut a = open(&srv, 100, 100);
    let mut b = open(&srv, 100, 100);
    a.clipboard_write("first");
    a.sync().unwrap();
    b.clipboard_write("second");
    b.sync().unwrap();
    // Drain until the server's SelectionClear has been processed.
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_millis(300) {
        let _ = a.wait_events(Some(Duration::from_millis(20))).unwrap();
    }
    a.clipboard_request_read();
    let mut got = None;
    let t = Instant::now();
    while got.is_none() && t.elapsed() < Duration::from_secs(3) {
        let _ = b.wait_events(Some(Duration::from_millis(20))).unwrap();
        for e in a.wait_events(Some(Duration::from_millis(20))).unwrap() {
            if let DisplayEvent::Ui(Event::Clipboard(s)) = e {
                got = Some(s);
            }
        }
    }
    assert_eq!(got.as_deref(), Some("second"));
}

#[test]
fn cursor_title_and_window_actions_are_accepted_by_the_server() {
    let srv = server!();
    let mut win = open(&srv, 100, 100);
    for s in [
        CursorShape::Text,
        CursorShape::Pointer,
        CursorShape::Default,
    ] {
        win.set_cursor(s);
    }
    win.window_action(WindowAction::Minimize);
    win.window_action(WindowAction::Maximize);
    win.set_ime_allowed(true);
    win.set_ime_cursor_area(Rect::new(0, 0, 1, 10));
    win.sync().unwrap();
    assert!(win.take_errors().is_empty());
    win.window_action(WindowAction::Close);
    wait_for(&mut win, |e| *e == DisplayEvent::Close);
}

#[test]
fn a_dead_server_is_an_error_not_a_hang() {
    let srv = server!();
    let mut win = open(&srv, 100, 100);
    drop(srv);
    // Buffered events (the first Expose) may come out first; then the error.
    let mut r = Ok(Vec::new());
    for _ in 0..5 {
        r = win.wait_events(Some(Duration::from_secs(2)));
        if r.is_err() {
            break;
        }
    }
    assert!(matches!(r, Err(px_display::DisplayError::Closed)), "{r:?}");
}
