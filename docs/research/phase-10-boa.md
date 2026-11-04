# Phase 10 research: what `boa_engine` costs and what it offers

*Research only. No `Cargo.toml`, `Cargo.lock` or source file was changed.
Date: 2026-11-04. Measured against `origin/main` (Rust 1.98.1). Appendix A
settles "Boa, interpreter only"; nothing here re-argues it. Two spec claims look
wrong and are reported in section 9, not acted on.*

**What was and was not measured.** Dependency counts, unsafe counts, `cargo
check`, feature sets and every API statement come from a throwaway crate and from
reading the `boa_engine` 0.22.0 source. **No script was executed.** The disk
filled twice while building the probe (other sessions share the volume), so the
runtime behaviours in section 5 (what a `while(true){}` or a memory bomb does)
are read from source and marked *from source, not run*. Phase 10 unit 4 must run
them before anyone relies on them.

## 1. Version and licence

| Item | Value | Source |
|---|---|---|
| Latest stable that resolves | `boa_engine` **0.22.0** (released 2026-08-27) | `cargo search`, `CHANGELOG.md` |
| Boa's MSRV | 1.95.0, edition 2024 (we pin 1.98.1: fine) | Boa `Cargo.toml` |
| Licence | `Unlicense OR MIT` | Boa `Cargo.toml` |
| Version vs the spec's "tracking 1.0" | **There is no 1.0.** Latest is 0.22 and the API still breaks between minors (0.21 deprecated `utc_now` for `Clock`; `JobExecutor` replaced the old queue trait). Pin exactly (`=0.22.0`). | section 9 |

## 2. Features and what pulls what

`boa_engine` 0.22.0 declares `default = ["float16", "xsum", "temporal"]`. Features
that matter (from its `Cargo.toml`):

| Feature | Effect | Phase 10 verdict |
|---|---|---|
| `temporal` | `temporal_rs`, `timezone_provider`, `icu_calendar` | **off** (default-on; the `Temporal` global is not needed by the web today; +10 crates) |
| `intl` / `intl_bundled` | the `Intl` object: ~10 `icu_*` crates, `sys-locale`, `yoke`; `intl_bundled` adds `boa_icu_provider` (baked locale data, large) | **off**; costs +31 crates with `intl_bundled` |
| `float16`, `xsum` | `Float16Array`, exact `Math.sumPrecise` | off |
| `js`, `trace` | wasm32 *target* support (`web-time`, `wasm-bindgen`) | off (this is Boa running *in* a browser, unrelated to WebAssembly *in* Boa) |
| `system-time-zone` | `iana-time-zone` for local tz | off; we supply the offset by `HostHooks` |
| `annex-b` | web-legacy syntax (`__proto__`, HTML comments, `escape`) | **ask**: real sites need Annex B. See section 8; tiny (Boa's own tester enables it) |
| `fuzz` | global instruction budget (section 5) and `arbitrary` derives | not for shipping; +2 crates |
| `deser`, `embedded_lz4`, `flowgraph`, `experimental`, `native-backtrace` | tooling | off |

So the interpreter-only set is `default-features = false` plus (probably)
`annex-b`. `Date`, `Math`, `Promise`, `Proxy`, typed arrays, `Atomics`,
`SharedArrayBuffer`, `BigInt`, `RegExp`, `JSON`, async/generators exist with no
feature. `String.prototype.normalize` is built in through `icu_normalizer`
(compiled data) even with `intl` off.

**Without `intl`, `Intl` is `undefined`** and `toLocaleString` falls back to
non-localised output. Many sites call `Intl.DateTimeFormat` or `NumberFormat`;
this is a real compatibility cost and a decision for the user (ADR 040).

## 3. Dependency numbers

Method: `cargo tree --edges normal --prefix none` for the Linux target, with the
repo's own `Cargo.lock` copied into the probe so version resolution prefers the
versions we already lock. "Already in repo" means the **exact name and version**
appears in `/Cargo.lock` (192 package entries there, 21 first-party). Windows
target: 131 crates (Linux 132). `--edges normal,build --target all`: 136, the
extra four being target-only (`windows-link`, `r-efi`, `redox_syscall`,
`autocfg`).

| `boa_engine` features | Unique crates (Linux, normal edges, excluding the probe) |
|---|---|
| `default-features = false` | **132** |
| `+ temporal` (the default set minus float16/xsum) | 142 |
| `+ intl_bundled` | 163 |
| `+ temporal + intl_bundled` | 167 |
| `+ fuzz` | 134 |

For the recommended set (`default-features = false`):

| Measure | Number |
|---|---|
| Crates in the tree | 132 (7 `boa_*`, 125 other) |
| Already in `Cargo.lock` at the same version | 59 (via stylo: `url`, `idna`/ICU, `serde`, `smallvec`, `libc`, `itoa`, ...) |
| **New to `Cargo.lock`** | **73** = 7 `boa_*` + 66 others |
| ... new by name (not merely a new version of a locked crate) | 63 |
| Hard (non-optional) dependencies that are surprising for an interpreter | `serde_json`, `serde`, `rand` 0.10 (+`chacha20`, `getrandom` 0.4), `time` (+macros), `dashmap`, `async-channel`, `futures-lite`, `futures-concurrency`, `oneshot`, `event-listener`, `intrusive-collections`, `bytemuck`, `phf`, `regress`, `num-bigint` |

`serde_json` is a hard dependency of `boa_engine`. The root rule "no serde_json
in IPC" is about the wire format and is unaffected, but `serde_json` will be in
the content process binary. The async crates exist because `JobExecutor` has an
`async fn run_jobs_async`; they are linked even if we never use that path.

**Build requirements.** None of the 132 crates uses `cc`, `cmake` or `bindgen`.
No Boa crate has a `build.rs`; the 15 `build.rs` in the closure are cfg probes
(`libc`, `serde`, `proc-macro2`, `getrandom`, ...). **No C compiler, Python or
cmake is required.** This is a real contrast with stylo (ADR 023, Python+Mako).
`cargo check` on the recommended set passes (36 s cold, 341 MB target, deleted).

## 4. Unsafe

Counted the way `crates/px-xtask/src/gates/unsafe_audit.rs` counts: the regex
`(?:^|[^\w])unsafe(?:[^\w]|$)` over every `.rs` file in the crate's source
directory, tests and benches included, text not AST, over-counting by design. The
regex was reproduced per file in a scratch script and not run through the xtask
gate itself.

| Measure | Number |
|---|---|
| `ci/unsafe-baseline.json` today | 21,675 tokens over 109 crates |
| Tokens in the 132-crate Boa tree, total | 8,025 |
| ... in the 42 crates already in the baseline at the same version | 2,882 (no change to the baseline) |
| ... in **crates new to the baseline** (55 with a non-zero count) | **5,143** |
| **Baseline after Boa** | **26,818 (+5,143, +23.7%)** |
| Boa's own seven crates | `boa_engine` 293, `boa_gc` 180, `boa_string` 97, `boa_interner` 23, `boa_macros` 9, `boa_parser` 2, `boa_ast` 0 = **604** |
| Largest new non-Boa contributors | `portable-atomic` 727, `intrusive-collections` 418, `bytemuck` 391, `memchr` 340, `hashbrown` 0.14.5 326 (a second hashbrown), `allocator-api2` 277, `time` 274, `pin-project` 223 |

So the **GC is 180 tokens, not the headline**. `boa_gc` is where the *kind* of
unsafe matters (a tracing GC with `unsafe impl Trace`; a mistake is a
use-after-free reachable from script), but by count 85% of the increase is
ordinary crates: atomics, intrusive lists, byte casting. The honest statement for
the ADR is "+5,143 tokens, of which 604 are Boa's and the GC is the part that can
be exploited from script".

Not counted: `unsafe` inside macro-generated code (`boa_macros` expands into
user crates). The baseline over-counts comments and strings, so this is an upper
bound on written `unsafe`, not a count of unsafe blocks.

## 5. What Phase 10 needs and how Boa exposes it

### 5.1 Embedding and eval

`Context::builder()...build()`, `context.eval(Source::from_bytes(..))` returns
`JsResult<JsValue>`. Globals: `register_global_property`,
`register_global_callable`, `register_global_class` (what Phase 11 codegen will
target). `Context` holds `Rc`s and is **`!Send`**: one `Context` per thread, and
it cannot be touched from another thread (matters for interruption, 5.4). Panics:
the changelog shows Boa still converting panics into `EngineError::Panic`
(`js_expect`); content builds `panic = "abort"` (build-spec 4.3), so a panic in
Boa kills the content process, which the broker treats as routine.

### 5.2 Job queue, microtasks, timers

- `ContextBuilder::job_executor(Rc<impl JobExecutor>)`. The trait is two
  required methods: `enqueue_job(self: Rc<Self>, Job, &mut Context)` and
  `run_jobs(self: Rc<Self>, &mut Context) -> JsResult<()>`. `Job` is
  `PromiseJob | AsyncJob | TimeoutJob | IntervalJob | GenericJob |
  FinalizationRegistryCleanupJob`.
- **`enqueue_job` returns nothing.** Boa cannot be told "the queue is full". A
  bound must be enforced by the host (drop, flag, terminate), not by an error
  Boa propagates. This shapes the ADR.
- The shipped `SimpleJobExecutor` uses unbounded `VecDeque`s and a `BTreeMap` of
  timeout jobs keyed by `clock().now() + timeout`, and runs to quiescence:
  `run_jobs` drains until empty. We write our own executor (about 150 lines).
- `Context::run_jobs()` drives it; a host that wants HTML's "perform a microtask
  checkpoint" semantics calls it at the right points and uses its own queue for
  tasks.
- **Boa core has no `setTimeout`, `setInterval`, `queueMicrotask`, `performance`
  or `console`.** `boa_runtime` (separate crate, 0.22.0, `Unlicense OR MIT`)
  provides `console`, `setTimeout`/`setInterval`, `queueMicrotask`, `fetch`
  (via `reqwest`), `URL`, `TextDecoder`, `process`. Adopting it would break "no
  network outside px-net" (its fetch is optional but present) and bring
  `futures`; we do not recommend it. Timers are ours, which is what lets us
  coarsen them (build-spec section 9, Phase 10).
- `Job::PromiseJob` carries `JobCallback` with a `host_defined` slot
  (`HostHooks::make_job_callback`): this is how the incumbent-settings /
  realm bookkeeping a browser needs is attached to a job. Phase 11 uses it.

### 5.3 Clock, `Date`, `Math.random`

- `ContextBuilder::clock(Rc<impl Clock>)`; `Clock` has `now() -> JsInstant`
  (monotonic, nanosecond) and `system_time_millis() -> i64`. `Date` and the job
  executor's timers read this. **Injecting a fake clock is supported**, which is
  what makes deterministic tests of coarsening possible.
- `HostHooks::local_timezone_offset_seconds` is overridable (with `time`'s
  `local-offset`, the default asks the OS; we should answer UTC or a user
  setting).
- **`Math.random` is not injectable.** It calls `rand::random::<f64>()`
  (thread-local CSPRNG seeded from the OS). A fixed seed for tests means
  replacing the property after context creation with our own native function.
  That is a few lines, but it must be done and covered by a test.
- `performance.now()` does not exist in Boa; we define it over the coarsened
  clock.
- `SharedArrayBuffer` and `Atomics` exist in core. `Atomics.wait` honours
  `Context::can_block`. With no workers these cannot build a high-resolution
  counter; still, `SharedArrayBuffer` should be removed until Phase 12
  (cross-origin isolation) says otherwise.

### 5.4 Runaway scripts (*from source, not run*)

What Boa offers, `RuntimeLimits` on the context:

| Limit | Default | What it really bounds |
|---|---|---|
| `loop_iteration_limit` | `u64::MAX` (off) | Counts **per call frame** (`frame.loop_iteration_count`), incremented by the `IncrementLoopIteration` opcode in `for/while/do`. Exceeding throws a `RuntimeLimitError`. **Correction (Phase 10 unit 4, run and read from source): this was written as catchable and is not: in 0.22.0 it is an `EngineError`, which the VM treats as uncatchable (`JsError::is_catchable`), so `try { } catch { }` never sees it.** Work done inside a callee frame starts a new count, so `for(;;){ f() }` where `f` loops 1000 times is not bounded by it. |
| `recursion_limit` | 512 | call depth |
| `stack_size_limit` | 10 KiB (stack slots) | VM value stack |
| `backtrace_limit` | 50 | error traces |

Other findings:

- **No interrupt, deadline or cancellation hook in the interpreter loop.** There
  is no callback polled every N opcodes. A script cannot be stopped from another
  thread (`Context` is `!Send`).
- The `fuzz` feature adds a **global instruction budget** (`instructions_remaining`
  decremented in `execute_one`; exhaustion throws `NoInstructionsRemain`). It is
  set on the `ContextBuilder` only; there is no setter to re-arm it per task, and
  the feature name says fuzzing. Usable for tests, not as a production per-task
  budget unless upstreamed.
- Builtins that run in one native call are uninterruptible whatever the above:
  `/(a+)+$/` catastrophic backtracking, `Array.prototype.sort` over millions of
  items, `'x'.repeat(2**30)`, `new Array(2**32-1).fill()`.
- **Memory:** Boa has no heap cap. `boa_gc` has a *collection threshold*
  (1 MiB initial, grows) which decides when to collect, not when to refuse.
  `HostHooks::max_buffer_size` caps `ArrayBuffer` allocations (default 1.5 GiB,
  overridable) and nothing else. Strings, arrays, objects, closures are
  unbounded; allocation failure in Rust aborts the process.
- Therefore the content process survives `while(true){}` and a memory bomb only
  through things **outside Boa**: (a) a loop-iteration limit as a cheap first
  line for the common case, (b) a broker-side deadline on content (build-spec 4.4;
  `px-broker` already puts a deadline on every wait), killing only that process,
  (c) the Phase 17 memory cap (cgroup `memory.max` / job object). Phase 10 has
  (a) and (b); (c) is Stage D, so until then a memory bomb is bounded only by the
  host machine. Said plainly in the ADR.

### 5.5 WebAssembly in Boa

`boa_engine` 0.22.0 contains **no `WebAssembly` global**: zero occurrences of the
string in its source. Boa runs *on* wasm32; it does not run wasm. So "no wasm at
all" is Boa's default state, and any WASM support is a separate implementation
and ADR.

## 6. What "WASM ADR due" means

build-spec section 6: "WASM is undecided. A WASM interpreter is far simpler than
a JS JIT, and much of the modern web now assumes WASM exists. ADR required by
Phase 10." Section 13 item 2: "ship an interpreter, or not at all. Due Phase 10."
Phase 10 itself: "WASM ADR due." So the obligation is a **decision recorded by
the end of Phase 10**, not an implementation. Options (not measured; no WASM
crate was probed):

| Option | What it is | Cost |
|---|---|---|
| A. None in v1 | `typeof WebAssembly === 'undefined'`, libraries feature-detect and fall back to JS where they can | zero dependencies, zero parser surface; breaks sites that hard-require wasm (some image codecs, games, Google Meet-class apps, many "pro" web apps) |
| B. Existing Rust interpreter crate | a pure-Rust wasm interpreter (`wasmi` is the usual name) behind `px-script`, plus a JS API (`WebAssembly.Module/Instance/Memory`) | a new dependency tree whose size is **unmeasured**, an untrusted-binary parser plus validator (fuzz target needed), linear memory needs its own cap, and an interpreter is 10x+ slower than a JIT on the workloads that use wasm |
| C. Write our own | violates "take from the ecosystem" and is a large safety-critical parser | rejected on size |
| D. JIT (`wasmtime`) | reopens invariant 6 | out of scope by Appendix A's spirit |

## 7. test262

| Item | Fact |
|---|---|
| Upstream | `tc39/test262`, HEAD `831359a` (2026-11-04) when measured |
| Size, all of `test/` | **54,078 files, 86.7 MB** (whole repo 57,215 files, 90.4 MB); `harness/` 44 files, 311 KB |
| By directory | `language` 24,135 files / 43.6 MB; `built-ins` 23,864 / 25.5 MB (of which `Temporal` 4,613 / 5.1 MB); `intl402` 3,382 / 10.0 MB; `staging` 1,495 / 6.0 MB; `annexB` 1,086 / 1.5 MB |
| Core-ES subset (`language` + `built-ins` minus `Temporal` + `annexB`) | ~44,500 files, ~65 MB |
| Licence | "BSD License" (Ecma International 2012, 3-clause style, with an explicit no-patent-licence note). Each test also says "governed by the BSD license found in the LICENSE file". Vendorable with the licence file kept beside it. |
| Test format | one `.js` file with a YAML frontmatter comment (`description`, `includes: [harness files]`, `flags: [onlyStrict, noStrict, module, async, raw]`, `features: [...]`, `negative: {phase, type}`); 316 `_FIXTURE` module helper files |
| Host contract | needs `print`, a `$262` object (`createRealm`, `evalScript`, `detachArrayBuffer`, `gc`, `agent` for Atomics) and `async` tests need a `$DONE` print protocol |
| Boa's published rate | **Not obtained.** `boajs.dev/conformance` is outside this session's allowed network domains. Boa's README says "support for **more than 90%**" and gives no figure; the spec's "roughly 96%" is unverified here and is not a rate for *our* feature set (Boa's tester runs with `intl_bundled`, `experimental` and `annex-b` on and ignores a list of unimplemented features in `test262_config.toml`, which pins the test262 commit). |
| Boa's own harness | `tests/tester` in the Boa repo (92 KB of Rust, pinned commit, ignore list, per-edition results, panics counted separately). It is the shape to copy; it depends on `serde_yaml`, `rayon`, `clap` etc., which we will not. |

**How a subset gate would run here.** `crates/px-xtask/src/gates/vendor_wpt.rs`
(1,470 lines, std only) is the pattern: a developer tool that takes a pinned SHA,
selects files by a pure function over a file listing plus file contents, and
copies them under `tests/` with a provenance header. It reaches the network with
`gh api` or `git` children and has `--from-checkout <dir>`. Two differences:
(1) `gh` may not be authorised for `tc39/test262` (it was not in this session),
so the vendor tool should use `git clone --depth 1` as a child, which worked
through the proxy (18 MB pack); (2) there is **no YAML or JSON parser allowed in
xtask** (std only, no dependencies), so the frontmatter must be read with a small
hand parser for the fixed key subset above (`includes`, `flags`, `features`,
`negative.phase/type`), which is a few hundred lines and is itself unit-tested.
The runner is a Rust xtask gate that spawns the content process per test (or per
batch with a respawn on crash, since a crash is a result, not a harness failure)
with a wall-clock limit, so the runaway-script defence is exercised by the suite
itself.

Whole-subset vendoring of 65 MB of mostly small files into the repo is probably
too much (the whole of `tests/wpt` is 8.9 MB); see the plan for the options.

## 8. Other facts the ADR relies on

- `px-script` and `px-bindings` have no dependencies and `#![forbid(unsafe_code)]`;
  neither will change. Boa's unsafe lives in its crates, not ours. Our code
  stays at zero `unsafe`, including the executor and the clock.
- `px-content`'s closure is compiled twice for `panic = "abort"` (ADR 004). Boa
  adds the 73 new crates to that closure. ADR 004 predicted this lever.
- `docs/backlog.md` already records that six `scripted_*` html5lib tests become
  real failures when scripting lands; that is Phase 11 work, because it needs the
  DOM bindings.
- `Context` is `!Send`: one JS thread per content process, with the broker pipe
  handled on that thread or a separate thread feeding a channel.

## 9. Things in the spec that look wrong (reported, not acted on)

1. **"Roughly 96% test262 ... tracking 1.0"** (build-spec section 6). The Boa
   README says "more than 90%"; the latest release is 0.22.0, not 1.0. The 96%
   figure could not be confirmed. This does not touch Appendix A.
2. **"timers coarsened to 100 microseconds with jitter (section Spectre)"**
   (Phase 10, and `px-script/CLAUDE.md`). **There is no Spectre section** in
   `docs/build-spec.md`: "Spectre" appears only at lines 269, 383, 401 and 547,
   none of which specify the mechanism. The ADR therefore writes the
   specification itself and flags it for the user to confirm.
3. Section 6 says Boa has "zero panics". The 0.22 changelog is converting
   remaining panics into `EngineError::Panic`, so there are some. With
   `panic = "abort"` it is contained either way.

## 10. The 73 crates new to `Cargo.lock` (recommended feature set, Linux)

`aligned-vec 0.6.4`, `allocator-api2 0.2.21`, `async-channel 2.5.0`, `boa_ast`,
`boa_engine`, `boa_gc`, `boa_interner`, `boa_macros`, `boa_parser`,
`boa_string` (all 0.22.0), `bytemuck 1.25.2`, `bytemuck_derive 1.12.1`,
`chacha20 0.10.2`, `concurrent-queue 2.5.0`, `cow-utils 0.1.3`, `cpufeatures
0.3.1`, `dashmap 6.2.1`, `deranged 0.5.9`, `dynify 0.1.2`, `dynify-macros 0.1.2`,
`equator 0.4.2`, `equator-macro 0.4.2`, `event-listener 5.4.2`,
`event-listener-strategy 0.5.4`, `fast-float2 0.2.4`, `fixedbitset 0.5.7`,
`foldhash 0.2.0`, `futures-concurrency 7.7.1`, `futures-core 0.3.34`,
`futures-io 0.3.34`, `futures-lite 2.6.1`, `getrandom 0.4.3`, `hashbrown
0.14.5`, `intrusive-collections 0.10.3`, `itertools 0.15.0`, `memchr 2.8.3`,
`num-bigint 0.5.1`, `num-conv 0.2.2`, `num_enum 0.7.6`, `num_enum_derive 0.7.6`,
`num_threads 0.1.8`, `oneshot 0.2.1`, `parking 2.2.1`, `pastey 0.2.3`, `phf
0.14.0`, `phf_generator 0.14.0`, `phf_macros 0.14.0`, `phf_shared 0.14.0`,
`pin-project 1.1.13`, `pin-project-internal 1.1.13`, `pin-project-lite 0.2.17`,
`portable-atomic 1.15.0`, `powerfmt 0.2.1`, `proc-macro-crate 3.5.0`, `rand
0.10.3`, `rand_core 0.10.1`, `regress 0.12.0`, `ryu-js 1.0.3`, `serde_json
1.0.154`, `small_btree 0.1.0`, `synstructure 0.14.0`, `tag_ptr 0.1.0`, `tap
1.0.1`, `time 0.3.55`, `time-core 0.1.9`, `time-macros 0.2.32`, `toml_datetime
1.1.2`, `toml_edit 0.25.18`, `toml_parser 1.1.5`, `utf16_iter 1.1.0`, `utf_types
1.0.0`, `write16 1.0.0`, `zmij 1.0.23`.

Versions are those the probe resolved. Where one differs from what the repo
has locked (for example `toml_parser`, `getrandom`) it shows as new although a
crate of that name exists, which is why "new by name" is 63.
