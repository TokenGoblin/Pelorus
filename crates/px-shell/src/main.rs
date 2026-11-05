#![forbid(unsafe_code)]

//! `px-shell [options] [PAGE ...]`
//!
//! Opens a window with the browser chrome and a real page below it. `PAGE` is a
//! path to a local HTML file or `about:welcome`, `about:flex`, `about:long`; one
//! tab each. No network.
//!
//! ```text
//! --light            light theme
//! --size WxH         initial window size (default 1000x680)
//! --scroll N         start the first tab scrolled N px (for screenshots)
//! --exit-after-ms N  close after N milliseconds (tools and tests)
//! ```

use std::cell::RefCell;
use std::time::{Duration, Instant};

use px_display::{WindowOptions, run};
use px_shell::{Location, Shell};
use px_ui::input::Event;
use px_ui::platform::PlatformWindow;
use px_ui::theme::Theme;

struct Args {
    light: bool,
    size: (u32, u32),
    scroll: i32,
    stop: Option<Instant>,
    pages: Vec<Location>,
}

fn parse_args(argv: &[String]) -> Result<Args, String> {
    let mut a = Args {
        light: false,
        size: (1000, 680),
        scroll: 0,
        stop: None,
        pages: Vec::new(),
    };
    let mut it = argv.iter();
    while let Some(arg) = it.next() {
        let mut value = |name: &str| -> Result<String, String> {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match arg.as_str() {
            "--light" => a.light = true,
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH")?;
                a.size = (
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                );
            }
            "--scroll" => a.scroll = value("--scroll")?.parse().map_err(|_| "bad --scroll")?,
            "--exit-after-ms" => {
                let ms: u64 = value("--exit-after-ms")?
                    .parse()
                    .map_err(|_| "bad --exit-after-ms")?;
                a.stop = Some(Instant::now() + Duration::from_millis(ms));
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            page => a.pages.push(Location::parse(page)?),
        }
    }
    Ok(a)
}

#[cfg(unix)]
fn open(opts: &WindowOptions) -> Result<px_display::x11::X11Window, String> {
    let (stream, auth) = px_display::x11::discover::open().map_err(|e| e.to_string())?;
    px_display::x11::X11Window::connect(stream, &auth, opts).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open(opts: &WindowOptions) -> Result<px_display::win32::Win32Window, String> {
    px_display::win32::Win32Window::create(opts).map_err(|e| e.to_string())
}

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = parse_args(&argv).and_then(real_main);
    if let Err(e) = result {
        eprintln!("px-shell: {e}");
        std::process::exit(1);
    }
}

fn real_main(args: Args) -> Result<(), String> {
    let theme = if args.light { Theme::Light } else { Theme::Dark };
    let opts = WindowOptions::new(px_brand::PRODUCT_NAME, args.size.0, args.size.1);
    let mut window = open(&opts)?;
    let mut shell = Shell::new(window.size(), window.config(), theme, &args.pages);
    shell.set_scroll(args.scroll);
    let shell = RefCell::new(shell);
    run(
        &mut window,
        args.stop,
        |w, ev: Event| shell.borrow_mut().handle(w, ev),
        |_| shell.borrow_mut().frame(),
    )
    .map_err(|e| e.to_string())
}
