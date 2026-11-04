//! The engine-facing half of the loop: our `JobExecutor`, the microtask
//! checkpoint, and the timer host functions.
//!
//! Boa gives a host no timers (`SimpleJobExecutor` and `boa_runtime` are not
//! used, ADR 040 decision 3), so `setTimeout`, `setInterval`, `clearTimeout`,
//! `clearInterval` and `queueMicrotask` are defined here over [`LoopCore`].
//!
//! # How a native function finds the host
//!
//! `NativeFunction::from_closure` is `unsafe` (a closure that captured a GC
//! value could be freed under the collector) and this crate forbids unsafe, so
//! the closures here capture only a `Copy` host number. That number keys a
//! thread-local registry of [`Shared`]. The `Context` is `!Send`, so one thread
//! owns a host for its whole life and a thread-local is the honest shape.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use boa_engine::job::{GenericJob, Job, JobExecutor, PromiseJob};
use boa_engine::native_function::NativeFunction;
use boa_engine::{Context, JsArgs, JsNativeError, JsObject, JsResult, JsValue, js_string};

use crate::clock::RawClock;
use crate::queue::{Caps, LoopCore, MAX_DELAY_MS, Task, TimerId};

/// Most uncaught-error messages kept for inspection. The count is exact; the
/// text of later ones is dropped.
pub const MAX_RECORDED_ERRORS: usize = 32;

/// A microtask: something Boa queued, or a callback from `queueMicrotask`.
#[derive(Debug)]
pub(crate) enum Microtask {
    Promise(PromiseJob),
    Generic(GenericJob),
    Callback(JsObject),
}

/// What a timer runs.
#[derive(Clone, Debug)]
pub(crate) struct TimerCall {
    pub callback: JsObject,
    pub args: Vec<JsValue>,
}

/// State shared by the host, the job executor and the host functions.
pub(crate) struct Shared {
    pub core: RefCell<LoopCore<Microtask, TimerCall>>,
    pub clock: Rc<dyn RawClock>,
    uncaught: RefCell<Vec<String>>,
    uncaught_count: Cell<u64>,
    /// Jobs Boa queued that this host does not run (async and timeout jobs).
    dropped_jobs: Cell<u64>,
}

impl Shared {
    pub fn new(caps: Caps, clock: Rc<dyn RawClock>) -> Self {
        Self {
            core: RefCell::new(LoopCore::new(caps)),
            clock,
            uncaught: RefCell::new(Vec::new()),
            uncaught_count: Cell::new(0),
            dropped_jobs: Cell::new(0),
        }
    }

    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    fn record_uncaught(&self, message: String) {
        self.uncaught_count
            .set(self.uncaught_count.get().saturating_add(1));
        let mut kept = self.uncaught.borrow_mut();
        if kept.len() < MAX_RECORDED_ERRORS {
            kept.push(message);
        }
    }

    pub fn uncaught(&self) -> Vec<String> {
        self.uncaught.borrow().clone()
    }

    pub fn uncaught_count(&self) -> u64 {
        self.uncaught_count.get()
    }

    pub fn dropped_jobs(&self) -> u64 {
        self.dropped_jobs.get()
    }

    /// One microtask checkpoint: drain the queue FIFO, under the budget. An
    /// uncaught error in a job is recorded and the next job still runs (HTML
    /// reports the exception and continues).
    pub fn checkpoint(&self, context: &mut Context) {
        self.core.borrow_mut().begin_checkpoint();
        loop {
            // The borrow ends before the job runs: the job enqueues.
            let next = self.core.borrow_mut().pop_microtask();
            let Some(job) = next else {
                return;
            };
            let outcome = match job {
                Microtask::Promise(job) => job.call(context),
                Microtask::Generic(job) => job.call(context),
                Microtask::Callback(callback) => callback.call(&JsValue::undefined(), &[], context),
            };
            if let Err(error) = outcome {
                self.record_uncaught(error.to_string());
            }
        }
    }

    /// Run one timer task's callback, then the microtask checkpoint that
    /// follows every task. `false` if the timer had been cleared.
    pub fn run_timer_task(&self, id: TimerId, context: &mut Context) -> bool {
        let fired = self.core.borrow_mut().begin_timer_task(id);
        let Some(fired) = fired else {
            return false;
        };
        let call = fired.payload;
        if let Err(error) = call
            .callback
            .call(&JsValue::undefined(), &call.args, context)
        {
            self.record_uncaught(error.to_string());
        }
        // Microtasks run after the callback and before the timer is re-armed
        // and before the next task, per HTML's "perform a microtask checkpoint"
        // at the end of the task.
        self.checkpoint(context);
        let now = self.now_ns();
        self.core.borrow_mut().finish_timer_task(id, now);
        true
    }
}

impl JobExecutor for Shared {
    fn enqueue_job(self: Rc<Self>, job: Job, _context: &mut Context) {
        // Boa cannot be told "full", so a refused job is dropped inside the
        // core, which also terminates the loop. See `queue`.
        let task = match job {
            Job::PromiseJob(job) => Microtask::Promise(job),
            Job::GenericJob(job) => Microtask::Generic(job),
            // Async futures, host timeouts and finalization cleanup need an
            // executor this host does not have: `Atomics.waitAsync` needs a
            // `SharedArrayBuffer` (absent) and module loading is idle. Counted,
            // never run.
            _ => {
                self.dropped_jobs
                    .set(self.dropped_jobs.get().saturating_add(1));
                return;
            }
        };
        self.core.borrow_mut().push_microtask(task);
    }

    fn run_jobs(self: Rc<Self>, context: &mut Context) -> JsResult<()> {
        self.checkpoint(context);
        Ok(())
    }
}

// ---- the registry ---------------------------------------------------------

thread_local! {
    static HOSTS: RefCell<HashMap<u64, Rc<Shared>>> = RefCell::new(HashMap::new());
    static NEXT_HOST: Cell<u64> = const { Cell::new(1) };
}

/// Register a host's shared state; returns the number native functions capture.
pub(crate) fn register(shared: Rc<Shared>) -> u64 {
    let id = NEXT_HOST.with(|n| {
        let id = n.get();
        n.set(id.saturating_add(1));
        id
    });
    HOSTS.with(|h| h.borrow_mut().insert(id, shared));
    id
}

/// Forget a host. Its functions, if script still holds them, then do nothing.
pub(crate) fn unregister(id: u64) {
    HOSTS.with(|h| h.borrow_mut().remove(&id));
}

fn lookup(id: u64) -> Option<Rc<Shared>> {
    HOSTS.with(|h| h.borrow().get(&id).cloned())
}

// ---- host functions -------------------------------------------------------

/// Define the timer functions on the global object.
pub(crate) fn install(context: &mut Context, host: u64) -> JsResult<()> {
    let timer = |repeat: bool| {
        // `Copy` closures only: see the module documentation.
        NativeFunction::from_copy_closure(move |_this, args, ctx| {
            set_timer(host, repeat, args, ctx)
        })
    };
    context.register_global_builtin_callable(js_string!("setTimeout"), 1, timer(false))?;
    context.register_global_builtin_callable(js_string!("setInterval"), 1, timer(true))?;
    let clear =
        NativeFunction::from_copy_closure(move |_this, args, ctx| clear_timer(host, args, ctx));
    context.register_global_builtin_callable(js_string!("clearTimeout"), 1, clear.clone())?;
    context.register_global_builtin_callable(js_string!("clearInterval"), 1, clear)?;
    context.register_global_builtin_callable(
        js_string!("queueMicrotask"),
        1,
        NativeFunction::from_copy_closure(move |_this, args, _ctx| queue_microtask(host, args)),
    )?;
    Ok(())
}

fn not_callable(name: &str) -> boa_engine::JsError {
    JsNativeError::typ()
        .with_message(format!("{name}: the callback is not a function"))
        .into()
}

/// WebIDL `long` conversion as HTML's timers use it, then HTML's clamp: NaN and
/// negatives are zero, fractions truncate, and the 32-bit ceiling applies.
fn delay_ms(value: &JsValue, context: &mut Context) -> JsResult<u64> {
    if value.is_undefined() {
        return Ok(0);
    }
    let number = value.to_number(context)?;
    if number.is_nan() || number <= 0.0 {
        return Ok(0);
    }
    if number >= MAX_DELAY_MS as f64 {
        return Ok(MAX_DELAY_MS);
    }
    Ok(number as u64)
}

fn set_timer(
    host: u64,
    repeat: bool,
    args: &[JsValue],
    context: &mut Context,
) -> JsResult<JsValue> {
    let name = if repeat { "setInterval" } else { "setTimeout" };
    let callback = args
        .get_or_undefined(0)
        .as_callable()
        .ok_or_else(|| not_callable(name))?;
    let delay = delay_ms(args.get_or_undefined(1), context)?;
    let extra = args.get(2..).unwrap_or_default().to_vec();
    let Some(shared) = lookup(host) else {
        return Ok(JsValue::from(0));
    };
    let now = shared.now_ns();
    let id = shared.core.borrow_mut().add_timer(
        now,
        delay,
        repeat,
        TimerCall {
            callback,
            args: extra,
        },
    );
    // Zero means "no timer": the loop was terminated, or this call hit the cap.
    Ok(JsValue::from(id.map_or(0.0, |id| id as f64)))
}

fn clear_timer(host: u64, args: &[JsValue], context: &mut Context) -> JsResult<JsValue> {
    let handle = args.get_or_undefined(0);
    if handle.is_undefined() || handle.is_null() {
        return Ok(JsValue::undefined());
    }
    let number = handle.to_number(context)?;
    if number.is_finite()
        && number >= 1.0
        && number < u64::MAX as f64
        && let Some(shared) = lookup(host)
    {
        shared.core.borrow_mut().clear_timer(number as u64);
    }
    Ok(JsValue::undefined())
}

fn queue_microtask(host: u64, args: &[JsValue]) -> JsResult<JsValue> {
    let callback = args
        .get_or_undefined(0)
        .as_callable()
        .ok_or_else(|| not_callable("queueMicrotask"))?;
    if let Some(shared) = lookup(host) {
        shared
            .core
            .borrow_mut()
            .push_microtask(Microtask::Callback(callback));
    }
    Ok(JsValue::undefined())
}

/// Turn the next due task (if any) into work: promote what is due, then pop.
pub(crate) fn next_task(shared: &Shared) -> Option<Task> {
    let now = shared.now_ns();
    let mut core = shared.core.borrow_mut();
    core.promote_due(now);
    core.pop_task()
}
