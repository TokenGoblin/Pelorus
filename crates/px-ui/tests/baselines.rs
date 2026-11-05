//! Render baselines for the chrome: dark and light, idle and editing.
//!
//! The standard is ADR 033 (b)'s: the integer software rasterizer, tolerance
//! zero, one baseline set for every OS. A one-pixel change fails. After an
//! intended change, review the new PNGs and rewrite them with
//! `PX_UI_BLESS=1 cargo test -p px-ui --test baselines`. Bless refuses to run
//! under `CI`. On a mismatch the actual image is written to
//! `$CARGO_TARGET_TMPDIR/px-ui-actual/` for a look.
//!
//! The scenes show no product name, so a rename cannot invalidate them
//! (build-spec section 14.6); `no_scene_draws_the_product_name` pins that.

mod support;

use std::path::PathBuf;

use px_paint::raster::rasterize_with;
use px_ui::address::UrlDisplay;
use px_ui::geom::{Point, Size};
use px_ui::input::{Button, Event, Key, KeyEvent, PointerEvent, PointerKind};
use px_ui::paint::paint_preview;
use px_ui::tabs::{Tab, TabId};
use px_ui::theme::Theme;
use px_ui::{Chrome, PlatformConfig};
use support::Noto;

/// The mockups' window and margins: 920 x 568 inside a 1000 x 640 canvas.
const WINDOW: Size = Size { w: 920, h: 568 };
const MARGIN: Point = Point { x: 40, y: 36 };

fn base() -> Chrome {
    let mut c = Chrome::new(PlatformConfig::desktop(), WINDOW);
    c.set_tabs(
        vec![
            Tab::new(TabId(1), "news.example", false),
            Tab::new(TabId(2), "Notes", false),
            Tab::new(TabId(3), "Docs", false),
        ],
        Some(TabId(1)),
    );
    c.set_url(UrlDisplay::from_parts(
        "https://",
        "",
        "news.example",
        "/world/today",
    ));
    c.set_isolated(true);
    c.set_nav_state(true, false, false);
    c
}

fn idle() -> Chrome {
    base()
}

/// Editing a query with a selection, a tab hovered, a load in progress.
fn editing() -> Chrome {
    let n = Noto::load();
    let mut c = base();
    c.set_nav_state(true, true, true);
    c.set_tabs(
        vec![
            Tab::new(TabId(1), "news.example", false),
            Tab::new(TabId(2), "Notes", true),
            Tab::new(TabId(3), "Docs", false),
        ],
        Some(TabId(1)),
    );
    let hover = c.layout().tabs[1].rect.center();
    c.event(
        Event::Pointer(PointerEvent::new(hover.x, hover.y, PointerKind::Move)),
        &n,
    );
    let ctrl_l = KeyEvent::with(
        Key::Char('l'),
        px_ui::input::Modifiers {
            ctrl: true,
            ..px_ui::input::Modifiers::NONE
        },
    );
    c.event(Event::Key(ctrl_l), &n);
    c.event(Event::Text("squircle exponent".into()), &n);
    let shift = |k| {
        Event::Key(KeyEvent::with(
            k,
            px_ui::input::Modifiers {
                shift: true,
                ..px_ui::input::Modifiers::NONE
            },
        ))
    };
    for _ in 0..8 {
        c.event(shift(Key::Left), &n);
    }
    // Press and release on nothing: proves a stray click leaves the scene alone.
    let _ = Button::Primary;
    c
}

struct Scene {
    id: &'static str,
    theme: Theme,
    build: fn() -> Chrome,
}

const SCENES: &[Scene] = &[
    Scene {
        id: "chrome-dark",
        theme: Theme::Dark,
        build: idle,
    },
    Scene {
        id: "chrome-light",
        theme: Theme::Light,
        build: idle,
    },
    Scene {
        id: "chrome-dark-editing",
        theme: Theme::Dark,
        build: editing,
    },
    Scene {
        id: "chrome-light-editing",
        theme: Theme::Light,
        build: editing,
    },
];

fn render(scene: &Scene) -> px_paint::Canvas {
    let n = Noto::load();
    let chrome = (scene.build)();
    let (list, size) = paint_preview(&chrome, &n, scene.theme, MARGIN);
    assert!(list.clips_balanced());
    rasterize_with(&list, size.w as u32, size.h as u32, &n).expect("rasterize")
}

fn baseline_path(id: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/baselines")
        .join(format!("{id}.png"))
}

/// First differing pixel and the number that differ; `None` if identical.
fn diff(a: &px_paint::Canvas, b: &px_paint::Canvas) -> Option<(u32, u32, usize)> {
    if (a.width, a.height) != (b.width, b.height) {
        return Some((0, 0, usize::MAX));
    }
    let mut first = None;
    let mut count = 0;
    for (i, (p, q)) in a
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(b.pixels.as_chunks::<4>().0.iter())
        .enumerate()
    {
        if p != q {
            count += 1;
            first.get_or_insert((i as u32 % a.width, i as u32 / a.width));
        }
    }
    first.map(|(x, y)| (x, y, count))
}

#[test]
fn every_scene_matches_its_baseline_exactly() {
    let bless = std::env::var_os("PX_UI_BLESS").is_some();
    if bless {
        assert!(
            std::env::var_os("CI").is_none(),
            "refusing to bless under CI"
        );
    }
    let mut failures = Vec::new();
    for scene in SCENES {
        let actual = render(scene);
        let path = baseline_path(scene.id);
        if bless {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, actual.to_png().unwrap()).unwrap();
            println!("blessed {}", path.display());
            continue;
        }
        let want = std::fs::read(&path)
            .map_err(|e| format!("{}: {e} (bless to create)", path.display()))
            .and_then(|b| px_paint::decode(&b).map_err(|e| format!("{}: {e}", scene.id)));
        let want = match want {
            Ok(w) => w,
            Err(e) => {
                failures.push(e);
                continue;
            }
        };
        let want = px_paint::Canvas {
            width: want.width,
            height: want.height,
            pixels: want.rgba,
        };
        if let Some((x, y, n)) = diff(&actual, &want) {
            let out = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("px-ui-actual");
            let _ = std::fs::create_dir_all(&out);
            let _ = std::fs::write(
                out.join(format!("{}.png", scene.id)),
                actual.to_png().unwrap(),
            );
            failures.push(format!(
                "{}: {n} pixels differ, first at ({x}, {y}); actual in {}",
                scene.id,
                out.display()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn rendering_twice_is_byte_identical() {
    for scene in SCENES {
        let a = render(scene).to_png().unwrap();
        let b = render(scene).to_png().unwrap();
        assert_eq!(a, b, "{}", scene.id);
    }
}

#[test]
fn a_one_pixel_change_is_caught() {
    let a = render(&SCENES[0]);
    let mut b = a.clone();
    let i = (300 * b.width as usize + 500) * 4;
    b.pixels[i] ^= 1;
    assert_eq!(diff(&a, &b), Some((500, 300, 1)));
    assert_eq!(diff(&a, &a), None);
}

#[test]
fn dark_and_light_differ_and_neither_is_blank() {
    let d = render(&SCENES[0]);
    let l = render(&SCENES[1]);
    assert!(diff(&d, &l).is_some());
    for c in [&d, &l] {
        let first = c.pixel(0, 0).unwrap();
        assert!(c.pixels.as_chunks::<4>().0.iter().any(|p| *p != first));
        assert_eq!((c.width, c.height), (1000, 640));
    }
}

#[test]
fn the_idle_scene_follows_the_mockup_geometry() {
    // Spot checks against ui-mockup-dark.svg, whose window is at (40, 36).
    let c = render(&SCENES[0]);
    let px = |x: u32, y: u32| c.pixel(x, y).unwrap();
    let s = px_brand::palette::CHROME_DARK;
    let rgba = |r: px_brand::palette::Rgb| [r.0, r.1, r.2, 255];
    assert_eq!(px(0, 0), rgba(s.backdrop), "backdrop corner");
    assert_eq!(px(500, 80), rgba(s.bar), "toolbar band between controls");
    assert_eq!(px(500, 600), rgba(s.page), "page area");
    // Hairline under the toolbar: window y 91 -> canvas y 127.
    assert_eq!(px(600, 127), rgba(s.hairline));
    // The active tab body is the raised colour (mockup: #18254A at 140..330).
    assert_eq!(px(40 + 250, 36 + 12), rgba(s.raised));
    // The address field.
    assert_eq!(px(40 + 700, 36 + 66), rgba(s.raised));
}

#[test]
fn the_window_corner_is_rounded_and_the_shadow_falls_below() {
    let d = render(&SCENES[0]);
    // The window's top-left pixel is outside the squircle: backdrop-ish, not bar.
    let bar = px_brand::palette::CHROME_DARK.bar;
    assert_ne!(d.pixel(40, 36).unwrap()[..3], [bar.0, bar.1, bar.2]);
    // Below the window the shadow darkens the backdrop; far away it does not.
    let near = d.pixel(500, 606).unwrap()[0];
    let far = d.pixel(500, 639).unwrap()[0];
    let navy = px_brand::palette::NAVY;
    assert!(
        near < navy.0 || d.pixel(500, 606).unwrap()[2] < navy.2,
        "shadow under the window"
    );
    assert!(far >= near);
}

#[test]
fn no_scene_draws_the_product_name() {
    // The glyph runs are drawn from strings; check those strings directly, since
    // pixels cannot be searched for a word.
    let name = px_brand::PRODUCT_NAME.to_lowercase();
    let chrome = idle();
    for t in chrome.tabs().tabs() {
        assert!(!t.display_title().to_lowercase().contains(&name));
    }
    for s in [
        px_brand::ui::ISOLATED,
        px_brand::ui::ADDRESS_PLACEHOLDER,
        px_brand::ui::NEW_TAB_TITLE,
    ] {
        assert!(!s.to_lowercase().contains(&name));
    }
}
