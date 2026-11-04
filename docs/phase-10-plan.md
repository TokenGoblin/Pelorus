# Phase 10 plan: script host

Scope and cost: ADR 040 (proposed; nothing starts until the user decides it).
Numbers: `docs/research/phase-10-boa.md`. Appendix A settles Boa; not re-argued.

Gate (build-spec section 9): test262 subset threshold; event ordering tests; no
unbounded task-queue growth under a stress page; timer resolution asserted.

Gate tests are written first and fail (unit 0).
## What the gate can and cannot prove before Phase 11

Phase 10 runs scripts that touch **no DOM**. There is no `document`, `window`
events, `fetch`, or `postMessage` yet; the only globals are the ES builtins plus
what `px-script` itself defines (`setTimeout`/`clearTimeout`, `setInterval`,
`queueMicrotask`, `performance.now`, a test-only `print`).

The gate **can** prove: the language conformance at a threshold on our feature
set; microtask versus timer ordering, which is a property of the loop and needs
no DOM; the bounds on queues and timers; the clock arithmetic; that a runaway
script cannot hurt the broker or another process; that script runs only in the
sandboxed content process.

The gate **cannot** prove anything that crosses into the DOM: event dispatch
order, `<script>` parse-blocking, the six `scripted_*` html5lib tests
(`docs/backlog.md`), `requestAnimationFrame` versus rendering,
`MutationObserver` timing. Event ordering tests here are *loop* ordering; HTML's
full ordering is re-checked in Phase 11 and is not "passed" because Phase 10 is
green. test262 is language conformance, not compat (section 6).

## Units

**Unit 0: gate tests, failing.** `cargo xtask script` (shell, no engine yet)
and the integration tests named below, all red. One commit.

**Unit 1: embedding and a first `eval` through the content process**
(`px-script`, `px-content`, `px-ipc`). Add `boa_engine =0.22.0` to `px-script`
only (after ADR 040 acceptance; update `ci/unsafe-baseline.json` in the same
commit with `cargo xtask unsafe-audit --write`, and `cargo vet`/`deny`
config). `px-script::Host` owns the `Context`. New IPC variants, size-capped
(a source-text limit, a result-string limit): `RunScript { source }` from the
broker to content and `ScriptResult { value | error | terminated }` back; no
identity in either (invariant 9). `px-content` gets a loop that receives
`RunScript`. Tests: `1+1` over a real pipe; a thrown error comes back as data;
the broker links no `boa_*` (checked by `cargo tree -p px-broker` in a test);
an oversized source is refused. Gate items: groundwork for all; the "script
only in content" property.
Risk: ADR 004's abort profile compiles the closure twice; record CI time.

**Unit 2: event loop and queues** (`px-script`). Our `JobExecutor`, the bounded
task queue, microtask checkpoint, per-checkpoint budget, the `terminated` state
and `ScriptTerminated` reporting. Pure Rust types, unit-testable without Boa
for the queue logic. Testing-only high-water-mark counters behind
`#[cfg(feature = "testing")]` (build-spec 14.4; add them to the release scan).
Tests: self-rearming timer, self-requeuing microtask, 1e6-job fan-out, 20,000
intervals; every high-water mark at or under its cap. Gate items: "no unbounded
task-queue growth", "event ordering" (microtasks before the next task; two
timers by due time then insertion; `clearTimeout` inside a callback; promise
reaction order; nested-timer 4 ms clamp).
Risk, medium: Boa's `enqueue_job` cannot fail, so overflow is a host flag; test
that nothing enqueued after `terminated` can run.

**Unit 3: timers and the coarse clock** (`px-script`). `RawClock` trait,
`CoarseClock` per ADR 040, `Clock` impl for Boa, `performance.now`, timer table
on the coarse clock, `Math.random` replacement. The seed arrives from the
broker in the first IPC message. Tests: the seven pure properties in the ADR
(multiples of 100 us, within one quantum of true time, monotonic, stable inside
a quantum, 100 distinct values per 10 ms, seed determinism, jitter not
constant), then one content-process test with the `testing` feature reading the
raw log at the native boundary. Gate item: "timer resolution asserted".
Independent of units 1 and 2 except for the final wiring.
Risk, medium: the spec's "section Spectre" does not exist; user confirms first.

**Unit 4: resource limits** (`px-script`, `px-content`). `RuntimeLimits`
(loop, recursion, stack), `max_buffer_size`, limit errors made terminal by the
host, and the watchdog thread (heartbeat per task or microtask boundary, abort
after 10 s). Broker side: the existing deadline path reports "unresponsive".
Tests: `while(true){}`, deep recursion, `try{f()}catch{}` loop, regex
backtracking, huge `repeat`, `new Array(2**32-1).fill(1)`, each ending in
`terminated` or a dead content process with the broker alive; two tabs, one
runaway, the other still answers. **This unit first runs the behaviours the
research doc only read from source** (the build the probe could not finish for
lack of disk) and its report replaces those claims. Gate items: supports "no
unbounded growth" and the process-survival property.
Risk, high: no Boa interrupt; a memory bomb is **not** contained until Phase 17.

**Unit 5: test262 vendoring and runner, as Rust tools** (`px-xtask`).
`cargo xtask vendor-test262 [<sha>] [--from-checkout <dir>]`, modelled on
`vendor_wpt.rs` but using `git` as a child (the `gh` API may not be authorised
for `tc39/test262`; a `--depth 1` clone worked through the proxy, an 18 MB
pack). Selection is a pure function over the file listing and each file's
frontmatter, so it is tested offline. A hand parser for the fixed frontmatter
keys (`includes`, `flags`, `features`, `negative`), no YAML dependency (xtask
is std only). Excluded by rule: `intl402`, `built-ins/Temporal`, `staging`,
`SharedArrayBuffer`/`Atomics`, `WebAssembly`, tests needing `$262.agent`, and
any `features:` entry for something we do not ship. `cargo xtask test262`
spawns the content binary per test (or per batch with respawn: a crash is a
**result**), with a wall-clock limit, implements `print`, `$262` (`evalScript`,
`createRealm`, `detachArrayBuffer`, `gc`) and the async `$DONE` protocol, and
compares against a committed expected-failures file so a regression is a diff,
not a percentage. Gate item: "test262 subset threshold".
Size: `test/` is 54,078 files / 86.7 MB; core-ES is about 44,500 files / 65 MB,
too large beside an 8.9 MB `tests/wpt` (user choice below). Licence: BSD, keep
`LICENSE` beside the corpus.
Risk, high: Boa's published rate could not be read here (`boajs.dev` blocked)
and is not a rate for our feature set. **Set the threshold from our first
measured run, and say so**, not from the spec's "96%".

**Unit 6: event-ordering tests, and the stress page** (`tests/script/`).
A fixture table (script, expected event log) run through the real content
process; collects the ordering and stress cases of units 2 and 4. No DOM.

**Unit 7: gate report.** Time Boa on a fixed set of CPU-bound scripts (section
11: "measured at Phase 10"); `docs/phase-10-gate-report.md`.

## Gate items to units

| Gate item | Units |
|---|---|
| test262 subset threshold | 5 (needs 1 for the runner to execute anything) |
| event ordering tests | 2, 6 |
| no unbounded task-queue growth under a stress page | 2, 4, 6 |
| timer resolution asserted | 3 |
| (implicit) script only in the sandboxed content process | 1, and 4 for survival |

## Ordering and parallelism

Unit 0 first. **1 is the trunk**; 3's pure `CoarseClock`, 2's queue logic and
5's vendoring tool need no engine and run in parallel with it (only 5's runner
needs 1). 4 needs 1 and 2; 6 needs 2, 3, 4; 7 last. Phase 10 needs nothing from
Phase 11; Phase 11 needs units 1 (`register_global_class`), 2 (`JobCallback`
host data) and 3.

## What the user must decide before unit 1

ADR 040 decisions 1 to 6, and **how test262 enters the repo**: (a) vendor a
rule-selected subset under about 15 MB, like `tests/wpt`; or (b) pin the SHA and
fetch into `target/` at gate time (smaller repo, but not offline-reproducible).
Recommended: (a).

## Honest risk

1. **Boa has no interrupt.** Limits plus a blunt watchdog abort; a memory bomb
   is uncontained until Phase 17.
2. **Limits claims are from source, not run;** unit 4 runs them first.
3. **The test262 threshold is unknown** until unit 5 measures it; the spec's 96%
   is unverified and not our feature set.
4. **Speed** (spec: 10 to 50 times slower than V8) is measured in unit 7; the
   section 11 options apply if unusable.
5. **Pre-1.0 API; CI cost:** exact pin; 73 more crates compiled twice.
