// The Phase 10 fixture table (docs/phase-10-plan.md, unit 6).
//
// Included, not compiled on its own, by `tests/fixtures.rs` (the in-process
// host on a manual clock) and by `px-content/tests/script_fixture_process.rs`
// (the real content process on the real clock). One table, two drivers.
//
// EVERY `expect` BELOW IS DERIVED FROM THE SPECIFICATIONS, NOT READ BACK FROM
// THE IMPLEMENTATION. The derivation is the row's `spec` text: the HTML event
// loop and timers (WHATWG HTML, "Timers" and "Event loops": microtask
// checkpoint, timer initialisation steps, "run steps after a timeout") and the
// ECMAScript job queue (ECMA-262 `HostEnqueuePromiseJob`, `PerformPromiseThen`,
// `NewPromiseResolveThenableJob`, `Await`). A row whose expectation the
// implementation fails is a finding: it stays asserting the spec.
//
// A row runs like this. `var log = [];` is evaluated, then `script`, then the
// loop is driven until `end` says it stops, then `probe` is evaluated and
// compared with `expect`. Rows with `process: true` also run through the real
// content binary; those use only delays that stay correct on a loaded machine
// (the process driver cannot freeze time).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    /// The loop runs dry: no task, no timer.
    Idle,
    /// The page never goes idle; the driver stops it after this many tasks and
    /// script must still be alive (a bound was NOT hit, by design).
    TaskLimit(usize),
    /// A bound is hit and script is stopped for good, for this reason (the
    /// `Debug` name of `TerminateReason`).
    Terminated(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct Row {
    pub name: &'static str,
    /// Where the expectation comes from, in the spec's words.
    pub spec: &'static str,
    pub script: &'static str,
    pub end: End,
    /// Evaluated after the loop; its string form is compared with `expect`.
    pub probe: &'static str,
    pub expect: &'static str,
    /// Uncaught errors thrown by tasks and microtasks (HTML "report the
    /// exception"), which the in-process driver counts.
    pub uncaught: u64,
    /// Also run through the real content process.
    pub process: bool,
}

/// A row whose observable is the event log.
const fn log(
    name: &'static str,
    spec: &'static str,
    script: &'static str,
    expect: &'static str,
) -> Row {
    Row {
        name,
        spec,
        script,
        end: End::Idle,
        probe: "log.join()",
        expect,
        uncaught: 0,
        process: false,
    }
}

/// `log`, and also run through the content process.
const fn both(
    name: &'static str,
    spec: &'static str,
    script: &'static str,
    expect: &'static str,
) -> Row {
    Row {
        process: true,
        ..log(name, spec, script, expect)
    }
}

/// A row whose observable is another expression.
const fn probed(
    name: &'static str,
    spec: &'static str,
    script: &'static str,
    probe: &'static str,
    expect: &'static str,
) -> Row {
    Row {
        probe,
        ..log(name, spec, script, expect)
    }
}

/// A row that ends in a bound.
const fn bound(
    name: &'static str,
    spec: &'static str,
    script: &'static str,
    reason: &'static str,
) -> Row {
    Row {
        end: End::Terminated(reason),
        ..log(name, spec, script, "")
    }
}

pub const ROWS: &[Row] = &[
    // ---- microtasks versus tasks -------------------------------------------
    both(
        "micro_before_timeout_zero",
        "HTML: a script is run to completion, then 'clean up after running script' performs a \
         microtask checkpoint; the timer is a task of the timer task source and runs in a later \
         turn. Promise reactions and queueMicrotask callbacks share one microtask queue in \
         enqueue order (ECMA-262 HostEnqueuePromiseJob; HTML queue a microtask). So: sync S, \
         then P (queued first), Q, then the task T.",
        r#"setTimeout(() => log.push('T'), 0);
           Promise.resolve().then(() => log.push('P'));
           queueMicrotask(() => log.push('Q'));
           log.push('S');"#,
        "S,P,Q,T",
    ),
    both(
        "nested_microtasks_drain_before_the_next_task",
        "HTML 'perform a microtask checkpoint' loops until the microtask queue is empty, so a \
         microtask queued by a microtask (m2) runs in the same checkpoint. The checkpoint \
         follows each task (t1) before the next task (t2) is chosen even though t2 is already \
         due. So t1, m1, m2, t2.",
        r#"setTimeout(() => {
             log.push('t1');
             queueMicrotask(() => { log.push('m1'); queueMicrotask(() => log.push('m2')); });
           }, 0);
           setTimeout(() => log.push('t2'), 0);"#,
        "t1,m1,m2,t2",
    ),
    log(
        "promise_chain_runs_to_the_end_before_a_timer",
        "Each .then reaction is a job queued when the previous one settles; the checkpoint \
         drains them all (queue depth never exceeds one) before the zero-delay timer task.",
        r#"let p = Promise.resolve();
           for (let i = 0; i < 5; i++) p = p.then(() => log.push(i));
           setTimeout(() => log.push('T'), 0);"#,
        "0,1,2,3,4,T",
    ),
    log(
        "microtask_queued_from_a_timer_runs_before_the_next_timer",
        "Same checkpoint rule, with the microtask queued from inside a timer task and a second \
         timer already due: a, a-micro, b.",
        r#"setTimeout(() => { log.push('a'); Promise.resolve().then(() => log.push('a-micro')); }, 5);
           setTimeout(() => log.push('b'), 5);"#,
        "a,a-micro,b",
    ),
    log(
        "timer_set_in_a_microtask_runs_after_an_earlier_timer_of_equal_delay",
        "'Run steps after a timeout' waits for earlier-started timers with an equal or smaller \
         delay: 'first' started in the script, 'fromMicro' later in the checkpoint, equal \
         delays, so first then fromMicro.",
        r#"Promise.resolve().then(() => setTimeout(() => log.push('fromMicro'), 0));
           setTimeout(() => log.push('first'), 0);"#,
        "first,fromMicro",
    ),
    // ---- timers -----------------------------------------------------------
    both(
        "timers_by_due_time_then_insertion",
        "A timer fires no earlier than its delay after it was set; with one start time, due \
         time order is delay order, ties broken by start order (the 'wait until earlier \
         invocations with equal or smaller timeout completed' step). Delays 20 a, 10 b, 10 c, \
         0 d, 15 e give d, b, c, e, a.",
        r#"setTimeout(() => log.push('a'), 20);
           setTimeout(() => log.push('b'), 10);
           setTimeout(() => log.push('c'), 10);
           setTimeout(() => log.push('d'), 0);
           setTimeout(() => log.push('e'), 15);"#,
        "d,b,c,e,a",
    ),
    both(
        "equal_delays_fire_in_insertion_order",
        "Five timers with the same delay, started in order, complete in that order (the \
         'started before this one and whose milliseconds is equal or less' wait).",
        r#"for (let i = 0; i < 5; i++) setTimeout(() => log.push(i), 7);"#,
        "0,1,2,3,4",
    ),
    log(
        "a_later_timer_with_a_short_delay_overtakes_an_earlier_long_one",
        "A set at t=0 for 10 ms is due at 10. The 5 ms timer runs at 5 and sets B for 3 ms, \
         due at 8, which is earlier than A. The spec's wait only orders timers whose earlier \
         sibling has an equal or SMALLER delay; A's 10 is larger than B's 3, so B is not held \
         behind A. Order: x5, b8, a10.",
        r#"setTimeout(() => { log.push('x5'); setTimeout(() => log.push('b8'), 3); }, 5);
           setTimeout(() => log.push('a10'), 10);"#,
        "x5,b8,a10",
    ),
    both(
        "clear_timeout_before_it_fires",
        "clearTimeout removes the entry from the list of active timers, so the callback never \
         runs.",
        r#"const id = setTimeout(() => log.push('never'), 5);
           setTimeout(() => log.push('kept'), 5);
           clearTimeout(id);"#,
        "kept",
    ),
    both(
        "clear_timeout_inside_a_callback_cancels_a_due_timer",
        "Both timers are due together. a runs first and clears b. HTML 'run steps after a \
         timeout' step: if the timer's handle is no longer in the map of active timers, abort. \
         So a, c and never b.",
        r#"var idB;
           setTimeout(() => { log.push('a'); clearTimeout(idB); }, 0);
           idB = setTimeout(() => log.push('b'), 0);
           setTimeout(() => log.push('c'), 0);"#,
        "a,c",
    ),
    log(
        "clearing_a_timer_that_already_fired_does_nothing",
        "The handle was removed after it fired; clearing it later is a no-op and does not \
         affect other timers (ids are not reused within a global, so b is not cleared by a's id).",
        r#"const a = setTimeout(() => log.push('a'), 0);
           setTimeout(() => { clearTimeout(a); log.push('b'); }, 5);"#,
        "a,b",
    ),
    both(
        "interval_rearm_and_clear_in_its_own_callback",
        "setInterval runs its callback each period until cleared; clearInterval inside the \
         callback stops the re-arm (the repeat step is skipped once the handle is gone). Three \
         runs.",
        r#"var n = 0, id;
           id = setInterval(() => { log.push('i' + (++n)); if (n == 3) clearInterval(id); }, 10);"#,
        "i1,i2,i3",
    ),
    log(
        "interval_interleaves_with_a_timeout_by_due_time",
        "Interval period 10 fires at 10, 20, 30, 40 (re-armed each time at the firing time). \
         The 25 ms timeout falls between the 20 and the 30 run. i1, i2, T25, i3, i4.",
        r#"var n = 0, id;
           id = setInterval(() => { log.push('i' + (++n)); if (n == 4) clearInterval(id); }, 10);
           setTimeout(() => log.push('T25'), 25);"#,
        "i1,i2,T25,i3,i4",
    ),
    log(
        "nested_timer_clamp_four_ms_from_the_sixth_level",
        "HTML timer initialisation: nesting level = current + 1, and when it is greater than \
         5 the delay is at least 4 ms. f is level 1 (from the script) up to f5 at level 5, all \
         due at once; f5 arms f6 at level 6, so f6 waits 4 ms, and f7 another 4 (t=8). A 2 ms \
         timer set from the script falls after f5 and before f6. Order f1..f5, B2, f6, f7.",
        r#"var n = 0;
           function f() { n++; log.push('f' + n); if (n < 7) setTimeout(f, 0); }
           setTimeout(f, 0);
           setTimeout(() => log.push('B2'), 2);"#,
        "f1,f2,f3,f4,f5,B2,f6,f7",
    ),
    log(
        "a_timer_set_from_the_script_is_not_clamped_beside_a_clamped_chain",
        "The nesting level belongs to the running task, not the global: the 1 ms timer set \
         from the script is level 1 and unclamped, due at 1 ms. The chain f1..f5 runs at 0, \
         f6 is level 6 and clamped to 4 ms. Order f1..f5, top, f6.",
        r#"var n = 0;
           function f() { n++; log.push('f' + n); if (n < 6) setTimeout(f, 0); }
           setTimeout(f, 0);
           setTimeout(() => log.push('top'), 1);"#,
        "f1,f2,f3,f4,f5,top,f6",
    ),
    log(
        "delay_two_to_the_31_wraps_to_zero",
        "setTimeout's timeout is the IDL type `long`, converted with no [Clamp] or \
         [EnforceRange], i.e. modulo 2^32 (WebIDL 3.2.x). 2^31 becomes -2147483648, and \
         timer initialisation sets a negative timeout to 0. So 'wrapped' (set first) runs \
         before the 5 ms timer. Infinity converts to 0 the same way.",
        r#"setTimeout(() => log.push('five'), 5);
           setTimeout(() => log.push('wrapped'), 2147483648);
           setTimeout(() => log.push('inf'), Infinity);"#,
        "wrapped,inf,five",
    ),
    log(
        "negative_nan_and_missing_delays_are_zero",
        "Negative: set to 0 by the timer initialisation steps. NaN and undefined convert to \
         0 under ToInt32 / the default. All three run as zero-delay timers in start order.",
        r#"setTimeout(() => log.push('nan'), NaN);
           setTimeout(() => log.push('neg'), -5);
           setTimeout(() => log.push('undef'));"#,
        "nan,neg,undef",
    ),
    log(
        "timer_arguments_are_passed_through",
        "HTML timers: the arguments after the delay are passed to the handler.",
        r#"setTimeout((a, b) => log.push(a + b), 0, 'x', 'y');"#,
        "xy",
    ),
    // ---- async / await ----------------------------------------------------
    both(
        "await_a_native_promise_takes_one_tick",
        "ECMA-262 Await uses PromiseResolve (a native promise is used as is) then \
         PerformPromiseThen: the continuation is one job. Script: 'script start', a1 start, \
         a2 (sync body of a2), the await queues J1; the executor logs p1 and queues J2 \
         (p2); 'script end'. J1 logs 'a1 end', J2 logs p2; then the timer.",
        r#"async function a1() { log.push('a1 start'); await a2(); log.push('a1 end'); }
           async function a2() { log.push('a2'); }
           log.push('script start');
           setTimeout(() => log.push('timeout'), 0);
           a1();
           new Promise(r => { log.push('p1'); r(); }).then(() => log.push('p2'));
           log.push('script end');"#,
        "script start,a1 start,a2,p1,script end,a1 end,p2,timeout",
    ),
    both(
        "await_a_plain_value_interleaves_with_a_promise_chain",
        "`await 1` is PromiseResolve(1) then one job. Queue after the script: X1 (continuation), \
         P1. X1 logs a and awaits again (queues X2); P1 logs p1 and queues P2. Queue is then \
         X2, P2: b, p2. Order a, p1, b, p2.",
        r#"(async () => { await 1; log.push('a'); await 1; log.push('b'); })();
           Promise.resolve().then(() => log.push('p1')).then(() => log.push('p2'));"#,
        "a,p1,b,p2",
    ),
    log(
        "async_function_returning_a_promise_costs_three_ticks",
        "`return V` of a promise from an async function resolves its result promise P with a \
         thenable: NewPromiseResolveThenableJob A, which calls V.then (job C), whose resolve \
         then queues P's reaction (E). Queue: A, B(log 1); then C, D(2); then E(f), F(3); \
         then G(4). Order: 1, 2, f, 3, 4.",
        r#"async function f() { return Promise.resolve('x'); }
           f().then(() => log.push('f'));
           Promise.resolve().then(() => log.push('1')).then(() => log.push('2'))
             .then(() => log.push('3')).then(() => log.push('4'));"#,
        "1,2,f,3,4",
    ),
    log(
        "async_function_that_throws_rejects_its_promise",
        "An exception in an async function body rejects the returned promise; it is not an \
         uncaught exception. The catch handler runs in the checkpoint, after the sync log.",
        r#"(async () => { throw new Error('x'); })().catch(e => log.push('caught:' + e.message));
           log.push('sync');"#,
        "sync,caught:x",
    ),
    both(
        "await_a_timer_interleaves_two_async_functions",
        "Each await of a timer promise resumes in the timer's task checkpoint. a sleeps 10, \
         wakes (a1), sleeps 10 more (due at >= 20); b sleeps 15 and wakes between. a0 at once, \
         a1, b, a2.",
        r#"const sleep = ms => new Promise(r => setTimeout(r, ms));
           (async () => { log.push('a0'); await sleep(10); log.push('a1'); await sleep(10); log.push('a2'); })();
           (async () => { await sleep(15); log.push('b'); })();"#,
        "a0,a1,b,a2",
    ),
    // ---- promise combinators ----------------------------------------------
    both(
        "promise_all_result_follows_input_order_not_completion_order",
        "Promise.all fills result[i] by index when element i settles; the array order is the \
         iteration order whatever the completion order (ECMA-262 Promise.all resolve element).",
        r#"const d = (ms, v) => new Promise(r => setTimeout(() => r(v), ms));
           Promise.all([d(30, 'a'), d(10, 'b'), d(20, 'c')]).then(v => log.push(v.join('')));"#,
        "abc",
    ),
    log(
        "promise_all_rejects_with_the_first_rejection",
        "The first element to reject (b, at 10 ms) rejects the result; later settlements are \
         ignored. Only one catch call.",
        r#"const d = (ms, v, rej) => new Promise((res, rj) => setTimeout(() => (rej ? rj : res)(v), ms));
           Promise.all([d(30, 'a'), d(10, 'b', true), d(20, 'c')]).catch(e => log.push('rej:' + e));"#,
        "rej:b",
    ),
    log(
        "promise_all_with_plain_values_resolves_after_one_tick_per_element",
        "Promise.all calls PromiseResolve then .then(resolveElement) on each element; both are \
         already fulfilled, so jobs E1 E2 are queued before the p1 reaction. E2 resolves the \
         result, queueing the 'all' reaction after P1. Queue: P1, A. P1 logs p1 (queues P2), A \
         logs all, then P2, P3. Order p1, all, p2, p3.",
        r#"Promise.all([1, 2]).then(() => log.push('all'));
           Promise.resolve().then(() => log.push('p1')).then(() => log.push('p2')).then(() => log.push('p3'));"#,
        "p1,all,p2,p3",
    ),
    both(
        "promise_race_settles_with_the_first_to_settle",
        "Promise.race resolves or rejects with whichever element settles first: b at 10 ms.",
        r#"const d = (ms, v) => new Promise(r => setTimeout(() => r(v), ms));
           Promise.race([d(30, 'a'), d(10, 'b'), d(20, 'c')]).then(v => log.push(v));"#,
        "b",
    ),
    log(
        "promise_race_a_first_rejection_wins",
        "The earliest settlement is a rejection (x at 5 ms), so the race rejects with x.",
        r#"const d = (ms, v, rej) => new Promise((res, rj) => setTimeout(() => (rej ? rj : res)(v), ms));
           Promise.race([d(5, 'x', true), d(10, 'y')]).then(v => log.push('ok:' + v), e => log.push('rej:' + e));"#,
        "rej:x",
    ),
    both(
        "promise_all_settled_reports_every_outcome_in_input_order",
        "allSettled never rejects; each slot is {status, value|reason} in input order, and the \
         result resolves after the slowest element (20 ms).",
        r#"const d = (ms, v, rej) => new Promise((res, rj) => setTimeout(() => (rej ? rj : res)(v), ms));
           Promise.allSettled([d(20, 'a'), d(5, 'b', true)])
             .then(r => log.push(r.map(x => x.status + ':' + (x.value || x.reason)).join('|')));"#,
        "fulfilled:a|rejected:b",
    ),
    both(
        "promise_any_takes_the_first_fulfilment_ignoring_earlier_rejections",
        "any resolves with the first FULFILMENT (c at 10 ms); the rejection at 5 ms is \
         recorded but does not settle the result.",
        r#"const d = (ms, v, rej) => new Promise((res, rj) => setTimeout(() => (rej ? rj : res)(v), ms));
           Promise.any([d(5, 'x', true), d(20, 'b'), d(10, 'c')]).then(v => log.push(v));"#,
        "c",
    ),
    log(
        "promise_any_all_rejected_is_an_aggregate_error_in_input_order",
        "When every element rejects, the result rejects with an AggregateError whose `errors` \
         are in input order (not rejection order): x, y even though y rejects first.",
        r#"const d = (ms, v) => new Promise((res, rj) => setTimeout(() => rj(v), ms));
           Promise.any([d(10, 'x'), d(5, 'y')]).catch(e => log.push(e.constructor.name + ':' + e.errors.join('')));"#,
        "AggregateError:xy",
    ),
    // ---- error propagation -------------------------------------------------
    log(
        "a_throwing_then_callback_rejects_the_next_promise_and_skips_then_handlers",
        "An exception in a reaction job rejects the derived promise; the next .then (no \
         rejection handler) is skipped, .catch handles it, and the chain continues fulfilled.",
        r#"Promise.resolve().then(() => { throw 'e1'; })
             .then(() => log.push('skipped'))
             .catch(e => log.push('c:' + e))
             .then(() => log.push('after'));"#,
        "c:e1,after",
    ),
    log(
        "finally_runs_then_passes_the_value_through",
        "Promise.prototype.finally calls onFinally with no argument and the derived promise \
         keeps the original value: fin, then v1.",
        r#"Promise.resolve(1).finally(() => log.push('fin')).then(v => log.push('v' + v));"#,
        "fin,v1",
    ),
    Row {
        uncaught: 1,
        ..log(
            "a_throwing_timer_is_reported_and_the_next_timer_still_runs",
            "HTML: an exception in a timer handler is 'reported' (an error event on the \
             global, then the console); it is not rethrown into the loop, so the next task \
             runs. One uncaught error.",
            r#"setTimeout(() => { throw new Error('first'); }, 0);
               setTimeout(() => log.push('second'), 0);"#,
            "second",
        )
    },
    Row {
        uncaught: 1,
        ..log(
            "a_throwing_queue_microtask_is_reported_and_the_next_microtask_runs",
            "HTML 'queue a microtask': the callback is invoked and an exception is reported, \
             after which the checkpoint continues. One uncaught error.",
            r#"queueMicrotask(() => { throw new Error('boom'); });
               queueMicrotask(() => log.push('after'));"#,
            "after",
        )
    },
    log(
        "a_throwing_promise_reaction_is_a_rejection_not_an_uncaught_error",
        "A throw in a PromiseReactionJob rejects the derived promise (ECMA-262 \
         NewPromiseReactionJob). It is not an uncaught exception; the count stays 0 and the \
         later timer runs.",
        r#"Promise.resolve().then(() => { throw new Error('x'); });
           setTimeout(() => log.push('later'), 0);"#,
        "later",
    ),
    log(
        "unhandled_rejection_does_not_stop_the_loop_and_is_not_an_uncaught_error",
        "HTML 'notify about rejected promises' fires `unhandledrejection` on the global and, \
         if not cancelled, reports to the console. With no event listeners (Phase 10 has no \
         EventTarget) nothing is script-visible: the loop goes on and window.onerror is not \
         involved, so the uncaught count stays 0. The HOST DOES NOT REPORT it anywhere \
         (docs/backlog.md); this row pins that the page survives.",
        r#"Promise.reject(new Error('nobody listens'));
           (async () => { throw new Error('async nobody listens'); })();
           setTimeout(() => log.push('still running'), 0);"#,
        "still running",
    ),
    log(
        "a_rejection_handled_in_a_later_task_is_still_delivered",
        "A rejected promise stays rejected; attaching a handler in a later task queues the \
         reaction then (HTML 'rejection handled' only retracts the earlier report).",
        r#"const p = Promise.reject('r');
           setTimeout(() => p.catch(e => log.push('late:' + e)), 0);"#,
        "late:r",
    ),
    // ---- bounds (ADR 040: host policy, not spec) ---------------------------
    Row {
        end: End::TaskLimit(5000),
        probe: "n",
        expect: "5000",
        ..log(
            "a_self_rearming_zero_timer_never_ends_and_never_grows",
            "Neither HTML nor ECMA-262 bounds a self-re-arming timer: it runs forever. ADR \
             040: one live timer, queue depth one, so no bound is hit; the driver cuts it \
             after 5000 tasks and script is still alive.",
            r#"var n = 0; function f() { n++; setTimeout(f, 0); } setTimeout(f, 0);"#,
            "",
        )
    },
    bound(
        "a_self_requeuing_microtask_hits_the_checkpoint_budget",
        "Per spec the checkpoint would never end (a starved event loop). ADR 040 adds a \
         per-checkpoint budget of 1,000,000 microtasks: the host stops script with \
         MicrotaskBudget.",
        r#"function f() { queueMicrotask(f); } f();"#,
        "MicrotaskBudget",
    ),
    Row {
        process: true,
        ..bound(
            "a_self_requeuing_promise_job_hits_the_checkpoint_budget",
            "As above through promise jobs instead of queueMicrotask.",
            r#"function f() { Promise.resolve().then(f); } f();"#,
            "MicrotaskBudget",
        )
    },
    bound(
        "a_timer_that_arms_a_hundred_of_itself_hits_the_timer_cap",
        "Each run arms 100 more timers: the live count passes the 10,000 cap (ADR 040) and \
         the host stops script with TimerOverflow.",
        r#"function f() { for (let i = 0; i < 100; i++) setTimeout(f, 0); } f();"#,
        "TimerOverflow",
    ),
    Row {
        process: true,
        ..bound(
            "twenty_thousand_timers_hit_the_timer_cap",
            "20,000 live timers is over the 10,000 cap of ADR 040: TimerOverflow. (No spec \
             limit exists; this is the host's policy.)",
            r#"for (let i = 0; i < 20000; i++) setTimeout(() => {}, 1000);"#,
            "TimerOverflow",
        )
    },
    probed(
        "nine_thousand_timers_all_fire_in_due_time_then_insertion_order",
        "9,000 timers, under the cap, with 50 distinct delays d = (7i) mod 50. A timer fires \
         no earlier than one with a smaller delay set at the same time, and equal delays in \
         insertion order. `bad` counts violations (expected 0) and n counts runs (9000).",
        r#"var n = 0, bad = 0, lastD = -1, lastI = -1;
           for (let i = 0; i < 9000; i++) {
             const d = (i * 7) % 50;
             setTimeout(() => {
               if (d < lastD || (d == lastD && i < lastI)) bad++;
               lastD = d; lastI = i; n++;
             }, d);
           }"#,
        "bad + ',' + n",
        "0,9000",
    ),
    probed(
        "a_deep_promise_chain_runs_to_the_end_in_one_checkpoint",
        "A chain of 20,000 .then calls: one job queued at a time (depth 1), so the 65,536 \
         queue cap is never approached and 20,000 is far under the 1,000,000 budget. The \
         zero-delay timer runs only after all of them, so it records n = 20000.",
        r#"var n = 0;
           let p = Promise.resolve();
           for (let i = 0; i < 20000; i++) p = p.then(() => { n++; });
           setTimeout(() => log.push(n), 0);"#,
        "log.join()",
        "20000",
    ),
    probed(
        "a_flat_fan_out_under_the_microtask_cap_runs_every_job",
        "30,000 independent reactions (under the 65,536 cap) all run in the checkpoint after \
         the script, before the timer: it sees 30000.",
        r#"var n = 0;
           for (let i = 0; i < 30000; i++) Promise.resolve().then(() => { n++; });
           setTimeout(() => log.push(n), 0);"#,
        "log.join()",
        "30000",
    ),
    // ---- the clock, as script sees it -------------------------------------
    probed(
        "performance_now_is_monotonic_on_the_100_microsecond_grid",
        "ADR 040: every output is a multiple of 100 us and never decreases (High Resolution \
         Time requires a monotonic clock). Values are sampled in 50 chained timer tasks; \
         10 x value is an integer to float noise, and elapsed time is at least the 49 \
         requested milliseconds (timers never fire early).",
        r#"var t = [];
           function tick() { t.push(performance.now()); if (t.length < 50) setTimeout(tick, 1); }
           tick();
           globalThis.ok = () => t.every((v, i) => (i == 0 || v >= t[i - 1])
                                      && Math.abs(v * 10 - Math.round(v * 10)) < 1e-6)
                                 && t[49] - t[0] >= 49;"#,
        "ok()",
        "true",
    ),
    probed(
        "date_now_is_an_integer_number_of_milliseconds_and_never_goes_back",
        "ECMA-262: Date.now returns an integral time value (milliseconds). The host derives \
         it from the same coarse, monotonic clock, so a series sampled across timer tasks is \
         integral and non-decreasing.",
        r#"var t = [];
           function tick() { t.push(Date.now()); if (t.length < 30) setTimeout(tick, 2); }
           tick();
           globalThis.ok = () => t.every((v, i) => Number.isInteger(v) && (i == 0 || v >= t[i - 1]));"#,
        "ok()",
        "true",
    ),
    probed(
        "math_random_is_in_the_unit_interval_and_not_constant",
        "ECMA-262 Math.random: a Number >= 0 and < 1. A thousand draws are in range and not \
         all equal. (Seeded determinism across hosts is its own test, not a row.)",
        r#"var r = [];
           for (let i = 0; i < 1000; i++) r.push(Math.random());
           globalThis.ok = () => r.every(v => v >= 0 && v < 1) && new Set(r).size > 900;"#,
        "ok()",
        "true",
    ),
];
