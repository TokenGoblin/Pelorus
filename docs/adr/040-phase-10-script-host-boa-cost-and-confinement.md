# 040 — Phase 10 script host: boa_engine, what it costs and how it is confined

- **Status:** accepted by the user, 2026-11-04, as recommended: `boa_engine =0.22.0`
  with default features off and `annex-b` on; `Intl`, `Temporal`, WebAssembly and
  SharedArrayBuffer absent in v1; the layered runaway-script limits including the
  10-second abort watchdog; the 100-microsecond jittered timer specification; a
  rule-selected test262 subset. Maintainer count and cargo-vet status of the new
  crates were blank at acceptance and are filled in below (2026-11-05, see
  "Corrections"). Nothing in any `Cargo.toml` or `Cargo.lock` had been changed
  when this was written.
- **Date:** 2026-11-04
- **Phase:** 10
- **Invariants touched:** 6 (No JIT: upheld). Bears on 1 (no ambient authority),
  §4 (the unsafe claim), the `ci/unsafe-baseline.json` ratchet, and the
  directive "as few dependencies as possible". Appendix A ("Boa, interpreter
  only") is **not** re-argued; this ADR prices it. Two statements in the spec
  look wrong and are reported in "Spec findings" below, not acted on.

## Context

`/CLAUDE.md` requires an ADR before any dependency. §3 names `boa_engine` as
taken from the ecosystem, §6 and Appendix A settle it, and §9 Phase 10 adds
"timers coarsened to 100µs with jitter (§Spectre). WASM ADR due." The numbers
below are from `docs/research/phase-10-boa.md` (read it for method). Everything
about runtime behaviour of runaway scripts there is **read from source, not run**.

## Decision

1. **Dependency.** Add `boa_engine =0.22.0` (exact pin: Boa is pre-1.0 and its
   API breaks between minors) with `default-features = false`, plus `annex-b`
   (decision 1 for the user). Only `px-script` depends on it. `px-content`
   depends on `px-script`. The broker and every other process do not link it.
2. **Process model.** Script runs **only** in the sandboxed content process, on
   one thread owning the `Context` (it is `!Send`). Script has no handle it was
   not given; all effects leave through the typed IPC channel (invariant 1).
   Nothing in `px-script` opens a file or socket.
3. **Our own host, not Boa's.** We write the `JobExecutor`, the timers, the
   clock, `performance.now` and the limits. We do not use `SimpleJobExecutor`
   (unbounded queues) or the `boa_runtime` crate (its `fetch`, `futures` and
   `reqwest` do not belong in the content process).
4. **Bounded event loop** (below), **coarsened time** (below), **layered
   runaway-script limits** (below), **WASM: none in v1** (below).
5. `px-script` keeps `#![forbid(unsafe_code)]`. No first-party `unsafe` is added.

## Numbers

Method in the research doc: `cargo tree --edges normal` with the repo's own
`Cargo.lock` copied in so resolution prefers locked versions; unsafe counted with
the `unsafe_audit.rs` regex.

| Measure | `default-features = false` |
|---|---|
| Crates in the tree | 132 (Linux), 131 (Windows) |
| New to `Cargo.lock` (exact name and version) | **73** (7 `boa_*`, 66 others) |
| Already locked via stylo and friends | 59 |
| Unsafe tokens in the tree / new to the baseline | 8,025 / **5,143** |
| `ci/unsafe-baseline.json` | 21,675 -> **26,818** (+23.7%) |
| Boa's own seven crates | 604 (`boa_engine` 293, **`boa_gc` 180**, `boa_string` 97, `boa_interner` 23, `boa_macros` 9, `boa_parser` 2) |
| Largest non-Boa additions | `portable-atomic` 727, `intrusive-collections` 418, `bytemuck` 391, `memchr` 340, second `hashbrown` 326, `allocator-api2` 277, `time` 274 |
| With `temporal` / `intl_bundled` / both | 142 / 163 / 167 crates |
| C compiler, Python, cmake needed | **none** (no `cc`/`cmake`/`bindgen`; no Boa `build.rs`) |
| `cargo check` | passes, 36 s cold |
| Licence | `Unlicense OR MIT` |

Surprising hard dependencies of an interpreter: `serde_json`, `rand` 0.10 (+
`chacha20`, `getrandom` 0.4), `time`, `dashmap`, `async-channel`, `futures-lite`,
`futures-concurrency`, `oneshot`. They arrive with `boa_engine` and cannot be
switched off. `serde_json` in the content binary does not conflict with "no
serde_json in IPC", which is about the wire format.

**Maintainers and audit.** Filled in on 2026-11-05 from the committed vet store and
the crates.io owners endpoint; see "Corrections" for the figures and for what is
still unknown. Original text, kept: Boa is a community project under the `boa-dev`
organisation, not one person; at the time of writing nothing had been counted or
checked. The prediction that most of the 73 would be unaudited was right for the
seven Boa crates (all seven are exemptions).

**The honest unsafe statement.** The GC is 180 tokens of the +5,143. It is the
part that matters by kind: `boa_gc` is a tracing collector with `unsafe impl
Trace`, and an error there is a use-after-free reachable from script, which is
why section 4 names it. By count, though, 85% of the increase is ordinary
crates. The ratchet is a text count over-counting comments; it measures movement,
not exploitability. The sandbox stays the primary control (§4).

**100% Rust and reproducibility.** Pure Rust, no non-Rust build step, so this is
cleaner than stylo (ADR 023). Pinned `=0.22.0`, committed lockfile, `--locked`.
Two cost lines to state: `px-content` builds twice for `panic = "abort"` (ADR
004), so all 73 new crates compile twice in CI; and Boa 0.22 raises MSRV to 1.95
(we pin 1.98.1).

## The event loop and queues

One thread, one `Context`, the HTML processing model reduced to what Phase 10
can honestly build:

- A **task queue** (macrotasks: timer firings, and later events and message
  tasks), FIFO per task source, with a **fixed capacity**.
- A **microtask queue**, implemented as our `JobExecutor` (`enqueue_job` /
  `run_jobs`), drained at each microtask checkpoint: after each task and after
  each callback invocation, per HTML.
- A **timer table**: a min-heap keyed on coarsened due time, with a cap on live
  timers. HTML's nesting rule applies (after 5 nested levels the minimum delay is
  4 ms), which also defeats `setTimeout(f,0)` busy-looping.

**Bounds.** Starting values, to be tuned by measurement: task queue 4,096
entries; live timers 10,000; microtask queue 65,536 entries; **and a
per-checkpoint budget** of 1,000,000 microtasks run. Boa's `enqueue_job`
returns `()`: Boa cannot be told "full". So overflow is a **host policy**, and
it is: drop the job, set a `terminated` flag, discard the queues, never run
script in that content process again, and report `ScriptTerminated` to the
broker over IPC. The DOM and last render remain; the tab shows a "page script
stopped" state. It is not a process abort (a bound hit is a defect in the page,
not in us) and it is not a silent drop (that would be a behaviour no spec has).

**Why a queue bound alone is not the gate.** `function f(){Promise.resolve().then(f)} f()`
keeps the queue at depth 1 forever and never yields. Only the per-checkpoint
budget catches it. The bound on size and the budget on work are two checks and
the gate tests each.

**Gate: "no unbounded task-queue growth under a stress page".** The host exports
high-water marks (task queue, microtask queue, timers, microtasks run per
checkpoint) from the content process, in `testing` builds only (§14.4: a
test-only capability, scanned for in release artefacts). The stress page
fixtures: a `setTimeout` that re-arms itself with 0 delay; a `setInterval(f,0)`
registered 20,000 times; a microtask that re-queues itself; a promise-chain
fan-out of 1e6 jobs; a timer that schedules 100 more. The assertions: every
high-water mark is at or under its cap, the process is alive afterwards (or the
`ScriptTerminated` message was sent), and resident memory after the page stopped
did not grow with the iteration count. Deterministic: it needs no wall-clock
assertion, only counters.

## Timer coarsening, and a testable "timer resolution asserted"

**The spec gap.** Phase 10 says "§Spectre". `docs/build-spec.md` has no such
section; "Spectre" appears at lines 269, 383, 401 and 547 and none states a
mechanism. This ADR therefore **writes the specification** and the user must
confirm it (decision 3).

**Specification.** Every time source a script can read is derived from one
`CoarseClock`: `performance.now()`, the `Clock::now` that Boa's `Atomics` and
job timers use, and timer due times. `Date.now()` is already millisecond
resolution through `Clock::system_time_millis`; it is not made finer and it is
floored from the same coarse value. For a raw monotonic instant `t` (integer
nanoseconds), quantum `Q = 100,000 ns`:

```
q    = t / Q
frac = t % Q                        // 0 .. Q-1
up   = hash(seed, q) % Q < frac     // deterministic per quantum, not per read
out  = max(previous_out, (q + up as u64) * Q)
```

The output is always a multiple of `Q`, never more than `Q` from the true time,
non-decreasing, and the same for every read within one quantum. The jitter is the
`up` decision: whether a read rounds down or up is unpredictable without `seed`
but consistent within the quantum so it cannot be averaged out by re-reading.
`seed` is **given to the content process by the broker at spawn** (authority from
the channel, invariant 9, and the content process needs no entropy source). The
hash is any fixed 64-bit mixer we own (no new dependency).

**Why it is deterministic to test.** `CoarseClock` is generic over a
`RawClock` trait (`fn now_ns(&self) -> u64`) and takes the seed as an argument;
production wires `std::time::Instant` and the broker seed, tests wire a fake.
The gate's assertions, all pure:

1. every output across 1 ns steps over 50 ms is a multiple of 100,000 ns;
2. `|out - t| <= Q` always;
3. outputs are non-decreasing;
4. within one quantum, repeated reads are identical;
5. the number of distinct outputs over 10 ms is 100 (+/- 1);
6. same `(seed, raw series)` gives byte-identical output; two different seeds
   give different output somewhere (jitter is real);
7. over many quanta the fraction of `up` decisions at `frac = Q/2` is within a
   stated tolerance of one half (checks the jitter is not constant).

Then one end-to-end test in the content process with the `testing` feature:
script reads `performance.now()` in a loop and the host's raw log of returned
nanosecond values confirms (1) and (3) on the real path. Asserting on the JS
float would be wrong: `0.1` ms is not exact in binary, so the log is taken at
the native boundary before conversion.

**Not claimed.** 100 us granularity with this jitter blunts a cache-timing
primitive, it does not eliminate one: an attacker can amplify (repeat the
measured operation) at a cost. The real Spectre boundary is the process, which
is why Phase 14 matters. We state that in the docs and we do not call it a fix.
`SharedArrayBuffer` is **not exposed** (a stated non-goal, §1) and stays off unless Phase 12 can say a page is
cross-origin isolated, because shared memory plus a worker is a counter that
bypasses any clock coarsening. `Math.random` is replaced by our own native
function (Boa's calls `rand` directly and cannot be seeded), also making the
test runs reproducible.

## Resource limits against runaway scripts

*From the Boa 0.22 source, not run; Phase 10 unit 4 must confirm each.*

What Boa gives: `RuntimeLimits` (loop iteration count **per call frame**,
recursion depth 512, stack 10 KiB), `HostHooks::max_buffer_size` for
`ArrayBuffer`. What it lacks: **no interrupt hook, no deadline, no cancellation,
no heap cap**. The `fuzz` feature has a global instruction counter, but it can
only be set when the context is built, so it cannot be re-armed per task; we do
not use it. The loop limit throws a catchable error and counts per frame, so
`for(;;){ try{ f() }catch(e){} }` with a looping `f` is not bounded by it.
Native builtins (`/(a+)+$/`, a 3-million-element `sort`, `'x'.repeat(2**30)`)
cannot be interrupted at all. So three layers, each needed:

1. **In-engine, cheap, first line.** Set the loop limit (start 10,000,000 per
   frame), recursion 256, stack, and `max_buffer_size` 256 MiB. This catches the
   common `while(true){}`, deep recursion and a huge `ArrayBuffer`. Errors are
   rethrown as uncatchable by the host: after the first limit error the host sets
   `terminated`.
2. **Deadline, out of the engine.** A watchdog thread in the content process
   compares a heartbeat the JS thread bumps at every task and microtask boundary
   with the clock. If no boundary passes for **10 seconds** (starting value) it
   calls `std::process::abort()`. A dead content process is routine to the broker
   (§4.3), and the tab shows "page unresponsive". The broker keeps its own
   per-request deadline as the backstop (already in `px-broker`). This is the
   layer that makes "survives `while(true){}`" a property of the process and not
   of the engine. A cooperative interrupt in Boa is the better fix and should be
   proposed upstream; we do not fork Boa.
3. **Memory.** Boa has no cap and allocation failure aborts the process. The
   real cap is Phase 17 (cgroup `memory.max`, job object), outside this phase's
   gate. Until it lands a memory bomb is limited only by the host machine; the
   ADR says so rather than imply layer 1 covers it. If `px-sandbox` can apply an
   address-space limit cheaply on Linux, pulling that forward is worth doing and
   is the user's call (decision 5).

Gate coverage: a page with `while(true){}`, one with recursion, one with
catch-and-continue, and one with regex backtracking each end with the content
process either reporting `ScriptTerminated` or dead within the deadline, and the
broker alive in every case. These run in `cargo xtask script`.

## WASM recommendation

**Boa 0.22 has no `WebAssembly` global** (zero occurrences in its source). So
"no WASM" is the default and costs nothing. Options and the wasm-interpreter
crate are in the research doc, section 6; none was probed, so no number is given
for a wasm interpreter and I will not fabricate one.

**Recommendation: no WebAssembly in v1.** Reasons: a wasm module is an untrusted
binary parsed and validated in the content process, a new fuzz target and a new
parser surface, plus a JS API surface (`Module`, `Instance`, `Memory`, `Table`)
and its own memory cap; the dependency directive says as few as possible; and the
compat gate (Phase 23, forty real sites) is the instrument that says which sites
fail because `WebAssembly` is undefined. Make the decision on that data, not on
the claim in §6 that "much of the web assumes it". Libraries that feature-detect
fall back to JS. Phase 10 then **records**: `typeof WebAssembly === "undefined"`
is tested and deliberate, and the preferences page documents it beside the no-JIT
statement. The decision is reopened by its own ADR with a measured crate.

## Alternatives rejected

- **`intl_bundled`** (+31 crates, baked locale data of size not measured): gives
  `Intl`. Rejected for now, at a cost: `Intl` is `undefined` and `toLocaleString`
  is unlocalised, which real sites will hit. Revisit with compat-gate data.
- **`temporal`** (default-on, +10 crates): no site needs it yet.
- **Boa's `SimpleJobExecutor` and `boa_runtime`:** unbounded queues, and a
  runtime that brings `fetch`/`futures`. Both would hide the thing the gate tests.
- **Boa's `fuzz` instruction budget as the limit:** fixed at build time; not
  re-armable.
- **Forking Boa to add an interrupt hook:** carries the cost of a fork of the
  largest unsafe-bearing crate in the content process forever. Upstream instead.
- **Interrupting script from another thread:** `Context` is `!Send`.
- **No in-process watchdog; rely on the broker deadline alone:** workable, but
  then the content process's own queues have no liveness check, and tests of the
  content binary in isolation cannot assert the property.
- **Running script in the broker for convenience:** forbidden by invariant 1.

## Consequences

- +73 crates, +5,143 unsafe tokens, baseline 26,818 after this lands; `cargo
  vet` exemptions up by about the same crate count; `px-content` compiled twice
  with 73 more crates in the doubled closure. Build time of CI rises visibly.
- A hostile script can make a tab die or stop its own script; it cannot make the
  broker or another tab do either (that is the point of layers 1 and 2).
- Until Phase 17 a memory bomb is not contained.
- No `Intl`, no `WebAssembly`, no `SharedArrayBuffer`, no `Temporal`: some pages
  break. The compat gate is where that is measured.
- Pinning `=0.22.0` means every Boa upgrade is a deliberate, tested change.
- Phase 10 cannot touch the DOM (Phase 11): see `docs/phase-10-plan.md` for what
  that means for the gate.

## Verification

The decision is wrong if: (a) the layer-1 and layer-2 tests show a runaway page
that neither terminates the script nor kills only its own process; (b) the
queue-bound high-water marks in the stress page exceed their caps; (c) the
`CoarseClock` property tests fail for any real clock series; (d) Boa's measured
speed makes the compat sites unusable (the §11 risk row: "options are optimizing
Boa, accepting slowness, or reopening invariant 6 by ADR"; that measurement is a
Phase 10 deliverable and must be taken before Phase 11 starts); (e) `Intl` being
absent breaks more than a handful of the forty sites, which would flip the
`intl_bundled` decision. (f) there is no observation that would falsify "no
WASM in v1" before the compat gate runs; that is why it is deferred, not closed.

## Spec findings (reported, not acted on)

1. §6 says "roughly 96% test262 ... tracking 1.0". Boa's README says "more than
   90%" and the latest release is **0.22.0**. The 96% could not be confirmed and
   it is not a figure for our feature set.
2. Phase 10 cites "§Spectre"; no such section exists. Specified above.
3. §6 says "zero panics"; Boa's 0.22 changelog is still converting panics to
   `EngineError::Panic`. Contained by `panic = "abort"` either way.

## Corrections and fill-ins (2026-11-05, Phase 10 close-out)

Recorded after the code and the tests existed. Nothing above was rewritten;
where a claim above is wrong, this section says so.

1. **A limit error is NOT catchable by script.** "Resource limits" above (and the
   research note) say the loop-iteration limit "throws a catchable error" and that
   `for(;;){ try{ f() }catch(e){} }` escapes it. In Boa 0.22.0 the limit raises
   `EngineError::RuntimeLimit`, which the VM treats as uncatchable
   (`JsError::is_catchable`); a `try/catch` around it does not run its `catch`
   block. Pinned by `script_limits_engine_the_limit_error_is_not_catchable_by_script`
   and `script_limits_engine_places_the_spec_says_swallow_errors_do_not_swallow_the_limit`.
   The host does not rely on it: any engine error crossing the native boundary
   terminates script (`TerminateReason::RuntimeLimit`). What the claim got right
   is the real hole, which is different: the loop count is per call frame, so a
   loop of CALLS to looping functions is not bounded by it, and native builtins
   (regex backtracking, a huge array fill) are not interrupted at all. Only the
   watchdog ends those shapes.
2. **Timer properties 4 and 5 conflict; the implementation follows the formula.**
   Property 4 ("within one quantum, repeated reads are identical") and property 5
   ("the number of distinct outputs over 10 ms is 100 +/- 1") cannot both hold:
   if a whole quantum returned one value, consecutive quanta rounding to the same
   grid point would repeat it and the distinct count would fall to about 75. The
   code implements the ADR's own formula (`up = hash(seed, q) % Q < frac`): 100
   +/- 1 distinct values, at most one change inside a quantum, and re-reading one
   raw instant is always stable. The tests assert that and say why. The wording of
   property 4 above is therefore superseded; the formula stands. (Still the user's
   to confirm; `docs/backlog.md` has the entry.)
3. **Memory bomb, after Phase 17 (ADR 043).** Layer 3 above said a memory bomb is
   uncontained until Phase 17. On Linux Phase 17 added a 1 GiB `RLIMIT_AS` applied
   before exec and confirmed from `/proc/<pid>/limits`; it is in force and proven
   by `sandbox_memory_*` tests. On Windows nothing caps the heap and the Phase 17
   gate is not met there, so a memory bomb remains uncontained on Windows. The cap
   is address space, not resident memory (`docs/backlog.md`).
4. **A timer delay is a WebIDL `long`.** Found by the Phase 10 fixture table: the
   host clamped `setTimeout(f, 2**31)` to the 32-bit ceiling, but the delay
   converts with `ToInt32`, so `2**31` and `Infinity` are zero (as in browsers).
   Fixed in `px-script`; the row `delay_two_to_the_31_wraps_to_zero` pins it.
5. **Maintainers and audit status of the new crates (the blanks above).**
   - cargo-vet (`supply-chain/config.toml`, `imports.lock`, read 2026-11-05):
     the store imports Bytecode Alliance, Embark, Google, ISRG and Mozilla audits.
     **All seven Boa crates are exemptions** (`safe-to-deploy`, version 0.22.0):
     nobody whose audits we import has read them, and nobody here has. Of the 132
     non-first-party crates in `px-script`'s tree, 86 are exemptions (some shared
     with `stylo`) and 46 have no exemption, which `cargo vet` can satisfy only
     through an imported audit. That last statement is inferred from the store;
     `cargo vet` could not be run in the environment that wrote this (not
     installed), and the newest completed CI run's `supply-chain` jobs failed on
     the store's formatting (fixed on `main` afterwards, not yet re-run in CI), so
     "the vet gate passes" is NOT established here.
   - crates.io owners (API read 2026-11-05) for the 64 crate names this
     dependency added to `Cargo.lock` (`Cargo.lock` at the commit that added Boa
     versus the one before it; the ADR text counts 73 name+version pairs). Of the 64:
     39 list exactly one individual and no team; 21 list a team (alone or beside
     individuals); 4 list two or three individuals and no team. Seven are owned
     by the `boa-dev:owners` team (`boa_ast`, `boa_engine`, `boa_gc`,
     `boa_interner`, `boa_macros`, `boa_parser`, and `ryu-js`); the first six also
     list `jasonwilliams`. `boa_string`, `small_btree` and `tag_ptr` list
     `jasonwilliams` alone. **One named individual with publish rights is the
     common case in the tree** (`memchr`, `time`, `portable-atomic`,
     `intrusive-collections`, `bytemuck` and others), as it is across the Rust
     ecosystem; that is a statement about owners, not about active maintainers
     or reviewers.
   - **Unknown, and not guessable from here:** how many people are in the
     `boa-dev:owners` team (the GitHub organisation API is not reachable from the
     build environment); how many are active; whether any of the 64 crates has
     been audited by anyone outside the imported sets. "Maintainer count" in the
     dependency block is therefore an owner listing, not a count of maintainers.

## What the user must decide

1. **Approve adding `boa_engine =0.22.0`** (+73 crates, baseline 21,675 ->
   26,818), and whether `annex-b` is on (recommended: yes; real sites use
   `__proto__`, HTML-style comments and `escape`).
2. **`Intl` absent in v1 (recommended) or `intl_bundled` now** (+31 crates and
   locale data).
3. **Confirm the timer specification** above, since the spec's "§Spectre" does
   not exist, including seed-from-broker and no `SharedArrayBuffer`.
4. **WASM: none in v1 (recommended)**, decided again after the Phase 23 compat run.
5. **Runaway scripts:** accept a 10 s watchdog abort for the hung-page case, and
   say whether to pull a Linux address-space limit forward from Phase 17 so a
   memory bomb is contained in Phase 10 or accept the gap until then.
6. **Maintainer and `cargo vet` audit status** of the new crates, which I could
   not check from here, must be filled in before this is marked accepted.

---

## Dependency ADR block (template)

- **Crate and version:** `boa_engine` =0.22.0, `default-features = false`,
  `annex-b` (pending decision 1)
- **What it does:** ECMAScript lexer, parser, bytecode compiler and interpreter
  in Rust, with `boa_gc` tracing GC. Interpreter only.
- **Why not write it:** a conformant ES2025 engine is years of work; Appendix A
  settles reuse.
- **Maintainer count:** crates.io lists, for `boa_engine` and five of the other Boa
  crates, one user (`jasonwilliams`) plus the GitHub team `boa-dev:owners`;
  `boa_string` has the user alone. The team's membership is not readable from here,
  so the number of people who can publish Boa is UNKNOWN. See "Corrections".
- **Audit status:** all seven Boa crates (`boa_ast`, `boa_engine`, `boa_gc`,
  `boa_interner`, `boa_macros`, `boa_parser`, `boa_string`, 0.22.0) are cargo-vet
  EXEMPTIONS in `supply-chain/config.toml`: trusted, not audited by anyone. None
  of the five imported audit sets covers them (`imports.lock` has no `boa`
  entry). See "Corrections".
- **Unsafe-line count:** Boa's seven crates 604; tree total 8,025; new to the
  baseline 5,143; baseline 21,675 -> 26,818.
- **Transitive dependencies added:** 73 new crates (66 non-Boa), full list in
  `docs/research/phase-10-boa.md` method; 132 in the tree.
