# Phase 10 gate report

*Script host: Boa, the event loop, timers, limits. Built on `main` through
`phase10/*` branches; this report is the close-out branch `phase10/closeout`,
2026-11-05.*

Read the second table before the first. **Phase 10 is built, not closed.** The
gate has four items (plus an implicit fifth); every one has tests that pass
locally on Linux, and the test262 ratchet is green on both operating systems in
the last completed CI run. What does not exist is a CI run of the *whole* gate,
green on both OSes, on a commit that contains the fixture table and this report:
the newest completed run predates both, and the `script` job in it failed on
Windows for a reason that has since been fixed on `main` but not yet re-run.
The ticks below say where each item is proven. The section after them says what
a tick does not mean.

## The four gate items (build-spec section 9)

| Item | Where | Result |
|---|---|---|
| test262 subset at threshold | `cargo xtask test262`; `crates/px-script/tests/test262.rs` (`script_test262_`, 9 tests); `tests/test262/{RESULTS,EXPECTED_FAILURES,PROVENANCE}` | **Met on the vendored subset, by the project's own ratchet, on Linux and (per CI) Windows.** 10,077 of 10,233 graded pass (98.48%); 156 recorded expected failures; 123 ungraded. Read "What 98.5% means" before the tick |
| event ordering tests | `crates/px-script/tests/ordering.rs` (21, `script_order_`); `crates/px-script/tests/fixtures.rs` + `fixtures/table.rs` (52 in-process, `script_fixture_`); `crates/px-content/tests/script_fixture_process.rs` (19 through the real content process) | **Met for loop-level ordering, on Linux.** Every table expectation is derived from the HTML and ECMAScript text in the row, not from a run. DOM-level ordering is Phase 11 and is not claimed |
| no unbounded task-queue growth under a stress page | `crates/px-script/tests/stress.rs` (13, `script_stress_`); `px-content/tests/script_loop.rs` (5); the bound rows of the fixture table; `script_limits_*` (40) and `script_broker_*` (31) for the runaway shapes | **Met on Linux.** Every high-water mark (task queue, microtask queue, timers, microtasks per checkpoint) is asserted at or under its cap; runaway shapes end in `terminated` or a dead content process with the broker alive |
| timer resolution asserted | `crates/px-script/src/clock.rs` (11 pure properties, `script_clock_prop_`), `tests/clock_host.rs` (8), `px-content/tests/script_clock.rs` (`script_clock_process_`, 5, native-boundary log), fixture rows for `performance.now`, `Date.now`, `Math.random` | **Met on Linux, with the property conflict below.** Outputs are multiples of 100 us, within one quantum of true time, monotonic |
| (implicit) script only in the sandboxed content process | `script_confinement_` (4), `script_broker_sandbox_` (7), the broker links no `boa_*` (a test enforces it) | **Met on Linux.** Not met on Windows: the content process has no stage-two profile there (docs/backlog.md, Phase 17) |

CI state, from the GitHub Actions API on 2026-11-05 (the section "What CI says"
has the detail): newest completed `gate` run for `main` is `41920613954`
(commit `ab0907ee`). `test262` passed on both OSes with identical counts.
`script` failed on both OSes; on Ubuntu the only failures were the two items
this branch supplies (the unit 6 table and this file); on Windows there was one
more, a test failing on a path-escaping bug that `main` has since fixed
(`7ae6d82a`). No completed run contains that fix. Two newer runs exist and are
not finished.

## What each tick does and does not mean

**98.5% is not a test262 pass rate.** It is 10,077 of 10,233 *graded* tests in a
subset the vendoring tool selected (`crates/px-xtask/src/gates/vendor_test262.rs`):
10,356 of the 54,078 files under `test/`. The selection is by feature and by
sampling. Whole directories are dropped (`intl402` 3,382, `Temporal` 4,613,
`staging` 1,495, `Atomics`, `SharedArrayBuffer`, `ShadowRealm`), files are dropped
by `features:` (dynamic-import 968, SharedArrayBuffer 117, and others) and by
flag (module 818), and 31,043 files are *sampled out* at 117/1000 per directory so
the corpus fits the 15 MB budget. The dropped features are exactly the ones this
engine build lacks or the project does not ship (ADR 040), so the figure answers
"how much of the language we ship does Boa 0.22 get right, on a sample", which is
a fair question and a different one from "what is Boa's test262 rate". The spec's
"roughly 96%" was never confirmed and this number does not confirm or refute it.

**The threshold is our own first measurement.** `MATCH_FLOOR = 10,077` in
`crates/px-xtask/src/test262_core.rs` was set from the first honest run (debug
profile, 4 workers; two runs gave identical counts). The plan said to do exactly
this rather than borrow a number, and it is what makes the gate a *ratchet*: a
test that passed may not start failing, and the floor only rises. It is not a
bar set independently of the engine, and it passes by construction on the day it
was set.

**123 tests are excluded from grading, not failed.** They need harness hooks the
host does not provide: `$262.createRealm` (50), `$262.detachArrayBuffer` (71), and
2 others that the RESULTS file does not classify and this report did not
re-derive (`agent`, `IsHTMLDDA`, or an absent-by-ADR global). Counting them as
failures, the rate is 10,077 of 10,356, or 97.3%. By area: language 99.43%,
annexB 99.26%, built-ins 96.86% (graded). Built-ins is where the failures
concentrate (119 of 156).

**The 156 expected failures are not hidden.** They are in `EXPECTED_FAILURES`
with a class each: unexpected-typeerror 40, assertion 32, unexpected-throw 30,
async-failed 19 (mostly `Array.fromAsync`), syntax-rejected 16, limit-terminated
11, negative-wrong-error 4, async-no-done 2, engine-panic 2. Two tests make Boa
panic (the CI log shows the panic from `boa_engine-0.22.0/src/vm/opcode/define`);
the test262 runner catches that, the shipping content process is `panic = abort`
and would die, which the broker treats as an ordinary crash. A failure not on the
list, or a listed test that starts passing, fails the gate.

**The test262 runner is not the content process.** It runs each test in a thread
inside the test binary on a *manual clock* (timers cost no wall time) with a hard
per-test deadline. It proves the engine and our host's globals, not the process
boundary, the sandbox, the real clock or the watchdog. Those have their own
suites.

**"Event ordering" is loop ordering.** HTML's ordering also covers event
dispatch, `<script>` parse-blocking and `async`/`defer`, `requestAnimationFrame`
against rendering, `MutationObserver` timing, `postMessage` and fetch task
sources. None of those exist before Phase 11 and none is claimed. The six
`scripted_*` html5lib tests are still open for the same reason.

**The fixture table's expectations are the spec's, and one disagreed.** Each of
the 49 rows carries the passage it was derived from. Written before running,
they found one real defect: `setTimeout(f, 2**31)` and `setTimeout(f, Infinity)`
were clamped to the 32-bit ceiling (about 24 days), but the delay is a WebIDL
`long` converted with `ToInt32`, so both are zero. Fixed in `px-script`
(`delay_ms`, `runtime.rs`) in the unit 6 commit; no row is `#[ignore]`d. The
table also pins two behaviours that are *choices*, not spec: a self-re-arming
zero-delay timer is never terminated (one live timer, queue depth one, the driver
cuts it after 5,000 tasks) and the 20,000-timer page is stopped at the 10,000
cap (`TimerOverflow`). HTML bounds neither.

**What the table does not cover.** Only 16 of 49 rows also run through the
real content process, because only those stay true on a loaded machine; the
close-spaced timing rows (a 2 ms timer against the 4 ms nested clamp, a 3 ms
timer overtaking a 10 ms one) run on the manual clock only. Unhandled promise
rejections are not reported anywhere (see below), so the table can pin only that
the page survives them.

**"No unbounded growth" is proven with counters, not with memory.** ADR 040 also
promised that resident memory after the page stopped did not grow with the
iteration count; the tests assert the high-water marks and the termination
reason and do not read RSS. The caps (4,096 tasks, 10,000 timers, 65,536
microtasks, a 1,000,000 per-checkpoint budget) are the ADR's *starting values*,
never tuned by measurement. The high-water counters exist only under the
`testing` feature and are scanned for in release artifacts (`cargo xtask sandbox`).

**"Timer resolution asserted" is a statement about arithmetic, not about
Spectre.** The clock rounds to 100 us with seeded jitter, and the properties are
asserted over a fake raw clock and, at the native boundary, over the real one.
This blunts a cache-timing primitive and does not remove it (ADR 040 says so);
the real boundary is the process. Properties 4 and 5 of the ADR conflict and the
code follows the ADR's formula (see ADR 040, "Corrections").

**Everything ran in the debug profile, on Linux, locally.** No test or timing in
this report is from a release build. How fast Boa runs CPU-bound scripts, which
the plan assigned to this unit ("Time Boa on a fixed set of CPU-bound scripts"),
was **not measured**; the spec's "10 to 50 times slower than V8" is unverified
and the section 11 options have not been weighed against a number. Whether the
`MATCH_FLOOR` and the stress thresholds hold in a release build, where timings
shift, is also untested.

## What CI says (read 2026-11-05, not assumed)

`gh api repos/TokenGoblin/Pelorus/actions/runs?branch=main&per_page=5` gave five
`gate` runs on `main`. Two were `queued` or in progress (`41945498469` on
`97740c20`, 36 jobs queued; `41921149529` on `714ced3d`, 29 of 36 complete). The
newest **completed** run is `41920613954` on `ab0907ee` (2026-11-04 21:55 UTC),
overall conclusion **failure**. Its jobs relevant to Phase 10:

| Job | Ubuntu | Windows | Note |
|---|---|---|---|
| `test262` | success | success | Same counts on both: pass 10,077, fail 156, ungraded 123, 98.48%; "ratchet holds". 92 s on Ubuntu, 152 s on Windows (debug) |
| `script` | **failure** | **failure** | Ubuntu: the only `FAIL` lines are `remaining (unit 6): ... no tests matching 'script_fixture_'` and `remaining (unit 7): docs/phase-10-gate-report.md does not exist`; every test binary before them passed. Windows: the same two, and `script_order_the_default_engine_would_have_read_the_file` failed (the positive control read no file because the Windows manifest path was not escaped inside a JS string); commit `7ae6d82a` fixed it afterwards and no completed run contains it |
| `supply-chain` | failure | failure | `cargo deny check` passed (advisories, bans, licenses, sources ok); `cargo vet --locked` failed on the store's consistency (formatting), not on an unaudited crate. `e289bc69` ran `cargo vet fmt` afterwards; not yet re-run |

Other red jobs in that run (`build (windows)`, `ipc (windows)`, `sandbox
(windows)`, `layout (windows)`, `style (windows)`, `dom (windows)`, `flex-grid
(windows)`, `network (windows)`, `fuzz-smoke`, `compat-list`) are not Phase 10's;
`7ae6d82a` and later commits target several of them. They matter here only because
"green on both OSes" cannot be said while they stand. Both Phase 10 jobs carry
`continue-on-error: true` in `.github/workflows/gate.yml` (lines 354 and 465),
so a job whose conclusion reads `success` there is not by itself a pass; the
`test262` log line `test262: PASS` is.

This report does not claim anything about the two newer runs. **Windows results
for the fixture table, the process rows and this report's other tests do not
exist; "see CI" is the whole of it.**

## ADR 040 corrections and fill-ins

Recorded in the ADR itself (`docs/adr/040-...`, section "Corrections and
fill-ins"), summarised here:

1. **A limit error is uncatchable by script.** The ADR and the research note said
   it was catchable. In Boa 0.22.0 it is `EngineError::RuntimeLimit`, which the VM
   treats as uncatchable; the host terminates on any engine error anyway. The real
   hole is different: the loop count is per call frame, and native builtins are
   not interrupted, so only the 10-second watchdog ends regex backtracking, a huge
   array fill and loops of calls.
2. **Timer properties 4 and 5 conflict.** "Identical within a quantum" and "100
   distinct values per 10 ms" cannot both hold. The implementation follows the
   ADR's formula (100 +/- 1 distinct, at most one change per quantum). The user
   should confirm the formula over the wording; `docs/backlog.md` has the entry.
3. **Memory bomb after Phase 17.** Linux: a 1 GiB address-space limit is applied
   before exec, confirmed from `/proc/<pid>/limits`, and proven by `sandbox_memory_*`
   (ADR 043). **Windows: nothing caps the heap; the bomb is uncontained there.**
   The cap is address space, not resident memory.
4. **Maintainers and audit status (the blank the ADR left).** From the committed
   `supply-chain/` store: **all seven Boa crates are cargo-vet exemptions**
   (`safe-to-deploy`, 0.22.0): none of the five imported audit sets (Bytecode
   Alliance, Embark, Google, ISRG, Mozilla) covers them and nobody here has read
   them. Of the 132 third-party crates in `px-script`'s tree, 86 are exemptions
   (some shared with `stylo`) and 46 are not, which vet can satisfy only through an
   imported audit (inferred from the store; `cargo vet` could not be run where this
   was written). From the crates.io owners endpoint, which was reachable: of the 64
   crate names Boa's addition put in `Cargo.lock`, 39 list one individual and no
   team, 21 list a team, 4 list two or three individuals; the `boa_*` crates are
   `jasonwilliams` plus the `boa-dev:owners` team (`boa_string`: him alone).
   **Unknown:** how many people are in that team and how many are active (the
   GitHub organisation API is not reachable from the build environment), and
   whether any crate in the tree has been audited by anyone outside the imported
   sets. An owner listing is not a maintainer count; the ADR says so.

## Not met or not proven

- **Windows, all of it.** No completed CI run contains the fixture table, the
  process rows or this report. The sandbox profile for the content process does
  not exist on Windows (stage two, memory cap, escape suite), so the implicit
  "script only in the sandboxed content process" item is not met there. A memory
  bomb is uncontained on Windows.
- **A CI run of the whole `script` gate that is green.** Not yet; see above.
- **Release-profile behaviour.** No release build of the content process was run
  against the stress page, the clock tests or test262. Speed is unmeasured.
- **DOM-touching tests (Phase 11).** No DOM, no `window` events, no
  `unhandledrejection`, no parse-blocking, no `requestAnimationFrame`.
- **Unhandled promise rejections are silent.** The host installs no rejection
  tracker and there is no console; HTML would fire `unhandledrejection` and then
  report. Not script-visible before Phase 11, so not asserted beyond "the page
  survives" (backlog entry added).
- **WebAssembly has no recorded decision.** `typeof WebAssembly` is `"undefined"`
  and tested; the ADR the plan promised is now drafted as
  `docs/adr/047-webassembly-in-v1.md`, status **proposed**: none in v1, revisit
  after the Phase 23 compat run. It needs the user.
- **ADR 040 timer wording** (properties 4 and 5) awaits the user's confirmation.
- **Caps are untuned starting values; resident memory is not asserted.**
- **Vet and owners:** see correction 4. Unaudited Boa is a standing supply-chain
  fact, not a finished item.

## What was built

| Unit | Result |
|---|---|
| 0 gate tests, `cargo xtask script` | Gate with named suites per item; `REMAINING` is now empty |
| 1 embedding | `boa_engine =0.22.0` (default features off, `annex-b` on) in `px-script` only; `RunScript` / `ScriptResult` over IPC, size-capped; broker links no `boa_*` |
| 2 event loop and queues | Bounded task, microtask and timer queues, per-checkpoint budget, terminated state, testing-only high-water marks |
| 3 timers and the coarse clock | 100 us grid, seeded round-up jitter, monotonic; `performance.now`, `Date.now`, seeded `Math.random` |
| 4 resource limits | Engine limits, uncatchable limit errors, 10-second watchdog abort, runaway-shape and two-tab isolation tests, honest memory tests |
| 5 test262 | 10,356 tests vendored (14.9 MB, pinned commit `831359a4`), Boa-bound runner, ratchet and `MATCH_FLOOR` |
| 6 fixture table | 49 spec-derived rows, in-process and (16) through the real content process; one defect found and fixed |
| 7 this report | With ADR 040 fill-ins and ADR 047 (proposed) |

Dependencies added in the phase: `boa_engine =0.22.0` and its closure (ADR 040:
73 name+version pairs new to `Cargo.lock`, +5,143 unsafe tokens in the baseline).
This branch adds none.

Local results on this branch, Linux, debug: `px-script`, `px-content`, `px-ipc`,
`px-broker` tests with and without `--features testing` pass; the fixture table
takes about 42 seconds in debug (not profiled; the rows that run a million
microtasks are the likely cost);
fmt and `clippy -p px-script -p px-content -p px-xtask --tests --all-features`
clean.

## Closing the phase

What closes it, exactly:

1. A CI run of `gate` on a commit containing this branch, **green on both
   operating systems for the `script` and `test262` jobs** (and, since "green on
   both" is the rule, the Phase 10 gate's dependencies: `sandbox`, `supply-chain`
   and the Windows jobs red today).
2. In the phase's **last commit**, remove `continue-on-error: true` from the
   `script` and `test262` jobs in `.github/workflows/gate.yml`.
3. The user's decisions still open: ADR 047 (WebAssembly), the timer wording of
   ADR 040, and whether the Windows memory-cap gap is accepted for Phase 10.

Until then the phase is **built, not closed**.
