#![forbid(unsafe_code)]

//! Boa host, event loop, task and microtask queues (ADR 040).
//!
//! [`Host`] owns the one [`boa_engine::Context`] of a content process. The
//! engine is an interpreter only (invariant 6), runs on the one thread that
//! created it (the `Context` is `!Send`), and has no handle it was not given:
//! nothing in this crate opens a file or a socket.
//!
//! Phase 10 scripts touch no DOM. The only globals are the ES builtins plus
//! what this crate defines. `Intl`, `Temporal`, `WebAssembly` and
//! `SharedArrayBuffer` are absent on purpose (ADR 040); `tests/globals.rs`
//! asserts it.

use boa_engine::{Context, JsError, JsValue, Source, js_string};

/// What running a script came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The completion value, converted to a string.
    Value(String),
    /// An uncaught exception, converted to a string. The script's own text of
    /// the error; nothing about the host.
    Error(String),
}

/// The script host of one content process.
pub struct Host {
    context: Context,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    /// A host with a fresh realm and the ES builtins.
    pub fn new() -> Self {
        let mut host = Self {
            context: Context::default(),
        };
        host.remove_deliberately_absent_globals();
        host
    }

    /// Boa 0.22 defines `SharedArrayBuffer` and has no feature to turn it off,
    /// so the host deletes the binding (ADR 040: shared memory plus a worker is
    /// a counter that bypasses any clock coarsening, and it stays off until a
    /// page can be said to be cross-origin isolated). `Intl`, `Temporal` and
    /// `WebAssembly` are absent because their features are off, not by this
    /// function; `tests/globals.rs` asserts all four.
    fn remove_deliberately_absent_globals(&mut self) {
        let global = self.context.global_object();
        // The binding is configurable, so this cannot fail; if it somehow did,
        // the globals test is what notices, and it must not be hidden here.
        let _ = global.delete_property_or_throw(js_string!("SharedArrayBuffer"), &mut self.context);
    }

    /// Run `source` as a classic script and report its completion.
    ///
    /// A thrown value is data, never a Rust error and never a panic: this is
    /// the boundary hostile script text crosses.
    pub fn eval(&mut self, source: &str) -> Outcome {
        match self.context.eval(Source::from_bytes(source)) {
            Ok(value) => Outcome::Value(self.describe(&value)),
            Err(error) => Outcome::Error(self.describe_error(error)),
        }
    }

    /// `ToString`, or the engine's display form for what has none (a symbol).
    fn describe(&mut self, value: &JsValue) -> String {
        match value.to_string(&mut self.context) {
            Ok(text) => text.to_std_string_escaped(),
            Err(_) => value.display().to_string(),
        }
    }

    fn describe_error(&mut self, error: JsError) -> String {
        match error.into_opaque(&mut self.context) {
            Ok(thrown) => self.describe(&thrown),
            // An engine-level error: no script value exists to print.
            Err(engine) => engine.to_string(),
        }
    }
}
