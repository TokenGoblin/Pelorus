//! The event loop's queues, as pure Rust with no engine in it (ADR 040,
//! "The event loop and queues").
//!
//! [`LoopCore`] holds the microtask queue, the task queue and the timer table,
//! the bounds on each, the nested-timer clamp and the `terminated` state. It is
//! generic over what a microtask (`M`) and a timer (`P`) carry, so every rule
//! here is unit-tested with integers.
//!
//! # Overflow is a host policy
//!
//! Boa's `enqueue_job` returns `()`: the engine cannot be told "full". So when
//! a bound is hit the core does what ADR 040 says and nothing less: it drops
//! the job, sets `terminated`, discards every queue, and from then on refuses to
//! hand out any work. The first reason wins and is kept. It never resumes.
//!
//! # Two bounds, because one is not enough
//!
//! `function f(){Promise.resolve().then(f)} f()` keeps the microtask queue at
//! depth 1 forever and never yields, so a bound on *size* never fires. The
//! per-checkpoint *budget* on how many microtasks one checkpoint may run is the
//! second check, and the one that catches it.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

/// A timer's handle, as script sees it. Never zero, never reused.
pub type TimerId = u64;

/// Nesting level above which the minimum delay applies (HTML timer
/// initialisation steps).
pub const NESTING_CLAMP_LEVEL: u32 = 5;

/// The minimum delay once nested deeper than [`NESTING_CLAMP_LEVEL`].
pub const NESTED_MIN_DELAY_MS: u64 = 4;

/// Largest delay a timer may ask for: a 32-bit signed value of milliseconds.
pub const MAX_DELAY_MS: u64 = i32::MAX as u64;

const NS_PER_MS: u64 = 1_000_000;

/// The bounds. Starting values from ADR 040, to be tuned by measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    /// Macrotask queue entries.
    pub tasks: usize,
    /// Microtask queue entries.
    pub microtasks: usize,
    /// Live timers (pending, queued or running).
    pub timers: usize,
    /// Microtasks one checkpoint may run.
    pub checkpoint_budget: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Self {
            tasks: 4096,
            microtasks: 65_536,
            timers: 10_000,
            checkpoint_budget: 1_000_000,
        }
    }
}

/// Why script was stopped for good. Mirrors `px_ipc::script::TerminateReason`
/// (this crate does not depend on the wire crate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminateReason {
    /// The macrotask queue was full.
    TaskQueueOverflow,
    /// The microtask queue was full.
    MicrotaskQueueOverflow,
    /// More live timers than [`Caps::timers`].
    TimerOverflow,
    /// One checkpoint ran more than [`Caps::checkpoint_budget`] microtasks.
    MicrotaskBudget,
}

/// A macrotask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Task {
    /// Run this timer's callback.
    Timer(TimerId),
}

/// High-water marks: the largest value each bounded quantity ever took.
/// Testing builds only (build-spec 14.4); a release build has no counters and
/// `cargo xtask sandbox` scans for the marker below.
#[cfg(feature = "testing")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HighWater {
    /// Task queue depth.
    pub tasks: usize,
    /// Microtask queue depth.
    pub microtasks: usize,
    /// Live timers.
    pub timers: usize,
    /// Microtasks run by the busiest checkpoint.
    pub checkpoint: usize,
}

/// Present in a binary only if the testing counters were compiled into it.
#[cfg(feature = "testing")]
pub const COUNTERS_MARKER: &str = "px-script-testing-high-water-counters";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// In the heap, waiting for `due`; `seq` identifies this arming.
    Pending { due: u64, seq: u64 },
    /// Due, and in the task queue.
    Queued,
    /// Its callback is running (repeating timers only; a one-shot is removed
    /// when it starts).
    Running,
}

#[derive(Debug)]
struct Entry<P> {
    payload: P,
    /// `Some` for an interval.
    repeat_ms: Option<u64>,
    nesting: u32,
    state: State,
}

/// What a timer task hands the host to run.
#[derive(Debug, PartialEq, Eq)]
pub struct Fired<P> {
    /// The callback and its arguments.
    pub payload: P,
    /// Its nesting level; script it runs sees new timers at this plus one.
    pub nesting: u32,
}

/// The queues, the timer table and the termination state.
#[derive(Debug)]
pub struct LoopCore<M, P> {
    caps: Caps,
    microtasks: VecDeque<M>,
    tasks: VecDeque<Task>,
    timers: HashMap<TimerId, Entry<P>>,
    /// `(due ns, arming sequence, id)`, earliest first. May hold stale entries
    /// (cleared or re-armed timers); an entry is live only if the table's state
    /// for that id is `Pending` with the same `due` and `seq`.
    heap: BinaryHeap<Reverse<(u64, u64, TimerId)>>,
    next_id: TimerId,
    next_seq: u64,
    /// Nesting level of the timer task running now; zero outside one.
    nesting: u32,
    checkpoint_run: usize,
    terminated: Option<TerminateReason>,
    #[cfg(feature = "testing")]
    marks: HighWater,
}

impl<M, P: Clone> LoopCore<M, P> {
    /// An empty loop with these bounds.
    pub fn new(caps: Caps) -> Self {
        Self {
            caps,
            microtasks: VecDeque::new(),
            tasks: VecDeque::new(),
            timers: HashMap::new(),
            heap: BinaryHeap::new(),
            next_id: 1,
            next_seq: 0,
            nesting: 0,
            checkpoint_run: 0,
            terminated: None,
            #[cfg(feature = "testing")]
            marks: HighWater::default(),
        }
    }

    /// The bounds in force.
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// The reason script was stopped, if it was.
    pub fn terminated(&self) -> Option<TerminateReason> {
        self.terminated
    }

    /// High-water marks so far.
    #[cfg(feature = "testing")]
    pub fn high_water(&self) -> HighWater {
        self.marks
    }

    /// Stop for good. Keeps the first reason, discards everything queued, and
    /// frees the callbacks. Idempotent.
    pub fn terminate(&mut self, reason: TerminateReason) {
        if self.terminated.is_some() {
            return;
        }
        self.terminated = Some(reason);
        self.microtasks.clear();
        self.tasks.clear();
        self.timers.clear();
        self.heap.clear();
    }

    /// Free everything queued without changing the state: for shutdown, so the
    /// callbacks are released while the engine still exists.
    pub fn discard(&mut self) {
        self.microtasks.clear();
        self.tasks.clear();
        self.timers.clear();
        self.heap.clear();
    }

    // ---- microtasks -------------------------------------------------------

    /// Queue a microtask. `false` means it was dropped: the loop is terminated,
    /// or this very push overflowed the queue and terminated it.
    pub fn push_microtask(&mut self, job: M) -> bool {
        if self.terminated.is_some() {
            return false;
        }
        if self.microtasks.len() >= self.caps.microtasks {
            self.terminate(TerminateReason::MicrotaskQueueOverflow);
            return false;
        }
        self.microtasks.push_back(job);
        #[cfg(feature = "testing")]
        {
            self.marks.microtasks = self.marks.microtasks.max(self.microtasks.len());
        }
        true
    }

    /// Microtasks waiting.
    pub fn microtask_len(&self) -> usize {
        self.microtasks.len()
    }

    /// Start a microtask checkpoint: the budget counts from here.
    pub fn begin_checkpoint(&mut self) {
        self.checkpoint_run = 0;
    }

    /// The next microtask of this checkpoint, FIFO. `None` when the queue is
    /// empty, when the loop is terminated, or when the budget is spent with
    /// work still queued (which terminates the loop).
    pub fn pop_microtask(&mut self) -> Option<M> {
        if self.terminated.is_some() || self.microtasks.is_empty() {
            return None;
        }
        if self.checkpoint_run >= self.caps.checkpoint_budget {
            self.terminate(TerminateReason::MicrotaskBudget);
            return None;
        }
        self.checkpoint_run += 1;
        #[cfg(feature = "testing")]
        {
            self.marks.checkpoint = self.marks.checkpoint.max(self.checkpoint_run);
        }
        self.microtasks.pop_front()
    }

    // ---- timers -----------------------------------------------------------

    /// Arm a timer `delay_ms` after `now_ns`, repeating if `repeat`. Returns
    /// its id, or `None` when the loop is terminated or this call hit the timer
    /// cap (which terminates it).
    ///
    /// The nesting rule is HTML's: a timer made while a timer task runs has that
    /// task's nesting plus one, and past [`NESTING_CLAMP_LEVEL`] its delay is at
    /// least [`NESTED_MIN_DELAY_MS`]. That is what makes `setTimeout(f, 0)`
    /// re-arming itself unable to spin at full speed.
    pub fn add_timer(
        &mut self,
        now_ns: u64,
        delay_ms: u64,
        repeat: bool,
        payload: P,
    ) -> Option<TimerId> {
        if self.terminated.is_some() {
            return None;
        }
        if self.timers.len() >= self.caps.timers {
            self.terminate(TerminateReason::TimerOverflow);
            return None;
        }
        let nesting = self.nesting.saturating_add(1);
        let delay = effective_delay_ms(delay_ms, nesting);
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        let due = now_ns.saturating_add(delay.saturating_mul(NS_PER_MS));
        let seq = self.take_seq();
        self.timers.insert(
            id,
            Entry {
                payload,
                repeat_ms: repeat.then_some(delay_ms.min(MAX_DELAY_MS)),
                nesting,
                state: State::Pending { due, seq },
            },
        );
        self.heap.push(Reverse((due, seq, id)));
        #[cfg(feature = "testing")]
        {
            self.marks.timers = self.marks.timers.max(self.timers.len());
        }
        Some(id)
    }

    /// Cancel a timer, whatever state it is in. A task already queued for it
    /// finds it gone and does nothing. Returns whether it existed.
    pub fn clear_timer(&mut self, id: TimerId) -> bool {
        let existed = self.timers.remove(&id).is_some();
        // Cancelling in a loop (`setTimeout` then `clearTimeout`, forever) must
        // not grow the heap: rebuild when stale entries dominate.
        if self.heap.len() > self.timers.len().saturating_mul(2).saturating_add(1024) {
            self.compact_heap();
        }
        existed
    }

    /// Live timers (pending, queued or running).
    pub fn timer_len(&self) -> usize {
        self.timers.len()
    }

    /// Entries in the heap, stale ones included. For the compaction test.
    pub fn heap_len(&self) -> usize {
        self.heap.len()
    }

    /// Due time of the earliest pending timer.
    pub fn next_due_ns(&mut self) -> Option<u64> {
        loop {
            let &Reverse((due, seq, id)) = self.heap.peek()?;
            if self.is_live_arming(id, due, seq) {
                return Some(due);
            }
            self.heap.pop();
        }
    }

    /// Move every timer due at `now_ns` into the task queue, earliest due time
    /// first and by arming order among equals, while the queue has room. A
    /// timer that does not fit stays pending (back-pressure, not loss): a page
    /// that arms 5,000 zero-delay timers is slow, not wrong.
    pub fn promote_due(&mut self, now_ns: u64) {
        while self.terminated.is_none() && self.tasks.len() < self.caps.tasks {
            let Some(&Reverse((due, seq, id))) = self.heap.peek() else {
                return;
            };
            if !self.is_live_arming(id, due, seq) {
                self.heap.pop();
                continue;
            }
            if due > now_ns {
                return;
            }
            self.heap.pop();
            if let Some(entry) = self.timers.get_mut(&id) {
                entry.state = State::Queued;
            }
            self.enqueue_task(Task::Timer(id));
        }
    }

    // ---- tasks ------------------------------------------------------------

    /// Queue a macrotask. `false` means it was dropped: the loop is
    /// terminated, or this push overflowed the queue and terminated it. The
    /// entry point for task sources added later (Phase 11).
    pub fn enqueue_task(&mut self, task: Task) -> bool {
        if self.terminated.is_some() {
            return false;
        }
        if self.tasks.len() >= self.caps.tasks {
            self.terminate(TerminateReason::TaskQueueOverflow);
            return false;
        }
        self.tasks.push_back(task);
        #[cfg(feature = "testing")]
        {
            self.marks.tasks = self.marks.tasks.max(self.tasks.len());
        }
        true
    }

    /// Macrotasks waiting.
    pub fn task_len(&self) -> usize {
        self.tasks.len()
    }

    /// The next macrotask, FIFO. `None` once terminated.
    pub fn pop_task(&mut self) -> Option<Task> {
        if self.terminated.is_some() {
            return None;
        }
        self.tasks.pop_front()
    }

    /// Begin a timer task. `None` if the timer was cleared after it was queued
    /// (the case `clearTimeout` inside another callback creates). A one-shot is
    /// removed here; an interval stays, marked running, so that
    /// `clearInterval` from inside its own callback cancels the next run.
    ///
    /// Sets the nesting level that timers armed by the callback inherit; pair
    /// with [`Self::finish_timer_task`].
    pub fn begin_timer_task(&mut self, id: TimerId) -> Option<Fired<P>> {
        if self.terminated.is_some() {
            return None;
        }
        let entry = self.timers.get_mut(&id)?;
        if entry.state != State::Queued {
            return None;
        }
        let nesting = entry.nesting;
        self.nesting = nesting;
        if entry.repeat_ms.is_some() {
            entry.state = State::Running;
            Some(Fired {
                payload: entry.payload.clone(),
                nesting,
            })
        } else {
            let entry = self.timers.remove(&id)?;
            Some(Fired {
                payload: entry.payload,
                nesting,
            })
        }
    }

    /// End a timer task: nesting returns to zero, and an interval that its own
    /// callback did not cancel is armed again, `repeat` after `now_ns` (the time
    /// the callback finished, so a slow callback cannot make a burst).
    pub fn finish_timer_task(&mut self, id: TimerId, now_ns: u64) {
        self.nesting = 0;
        if self.terminated.is_some() {
            return;
        }
        let seq = self.take_seq();
        let Some(entry) = self.timers.get_mut(&id) else {
            return;
        };
        if entry.state != State::Running {
            return;
        }
        let Some(repeat_ms) = entry.repeat_ms else {
            return;
        };
        entry.nesting = entry.nesting.saturating_add(1);
        let delay = effective_delay_ms(repeat_ms, entry.nesting);
        let due = now_ns.saturating_add(delay.saturating_mul(NS_PER_MS));
        entry.state = State::Pending { due, seq };
        self.heap.push(Reverse((due, seq, id)));
    }

    // ---- internals --------------------------------------------------------

    fn take_seq(&mut self) -> u64 {
        let seq = self.next_seq;
        self.next_seq = self.next_seq.saturating_add(1);
        seq
    }

    fn is_live_arming(&self, id: TimerId, due: u64, seq: u64) -> bool {
        matches!(
            self.timers.get(&id).map(|e| e.state),
            Some(State::Pending { due: d, seq: s }) if d == due && s == seq
        )
    }

    fn compact_heap(&mut self) {
        let live: Vec<Reverse<(u64, u64, TimerId)>> = self
            .timers
            .iter()
            .filter_map(|(id, e)| match e.state {
                State::Pending { due, seq } => Some(Reverse((due, seq, *id))),
                State::Queued | State::Running => None,
            })
            .collect();
        self.heap = BinaryHeap::from(live);
    }
}

/// The delay a timer actually gets: clamped to the maximum, and to the nested
/// minimum when `nesting` is past the clamp level.
pub fn effective_delay_ms(requested_ms: u64, nesting: u32) -> u64 {
    let delay = requested_ms.min(MAX_DELAY_MS);
    if nesting > NESTING_CLAMP_LEVEL {
        delay.max(NESTED_MIN_DELAY_MS)
    } else {
        delay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Core = LoopCore<u32, &'static str>;

    fn core() -> Core {
        LoopCore::new(Caps::default())
    }

    fn small(tasks: usize, microtasks: usize, timers: usize, budget: usize) -> Core {
        LoopCore::new(Caps {
            tasks,
            microtasks,
            timers,
            checkpoint_budget: budget,
        })
    }

    /// Fire every timer due at `now` in order, returning their payloads.
    fn fire_all(c: &mut Core, now: u64) -> Vec<&'static str> {
        c.promote_due(now);
        let mut out = Vec::new();
        while let Some(Task::Timer(id)) = c.pop_task() {
            if let Some(f) = c.begin_timer_task(id) {
                out.push(f.payload);
                c.finish_timer_task(id, now);
            }
        }
        out
    }

    #[test]
    fn script_queue_pure_delay_clamps_only_past_nesting_five() {
        // Hand-derived from HTML timer initialisation: nesting > 5 and delay < 4
        // gives 4; nesting 5 is not past the level.
        for nesting in 1..=5 {
            assert_eq!(effective_delay_ms(0, nesting), 0);
        }
        assert_eq!(effective_delay_ms(0, 6), 4);
        assert_eq!(effective_delay_ms(3, 6), 4);
        assert_eq!(effective_delay_ms(4, 6), 4);
        assert_eq!(effective_delay_ms(5, 6), 5);
        assert_eq!(effective_delay_ms(u64::MAX, 1), MAX_DELAY_MS);
    }

    #[test]
    fn script_queue_pure_microtasks_are_fifo() {
        let mut c = core();
        for n in [3, 1, 2] {
            assert!(c.push_microtask(n));
        }
        c.begin_checkpoint();
        let drained: Vec<u32> = std::iter::from_fn(|| c.pop_microtask()).collect();
        assert_eq!(drained, [3, 1, 2]);
        assert_eq!(c.pop_microtask(), None);
    }

    #[test]
    fn script_queue_pure_the_microtask_cap_drops_and_terminates() {
        let mut c = small(8, 3, 8, 100);
        assert!(c.push_microtask(1));
        assert!(c.push_microtask(2));
        assert!(c.push_microtask(3));
        // The fourth does not fit: dropped, and the loop is finished.
        assert!(!c.push_microtask(4));
        assert_eq!(
            c.terminated(),
            Some(TerminateReason::MicrotaskQueueOverflow)
        );
        assert_eq!(c.microtask_len(), 0, "the queue was discarded");
        c.begin_checkpoint();
        assert_eq!(c.pop_microtask(), None);
        // Nothing enqueued after termination can ever run.
        assert!(!c.push_microtask(5));
        assert_eq!(c.microtask_len(), 0);
        assert_eq!(c.pop_microtask(), None);
    }

    #[test]
    fn script_queue_pure_the_budget_catches_what_the_size_bound_cannot() {
        // A microtask that requeues itself keeps the queue at depth one forever.
        let mut c = small(8, 8, 8, 5);
        assert!(c.push_microtask(0));
        c.begin_checkpoint();
        let mut ran = 0;
        while let Some(job) = c.pop_microtask() {
            ran += 1;
            assert!(c.push_microtask(job + 1));
        }
        assert_eq!(ran, 5, "exactly the budget ran");
        assert_eq!(c.terminated(), Some(TerminateReason::MicrotaskBudget));
        assert_eq!(c.microtask_len(), 0);
        #[cfg(feature = "testing")]
        {
            assert_eq!(c.high_water().microtasks, 1);
            assert_eq!(c.high_water().checkpoint, 5);
        }
    }

    #[test]
    fn script_queue_pure_the_budget_resets_at_each_checkpoint() {
        let mut c = small(8, 8, 8, 3);
        for round in 0..4 {
            for n in 0..3 {
                assert!(c.push_microtask(round * 10 + n));
            }
            c.begin_checkpoint();
            let ran = std::iter::from_fn(|| c.pop_microtask()).count();
            assert_eq!(ran, 3);
        }
        assert_eq!(c.terminated(), None, "exactly the budget is not over it");
    }

    #[test]
    fn script_queue_pure_two_timers_run_by_due_time_then_insertion() {
        let mut c = core();
        let ms = 1_000_000;
        c.add_timer(0, 10, false, "late");
        c.add_timer(0, 5, false, "early-first");
        c.add_timer(0, 5, false, "early-second");
        assert_eq!(c.next_due_ns(), Some(5 * ms));
        assert_eq!(fire_all(&mut c, 4 * ms), Vec::<&str>::new());
        assert_eq!(
            fire_all(&mut c, 10 * ms),
            ["early-first", "early-second", "late"]
        );
        assert_eq!(c.timer_len(), 0);
        assert_eq!(c.next_due_ns(), None);
    }

    #[test]
    fn script_queue_pure_a_cleared_timer_never_runs_even_when_already_queued() {
        let mut c = core();
        let a = c.add_timer(0, 1, false, "a").expect("a");
        let b = c.add_timer(0, 1, false, "b").expect("b");
        c.promote_due(10_000_000);
        assert_eq!(c.task_len(), 2);
        // a's callback clears b.
        assert!(matches!(c.pop_task(), Some(Task::Timer(id)) if id == a));
        assert!(c.begin_timer_task(a).is_some());
        assert!(c.clear_timer(b));
        c.finish_timer_task(a, 10_000_000);
        // b's task is still queued, and finds nothing to run.
        assert!(matches!(c.pop_task(), Some(Task::Timer(id)) if id == b));
        assert!(c.begin_timer_task(b).is_none());
        assert!(!c.clear_timer(b), "clearing twice is harmless");
    }

    #[test]
    fn script_queue_pure_an_interval_cleared_in_its_own_callback_is_not_rearmed() {
        let mut c = core();
        let ms = 1_000_000;
        let id = c.add_timer(0, 10, true, "tick").expect("tick");
        c.promote_due(10 * ms);
        let Some(Task::Timer(t)) = c.pop_task() else {
            panic!("queued");
        };
        assert!(c.begin_timer_task(t).is_some());
        assert!(c.clear_timer(id));
        c.finish_timer_task(t, 10 * ms);
        assert_eq!(c.next_due_ns(), None);
        assert_eq!(c.timer_len(), 0);
    }

    #[test]
    fn script_queue_pure_an_interval_rearms_after_the_callback_by_its_period() {
        let mut c = core();
        let ms = 1_000_000;
        c.add_timer(0, 10, true, "tick");
        // Fires at 10 ms, finishes at 12 ms: next is 22 ms, not 20.
        c.promote_due(10 * ms);
        let Some(Task::Timer(t)) = c.pop_task() else {
            panic!("queued");
        };
        assert!(c.begin_timer_task(t).is_some());
        c.finish_timer_task(t, 12 * ms);
        assert_eq!(c.next_due_ns(), Some(22 * ms));
    }

    #[test]
    fn script_queue_pure_nested_zero_delay_timers_clamp_to_four_ms_from_the_sixth() {
        let mut c = core();
        let ms = 1_000_000;
        let mut now = 0;
        let mut gaps = Vec::new();
        c.add_timer(now, 0, false, "t");
        for _ in 0..8 {
            let due = c.next_due_ns().expect("armed");
            gaps.push((due - now) / ms);
            now = due;
            c.promote_due(now);
            let Some(Task::Timer(id)) = c.pop_task() else {
                panic!("due");
            };
            // The callback runs; it re-arms itself with delay 0.
            assert!(c.begin_timer_task(id).is_some());
            c.add_timer(now, 0, false, "t");
            c.finish_timer_task(id, now);
        }
        // Timer n is armed at nesting n: delays 0 for n = 1..=5, then 4.
        assert_eq!(gaps, [0, 0, 0, 0, 0, 4, 4, 4]);
    }

    #[test]
    fn script_queue_pure_a_top_level_timer_resets_nesting() {
        let mut c = core();
        c.add_timer(0, 0, false, "a");
        c.promote_due(0);
        let Some(Task::Timer(id)) = c.pop_task() else {
            panic!("queued");
        };
        assert!(c.begin_timer_task(id).is_some());
        c.finish_timer_task(id, 0);
        // Outside any timer task, a new timer starts again at nesting one.
        let fresh = c.add_timer(0, 0, false, "b").expect("b");
        assert_eq!(c.timers.get(&fresh).map(|e| e.nesting), Some(1));
    }

    #[test]
    fn script_queue_pure_the_timer_cap_terminates_and_frees_everything() {
        let mut c = small(8, 8, 3, 100);
        for _ in 0..3 {
            assert!(c.add_timer(0, 1, true, "i").is_some());
        }
        assert_eq!(c.add_timer(0, 1, true, "i"), None);
        assert_eq!(c.terminated(), Some(TerminateReason::TimerOverflow));
        assert_eq!(c.timer_len(), 0);
        assert_eq!(c.next_due_ns(), None);
        assert_eq!(c.add_timer(0, 1, false, "late"), None);
        #[cfg(feature = "testing")]
        assert_eq!(c.high_water().timers, 3);
    }

    #[test]
    fn script_queue_pure_a_full_task_queue_holds_timers_back_rather_than_losing_them() {
        let mut c = small(2, 8, 8, 100);
        for _ in 0..5 {
            c.add_timer(0, 0, false, "t");
        }
        c.promote_due(1);
        assert_eq!(c.task_len(), 2, "the queue is at its cap");
        assert_eq!(c.terminated(), None, "back-pressure is not overflow");
        assert_eq!(c.timer_len(), 5);
        // Drain two, promote again: two more.
        while let Some(Task::Timer(id)) = c.pop_task() {
            let _ = c.begin_timer_task(id);
            c.finish_timer_task(id, 1);
        }
        c.promote_due(1);
        assert_eq!(c.task_len(), 2);
        #[cfg(feature = "testing")]
        assert_eq!(c.high_water().tasks, 2);
    }

    #[test]
    fn script_queue_pure_an_external_task_over_the_cap_drops_and_terminates() {
        let mut c = small(2, 8, 8, 100);
        assert!(c.enqueue_task(Task::Timer(1)));
        assert!(c.enqueue_task(Task::Timer(2)));
        assert!(!c.enqueue_task(Task::Timer(3)));
        assert_eq!(c.terminated(), Some(TerminateReason::TaskQueueOverflow));
        assert_eq!(c.pop_task(), None);
        assert!(!c.enqueue_task(Task::Timer(4)));
    }

    #[test]
    fn script_queue_pure_the_first_termination_reason_wins() {
        let mut c = small(8, 1, 1, 100);
        assert!(c.push_microtask(1));
        assert!(!c.push_microtask(2));
        c.terminate(TerminateReason::TimerOverflow);
        assert_eq!(
            c.terminated(),
            Some(TerminateReason::MicrotaskQueueOverflow)
        );
    }

    #[test]
    fn script_queue_pure_nothing_runs_after_termination() {
        let mut c = core();
        let id = c.add_timer(0, 0, false, "t").expect("t");
        c.promote_due(1);
        assert!(c.push_microtask(7));
        c.terminate(TerminateReason::MicrotaskBudget);
        assert_eq!(c.pop_task(), None);
        assert!(c.begin_timer_task(id).is_none());
        c.begin_checkpoint();
        assert_eq!(c.pop_microtask(), None);
        c.promote_due(u64::MAX);
        assert_eq!(c.task_len(), 0);
        assert_eq!(c.next_due_ns(), None);
    }

    #[test]
    fn script_queue_pure_set_then_clear_forever_does_not_grow_the_heap() {
        let mut c = core();
        for i in 0..100_000u64 {
            let id = c.add_timer(i, 1_000, false, "t").expect("armed");
            assert!(c.clear_timer(id));
        }
        assert_eq!(c.timer_len(), 0);
        assert!(
            c.heap_len() <= 1024 + 2,
            "heap grew to {} entries for zero live timers",
            c.heap_len()
        );
    }

    /// The release scan (`cargo xtask sandbox`) looks for this exact text. If
    /// the two drift apart the scan passes on a binary that carries the
    /// counters, which is the failure it exists to catch.
    #[test]
    fn script_queue_pure_the_counters_marker_is_the_one_the_release_scan_looks_for() {
        #[cfg(feature = "testing")]
        {
            let scan = include_str!("../../px-xtask/src/gates/sandbox.rs");
            let wanted = format!("SCRIPT_COUNTERS_MARKER: &str = \"{COUNTERS_MARKER}\";");
            assert!(
                scan.contains(&wanted),
                "sandbox.rs does not scan for {COUNTERS_MARKER}"
            );
        }
    }

    #[test]
    fn script_queue_pure_ids_are_nonzero_and_never_reused() {
        let mut c = core();
        let a = c.add_timer(0, 1, false, "a").expect("a");
        assert!(c.clear_timer(a));
        let b = c.add_timer(0, 1, false, "b").expect("b");
        assert!(a >= 1 && b > a);
    }

    #[test]
    fn script_queue_pure_a_delay_is_clamped_to_the_32_bit_maximum() {
        let mut c = core();
        let id = c.add_timer(0, u64::MAX, false, "far").expect("far");
        let _ = id;
        assert_eq!(c.next_due_ns(), Some(MAX_DELAY_MS * 1_000_000));
    }
}
