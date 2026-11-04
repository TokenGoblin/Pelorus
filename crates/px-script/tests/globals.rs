//! What the script realm has and, deliberately, has not (ADR 040).
//!
//! `typeof WebAssembly === "undefined"` is a decision, not an omission, and the
//! preferences page documents it beside the no-JIT statement. The same holds for
//! `Intl` (decision 2), `Temporal` and `SharedArrayBuffer` (a shared-memory clock
//! bypasses any coarsening, ADR 040 "Not claimed").

use px_script::{Host, Outcome};

#[cfg(test)]
fn eval(source: &str) -> Outcome {
    Host::new().expect("host").eval(source)
}

#[cfg(test)]
fn value(text: &str) -> Outcome {
    Outcome::Value(text.to_string())
}

#[test]
fn script_absent_webassembly_is_undefined() {
    assert_eq!(eval("typeof WebAssembly"), value("undefined"));
}

#[test]
fn script_absent_intl_is_undefined() {
    assert_eq!(eval("typeof Intl"), value("undefined"));
}

#[test]
fn script_absent_temporal_is_undefined() {
    assert_eq!(eval("typeof Temporal"), value("undefined"));
}

#[test]
fn script_absent_shared_array_buffer_is_undefined() {
    assert_eq!(eval("typeof SharedArrayBuffer"), value("undefined"));
}

#[test]
fn the_es_builtins_and_annex_b_are_present() {
    assert_eq!(
        eval("typeof Promise + typeof Map + typeof Proxy"),
        value("functionfunctionfunction")
    );
    // annex-b is on (ADR 040 decision 1): real sites use these.
    assert_eq!(
        eval("typeof escape + typeof unescape"),
        value("functionfunction")
    );
    assert_eq!(eval("({}).__proto__ === Object.prototype"), value("true"));
}

#[test]
fn a_completion_value_and_a_thrown_value_are_data() {
    assert_eq!(eval("1+1"), value("2"));
    assert_eq!(eval("'a' + 'b'"), value("ab"));
    assert_eq!(eval("undefined"), value("undefined"));
    assert_eq!(
        eval("throw new TypeError('boom')"),
        Outcome::Error("TypeError: boom".to_string())
    );
    assert_eq!(eval("throw 42"), Outcome::Error("42".to_string()));
    assert_eq!(eval("1 +"), eval("1 +"));
    assert!(matches!(eval("1 +"), Outcome::Error(m) if m.contains("SyntaxError")));
    // A symbol has no ToString; it must still come back as text, not a panic.
    assert!(matches!(eval("Symbol('s')"), Outcome::Value(t) if t.contains("Symbol(s)")));
}

#[test]
fn state_persists_across_runs_in_one_host() {
    let mut host = Host::new().expect("host");
    assert_eq!(host.eval("var x = 20"), value("undefined"));
    assert_eq!(host.eval("x + 22"), value("42"));
}
