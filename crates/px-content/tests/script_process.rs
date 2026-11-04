//! Phase 10 unit 1: the script host reached through the real content binary,
//! over its real pipes (docs/phase-10-plan.md, ADR 040).
//!
//! What is proven here is the property that matters first: script text goes in
//! over the channel and a result or an error comes back as **data**, the
//! protocol fails closed, and every field is bounded. The sandbox, the queues
//! and the clock have their own suites.

// `cfg(test)` is how clippy knows helper code in an integration test is test
// code (clippy.toml allows expect in tests); it is always true here.
#[cfg(test)]
mod common;

use common::{Peer, value};
use px_ipc::script::{
    MAX_SCRIPT_RESULT_BYTES, MAX_SCRIPT_SOURCE_BYTES, ScriptRequest, ScriptResponse, ScriptResult,
    ScriptSource,
};

#[test]
fn script_eval_one_plus_one_over_a_real_pipe() {
    let mut peer = Peer::spawn(1);
    assert_eq!(peer.run("1+1"), value("2"));
}

#[test]
fn script_eval_a_thrown_error_returns_as_data_and_the_process_lives_on() {
    let mut peer = Peer::spawn(1);
    match peer.run("throw new RangeError('out of range')") {
        ScriptResult::Error { message } => assert_eq!(message.as_str(), "RangeError: out of range"),
        other => panic!("expected an Error, got {other:?}"),
    }
    // The same process, the same realm: nothing about the error ended it.
    assert_eq!(peer.run("var survived = 'yes'; survived"), value("yes"));
    assert!(matches!(
        peer.run("1 +"),
        ScriptResult::Error { message } if message.as_str().contains("SyntaxError")
    ));
    assert_eq!(peer.run("survived"), value("yes"));
}

#[test]
fn script_eval_state_lives_in_one_realm_across_requests() {
    let mut peer = Peer::spawn(1);
    assert_eq!(peer.run("globalThis.counter = 40"), value("40"));
    assert_eq!(peer.run("counter + 2"), value("42"));
}

#[test]
fn script_eval_a_result_over_its_cap_is_cut_not_refused() {
    let mut peer = Peer::spawn(1);
    match peer.run("'a'.repeat(200000)") {
        ScriptResult::Value { text } => {
            assert_eq!(text.as_str().len(), MAX_SCRIPT_RESULT_BYTES);
            assert!(text.as_str().bytes().all(|b| b == b'a'));
        }
        other => panic!("expected a Value, got {other:?}"),
    }
}

#[test]
fn script_eval_an_oversized_source_is_refused_and_the_process_ends() {
    // Hand-built: a RunScript (variant 1) claiming one byte over the cap, with
    // the bytes present, inside a frame the codec itself accepts (under 1 MiB).
    // The field cap is what refuses it.
    let n = MAX_SCRIPT_SOURCE_BYTES + 1;
    let mut body = vec![1u8];
    let mut length = n;
    loop {
        let byte = (length & 0x7f) as u8;
        length >>= 7;
        if length == 0 {
            body.push(byte);
            break;
        }
        body.push(byte | 0x80);
    }
    body.extend(std::iter::repeat_n(b'1', n));
    let mut frame = u32::try_from(body.len() + 1)
        .expect("fits")
        .to_le_bytes()
        .to_vec();
    frame.push(<ScriptRequest as px_ipc::Message>::TAG);
    frame.extend(body);

    let mut peer = Peer::spawn(1);
    peer.send_raw(&frame);
    assert!(
        !peer.wait_exit().success(),
        "an oversized source must end the process"
    );
    // And no result was produced for it.
    assert!(peer.recv().is_err());
}

#[test]
fn script_eval_the_sender_cannot_build_an_oversized_source() {
    assert!(ScriptSource::new("1".repeat(MAX_SCRIPT_SOURCE_BYTES + 1)).is_err());
    assert!(ScriptSource::new("1".repeat(MAX_SCRIPT_SOURCE_BYTES)).is_ok());
}

#[test]
fn script_eval_a_source_at_the_cap_runs() {
    let mut peer = Peer::spawn(1);
    // `1+1` padded to the cap with a trailing comment.
    let mut source = String::from("1+1//");
    source.push_str(&"x".repeat(MAX_SCRIPT_SOURCE_BYTES - source.len()));
    assert_eq!(source.len(), MAX_SCRIPT_SOURCE_BYTES);
    assert_eq!(peer.run(&source), value("2"));
}

#[test]
fn script_eval_run_script_before_init_ends_the_process() {
    let mut peer = Peer::spawn_unseeded();
    peer.send(&ScriptRequest::RunScript {
        source: ScriptSource::new("1+1".into()).expect("small"),
    });
    assert!(!peer.wait_exit().success());
    assert!(peer.recv().is_err());
}

#[test]
fn script_eval_a_second_init_ends_the_process() {
    let mut peer = Peer::spawn(7);
    assert_eq!(peer.run("1"), value("1"));
    peer.send(&ScriptRequest::Init { seed: 8 });
    assert!(!peer.wait_exit().success());
}

#[test]
fn script_eval_garbage_on_the_wire_ends_the_process_without_a_reply() {
    let mut peer = Peer::spawn(1);
    peer.send_raw(&[0xFF; 64]);
    assert!(!peer.wait_exit().success());
    assert!(peer.recv().is_err());
}

#[test]
fn script_eval_a_reflected_response_does_not_run() {
    // The broker's own direction's bytes, sent back at the content process.
    let mut peer = Peer::spawn(1);
    let reflected = px_ipc::encode_frame(&ScriptResponse::Result(ScriptResult::Value {
        text: px_ipc::script::ResultText::new("1".into()).expect("short"),
    }))
    .expect("encode");
    peer.send_raw(&reflected);
    assert!(!peer.wait_exit().success());
}

#[test]
fn script_eval_the_broker_closing_the_pipe_is_an_orderly_exit() {
    let mut peer = Peer::spawn(1);
    assert_eq!(peer.run("2*3"), value("6"));
    peer.close_stdin();
    assert!(peer.wait_exit().success());
}
