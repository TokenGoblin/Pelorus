#![forbid(unsafe_code)]

//! Boa host, event loop, task and microtask queues (ADR 040).
//!
//! [`Host`] owns the one [`boa_engine::Context`] of a content process. The
//! engine is an interpreter only (invariant 6), runs on the one thread that
//! created it (the `Context` is `!Send`), and has no handle it was not given:
//! nothing in this crate opens a file or a socket, and the engine's default
//! filesystem module loader is replaced by one that refuses everything.
//!
//! Phase 10 scripts touch no DOM. The only globals are the ES builtins plus
//! what this crate defines. `Intl`, `Temporal`, `WebAssembly` and
//! `SharedArrayBuffer` are absent on purpose (ADR 040); `tests/globals.rs`
//! asserts it.
//!
//! The loop is [`queue::LoopCore`] (pure, bounded, unit-tested without the
//! engine) driven by [`Host`]: `eval` runs a script then a microtask
//! checkpoint, and [`Host::step`] runs one timer task then its checkpoint.

pub mod clock;
pub mod queue;
mod runtime;

use std::rc::Rc;
use std::time::Duration;

use boa_engine::module::IdleModuleLoader;
use boa_engine::{Context, JsError, JsValue, Source, js_string};

use clock::{RawClock, StdClock};
use queue::{Caps, Task};
use runtime::Shared;

pub use queue::TerminateReason;
#[cfg(feature = "testing")]
pub use queue::{COUNTERS_MARKER, HighWater};

/// What running a script came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The completion value, converted to a string.
    Value(String),
    /// An uncaught exception, converted to a string. The script's own text of
    /// the error; nothing about the host.
    Error(String),
    /// The host stopped script, for good, because a bound was hit. Nothing is
    /// run once this is the state.
    Terminated(TerminateReason),
}

/// One step of the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// A task ran (and the microtask checkpoint after it).
    Ran,
    /// Nothing is due yet; the earliest timer is due after this long.
    Waiting(Duration),
    /// No task, no timer: the loop has nothing to do.
    Idle,
    /// Script was stopped. Nothing will run again.
    Terminated(TerminateReason),
}

/// How a host is built.
pub struct HostOptions {
    /// The bounds.
    pub caps: Caps,
    /// Where time comes from.
    pub clock: Rc<dyn RawClock>,
}

impl Default for HostOptions {
    fn default() -> Self {
        Self {
            caps: Caps::default(),
            clock: Rc::new(StdClock::new()),
        }
    }
}

/// The script host of one content process.
pub struct Host {
    // Declared first so it drops first: the callbacks and promise jobs it holds
    // are collected objects, and must be released while the engine exists.
    shared: Rc<Shared>,
    id: u64,
    context: Context,
}

/// The engine could not be created. The host does not exist, and nothing falls
/// back to a default engine: that one has an unbounded queue and reads modules
/// from disk.
#[derive(Debug)]
pub struct HostError(String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not create the script engine: {}", self.0)
    }
}

impl std::error::Error for HostError {}

impl Drop for Host {
    fn drop(&mut self) {
        // Free every callback, then forget the registry entry, before the
        // context goes.
        self.shared.core.borrow_mut().discard();
        runtime::unregister(self.id);
    }
}

impl Host {
    /// A host with the default bounds and the real clock.
    pub fn new() -> Result<Self, HostError> {
        Self::with_options(HostOptions::default())
    }

    /// A host with these bounds and this clock.
    pub fn with_options(options: HostOptions) -> Result<Self, HostError> {
        let shared = Rc::new(Shared::new(options.caps, options.clock));
        let id = runtime::register(Rc::clone(&shared));
        let built = Context::builder()
            // `Atomics.wait` would block the thread; no script may.
            .can_block(false)
            .job_executor(Rc::clone(&shared))
            // The default loader reads modules from the working directory.
            .module_loader(Rc::new(IdleModuleLoader))
            .build();
        let context = match built {
            Ok(context) => context,
            Err(error) => {
                runtime::unregister(id);
                return Err(HostError(error.to_string()));
            }
        };
        let mut host = Self {
            shared,
            id,
            context,
        };
        host.install_globals();
        Ok(host)
    }

    fn install_globals(&mut self) {
        self.remove_deliberately_absent_globals();
        // A definition can only fail on a frozen global, which this is not; if
        // it did, the ordering suites fail loudly because the functions are
        // missing, which is the right place to find out.
        let _ = runtime::install(&mut self.context, self.id);
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

    /// Why script was stopped, if it was.
    pub fn terminated(&self) -> Option<TerminateReason> {
        self.shared.core.borrow().terminated()
    }

    /// Run `source` as a classic script, then a microtask checkpoint, and report
    /// its completion.
    ///
    /// A thrown value is data, never a Rust error and never a panic: this is
    /// the boundary hostile script text crosses. Once the host is terminated
    /// nothing is evaluated at all.
    pub fn eval(&mut self, source: &str) -> Outcome {
        if let Some(reason) = self.terminated() {
            return Outcome::Terminated(reason);
        }
        let result = self.context.eval(Source::from_bytes(source));
        // A job queued by this script runs now, before anything else: the
        // microtask checkpoint after a script (HTML "clean up after running
        // script").
        self.shared.checkpoint(&mut self.context);
        if let Some(reason) = self.terminated() {
            return Outcome::Terminated(reason);
        }
        match result {
            Ok(value) => Outcome::Value(self.describe(&value)),
            Err(error) => Outcome::Error(self.describe_error(error)),
        }
    }

    /// One turn of the loop: promote due timers, run the next task and the
    /// checkpoint after it, or say what the loop is waiting for.
    pub fn step(&mut self) -> Step {
        if let Some(reason) = self.terminated() {
            return Step::Terminated(reason);
        }
        if let Some(Task::Timer(id)) = runtime::next_task(&self.shared) {
            self.shared.run_timer_task(id, &mut self.context);
            return match self.terminated() {
                Some(reason) => Step::Terminated(reason),
                None => Step::Ran,
            };
        }
        let now = self.shared.now_ns();
        let due = self.shared.core.borrow_mut().next_due_ns();
        match due {
            Some(due) => Step::Waiting(Duration::from_nanos(due.saturating_sub(now))),
            None => Step::Idle,
        }
    }

    /// Run the loop until it is idle, terminated, or `max_tasks` tasks have run,
    /// waiting on the clock for timers that are not yet due. `max_tasks` is the
    /// caller's bound on a page that never goes idle (a self-arming timer).
    pub fn run_loop(&mut self, max_tasks: usize) -> LoopEnd {
        let mut ran = 0usize;
        loop {
            if ran >= max_tasks {
                return LoopEnd::TaskLimit { ran };
            }
            match self.step() {
                Step::Ran => ran += 1,
                Step::Waiting(wait) => {
                    let target = self
                        .shared
                        .now_ns()
                        .saturating_add(u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX));
                    self.shared.clock.wait_until_ns(target);
                }
                Step::Idle => return LoopEnd::Idle { ran },
                Step::Terminated(reason) => return LoopEnd::Terminated { ran, reason },
            }
        }
    }

    /// How many uncaught errors jobs and timer callbacks have thrown.
    pub fn uncaught_error_count(&self) -> u64 {
        self.shared.uncaught_count()
    }

    /// The first few uncaught error messages (at most
    /// [`runtime::MAX_RECORDED_ERRORS`]).
    pub fn uncaught_errors(&self) -> Vec<String> {
        self.shared.uncaught()
    }

    /// Jobs the engine queued that this host never runs (async, timeout and
    /// finalization jobs).
    pub fn dropped_job_count(&self) -> u64 {
        self.shared.dropped_jobs()
    }

    /// Testing only: the largest depth each bounded quantity reached.
    #[cfg(feature = "testing")]
    pub fn high_water(&self) -> HighWater {
        self.shared.core.borrow().high_water()
    }

    /// The bounds this host enforces.
    pub fn caps(&self) -> Caps {
        self.shared.core.borrow().caps()
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

/// Why [`Host::run_loop`] returned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoopEnd {
    /// No task and no timer remain.
    Idle {
        /// Tasks run.
        ran: usize,
    },
    /// The caller's task limit was reached with work remaining.
    TaskLimit {
        /// Tasks run.
        ran: usize,
    },
    /// A bound was hit: script is stopped for good.
    Terminated {
        /// Tasks run before it.
        ran: usize,
        /// Which bound.
        reason: TerminateReason,
    },
}
