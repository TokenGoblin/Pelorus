//! Phase 17: the memory cap, against the real content binary created through
//! `px_sandbox::spawn` (build-spec section 9: "memory cap kills a runaway tab
//! and only that tab"; ADR 043).
//!
//! The memory shapes are the ones `script_limits.rs` recorded as uncontained in
//! Phase 10: a doubling string, a huge `repeat`, array fills. They now end by
//! the allocator failing under the address-space limit, which aborts the
//! process (panic = abort, and Rust aborts on allocation failure). The tests
//! judge only what a broker can observe: the process is dead, it was killed by
//! a signal and not by an orderly exit, its address space never passed the cap
//! (the kernel's own high-water mark), and everything next to it is untouched.
//!
//! The watchdog is lengthened to a minute (under `testing`) so that a death
//! here cannot be the watchdog's: it ends the process by SIGABRT too, and the
//! two are told apart by the clock. Without the feature the shipping ten
//! seconds applies and the timing check is skipped; the high-water mark still
//! shows the process grew to the cap.
//!
//! Linux only: the Windows job-object memory cap is not built (docs/backlog.md).

#![cfg(target_os = "linux")]

#[cfg(test)]
mod common;

use std::time::Duration;

use common::sandboxed::{Death, SandboxedPeer, can_sandbox};
use common::{GENEROUS, value};
use px_sandbox::{CONTENT_MEMORY_LIMIT, Rung};

fn cap_kib() -> u64 {
    CONTENT_MEMORY_LIMIT.bytes() / 1024
}

/// A runaway-memory peer: sandboxed under the shipping content policy, with the
/// watchdog out of the way when the feature allows it.
#[cfg(test)]
fn runaway_peer() -> SandboxedPeer {
    #[allow(unused_mut)] // mutated only under `testing`
    let mut peer = SandboxedPeer::spawn(1);
    #[cfg(feature = "testing")]
    peer.set_watchdog_ms(60_000);
    peer
}

/// The process was killed (by a signal, code -1), never exited on its own, and
/// the kernel's address-space high-water mark never passed the cap.
#[cfg(test)]
fn assert_killed_under_the_cap(death: &Death) {
    assert_eq!(death.code, -1, "ended by a signal, not an exit: {death:?}");
    assert!(
        death.peak_kib <= cap_kib(),
        "address space {} KiB passed the {} KiB cap: {death:?}",
        death.peak_kib,
        cap_kib()
    );
    #[cfg(feature = "testing")]
    assert!(
        death.elapsed < Duration::from_secs(30),
        "died after {:?}: that is the watchdog's clock, not the cap's: {death:?}",
        death.elapsed
    );
}

/// ...and it got there by growing: a high-water mark near the cap shows the
/// limit is what stopped it.
#[cfg(test)]
fn assert_grew_to_the_cap(death: &Death) {
    assert!(
        death.peak_kib >= cap_kib() / 2,
        "the process never grew toward the cap: {death:?}"
    );
}

#[cfg(test)]
fn the_next_process_runs_one_plus_one() {
    let mut next = SandboxedPeer::spawn(2);
    assert_eq!(next.run("1+1"), value("2"));
}

#[test]
fn sandbox_memory_cap_is_applied_to_the_content_process_and_confirmed_by_the_kernel() {
    if !can_sandbox() {
        return;
    }
    let peer = SandboxedPeer::spawn(1);
    assert!(peer.child.applied().contains(&Rung::MemoryCap));
    assert_eq!(peer.child.memory_limit(), Some(CONTENT_MEMORY_LIMIT));
    // The parent's own reading of the table, independent of the child.
    let table =
        std::fs::read_to_string(format!("/proc/{}/limits", peer.child.id())).expect("limits");
    let row = table
        .lines()
        .find(|line| line.starts_with("Max address space"))
        .expect("the kernel lists the limit");
    let bytes = CONTENT_MEMORY_LIMIT.bytes().to_string();
    assert_eq!(
        row.split_whitespace().filter(|f| *f == bytes).count(),
        2,
        "soft and hard limit must both equal the cap: {row}"
    );
}

#[test]
fn sandbox_memory_cap_does_not_disturb_legitimate_work() {
    if !can_sandbox() {
        return;
    }
    // The control: the Phase 10 workload runs to completion under the cap with
    // room to spare, so a death in the tests below is the runaway and not the
    // cap being too tight for a normal page.
    let mut peer = SandboxedPeer::spawn(1);
    assert_eq!(peer.run("1+1"), value("2"));
    assert_eq!(
        peer.run("var s = 'x'; for (var i = 0; i < 24; i++) s += s; s.length"),
        value("16777216")
    );
    assert_eq!(
        peer.run("var a = []; for (var i = 0; i < 200000; i++) a.push({ i: i }); a.length"),
        value("200000")
    );
    assert_eq!(
        peer.run("typeof Date.now() + typeof performance.now()"),
        value("numbernumber")
    );
    assert_eq!(
        peer.run("var o = 0; setTimeout(function () { o = 1 }, 5); 'armed'"),
        value("armed")
    );
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(peer.run("o"), value("1"));
    let peak = peer.vm_peak_kib().expect("VmPeak");
    assert!(
        peak < cap_kib() / 2,
        "the ordinary workload used {peak} KiB of address space; the cap leaves too little margin"
    );
}

#[test]
fn sandbox_memory_cap_a_doubling_string_dies_at_the_cap() {
    if !can_sandbox() {
        return;
    }
    let mut peer = runaway_peer();
    let death = peer.run_until_death("var s = 'x'; for (;;) s += s;", GENEROUS);
    assert_killed_under_the_cap(&death);
    assert_grew_to_the_cap(&death);
    the_next_process_runs_one_plus_one();
}

#[test]
fn sandbox_memory_cap_a_huge_string_repeat_is_refused_or_dies() {
    if !can_sandbox() {
        return;
    }
    // `repeat(2**28)` is stopped by the engine's own counted loop (a layer-1
    // ending) on this engine; `repeat(2**30)` asks the allocator for tens of
    // GiB in one call, which Phase 10 could not run on a shared runner and the
    // cap now makes harmless: the allocation fails and the process aborts.
    let mut peer = runaway_peer();
    peer.send_script("'x'.repeat(2**28).length");
    match peer.recv_within(GENEROUS) {
        Some(Ok(_)) | Some(Err(_)) => {}
        None => panic!("neither an answer nor a death"),
    }
    // Whichever it was, the process is either gone or alive and answering; it
    // is not past the cap.
    assert!(peer.vm_peak_kib().is_none_or(|peak| peak <= cap_kib()));
    drop(peer);

    let mut peer = runaway_peer();
    let death = peer.run_until_death("'x'.repeat(2**30).length", GENEROUS);
    assert_killed_under_the_cap(&death);
    the_next_process_runs_one_plus_one();
}

#[test]
fn sandbox_memory_cap_array_growth_never_passes_the_cap() {
    if !can_sandbox() {
        return;
    }
    // Arrays are the shape the cap does NOT end in time. Boa writes array
    // elements one property at a time, and measured on the unoptimised build
    // the address space grows about 6 MiB a second (355 MiB after a minute),
    // so the watchdog's clock ends the process long before the cap is near.
    // What the cap guarantees here is the ceiling, not the speed: whichever
    // ends the process, its address space never passes the limit. (A native
    // `fill` of four billion slots, in script_limits.rs, is the same shape.)
    #[allow(unused_mut)] // mutated only under `testing`
    let mut peer = SandboxedPeer::spawn(1);
    #[cfg(feature = "testing")]
    peer.set_watchdog_ms(3_000);
    let death = peer.run_until_death("var a = [7]; for (;;) a = a.concat(a);", GENEROUS);
    assert_eq!(death.code, -1, "ended by a signal: {death:?}");
    assert!(death.peak_kib <= cap_kib(), "{death:?}");
    the_next_process_runs_one_plus_one();
}

/// "Memory cap kills a runaway tab and only that tab": the broker (this test),
/// a content process holding a frame and a second sandboxed process with live
/// script state are all untouched while the first one dies by its cap.
#[test]
fn sandbox_memory_cap_kills_only_that_tab() {
    if !can_sandbox() {
        return;
    }
    use px_broker::{Broker, ContentProcess, serve_once};
    use px_ipc::{Request, Response};

    let mut broker = Broker::new();
    let mut tab_b = ContentProcess::spawn(
        &mut broker,
        std::path::Path::new(env!("CARGO_BIN_EXE_px-content")),
    )
    .expect("spawn the broker's own content process");
    let frame = broker.create_frame(tab_b.channel_id()).expect("a frame");
    let channels = broker.channel_count();

    let mut tab_c = SandboxedPeer::spawn(3);
    assert_eq!(tab_c.run("globalThis.k = 41; k"), value("41"));

    let mut tab_a = runaway_peer();
    let pid_a = tab_a.child.id();
    let death = tab_a.run_until_death("var s = 'x'; for (;;) s += s;", GENEROUS);
    assert_killed_under_the_cap(&death);
    assert_ne!(pid_a, tab_c.child.id());

    // The broker is alive and unchanged; its frame is still tab B's.
    assert_eq!(broker.channel_count(), channels);
    assert!(broker.holds_frame(tab_b.channel_id(), frame));
    assert_eq!(
        broker
            .dispatch(tab_b.channel_id(), Request::FrameHost { frame })
            .response,
        Response::FrameHost {
            host: px_ipc::FrameHost::Local
        }
    );
    assert!(tab_b.is_alive());
    assert_eq!(
        serve_once(&mut broker, &mut tab_b, Duration::from_secs(30))
            .expect("tab B's opening ping")
            .response,
        Response::Pong
    );

    // Tab C kept its realm and its state, and is still being served.
    assert_eq!(tab_c.run("k + 1"), value("42"));
    assert_eq!(tab_c.child.try_wait().expect("try_wait"), None);
    the_next_process_runs_one_plus_one();
}
