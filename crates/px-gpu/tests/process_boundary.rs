//! Phase 8 gate items on the process boundary (docs/phase-08-plan.md table):
//!
//! - the GPU process has no filesystem or network capability (the profile is in
//!   place, confirmed by the kernel, and the forbidden operations fail);
//! - killing it, or any other way of losing it, falls back to software without
//!   losing the session, and the next frame is the software baseline.
//!
//! These spawn the real `px-gpu` binary through `px_sandbox::spawn`, as the
//! broker does. They need the testing vocabulary (probe, fault injection),
//! which `px-gpu`'s self dev-dependency switches on for the test and for the
//! binary it spawns, so a plain `cargo test --workspace` runs them.
//!
//! What they cannot show is in the plan's gate report: there is no GPU here, so
//! the rasterizer inside the process is the software one; and the Windows half
//! of the profile does not exist yet.

#![cfg(feature = "testing")]

use std::time::{Duration, Instant};

use px_broker::Broker;
use px_gpu::session::{Backend, Cause, GpuConfig, GpuEvent, GpuSession};
use px_ipc::gpu::{GpuFailure, GpuFault, ProbeOp};
use px_layout::geom::{au, px};
use px_paint::{BorderSide, BorderStyle, Canvas, Color, DisplayList, Item, Rect};
use px_sandbox::Rung;

const PATIENT: Duration = Duration::from_secs(20);
const IMPATIENT: Duration = Duration::from_millis(700);

fn binary() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_px-gpu"))
}

/// Whether this machine can sandbox at all, and a loud failure when it cannot
/// but was promised it could (the same rule `px-sandbox`'s own tests apply: a
/// silent skip is indistinguishable from a pass).
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

fn config(max_restarts: u32, frame: Duration) -> GpuConfig {
    let mut config = GpuConfig::process(binary());
    config.max_restarts = max_restarts;
    config.frame_deadline = frame;
    config.startup_deadline = PATIENT;
    config
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect {
        x: px(x),
        y: px(y),
        width: px(w),
        height: px(h),
    }
}

/// A page-like list: backgrounds, a border, a clip with content, placeholder
/// text. `seed` makes pages differ so a stale frame cannot pass for a new one.
fn page(seed: u8) -> DisplayList {
    let colour = |r: u8, g: u8, b: u8| Color { r, g, b, a: 255 };
    let side = BorderSide {
        width: px(3),
        style: BorderStyle::Solid,
        color: colour(seed, 0, 0),
    };
    let mut list = DisplayList::new();
    list.push(Item::FillRect {
        rect: rect(0, 0, 300, 200),
        color: colour(240, seed, 200),
    });
    list.push(Item::Border {
        rect: rect(10, 10, 120, 80),
        sides: [side, side, side, side],
    });
    list.push(Item::PushClip {
        rect: rect(20, 20, 60, 40),
    });
    list.push(Item::FillRect {
        rect: Rect {
            x: au(1234),
            y: px(15),
            width: px(200),
            height: px(100),
        },
        color: Color {
            r: 0,
            g: seed,
            b: 255,
            a: 128,
        },
    });
    list.push(Item::PopClip);
    list.push(Item::TextRun {
        rect: rect(140, 100, 90, 12),
        color: colour(20, 20, seed),
    });
    list
}

fn baseline(list: &DisplayList, w: u32, h: u32) -> Canvas {
    px_paint::rasterize(list, w, h).expect("software baseline")
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

/// Render and assert the frame is byte-identical to the software baseline,
/// whichever backend produced it. Returns the backend that did.
fn render_matching(session: &mut GpuSession, list: &DisplayList, w: u32, h: u32) -> Backend {
    let frame = session.render(list, w, h).expect("a frame");
    assert_eq!(frame, baseline(list, w, h), "not the software baseline");
    session.last_backend()
}

fn kill_externally(pid: u32) {
    let status = std::process::Command::new("kill")
        .args(["-KILL", &pid.to_string()])
        .status()
        .expect("kill(1)");
    assert!(status.success());
}

// ---------------------------------------------------------------- the profile

#[test]
fn gpu_process_denied_capabilities() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(0, PATIENT));
    session.warm_up();
    assert!(
        session.has_process(),
        "the process must start under the profile: {:?}",
        session.events().collect::<Vec<_>>()
    );

    // Confirmed by the kernel, not by the process: both stages, which means
    // both the creation-time filter and the one the process installed itself.
    let applied = session.applied().expect("a process").to_vec();
    for rung in px_sandbox::FLOOR {
        assert!(
            applied.contains(rung),
            "{rung} not confirmed in {applied:?}"
        );
    }
    assert_eq!(applied, vec![Rung::NoNewPrivs, Rung::Seccomp]);

    // The process attempts every forbidden operation. Each must have been
    // refused by policy: EPERM from the filter or EACCES from Landlock, not
    // some incidental failure.
    let attempts = session.probe().expect("a probe report");
    for op in [
        ProbeOp::OpenSystemFile,
        ProbeOp::OpenWorkingDirectory,
        ProbeOp::Socket,
        ProbeOp::Connect,
        ProbeOp::Bind,
        ProbeOp::Exec,
    ] {
        let attempt = attempts
            .iter()
            .find(|a| a.op == op)
            .unwrap_or_else(|| panic!("{op:?} was not probed"));
        assert!(
            !attempt.succeeded && (attempt.errno == 1 || attempt.errno == 13),
            "{op:?} was not denied by policy: {attempt:?}"
        );
    }

    // Landlock is a rung above the floor. Where the kernel has it, the process
    // must have entered it; where it does not, the process says 0 and the
    // seccomp denial above is what held.
    let landlock = px_sandbox::detect().has(Rung::Landlock);
    let reported = session.reported_landlock_abi().expect("a process");
    assert_eq!(reported >= 1, landlock, "reported ABI {reported}");

    // Confined, and still useful: it renders.
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Gpu
    );
    assert!(failures(&session).is_empty(), "{:?}", failures(&session));
}

/// A process whose first filter is missing, or whose second never landed, is
/// not used. Driven through the verification the session calls, with the
/// kernel's own text for this very process (which has no second filter).
#[test]
fn gpu_process_unconfined_is_not_used() {
    if !can_sandbox() {
        return;
    }
    // This test process is not a GPU process: it has neither of the two
    // filters. The kernel's account of it must be refused by the same function
    // the session applies to a real child.
    let mut child = px_sandbox::spawn(std::path::Path::new("/bin/cat")).expect("spawn");
    // `cat` is sandboxed at creation (stage one) but never enters stage two.
    // Give the kernel a moment to have exec'd it, then ask.
    std::thread::sleep(Duration::from_millis(100));
    let verdict = child.verify_gpu_profile();
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        verdict.is_err(),
        "a child that never entered stage two was confirmed: {verdict:?}"
    );
}

// ------------------------------------------------------------------ the frames

#[test]
fn gpu_frames_are_the_software_baseline() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(0, PATIENT));
    for (seed, w, h) in [
        (1u8, 300u32, 200u32),
        (2, 1, 1),
        (3, 1000, 600),
        (4, 37, 911),
    ] {
        assert_eq!(
            render_matching(&mut session, &page(seed), w, h),
            Backend::Gpu,
            "{w}x{h}"
        );
    }
    // 1000x600 is 2.4 MB: it cannot have been one message.
    assert!(failures(&session).is_empty());
    assert_eq!(session.restarts(), 0);
}

#[test]
fn gpu_a_long_display_list_crosses_in_chunks() {
    if !can_sandbox() {
        return;
    }
    let mut list = DisplayList::new();
    for i in 0..5000 {
        list.push(Item::FillRect {
            rect: rect(i % 290, (i / 290) % 190, 5, 5),
            color: Color {
                r: (i % 251) as u8,
                g: (i % 13) as u8,
                b: 7,
                a: 255,
            },
        });
    }
    let mut session = GpuSession::new(config(0, PATIENT));
    assert_eq!(render_matching(&mut session, &list, 300, 200), Backend::Gpu);
}

// ---------------------------------------------------------------- the fallback

#[test]
fn gpu_kill_falls_back() {
    if !can_sandbox() {
        return;
    }
    // The state a dead GPU process must not disturb: the broker's channels,
    // frames and audit log.
    let mut broker = Broker::new();
    let channel = broker.open_channel().expect("channel");
    let frame = broker.create_frame(channel).expect("frame");
    let channels_before = broker.channel_count();

    let mut session = GpuSession::new(config(8, PATIENT));
    let (a, b, c) = (page(10), page(20), page(30));

    // Page A on the GPU, equal to its baseline.
    assert_eq!(render_matching(&mut session, &a, 300, 200), Backend::Gpu);
    let pid = session.process_id().expect("a process");

    // SIGKILL from outside, as the OS or a crash would.
    kill_externally(pid);

    // Page B: the process is gone, the frame is still produced, in software,
    // and is byte-identical to its baseline.
    assert_eq!(
        render_matching(&mut session, &b, 300, 200),
        Backend::Software
    );
    assert!(!session.has_process());
    let causes = failures(&session);
    assert!(
        matches!(causes.as_slice(), [Cause::Exited | Cause::WriteFailed]),
        "{causes:?}"
    );

    // The session state is the one from before.
    assert_eq!(broker.channel_count(), channels_before);
    assert!(broker.is_open(channel));
    assert!(broker.holds_frame(channel, frame));
    assert_eq!(
        broker.audit_log().count(),
        0,
        "nothing was denied or changed"
    );

    // Page C: a replacement is created, confirmed, and used.
    assert_eq!(render_matching(&mut session, &c, 300, 200), Backend::Gpu);
    assert_eq!(session.restarts(), 1);
    assert_ne!(session.process_id(), Some(pid));
    assert_eq!(broker.channel_count(), channels_before);
}

#[test]
fn gpu_kill_mid_frame_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(8, PATIENT));
    let a = page(40);
    assert_eq!(render_matching(&mut session, &a, 300, 200), Backend::Gpu);

    // The next frame stops after the header and one strip, and waits; a second
    // thread kills the process while the broker is in the middle of it.
    assert!(session.arm_fault(GpuFault::StallMidFrame));
    let pid = session.process_id().expect("a process");
    let killer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        kill_externally(pid);
    });
    let started = Instant::now();
    let b = page(50);
    // 1000x600 so the frame has several strips and the first is not the whole.
    assert_eq!(
        render_matching(&mut session, &b, 1000, 600),
        Backend::Software
    );
    killer.join().expect("killer");
    assert!(
        started.elapsed() < PATIENT / 2,
        "the kill, not the deadline, ended it"
    );
    assert_eq!(failures(&session), vec![Cause::Exited]);

    // And the next frame is whole again.
    assert_eq!(
        render_matching(&mut session, &page(60), 300, 200),
        Backend::Gpu
    );
}

#[test]
fn gpu_malformed_frame_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(8, PATIENT));
    assert!(session.arm_fault(GpuFault::Malformed));
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Software
    );
    assert_eq!(failures(&session), vec![Cause::Malformed]);
    assert_eq!(
        render_matching(&mut session, &page(2), 300, 200),
        Backend::Gpu
    );
}

#[test]
fn gpu_oversized_frame_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(8, PATIENT));
    assert!(session.arm_fault(GpuFault::Oversized));
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Software
    );
    assert_eq!(failures(&session), vec![Cause::Oversized]);
    assert_eq!(
        render_matching(&mut session, &page(2), 300, 200),
        Backend::Gpu
    );
}

#[test]
fn gpu_hung_process_times_out_and_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(8, IMPATIENT));
    session.warm_up();
    assert!(session.arm_fault(GpuFault::Hang));
    let started = Instant::now();
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Software
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the deadline bounds a hung process"
    );
    assert_eq!(failures(&session), vec![Cause::TimedOut]);
    assert!(!session.has_process(), "a hung process is killed, not kept");
    assert_eq!(
        render_matching(&mut session, &page(2), 300, 200),
        Backend::Gpu
    );
}

#[test]
fn gpu_a_process_that_exits_without_answering_falls_back() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(8, PATIENT));
    assert!(session.arm_fault(GpuFault::Exit));
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Software
    );
    assert_eq!(failures(&session), vec![Cause::Exited]);
}

#[test]
fn gpu_lies_about_the_frame_are_rejected() {
    if !can_sandbox() {
        return;
    }
    for (fault, expected) in [
        (
            GpuFault::WrongRows,
            Cause::Protocol("a strip is not the next valid strip"),
        ),
        (
            GpuFault::WrongSerial,
            Cause::Protocol("the header answers a different request"),
        ),
    ] {
        let mut session = GpuSession::new(config(8, PATIENT));
        assert!(session.arm_fault(fault));
        assert_eq!(
            render_matching(&mut session, &page(1), 300, 200),
            Backend::Software,
            "{fault:?}"
        );
        assert_eq!(failures(&session), vec![expected], "{fault:?}");
    }
}

#[test]
fn gpu_restarts_are_bounded_then_the_session_stays_on_software() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(2, PATIENT));
    // Three failures: the bound of two replacements is spent on the third.
    for round in 0..3 {
        assert!(session.arm_fault(GpuFault::Exit), "round {round}");
        assert_eq!(
            render_matching(&mut session, &page(round), 300, 200),
            Backend::Software
        );
    }
    assert!(session.is_software_only());
    assert_eq!(session.restarts(), 2, "two replacements, no third");
    assert!(
        session
            .events()
            .any(|e| matches!(e, GpuEvent::GaveUp { restarts: 2 }))
    );

    // For the rest of its life: software, correct, and no new process.
    for round in 10..14 {
        assert_eq!(
            render_matching(&mut session, &page(round), 300, 200),
            Backend::Software
        );
        assert!(!session.has_process());
    }
    assert_eq!(session.restarts(), 2);
}

#[test]
fn gpu_a_binary_that_cannot_start_ends_in_software_within_the_bound() {
    if !can_sandbox() {
        return;
    }
    let mut config = GpuConfig::process("/nonexistent/px-gpu");
    config.max_restarts = 2;
    let mut session = GpuSession::new(config);
    for round in 0..6 {
        assert_eq!(
            render_matching(&mut session, &page(round), 300, 200),
            Backend::Software
        );
    }
    assert!(session.is_software_only());
    let refused = failures(&session)
        .into_iter()
        .filter(|c| matches!(c, Cause::SpawnRefused(_)))
        .count();
    assert_eq!(refused, 3, "one first try and two retries, then no more");
}

#[test]
fn gpu_disable_gpu_spawns_nothing() {
    let mut session = GpuSession::new(GpuConfig::software_only());
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Software
    );
    assert!(!session.has_process());
    assert!(session.is_software_only());
    assert_eq!(
        session.events().collect::<Vec<_>>(),
        vec![&GpuEvent::Disabled]
    );
}

#[test]
fn gpu_unrenderable_input_is_not_a_gpu_failure() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(0, PATIENT));
    session.warm_up();
    let pid = session.process_id();
    assert!(session.render(&page(1), 0, 200).is_err());
    let mut unbalanced = DisplayList::new();
    unbalanced.push(Item::PopClip);
    assert!(session.render(&unbalanced, 10, 10).is_err());
    assert!(failures(&session).is_empty());
    assert_eq!(session.process_id(), pid, "the process was not disturbed");
    assert_eq!(
        render_matching(&mut session, &page(1), 300, 200),
        Backend::Gpu
    );
}

// ------------------------------------------------------------------- the flood

fn resident_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.trim().trim_end_matches("kB").trim().parse().ok())
        })
        .unwrap_or(0)
}

#[test]
fn gpu_a_flood_of_frames_does_not_grow_memory_without_bound() {
    if !can_sandbox() {
        return;
    }
    let mut session = GpuSession::new(config(1000, PATIENT));
    let list = page(5);
    // Warm everything: process, threads, allocator.
    for _ in 0..5 {
        render_matching(&mut session, &list, 300, 200);
    }
    let before = resident_kib();

    // A great many honest frames.
    for _ in 0..300 {
        assert_eq!(render_matching(&mut session, &list, 300, 200), Backend::Gpu);
    }
    // And a process that answers, then keeps sending strips nobody asked for,
    // over and over, each time replaced and each time caught.
    let mut caught = 0;
    for _ in 0..12 {
        assert!(session.arm_fault(GpuFault::Flood));
        render_matching(&mut session, &list, 300, 200);
        render_matching(&mut session, &list, 300, 200);
        if session.has_process() {
            // Whatever it had sent was refused by now or it was a clean frame;
            // either way the next frame is right.
        }
        caught = failures(&session).len();
    }
    assert!(caught >= 1, "a flood must be noticed, at least once");

    let after = resident_kib();
    let growth = after.saturating_sub(before);
    // The bounded queue parks at most four strips of a quarter megabyte.
    // Allow generously for allocator slack; unbounded growth would be
    // gigabytes over this many strips.
    assert!(
        growth < 64 * 1024,
        "resident memory grew by {growth} KiB ({before} -> {after})"
    );
}

#[test]
fn gpu_the_reader_never_holds_more_than_its_queue() {
    if !can_sandbox() {
        return;
    }
    // After a flood the session must not be wedged or carrying the backlog:
    // the next frames are correct and quick.
    let mut session = GpuSession::new(config(100, PATIENT));
    assert!(session.arm_fault(GpuFault::Flood));
    let list = page(8);
    let started = Instant::now();
    for _ in 0..10 {
        render_matching(&mut session, &list, 300, 200);
    }
    assert!(started.elapsed() < PATIENT);
    assert_eq!(render_matching(&mut session, &list, 300, 200), Backend::Gpu);
    let _ = GpuFailure::Backend; // the vocabulary is in scope for readers of this file
}
