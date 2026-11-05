#![deny(unsafe_op_in_unsafe_fn)]

//! `px-chrome-demo`: opens a window showing the px-ui chrome, driven by the
//! real window layer. Built only with `--features demo`.
//!
//! ```text
//! cargo run -p px-display --features demo --bin px-chrome-demo -- [--light] [--exit-after-ms N]
//! ```
//!
//! Tabs, the address bar, clipboard and the window buttons work; navigation is
//! faked (the title becomes the typed text), because there is no browser behind
//! it. The page area is left blank: it is the compositor's.

mod font;

use std::cell::RefCell;
use std::time::{Duration, Instant};

use px_display::{WindowOptions, run};
use px_ui::address::UrlDisplay;
use px_ui::input::{AppRequest, Event};
use px_ui::paint;
use px_ui::platform::{PlatformWindow, pump};
use px_ui::tabs::{Tab, TabId};
use px_ui::theme::Theme;
use px_ui::{Chrome, PlatformConfig};

use font::Noto;

struct Demo {
    chrome: Chrome,
    tabs: Vec<Tab>,
    active: TabId,
    next: u64,
}

impl Demo {
    fn new(size: px_ui::geom::Size) -> Demo {
        let mut d = Demo {
            chrome: Chrome::new(PlatformConfig::desktop(), size),
            tabs: vec![
                Tab::new(TabId(1), "news.example", false),
                Tab::new(TabId(2), "Notes", false),
                Tab::new(TabId(3), "Docs", false),
            ],
            active: TabId(1),
            next: 4,
        };
        d.chrome.set_isolated(true);
        d.chrome.set_nav_state(true, false, false);
        d.chrome.set_url(UrlDisplay::from_parts(
            "https://",
            "",
            "news.example",
            "/world/today",
        ));
        d.sync();
        d
    }

    fn sync(&mut self) {
        self.chrome.set_tabs(self.tabs.clone(), Some(self.active));
    }

    fn apply(&mut self, r: AppRequest) {
        match r {
            AppRequest::NewTab => {
                let id = TabId(self.next);
                self.next += 1;
                self.tabs.push(Tab::new(id, "New tab", false));
                self.active = id;
            }
            AppRequest::CloseTab(id) => {
                self.tabs.retain(|t| t.id != id);
                if self.tabs.is_empty() {
                    self.tabs.push(Tab::new(TabId(self.next), "New tab", false));
                    self.next += 1;
                }
                if self.active == id {
                    self.active = self.tabs.first().map(|t| t.id).unwrap_or(id);
                }
            }
            AppRequest::ActivateTab(id) => self.active = id,
            AppRequest::MoveTab { tab, to } => {
                if let Some(from) = self.tabs.iter().position(|t| t.id == tab) {
                    let t = self.tabs.remove(from);
                    self.tabs.insert(to.min(self.tabs.len()), t);
                }
            }
            AppRequest::Navigate(text) => {
                let active = self.active;
                for t in self.tabs.iter_mut().filter(|t| t.id == active) {
                    *t = Tab::new(t.id, &text, false);
                }
            }
            _ => {}
        }
        self.sync();
    }
}

#[cfg(unix)]
fn open() -> Result<px_display::x11::X11Window, String> {
    let opts = WindowOptions::new(px_brand::PRODUCT_NAME, 920, 568);
    let (stream, auth) = px_display::x11::discover::open().map_err(|e| e.to_string())?;
    px_display::x11::X11Window::connect(stream, &auth, &opts).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open() -> Result<px_display::win32::Win32Window, String> {
    let opts = WindowOptions::new(px_brand::PRODUCT_NAME, 920, 568);
    px_display::win32::Win32Window::create(&opts).map_err(|e| e.to_string())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let light = args.iter().any(|a| a == "--light");
    let stop = args
        .iter()
        .position(|a| a == "--exit-after-ms")
        .and_then(|i| args.get(i + 1))
        .and_then(|v| v.parse::<u64>().ok())
        .map(|ms| Instant::now() + Duration::from_millis(ms));
    if let Err(e) = real_main(light, stop) {
        eprintln!("px-chrome-demo: {e}");
        std::process::exit(1);
    }
}

fn real_main(light: bool, stop: Option<Instant>) -> Result<(), String> {
    let font = Noto::load()?;
    let theme = if light { Theme::Light } else { Theme::Dark };
    let mut window = open()?;
    let demo = RefCell::new(Demo::new(window.size()));

    run(
        &mut window,
        stop,
        |w, ev: Event| {
            let mut d = demo.borrow_mut();
            let (reqs, _page) = pump(w, &mut d.chrome, &font, ev);
            for r in reqs {
                d.apply(r);
            }
        },
        |w| {
            let d = demo.borrow();
            let s = w.size();
            let mut list = px_paint::DisplayList::new();
            paint::paint(
                &d.chrome,
                &font,
                theme,
                px_ui::geom::Point::default(),
                &mut list,
            );
            px_paint::raster::rasterize_with(&list, s.w.max(1) as u32, s.h.max(1) as u32, &font)
                .ok()
        },
    )
    .map_err(|e| e.to_string())
}
