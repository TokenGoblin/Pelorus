//! Phase 9 unit 4 on the process boundary: glyph runs and the fonts they need
//! cross to the real `px-gpu` process, and losing the process, or meeting a
//! malformed font, falls back to software with the next frame equal to the
//! in-process baseline. Same rules and same caveats as `process_boundary.rs`
//! (no real GPU here; the rasterizer in the process is the software one).

#![cfg(feature = "testing")]

use std::time::Duration;

use px_gpu::fonts::FontSet;
use px_gpu::session::{Backend, Cause, FontRegisterError, GpuConfig, GpuEvent, GpuSession};
use px_ipc::gpu::{GpuFailure, GpuFault, MAX_FONT_BYTES};
use px_layout::geom::px;
use px_paint::raster::rasterize_with;
use px_paint::{Canvas, Color, DisplayList, FontId, Item, PositionedGlyph, Rect};
use px_text::Font;
use px_text::registry::AHEM_BYTES;

const PATIENT: Duration = Duration::from_secs(20);
const AHEM: u32 = 1;
const GARBAGE: u32 = 9;

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_px-gpu"))
}

fn can_sandbox() -> bool {
    if px_sandbox::detect().clears_floor() {
        return true;
    }
    assert!(
        std::env::var_os("PX_REQUIRE_SANDBOX").is_none(),
        "PX_REQUIRE_SANDBOX is set but this machine does not clear the sandbox floor"
    );
    false
}

fn session() -> GpuSession {
    let mut config = GpuConfig::process(binary());
    config.frame_deadline = PATIENT;
    config.startup_deadline = PATIENT;
    GpuSession::new(config)
}

/// Ahem plus trailing zero bytes so the font spans several chunks. The sfnt
/// directory addresses tables by offset, so bytes after the last table are
/// inert; `padded_ahem_still_parses` pins that.
fn big_ahem() -> Vec<u8> {
    let mut bytes = AHEM_BYTES.to_vec();
    bytes.resize(600 * 1024, 0);
    bytes
}

fn garbage() -> Vec<u8> {
    (0..4096u32).map(|i| (i * 7 + 3) as u8).collect()
}

fn glyph_page(seed: u8, font: u32) -> DisplayList {
    let font_file = Font::from_bytes(AHEM_BYTES).expect("ahem");
    let glyph = |c: char, x: i32| PositionedGlyph {
        glyph_id: u32::from(font_file.glyph_index(c).expect("glyph")),
        x: px(x),
        y: px(40),
    };
    let mut list = DisplayList::new();
    list.push(Item::FillRect {
        rect: Rect {
            x: px(0),
            y: px(0),
            width: px(200),
            height: px(80),
        },
        color: Color {
            r: 240,
            g: seed,
            b: 200,
            a: 255,
        },
    });
    list.push(Item::PushClip {
        rect: Rect {
            x: px(0),
            y: px(0),
            width: px(150),
            height: px(80),
        },
    });
    assert!(list.push_glyph_run(
        FontId(font),
        px(16),
        Color {
            r: 10,
            g: 10,
            b: seed,
            a: 255
        },
        &[glyph('X', 10), glyph('p', 26), glyph('X', 42)],
    ));
    list.push(Item::PopClip);
    assert!(list.push_glyph_run(
        FontId(font),
        px(24),
        Color::BLACK,
        &[glyph('X', 120), glyph('X', 144)],
    ));
    list
}

/// The in-process baseline: the software rasterizer with Ahem as font `AHEM`
/// and no other font (a run naming one draws nothing).
fn baseline(list: &DisplayList) -> Canvas {
    let mut fonts = FontSet::new();
    fonts.insert(AHEM, big_ahem()).expect("ahem");
    rasterize_with(list, 200, 80, &fonts).expect("baseline")
}

fn render_matching(session: &mut GpuSession, list: &DisplayList) -> Backend {
    let frame = session.render(list, 200, 80).expect("a frame");
    assert_eq!(frame, baseline(list), "not the software baseline");
    session.last_backend()
}

fn failures(session: &GpuSession) -> Vec<Cause> {
    session
        .events()
        .filter_map(|event| match event {
            GpuEvent::Failed(cause) => Some(cause.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn padded_ahem_still_parses() {
    let mut fonts = FontSet::new();
    assert!(fonts.insert(1, big_ahem()).is_ok());
}

#[test]
fn glyph_runs_draw_in_the_gpu_process_and_match_the_baseline() {
    if !can_sandbox() {
        return;
    }
    let mut session = session();
    session.register_font(AHEM, big_ahem()).expect("register");
    let list = glyph_page(50, AHEM);
    assert_eq!(render_matching(&mut session, &list), Backend::Gpu);
    // Ink actually reached the frame: the middle of the first X is the run's
    // colour, not the page's background.
    let frame = session.render(&list, 200, 80).expect("frame");
    assert_eq!(frame.pixel(18, 35), Some([10, 10, 50, 255]));
    // A second frame needs no second transfer and is still the baseline.
    assert_eq!(
        render_matching(&mut session, &glyph_page(60, AHEM)),
        Backend::Gpu
    );
    assert!(failures(&session).is_empty());
}

#[test]
fn a_malformed_font_kills_only_its_process_and_the_session_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = session();
    session.register_font(AHEM, big_ahem()).expect("register");
    session
        .register_font(GARBAGE, garbage())
        .expect("register: bytes are not validated here");

    // The frame that names the garbage font: the process refuses it and exits,
    // the same call renders in software, and the frame is the baseline (which
    // draws nothing for that font).
    let bad = glyph_page(70, GARBAGE);
    assert_eq!(render_matching(&mut session, &bad), Backend::Software);
    assert_eq!(
        failures(&session),
        vec![Cause::Reported(GpuFailure::BadFont)]
    );
    assert_eq!(session.rejected_fonts(), &[GARBAGE]);
    assert!(!session.has_process());

    // The next frame, from a good font, is whole again on a replacement.
    assert_eq!(
        render_matching(&mut session, &glyph_page(80, AHEM)),
        Backend::Gpu
    );
    assert_eq!(session.restarts(), 1);

    // The refused font is not sent again: naming it goes straight to software,
    // costing no further process.
    assert_eq!(render_matching(&mut session, &bad), Backend::Software);
    assert_eq!(failures(&session).len(), 1);
    assert!(session.has_process(), "the healthy process is untouched");
}

#[test]
fn killing_the_process_with_fonts_in_flight_falls_back_and_resends_them() {
    if !can_sandbox() {
        return;
    }
    let mut session = session();
    session.register_font(AHEM, big_ahem()).expect("register");
    let a = glyph_page(10, AHEM);
    assert_eq!(render_matching(&mut session, &a), Backend::Gpu);

    // The process stops mid-frame (fonts long since received), then dies.
    assert!(session.arm_fault(GpuFault::StallMidFrame));
    let pid = session.process_id().expect("a process");
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let status = std::process::Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status()
            .expect("kill(1)");
        assert!(status.success());
    });
    let big = {
        // 1000x600 so the frame has several strips and the first is not the
        // whole frame.
        let list = glyph_page(20, AHEM);
        let frame = session.render(&list, 1000, 600).expect("frame");
        let mut fonts = FontSet::new();
        fonts.insert(AHEM, big_ahem()).expect("ahem");
        assert_eq!(
            frame,
            rasterize_with(&list, 1000, 600, &fonts).expect("baseline")
        );
        list
    };
    killer.join().expect("killer");
    assert_eq!(session.last_backend(), Backend::Software);
    assert_eq!(failures(&session), vec![Cause::Exited]);

    // A kill with the process idle, and a fresh process needs the font again.
    assert_eq!(
        render_matching(&mut session, &glyph_page(30, AHEM)),
        Backend::Gpu
    );
    session.kill_process_for_test();
    assert_eq!(render_matching(&mut session, &big), Backend::Software);
    assert_eq!(
        render_matching(&mut session, &glyph_page(40, AHEM)),
        Backend::Gpu
    );
    assert_eq!(session.restarts(), 2);
}

#[test]
fn a_run_naming_an_unregistered_font_is_software_and_not_a_gpu_failure() {
    if !can_sandbox() {
        return;
    }
    let mut session = session();
    assert_eq!(
        render_matching(&mut session, &glyph_page(5, 42)),
        Backend::Software
    );
    assert!(failures(&session).is_empty());
}

#[test]
fn a_run_outside_its_arena_is_refused_like_the_software_path_refuses_it() {
    if !can_sandbox() {
        return;
    }
    let mut session = session();
    session.register_font(AHEM, big_ahem()).expect("register");
    let mut list = glyph_page(5, AHEM);
    list.items.push(Item::GlyphRun {
        font: FontId(AHEM),
        size: px(16),
        color: Color::BLACK,
        start: 0,
        len: 1000,
    });
    assert!(session.render(&list, 200, 80).is_err());
    assert!(
        failures(&session).is_empty(),
        "the input's fault, not the process's"
    );
}

#[test]
fn font_registration_enforces_the_wire_caps() {
    let mut session = GpuSession::new(GpuConfig::software_only());
    assert_eq!(
        session.register_font(1, Vec::new()),
        Err(FontRegisterError::BadLength)
    );
    assert_eq!(
        session.register_font(1, vec![0u8; MAX_FONT_BYTES as usize + 1]),
        Err(FontRegisterError::BadLength)
    );
    assert_eq!(session.register_font(1, big_ahem()), Ok(()));
    assert_eq!(
        session.register_font(1, big_ahem()),
        Err(FontRegisterError::Duplicate)
    );
    for id in 2..=px_ipc::gpu::MAX_FONTS as u32 {
        assert_eq!(session.register_font(id, vec![1u8]), Ok(()));
    }
    assert_eq!(
        session.register_font(1000, vec![1u8]),
        Err(FontRegisterError::OverCap)
    );
}

#[test]
fn software_only_sessions_draw_registered_fonts_too() {
    let mut session = GpuSession::new(GpuConfig::software_only());
    session.register_font(AHEM, big_ahem()).expect("register");
    let list = glyph_page(90, AHEM);
    assert_eq!(render_matching(&mut session, &list), Backend::Software);
    let frame = session.render(&list, 200, 80).expect("frame");
    assert_eq!(frame.pixel(18, 35), Some([10, 10, 90, 255]));
}
