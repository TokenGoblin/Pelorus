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
pub mod watch;

#[cfg(feature = "testing")]
pub use runtime::MAX_CLOCK_LOG;

use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use boa_engine::context::HostHooks;
use boa_engine::module::IdleModuleLoader;
use boa_engine::{Context, JsError, JsValue, Source, js_string};

use clock::{RawClock, StdClock};
use queue::{Caps, Task};
use runtime::Shared;

pub use queue::TerminateReason;
#[cfg(feature = "testing")]
pub use queue::{COUNTERS_MARKER, HighWater};
pub use watch::Heartbeat;

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

/// The engine-level limits on a runaway script: layer 1 of ADR 040.
///
/// Boa 0.22.0 has no interrupt, no deadline and no heap cap, so these are
/// everything the engine itself can do, and they are not enough alone:
///
/// * the loop count is **per call frame** (a loop of calls to looping
///   functions is not bounded by it),
/// * native builtins (`/(a+)+$/`, `'x'.repeat(2**30)`, a huge `sort`) are not
///   interrupted at all, and
/// * nothing here caps the heap.
///
/// The content process's watchdog (layer 2) ends the first two; the third
/// waits for Phase 17. A limit error is terminal: see [`Host::eval`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Iterations one call frame's loops may run before the engine raises a
    /// limit error. ADR 040: 10,000,000.
    pub loop_iterations: u64,
    /// Nested call depth. ADR 040: 256.
    pub recursion: usize,
    /// The VM value-stack size, in values. Boa's own default (10,240) is kept:
    /// at the recursion depth above it is not the binding limit for ordinary
    /// code, and it caps a function with a very large number of locals.
    pub stack_size: usize,
    /// The largest `ArrayBuffer` (and so typed array) one allocation may ask
    /// for, in bytes. A request over it is an ordinary, catchable `RangeError`
    /// to the script. ADR 040: 256 MiB.
    pub max_buffer_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            loop_iterations: 10_000_000,
            recursion: 256,
            stack_size: 10 * 1024,
            max_buffer_bytes: 256 * 1024 * 1024,
        }
    }
}

/// The host hooks: only the buffer cap differs from the engine's.
struct Hooks {
    max_buffer_bytes: u64,
}

impl HostHooks for Hooks {
    fn max_buffer_size(&self, _context: &mut Context) -> u64 {
        self.max_buffer_bytes
    }
}

/// How a host is built.
///
/// There is no `Default`: the seed comes from the broker, and a host built with
/// a seed nobody chose would have jitter anyone could predict.
pub struct HostOptions {
    /// The bounds.
    pub caps: Caps,
    /// Where time comes from.
    pub clock: Rc<dyn RawClock>,
    /// Entropy for the clock's jitter and `Math.random`, from the broker.
    pub seed: u64,
    /// The engine's runtime limits (layer 1 of ADR 040).
    pub limits: Limits,
    /// Unix milliseconds at the moment this host began, from the embedder (this
    /// crate reads no wall clock). `Date.now()` is this plus coarse elapsed time.
    pub wall_epoch_ms: i64,
}

impl HostOptions {
    /// The default bounds, the real monotonic clock, and this seed. The wall
    /// epoch is zero until the embedder sets it.
    pub fn new(seed: u64) -> Self {
        Self {
            caps: Caps::default(),
            clock: Rc::new(StdClock::new()),
            limits: Limits::default(),
            seed,
            wall_epoch_ms: 0,
        }
    }
}

/// The script host of one content process.
pub struct Host {
    // Declared first so it drops first: the callbacks and promise jobs it holds
    // are collected objects, and must be released while the engine exists.
    shared: Rc<Shared>,
    id: u64,
    limits: Limits,
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
    /// A host with the default bounds, the real clock and this seed.
    pub fn new(seed: u64) -> Result<Self, HostError> {
        Self::with_options(HostOptions::new(seed))
    }

    /// A host with these bounds, this clock and this seed.
    pub fn with_options(options: HostOptions) -> Result<Self, HostError> {
        let shared = Rc::new(Shared::new(options.caps, options.clock, options.seed));
        let id = runtime::register(Rc::clone(&shared));
        let built = Context::builder()
            // `Atomics.wait` would block the thread; no script may.
            .can_block(false)
            .job_executor(Rc::clone(&shared))
            .host_hooks(Rc::new(Hooks {
                max_buffer_bytes: options.limits.max_buffer_bytes,
            }))
            // Every time the engine reads is the coarse clock.
            .clock(Rc::new(runtime::BoaClock {
                coarse: Rc::clone(&shared.coarse),
                wall_epoch_ms: options.wall_epoch_ms,
            }))
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
            limits: options.limits,
            context,
        };
        host.apply_limits(options.limits);
        // Fail closed: a host whose `Math.random` is still the unseeded engine
        // one, or whose `SharedArrayBuffer` is still defined, does not exist.
        // (`host` is dropped on the way out, which unregisters it.)
        host.install_globals()?;
        Ok(host)
    }

    fn apply_limits(&mut self, limits: Limits) {
        let runtime = self.context.runtime_limits_mut();
        runtime.set_loop_iteration_limit(limits.loop_iterations);
        runtime.set_recursion_limit(limits.recursion);
        runtime.set_stack_size_limit(limits.stack_size);
        self.limits = limits;
    }

    fn install_globals(&mut self) -> Result<(), HostError> {
        self.remove_deliberately_absent_globals()?;
        runtime::install(&mut self.context, self.id).map_err(|e| HostError(e.to_string()))
    }

    /// Boa 0.22 defines `SharedArrayBuffer` and has no feature to turn it off,
    /// so the host deletes the binding (ADR 040: shared memory plus a worker is
    /// a counter that bypasses any clock coarsening, and it stays off until a
    /// page can be said to be cross-origin isolated). `Intl`, `Temporal` and
    /// `WebAssembly` are absent because their features are off, not by this
    /// function; `tests/globals.rs` asserts all four.
    fn remove_deliberately_absent_globals(&mut self) -> Result<(), HostError> {
        let global = self.context.global_object();
        match global.delete_property_or_throw(js_string!("SharedArrayBuffer"), &mut self.context) {
            Ok(true) => Ok(()),
            Ok(false) => Err(HostError("SharedArrayBuffer could not be removed".into())),
            Err(error) => Err(HostError(error.to_string())),
        }
    }

    /// The limits this host was built with.
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// The liveness signal for the process's watchdog (ADR 040, layer 2).
    pub fn heartbeat(&self) -> Arc<watch::Heartbeat> {
        Arc::clone(&self.shared.heartbeat)
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
        let heartbeat = self.heartbeat();
        let _running = heartbeat.enter();
        let result = self.context.eval(Source::from_bytes(source));
        // An engine error (a runtime limit) ends script here, before anything
        // else runs, whatever the script wrapped around it.
        if let Err(error) = &result {
            self.shared.note_error(error);
        }
        // A job queued by this script runs now, before anything else: the
        // microtask checkpoint after a script (HTML "clean up after running
        // script").
        self.shared.checkpoint(&mut self.context);
        if let Some(reason) = self.terminated() {
            return Outcome::Terminated(reason);
        }
        let outcome = match result {
            Ok(value) => Outcome::Value(self.describe(&value)),
            Err(error) => Outcome::Error(self.describe_error(error)),
        };
        // Describing runs script (`toString`): re-check.
        match self.terminated() {
            Some(reason) => Outcome::Terminated(reason),
            None => outcome,
        }
    }

    /// One turn of the loop: promote due timers, run the next task and the
    /// checkpoint after it, or say what the loop is waiting for.
    pub fn step(&mut self) -> Step {
        if let Some(reason) = self.terminated() {
            return Step::Terminated(reason);
        }
        let heartbeat = self.heartbeat();
        let _running = heartbeat.enter();
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
                    self.shared.wait_until_ns(target);
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

    /// Testing only: the nanosecond values `performance.now` returned to script,
    /// taken at the native boundary before conversion to a float, oldest first,
    /// at most [`MAX_CLOCK_LOG`] of them.
    #[cfg(feature = "testing")]
    pub fn clock_log(&self) -> Vec<u64> {
        self.shared.clock_log()
    }

    /// The bounds this host enforces.
    pub fn caps(&self) -> Caps {
        self.shared.core.borrow().caps()
    }

    /// `ToString`, or the engine's display form for what has none (a symbol).
    fn describe(&mut self, value: &JsValue) -> String {
        match value.to_string(&mut self.context) {
            Ok(text) => text.to_std_string_escaped(),
            // `ToString` runs script (a `toString` method): a limit hit there is
            // as terminal as anywhere else.
            Err(error) => {
                self.shared.note_error(&error);
                value.display().to_string()
            }
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
