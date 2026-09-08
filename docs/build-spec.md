# PELORUS — Project Handoff

*Single source of truth. Everything Claude Code needs to start and to keep going.*

---

## Part 0 — How to use this document

### 0.1 It does not all belong in one place in the repo

This file is the handoff. In the repository it splits into three, because Claude Code loads `CLAUDE.md` automatically on every session and this document is far too long to sit in that slot — it would consume context on every turn for content most sessions don't need.

| Content | Goes to | Loaded |
|---|---|---|
| §5 working agreement (also in Part 0.3, ready to copy) | `/CLAUDE.md` | Every session, automatically |
| Everything else in this document | `/docs/build-spec.md` | On request, per phase |
| Per-crate invariants | `/crates/px-*/CLAUDE.md` | When that crate is in context |

The rule to give Claude Code: **read `docs/build-spec.md` §9 for the current phase at the start of a phase, not every session.**

### 0.2 The first prompt

Paste this to start Phase 0, with this document attached:

> Read `docs/build-spec.md` in full. We are starting Phase 0.
>
> Before writing any code, produce a task decomposition for Phase 0 only, and stop for my approval. Include for each task: what it produces, which invariant or gate it serves, and how we verify it.
>
> Constraints for the decomposition:
> - Phase 0's gate is in §9. Write the gate checks as failing CI jobs first, before the things they check exist.
> - Do not create any crate beyond the empty `px-*` skeletons in §3 and a working `px-brand`.
> - Do not add a single dependency that isn't named in §3, and propose any you think are needed rather than adding them.
> - The brand-leak gate must be proven by deliberately violating it and showing CI fail.
> - `docs/adr/000-name.md` and `docs/adr/001-threat-model.md` are Phase 0 deliverables, not paperwork to do later.
>
> If anything in the spec is ambiguous or looks wrong, say so now rather than picking an interpretation.

### 0.3 Root `CLAUDE.md`

The working agreement lives at [`/CLAUDE.md`](../CLAUDE.md), loaded automatically
every session. **Amend it there and nowhere else** — a second copy in this file
would drift, and the copy that drifts is always the one nobody is reading.

### 0.4 Per-phase working rhythm

1. Start the phase in plan mode. Claude reads §9 for that phase plus its ADRs, and produces a decomposition for approval.
2. Gate tests are written first, failing.
3. Implementation.
4. For `px-sandbox`, `px-ipc`, `px-websec`, and `px-mcp`: a separate adversarial session whose only job is attacking the previous session's output.
5. Gate passes on both OSes in CI. Then the branch merges.

### 0.5 What to watch for

The two most common failure modes in a project this size are Claude reaching for a convenient dependency mid-task, and hardcoding the product name in a new file. Both have CI gates. Both will still be attempted. The `CLAUDE.md` rules exist so the correction is a one-line reference rather than an argument.

---

# Pelorus — Build Specification

*A ground-up, memory-safe web browser for Windows and Linux.*

> **Working name.** A pelorus is a sighting compass with no magnet — it reads bearings relative to your own vessel rather than to any external reference. Provisional; the codebase is built so it can be replaced in a day (§2). See Appendix A before proposing any change to a settled decision.

## 1. Product invariants

Load-bearing. Every phase gate checks against them. Changing one requires an ADR.

1. **No ambient authority.** Nothing holds a capability it wasn't handed. Content processes cannot touch filesystem, network, clipboard, or GPU except through brokered IPC.
2. **State is partitioned by top-level site, structurally.** Cookies, cache, storage, connection pools, DNS cache, and TLS sessions are keyed by `(eTLD+1 of top-level, origin)`. There is no unpartitioned code path to leave enabled by accident.
3. **The UI does not move.** Chrome layout is locked by screenshot-diff tests from Phase 18. Changes require an ADR with a stated user-facing reason.
4. **The network is used for pages and for patches, nothing else.** No traffic occurs except (a) a page the user requested, and (b) a signed update manifest fetched at a user-configured cadence, default weekly, from one URL, carrying **no identifiers** — no unique ID, no cookies, no detailed user-agent, no query parameters — over a connection sharing nothing with browsing state. No telemetry endpoint is compiled into the binary. No crash reporting, no safe-browsing beacon, no captive-portal probe, no first-run call home.
5. **Silence by default.** No new-tab feed, no onboarding, no feature promos, no notification prompts unless the user has interacted with the origin, no badges, no nags.
6. **No JIT.** Interpreter-only JavaScript is a deliberate security posture (§6). Performance regressions on JS-heavy apps are accepted and documented, never fixed by adding a compiler.
7. **Reproducible builds.** Same source + same toolchain = same binary hash on both platforms.
8. **Fail closed.** If a security control cannot be applied — sandbox unavailable, policy rejected, capability check inconclusive — the operation does not proceed. A browser that silently runs unsandboxed because a syscall failed is worse than one that refuses to start.
9. **Authority comes from the channel, never from the message.** The broker identifies a caller by which connection a request arrived on. No process ever sends its own identity, origin, or partition key. This single rule eliminates the confused-deputy class.
10. **No public release before Phase 20.** Nothing is distributed to anyone — not a friend, not a "just try it" build — until the update channel and signed release pipeline exist. Shipping a security tool you cannot patch is the one unrecoverable mistake available here.

### Non-goals

Mobile, macOS, WebGL/WebGPU-for-content, WebRTC, DRM/EME, Web Bluetooth/USB/Serial/NFC, background sync, push notifications, WebExtensions compatibility, `document.domain`, SharedArrayBuffer, a built-in PDF viewer, a password manager, crash telemetry, beating Chrome on benchmarks, and supporting the whole web (§8).

---

## 2. Naming and rename safety

The name is provisional; the codebase never assumes it.

### 2.1 Crate names carry no brand

Internal crates use the neutral prefix **`px-`**. The brand appears in crate names nowhere.

```
px-brand    px-broker   px-ipc      px-sandbox  px-net
px-dom      px-css      px-layout   px-paint    px-gpu
px-text     px-script   px-bindings px-websec   px-a11y
px-content  px-ui       px-store    px-block    px-update
px-mcp
```

Only the shipped binary and the installer carry the name, both set from one workspace variable.

### 2.2 One source of truth: `px-brand`

Every user-visible string is a constant in `px-brand`. CI fails the build if the literal appears anywhere outside `px-brand`, `docs/`, `packaging/`, `README`, and the workspace `Cargo.toml`. Add the gate in Phase 0 and prove it by deliberately hardcoding the name and watching CI reject it.

### 2.3 Rename-sensitive decisions

- **User agent carries no product name.** For fingerprinting reasons first (Phase 19); rename-safety is a side benefit.
- **Config directories** are the painful one. `px-store` reads `CONFIG_DIR` plus a `LEGACY_CONFIG_DIRS: &[&str]` list and migrates on first run. Write the migration path in Phase 0 with an empty list; Phase 13's gate exercises a simulated rename against a populated profile.
- **Internal URL scheme** comes from `INTERNAL_SCHEME`. Register both new and legacy for one release cycle after a rename.
- **The update URL is brand-independent and permanent** — see §14.1. It does *not* live behind `PRODUCT_NAME`.

### 2.4 Reserve the name before Phase 3

crates.io (publish `px-brand` as a stub to hold the prefix), the GitHub org, a domain, a USPTO search in classes 9 and 42. `docs/adr/000-name.md` records the choice as provisional with the surviving alternates: Quindar, Palfrey, Azimuth, Binnacle, Lethe, Temenos.

---

## 3. Architecture

```
┌──────────────────────────────────────────────────────────────┐
│ px-broker (parent, privileged)                                │
│  · owns all OS handles; spawns and sandboxes every child      │
│  · capability table keyed by CHANNEL, never by message        │
│  · consent prompts, audit log, download writer                │
│  · profile / partition manager, frame-tree authority          │
└─┬────────┬─────────┬─────────┬──────────┬─────────┬───────────┘
  │        │         │         │          │         │
┌─▼─────┐┌─▼──────┐┌─▼──────┐┌─▼────────┐┌─▼──────┐┌─▼─────────┐
│ px-ui ││ px-net ││ px-gpu ││px-content││px-update││ px-mcp    │
│ chrome││ TLS/DNS││ raster ││  × N      ││ signed  ││ off by    │
│ only  ││ /HTTP  ││sandbox'd││one per   ││manifest ││ default,  │
│       ││separate││+ sw     ││site, zero││only     ││not spawned│
│       ││process ││fallback ││authority ││         ││when off   │
└───────┘└────────┘└────────┘└──────────┘└────────┘└───────────┘
```

Every arrow is a typed IPC channel. `px-content` holds no handle it was not passed. The frame tree spans processes: a `FrameId` may resolve to a remote process from Phase 1 onward, even before out-of-process iframes actually work (§Phase 14).

### Workspace layout

```
pelorus/                    ← directory name is cosmetic
├─ crates/
│  ├─ px-brand/         ONLY crate containing the product name
│  ├─ px-broker/        parent, capability broker, policy, frame tree
│  ├─ px-ipc/           typed channels, postcard codec, handle passing
│  ├─ px-sandbox/       the ONLY crate allowed `unsafe`
│  ├─ px-net/           rustls, DoH, HTTP/1.1+2, partitioned pools
│  ├─ px-dom/           generational arena DOM
│  ├─ px-css/           stylo integration, cascade, computed values
│  ├─ px-layout/        box tree, block/inline, flex, grid (Au fixed-point)
│  ├─ px-paint/         display list construction
│  ├─ px-gpu/           wgpu backend + software rasterizer fallback
│  ├─ px-text/          shaping, fallback, bidi, IME integration
│  ├─ px-script/        Boa host, event loop, microtask queue
│  ├─ px-bindings/      WebIDL → Rust codegen
│  ├─ px-websec/        SOP, CORS, CSP, mixed content, referrer, COOP/COEP
│  ├─ px-a11y/          platform-independent a11y tree + UIA/AT-SPI
│  ├─ px-content/       content process binary
│  ├─ px-ui/            browser chrome (winit + wgpu, thin widgets)
│  ├─ px-store/         partitioned storage + config-dir migration
│  ├─ px-block/         content blocking, tracker mitigations
│  ├─ px-update/        signed manifest fetch, data-file updates
│  └─ px-mcp/           MCP subsystem, feature- and runtime-gated
├─ data/                PSL, HSTS preload, root store — versioned, updatable
├─ packaging/           installers, .desktop, icons, signing scripts
├─ tests/
│  ├─ wpt/              vendored WPT subset + runner + testdriver shim
│  ├─ compat/           forty-site suite, recorded traffic archives
│  ├─ sandbox/          syscall denial assertions, escape attempts
│  ├─ spoof/            address-bar and IDN spoofing corpus
│  ├─ brand/            brand-leak gate
│  └─ fuzz/             cargo-fuzz targets per parser AND per IPC message
└─ docs/adr/
```

### Dependency policy

Write yourself: DOM, layout, compositor, networking *policy*, process model, sandbox, web security model, UI, storage.

Take from the ecosystem: `html5ever`, `stylo`, `rustls`, `swash`/`cosmic-text`, `wgpu`, `image`, `boa_engine`, `postcard`, `url`, `idna`.

**No new dependency without an ADR** recording what it does, why not write it, maintainer count, audit status, and unsafe-line count.

---

## 4. Memory safety — the actual claim

The README says this, not "written in Rust, therefore safe":

> Memory-safe application code, over a small audited unsafe core, with process isolation for the parts that cannot be made safe.

Because the dependency closure contains real unsafe: **stylo** (Gecko-derived, thread-safety by convention), **boa_gc** (a tracing GC), **wgpu** and the **C GPU drivers beneath it**, **swash** (font parsing). Safety ends at those boundaries, which is why the sandbox is the primary control and Rust is what reduces how often it has to save you.

### 4.1 Generational handles — mandatory

Plain `NodeId(u32)` arena indices with slot reuse reproduce use-after-free in safe code: a stale ID silently addresses whatever now occupies that slot. That is DOM-level type confusion, which is how browsers get exploited.

```rust
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct NodeId { index: u32, generation: u32 }

pub fn get(&self, id: NodeId) -> Option<&Node> {
    let slot = self.slots.get(id.index as usize)?;
    (slot.generation == id.generation).then(|| &slot.node)
}
```

Every accessor returns `Option`. **There is no infallible index API, not even a private one.** On generation overflow the slot is retired permanently rather than wrapping (§14.3). Fuzz target: hold stale IDs across mutations, assert every lookup is `None`.

### 4.2 Arithmetic

```toml
[profile.release]
overflow-checks = true
lto = "fat"
codegen-units = 1
```

Layout uses `Au` — app units, `i32` at 1/60 px, as Servo and Gecko do — with explicit saturating operations, so geometry overflow is defined and testable rather than a panic or a wrap.

### 4.3 Panic policy

The v1 "no panics" rule was unenforceable, since `GcRefCell` and arena indexing panic. Replaced with a mechanism:

```toml
# px-content, px-net, px-mcp
[lints.clippy]
unwrap_used = "deny"
expect_used = "deny"
indexing_slicing = "deny"
panic = "deny"
```

Plus: **content processes build with `panic = "abort"`**, and the broker treats a dead content process as routine. A content process that dies is contained; one that unwinds through a half-mutated DOM is not. The broker itself unwinds, with `catch_unwind` around IPC dispatch. Note the Cargo constraint in §14.2 — this requires separate build profiles.

### 4.4 Exhaustion

- **Stack:** explicit depth limits on HTML nesting, CSS selector recursion, and DOM APIs (start at 512). Layout walks are iterative with a work stack, never recursive. Fuzz corpus includes 100,000-level nesting for each parser.
- **Heap:** per-content-process caps — Windows job object `JOB_OBJECT_LIMIT_PROCESS_MEMORY`, Linux cgroup v2 `memory.max`. Start at 1 GB per tab. Exceeding it kills that process only. IPC messages are size-capped and no length prefix from untrusted bytes drives an allocation without a bound check.

### 4.5 Verification in CI

Miri on `px-dom`, `px-ipc`, `px-store` unit tests (pure-Rust paths only — Miri cannot run the FFI ones, and that limitation is documented rather than papered over). ASAN and TSAN builds of `px-sandbox`. Loom for lock-free code in `px-ipc`. `cargo-fuzz` targets required for every parser **and every IPC deserializer**. An unsafe-counting gate over the dependency tree with a committed baseline; any increase fails CI and needs an ADR.

---

## 5. Engineering invariants (root `CLAUDE.md`)

**The authoritative text is [`/CLAUDE.md`](../CLAUDE.md).** It is not repeated here — two copies of a working agreement drift, and the one that drifts is always the one nobody is reading. Amend it there and nowhere else.

Per-crate `CLAUDE.md` carries local invariants.

### Supply chain

`cargo-deny`, `cargo-vet` **importing Mozilla's and Google's audit sets** (auditing a browser tree from scratch is a project of its own and the gate would get waived by month two), `cargo-auditable` for an in-binary SBOM, pinned `rust-toolchain.toml`, committed `Cargo.lock`, CI builds `--locked`.

---

## 6. JavaScript: Boa, no JIT

Boa is a pure-Rust bytecode interpreter at roughly 96% test262 with zero panics, tracking 1.0. Roughly 10–50× slower than V8 on JS-heavy work.

That is accepted. JIT compilers are the largest single source of exploitable RCE in every shipping browser, and **no JIT also means no attacker-controlled gadget generation**, which removes the most practical Spectre exploitation path. State that advantage in the docs; it is real and unusual.

Two honest caveats:

- **96% test262 is not 96% of the web.** It measures the language. Sites fail on DOM surface, timing, and performance. Never read that number as compat.
- **WASM is undecided.** A WASM interpreter is far simpler than a JS JIT, and much of the modern web now assumes WASM exists. ADR required by Phase 10.

Document the tradeoff in preferences. Adding a compiler tier later reopens invariant 6 in an ADR.

---

## 7. MCP subsystem — `px-mcp`

Let the user connect an external agent (Claude Code, a local model, a script) to the browser without that becoming a standing attack surface.

### 7.1 Off means off

1. **Compile:** `--features mcp`. A `minimal` profile omits the code entirely.
2. **Runtime toggle:** default `false`. When false, `px-mcp` **is not spawned**, no socket or pipe exists, no listener is in the process table. Toggling off tears down the process and unlinks the endpoint.
3. **Grant:** with the subsystem on, every tool and server is unauthorized until granted.

A small persistent chrome indicator shows when the subsystem is running, and a distinct one when a session is attached. No modal, no color, no animation.

### 7.2 Tool surface

**Pelorus as MCP server** — each tool individually grantable:

| Tool | Grant scope |
|---|---|
| `list_tabs` | session |
| `read_tab` (accessibility tree + text, not raw DOM) | per-tab, explicit |
| `screenshot_tab` | per-tab, explicit |
| `navigate` | per-tab or agent-tabs-only; **`http`/`https` only** |
| `click` / `type` / `scroll` | per-tab, explicit, expiring |
| `open_tab` | session, always into the agent profile |
| `console_eval` | off by default, separate grant, never remembered |

**Pelorus as MCP client** — connects to user-configured servers, configured only through preferences or a user-edited config file. Never via a web page, URL scheme, or downloaded file.

### 7.3 Security model

- **Local transport only.** stdio, Unix socket (`0600`, user runtime dir), or Windows named pipe with an explicit DACL. No TCP listener, ever, on any interface. Remote MCP servers are explicit outbound connections through `px-net` under the same partitioning rules as any request.
- **`px-mcp` is sandboxed like a content process.** No filesystem, no network, no clipboard. Every action is an IPC request the broker checks against the grant table.
- **Scheme allowlist.** `file://` and internal schemes are unreachable from every MCP tool, permanently, with no grant that unlocks them. Nothing an agent does can read `~/.ssh/id_rsa` or open the settings page.
- **No download capability.** Writing attacker-chosen bytes to disk is explicitly denied, not merely unlisted.
- **Grants are explicit, scoped, expiring.** `(tool, scope, expiry)`. Session grants die with the session, tab grants with the tab. No "always allow" for `console_eval` or input injection. Prompts state the concrete action in plain language and never default to the permissive button.
- **Restart resets everything.** If `px-mcp` crashes or restarts, all grants die. A restarted subsystem begins with zero authority, always.
- **Agent tabs are a separate partition** with their own cookie jar, covered by clear-on-exit like any other profile. Granting access to a signed-in tab is a distinct, per-tab act.
- **No credential surface.** No tool returns cookies, storage, saved passwords, or the certificate store — so no bug can leak them.
- **Prompt injection:** page content is untrusted input. `read_tab` wraps content in an explicit untrusted envelope and sanitizes nothing. The defense is the grant model, not filtering prose.
- **Audit log** is written by the **broker** (`px-mcp` has no filesystem). Append-only from `px-mcp`'s perspective — it can never read or truncate it. Local, user-readable, uploaded nowhere.

---

## 8. The compat gate

Forty sites you actually use, in `tests/compat/sites.toml`, each with scripted assertions. This is the real progress meter; spec percentages are not.

**Run against recorded traffic, not the live web.** Live CI would be flaky, slow, likely to get the IP blocked, arguably against several sites' terms, and it leaks your test list. Capture archives once, commit them, replay in CI. A separate weekly manual job runs live to detect drift.

Replay requires a local CA trust anchor in the test build. That anchor is `#[cfg(feature = "testing")]` and **CI asserts it is absent from release binaries** (§14.4).

---

## 9. Phase plan

Branch `phase/NN-slug`. Gate must pass on both OSes in CI. ADRs precede implementation.

### Stage A — Foundation and boundaries

**Phase 0 — Skeleton and policy**
Workspace, CI matrix, `cargo-deny`/`cargo-vet`/`cargo-auditable`/unsafe-count baseline, ADR template, `000-name.md`, `001-threat-model.md`, root and per-crate `CLAUDE.md`, `px-brand` and the brand-leak gate, `LEGACY_CONFIG_DIRS` stub, compat site list, release profile flags from §4.2, reproducible-build proof.
*Threat model names the adversaries:* malicious page; malicious ad or third-party script; network observer; cross-site tracker; malicious MCP server; compromised agent; local unprivileged process. *And who you are not defending against:* local admin, physical access, hostile kernel, targeted state actor with a full 0-day chain.
*Gate:* builds on both OSes; hashes reproducible across two machines with different paths and usernames; supply-chain checks green; `forbid(unsafe_code)` everywhere; brand-leak gate proven by a deliberate violation failing CI.

**Phase 1 — Process and IPC skeleton** *(was Phase 12)*
A real separate content process that does nothing but echo. `px-ipc` typed channels, handle passing, broker request/response shape, channel-keyed capability table, frame tree with `FrameId` that may resolve remotely, crash detection and restart.
*This is the most important reordering in v2.* Everything after it is written against a boundary that already exists. Retrofitting process boundaries into a DOM/layout/script stack is a rewrite, not a refactor.
*Gate:* IPC deserializers fuzz clean for 24h; broker rejects any message asserting its own identity; killing the content process is recovered from cleanly; message size limits enforced and tested with a hostile length prefix.

**Phase 2 — Sandbox skeleton**
Permissive but real policies on both platforms, plus the capability-detection ladder and fail-closed behavior. Tightening happens in Phase 17; what matters now is that no code is ever written against ambient authority.
*Gate:* content process launches under a policy on both OSes; with the sandbox forced unavailable, the browser **refuses to launch content processes and says why**; Linux fallback ladder documented and exercised (§14.5).

**Phase 3 — Network core**
`px-net` as its own process: rustls, DoH resolver, HTTP/1.1 and HTTP/2, connection pool keyed by partition key, redirect and cookie policy. `data/` gains versioned PSL, HSTS preload, and root store, bundled at build time.
*ADR required:* root store (platform vs bundled — recommend platform on Windows so enterprise and user roots work), revocation strategy (CRL sets, not OCSP — OCSP leaks browsing to the CA and violates invariant 4), and the DoH provider choice with a first-run free choice among system resolver, a named provider, and custom. A hardcoded default provider transfers the tracking rather than eliminating it.
*Gate:* 200 URLs fetched correctly; 24h fuzz on HTTP framing; packet capture shows zero plaintext DNS and zero connections outside the requested set; PSL version is asserted and a stale-PSL test fails.

### Stage B — Engine

**Phase 4 — DOM**
`html5ever` into a **generational** arena (§4.1). Mutation-safe iteration, tree ordering, ranges, depth limits.
*Gate:* html5lib-tests ≥99%; stale-handle fuzz target proves every stale lookup returns `None`; 24h mutation fuzz clean; 100,000-level nesting handled without stack overflow.

**Phase 5 — Style** *(highest-risk phase)*
`stylo` integration. This is not wiring: stylo requires implementing its `TElement`/`TNode` traits over your DOM with thread-safety invariants it assumes and does not check. Expect it to be the phase most likely to force a `px-dom` redesign, which is why it sits before layout.
*Gate:* computed-style fixtures match across a defined property set; WPT `css/css-cascade` subset passes; a documented decision on whether stylo survived contact or is being replaced.

**Phase 6 — Block and inline layout** — box tree, BFCs, inline, floats, positioned. `Au` fixed-point throughout.
*Gate:* WPT CSS2 reftest subset at threshold; identical box tree on repeat runs; iterative (non-recursive) tree walks verified by a deep-nesting test.

**Phase 7 — Flex and grid** — *Gate:* WPT `css-flexbox` and `css-grid` subsets at threshold.

**Phase 8 — Paint and compositing**
Display list, `px-gpu` on wgpu **in its own sandboxed process** with crash-restart, plus a **software rasterizer fallback** (`--disable-gpu`), which also makes CI rendering deterministic.
*Gate:* 25 static pages render headless; pixel-diff baselines committed; GPU process proven to have no filesystem or network capability; killing it falls back to software without losing the session.

**Phase 9 — Text and input**
Shaping, font fallback, bidi, line breaking, system font enumeration, and **IME integration** — Windows TSF, Linux IBus/Fcitx. Without this, CJK/Korean/Vietnamese users cannot type at all.
*Gate:* multi-script fixtures (Latin, Cyrillic, CJK, Arabic, Devanagari, Hebrew) baselined per platform; IME composition test in the compat suite on both OSes.

### Stage C — Behavior and the security model

**Phase 10 — Script host** — Boa, event loop, task and microtask queues, timers coarsened to 100µs with jitter (§Spectre). WASM ADR due.
*Gate:* test262 subset threshold; event ordering tests; no unbounded task-queue growth under a stress page; timer resolution asserted.

**Phase 11 — Bindings** — WebIDL parser and Rust codegen. The generator is the deliverable; hand-written glue count is zero.
*Gate:* IDL subset generates and compiles; WPT `dom/` subset at threshold.

**Phase 12 — Web security model** *(new in v2, and non-negotiable)*
`px-websec`: same-origin policy enforcement points, CORS preflight and response validation, CSP, mixed content blocking, `SameSite`, Referrer Policy, `X-Frame-Options`/`frame-ancestors`, `iframe sandbox`, COOP/COEP/CORP, Subresource Integrity, secure-context gating.
A same-origin bypass needs no memory corruption at all, which makes it the highest-severity bug class in the project — above sandbox escapes.
*Gate:* WPT `content-security-policy/`, `cors/`, `fetch/`, `mixed-content/`, `referrer-policy/` at agreed thresholds. Running these needs a `testdriver.js` shim, which is test-only and must not compile into release (§14.4).

**Phase 13 — Navigation and storage**
Fetch, XHR, forms, history, session restore, `px-store` with full partitioning and the config-dir migration path.
*ADR due before starting:* federated login. Partitioning breaks SSO and OAuth popups — decide between the Storage Access API and accepting the breakage, because the compat suite will surface it either way.
*Gate:* login flows on five compat sites; cross-site leak harness finds no shared state; a simulated rename migrates a populated profile with zero data loss.

**Phase 14 — Out-of-process iframes**
The frame tree from Phase 1 becomes real: cross-origin iframes render in their own process, with `window.opener` relationships and popups handled across the boundary.
Without this, "one process per site" is a claim rather than a property, and it is precisely the configuration Spectre broke.
*Gate:* a cross-origin iframe is demonstrably in a different process; a compromised-frame simulation cannot reach the parent's memory or cookies; nested cross-origin frames handled to depth 5.

**Phase 15 — Media** — images including animated, canvas 2D. Video by ADR: Rust decoders (`symphonia`, `rav1d`) in a dedicated locked-down process, or no video in v1.
*Gate:* image fuzz clean 24h; canvas conformance subset; media process (if built) has no filesystem or network capability.

**Phase 16 — Accessibility** *(new in v2)*
`px-a11y`: a platform-independent accessibility tree with UIA (Windows) and AT-SPI (Linux) adapters. Correct on its own merits, and a **hard dependency of MCP** — v1 specified `read_tab` to return an a11y tree that no phase ever built.
*Gate:* screen readers navigate a real page on both platforms; a11y tree snapshot tests; find-in-page built on the same tree.

### Stage D — Hardening and product

**Phase 17 — Sandbox hardening**
Windows: AppContainer, restricted token, job object limits including memory caps, Win32k lockdown, mitigation policies. Linux: user namespace, seccomp-bpf allowlist, Landlock, no-new-privs, cgroup memory caps.
*Gate:* escape-attempt suite (open file, connect socket, spawn process, read another process, access clipboard, enumerate devices) fails every attempt on both OSes; ProcMon/strace audit committed; memory cap kills a runaway tab and only that tab.

**Phase 18 — Chrome**
`px-ui`: tab strip, address bar, menu, preferences, dialogs, downloads, find-in-page, printing. All strings from `px-brand`. Keyboard-first. Then freeze it with screenshot-diff tests.
*Origin display is specified, not assumed:* eTLD+1 emphasized and the rest de-emphasized; punycode shown for mixed-script labels per the Chromium/Mozilla IDN rules; no padlock theater (mark the absence of security, not its presence); no page-controlled content anywhere in the chrome region.
*Downloads* are written by the broker and carry the Windows Zone.Identifier stream (mark-of-the-web) or the Linux xattr equivalent — omitting this silently disables the OS's own protections.
*Gate:* identical rendering on both OSes; UI snapshots committed and a one-pixel change fails CI; spoofing corpus in `tests/spoof/` fully defeated; MOTW asserted on every downloaded file. Snapshot baselines mask the product-name region so a rename doesn't invalidate them (§14.6).

**Phase 19 — Privacy**
Partitioning audit across every subsystem. `px-block`: declarative blocking, no remote rule fetch without consent, CNAME-cloaking detection, query-parameter stripping on navigation, bounce-tracking mitigation. Fingerprinting resistance: frozen generic UA with no product identifier, **letterboxing** (quantized viewport dimensions — window size is the largest vector and v1 omitted it), font and canvas and timing policy. Profiles, clear-on-exit covering the agent partition.
*ADR:* profile encryption at rest, stating honestly that OS-keyring encryption (DPAPI, libsecret) protects against other users, not against malware running as you.
*Gate:* leak harness green across all storage classes; fingerprinting battery at target; a fresh profile makes zero network requests until a URL is entered; UA contains no product identifier.

**Phase 20 — Release engineering** *(gates invariant 10)*
`px-update`: signed manifest fetch with no identifiers, signed releases (minisign or Sigstore), documented key rotation, `SECURITY.md` with a disclosure address and response commitment, Windows code signing, `data/` updates for PSL, HSTS, and root store riding the same channel. Mirror configuration so one URL is not a single point of censorship.
*Alternative accepted by ADR:* distribution solely via distro packages and winget, delegating updates entirely. Legitimate, but it must be a written decision rather than an omission.
*Gate:* an end-to-end update from a signed manifest; a tampered manifest is rejected; packet capture of an update check shows no identifier of any kind; a stale-data-file test forces a refresh.

### Stage E — Extensibility and daily driver

**Phase 21 — MCP** — implement §7 in full.
*Gate:* with the toggle off, no endpoint exists (verified by socket/pipe enumeration) and no process is spawned. On but ungranted, every tool call is denied and logged. `file://` and internal schemes are unreachable under every grant combination. No tool can trigger a download. Granted tools cannot reach a non-agent tab's cookies. Toggling off mid-session terminates the process within 100 ms. A `px-mcp` restart drops all grants.

**Phase 22 — Extension story** — ADR first: a narrow Rust plugin API for blocking and page transforms, or nothing. WebExtensions is a second browser's worth of surface.

**Phase 23 — Daily driver** — *Gate:* compat suite green on all forty sites; thirty consecutive days of self-hosted use with no fallback; crash-free session rate at target; memory stable over a 24h soak with fifty tabs.

**Phase 24 — Name lock** — clear the trademark and drop "provisional," or execute the rename: change `px-brand`, run the leak gate, regenerate UI baselines, append the old `CONFIG_DIR` to `LEGACY_CONFIG_DIRS`, register the legacy scheme for one cycle, `git mv`. One day — **except the update URL, which never changes** (§14.1).

---

## 10. Claude Code operating notes

- **Plan mode before each phase.** Claude reads this spec plus the phase ADRs and produces a decomposition for approval before any code.
- **Narrow context.** One crate per session where possible; per-crate `CLAUDE.md` carries local invariants.
- **Gate tests first, failing.** A phase that cannot state its gate in code is not specified well enough to start.
- **Spec over inference** for anything web-platform-shaped.
- **Guard the dependency tree.** The most common failure mode is reaching for a convenient crate mid-task.
- **Guard the brand boundary.** The second most common is a hardcoded product name in a new file.
- **Adversarial review sessions.** `px-sandbox`, `px-ipc`, `px-websec`, and `px-mcp` get a dedicated session whose only job is attacking the previous session's output.
- **Backlog discipline.** `docs/backlog.md` catches out-of-phase defects, or phases expand until they never close.

---

## 11. Risk register

| Risk | Mitigation |
|---|---|
| Scope death | Forty-site compat gate, not spec percentages, defines progress |
| Stylo integration forces a DOM redesign | Phase 5 sits before layout so discovery is early and cheap |
| Boa too slow for daily use | Measured at Phase 10; options are optimizing Boa, accepting slowness, or reopening invariant 6 by ADR |
| Same-origin bypass | Phase 12 exists; treated as the top severity class, above sandbox escapes |
| Sandbox correctness | Phase 17 gate is adversarial; escape suite runs in CI forever after |
| Linux sandbox unavailable on user's distro | Capability ladder + fail closed; SUID-helper decision in §14.5 |
| Cannot ship a patch | Invariant 10 blocks any release before Phase 20 |
| Stale PSL corrupts partition boundaries | `data/` versioned, asserted at Phase 3, updated via Phase 20 channel |
| No crash telemetry means no field visibility | Accepted (§14.7); local dumps the user may voluntarily send |
| Windows/Linux text divergence | Per-platform baselines reviewed together from Phase 9 |
| MCP as the soft underbelly | Three-layer gating, sandboxed process, scheme allowlist, no credential or download surface, separate partition |
| Name blocked or wrong | §2 keeps blast radius at one crate, one gate, one migration list |
| Solo maintenance of a security product | Deliberately small surface: no WebRTC, DRM, extensions, PDF viewer, or JIT |

---

## 12. Honest calibration

Servo has had institutional funding since 2012 and reached an embeddable 0.1 in 2026. Ladybird has a paid team and remains pre-alpha after four years. Neither is a daily driver.

Solo with AI assistance, Phases 0–9 are genuinely achievable and constitute a real rendering engine. Phases 10–16 are where the slope steepens sharply. Phase 23 is a multi-year target and should be held as a direction, not a date.

The plan is not wrong for being ambitious. It would be wrong to discover in year two that the process model must be rebuilt — which is what the v2 reordering prevents.

---

## 13. Open decisions

1. ~~Project name~~ — Pelorus, provisional, ADR-000.
2. **WASM** — ship an interpreter, or not at all. Due Phase 10.
3. **Video** — Rust decoders in a locked-down process, or none in v1. Due Phase 15.
4. **Root store** — platform versus bundled. Due Phase 3.
5. **DoH provider** — which, and the first-run choice design. Due Phase 3.
6. **Federated login** — Storage Access API versus accepted breakage. Due Phase 13.
7. **Linux sandbox fallback** — SUID helper versus refuse-to-run. Due Phase 2 (§14.5).
8. **HTTP/3** — deferred; revisit after Phase 23. QUIC is a large new parser surface for latency you do not need yet.
9. **License**, and whether the repo is public before Phase 20.
10. **Distribution** — own update channel versus distro/winget delegation. Due Phase 20.

---

## 14. New findings surfaced during this revision

Auditing the revision produced seven issues that were not in the original audit.

### 14.1 The rename-safety design breaks the update channel — HIGH
§2 routes every user-visible string through `px-brand`, and the update URL is a user-visible string. Renaming would therefore change the update URL, and every already-installed copy would keep polling the old one — silently stopping security updates for exactly the users you can no longer reach.
**Fix:** the update URL and signing key are **brand-independent and permanent**. They live in `px-update`, not `px-brand`, and are explicitly out of scope for a rename. If the domain must change, serve both for a minimum of two years. Added to the Phase 24 checklist as an exception.

### 14.2 Cargo cannot set panic strategy per crate — MEDIUM
§4.3 wants `panic = "abort"` for content processes and unwinding for the broker. Cargo's panic setting is **per profile, not per crate**, so a single `cargo build` cannot produce both.
**Fix:** two profiles and two build invocations (`--profile content-release -p px-content`), accepting that shared dependencies are compiled twice. ADR in Phase 1 to confirm the build-time cost is tolerable, since this shapes CI duration for the whole project.

### 14.3 Generational handles have two costs v1 ignored — MEDIUM
`NodeId` doubles from 4 to 8 bytes, and DOM handles are everywhere, so this is a real memory cost at scale. Worse, **generation counters wrap**: after 2³² reuses, a stale handle becomes valid again — the exact bug the design exists to prevent.
**Fix:** retire a slot permanently on generation overflow rather than wrapping. Consider a 24/8 index/generation split with slot retirement to keep handles at 4 bytes; measure both in Phase 4 and record the choice. Add a fuzz target that forces generation exhaustion.

### 14.4 Test-only capabilities are a shipping risk — HIGH
v2 introduces two features that must never reach users: the **local CA trust anchor** for compat replay (§8) and the **WebDriver/testdriver automation surface** WPT requires (Phase 12). Either one in a release build is a critical vulnerability — an attacker-reachable automation port is how remote-debugging bugs have repeatedly bitten Chrome.
**Fix:** both behind `#[cfg(feature = "testing")]`, and a **release-artifact scan** in CI that greps the built binary for the test CA's fingerprint and the automation entry symbols and fails if either is present. Symbol absence in the artifact, not merely a feature flag being off — flags get enabled by accident.

### 14.5 Fail-closed collides with Linux reality — HIGH
Invariant 8 says refuse to launch without a sandbox. Several distributions restrict unprivileged user namespaces by default, so on those systems a strict reading means the browser simply does not work — and users will respond by disabling security, not by fixing their kernel.
**Fix:** ADR due in Phase 2, choosing between a **SUID helper** (Chromium's approach; adds a setuid binary, which is its own attack surface) and **refuse-to-run with a clear, specific message naming the sysctl**. Recommendation: refuse to run, with precise instructions and a documented `--no-sandbox` flag that requires an interactive confirmation and displays a permanent warning banner. Never a silent fallback.

### 14.6 UI snapshot tests and rename-safety conflict — LOW
Phase 18 freezes the chrome with pixel-diff tests, and the chrome displays the product name. A rename would fail every snapshot.
**Fix:** mask the name region in snapshot comparison, or treat baseline regeneration as a documented rename step. Noted in Phase 18 and 24.

### 14.7 No crash reporting means no field visibility — MEDIUM, accepted
Invariant 4 forbids crash telemetry, which is right for privacy and means you will never learn about failures on user machines. For a security product this is a genuine cost, not a free win.
**Fix:** write local crash dumps the user can inspect and voluntarily attach to a report. State the tradeoff in the docs rather than presenting the absence as pure benefit. Added to the risk register.

---

## Appendix A — Settled decisions

These were argued and closed. They are recorded here so they are not re-proposed halfway through a phase. If one turns out to be wrong, say so explicitly and stop; do not quietly work around it.

| Decision | Rejected alternative | Why |
|---|---|---|
| Ground-up engine in Rust | Fork Firefox (LibreWolf model) | A fork inherits Mozilla's UI churn, which invariant 3 exists to prevent, and re-patching every ESR is permanent work |
| Ground-up engine in Rust | Embed Chromium via CEF | Google's engine, Manifest V3's blocking limits, and a mandatory ~4-week rebase treadmill or knowingly shipped CVEs |
| Reuse `html5ever`, `stylo`, `rustls`, `swash` | Write parsers and TLS from scratch | Writing your own HTML tokenizer or TLS stack buys bugs, not sovereignty. "Ground up" means the DOM, layout, compositor, process model, and security model — not Unicode tables |
| Boa, interpreter only | SpiderMonkey or V8 bindings | Imports the largest CVE surface in any browser: a C++ JIT. No JIT also removes attacker-controlled gadget generation, the practical Spectre path |
| Hand-rolled widgets on winit + wgpu | iced, egui, gtk-rs, Slint | A general toolkit imports its own redesigns and breaking changes into your chrome. The widget set here is tiny and you already own a compositor |
| Partitioning as a type-level property | A "block third-party cookies" setting | A setting can be wrong; a keyed type has no unpartitioned code path to leave enabled |
| MCP at Phase 21 | MCP early, as a feature | It consumes the capability broker. Built before the broker and sandbox, it necessarily has ambient authority, and retrofitting constraint never works |
| Process boundary at Phase 1 | Multi-process after the engine works | Converting synchronous engine calls into async IPC is a rewrite. This was the most expensive error in v1 of the plan |
| Compat suite on recorded traffic | Live CI against 40 real sites | Flaky, slow, gets the IP blocked, arguably violates terms, and leaks the test list. Live runs are a weekly manual job |
| No PDF viewer | Bundle one | One of the largest attack surfaces in any browser, for a feature the OS already provides |
| No WebExtensions | Chrome extension compatibility | A second browser's worth of surface for a solo maintainer |
| No crash telemetry | Opt-in crash reporting | Invariant 4. The cost — no field visibility — is real and accepted; local dumps the user may voluntarily send |
| Update channel exists, narrowly scoped | No network except pages | A security product that cannot ship a patch is not a security product. The exception is written precisely into invariant 4 |

## Appendix B — Where this plan came from

The specification was audited before implementation began. The audit found four issues at a level that would have cost months:

1. **Multi-process sequenced too late** — the engine would have been built single-process and then rewritten.
2. **The web security model was entirely absent** — SOP, CORS, CSP, mixed content, SameSite, frame-ancestors appeared in no phase. A same-origin bypass needs no memory corruption at all, which makes it the highest-severity bug class in the project, above sandbox escapes.
3. **No update mechanism, and invariant 4 forbade building one.**
4. **Partitioning depends on the Public Suffix List**, a living document, with no update path — a stale PSL means wrong security boundaries.

Plus one concrete memory-safety flaw that Rust would not have caught: plain `NodeId(u32)` arena indices with slot reuse are use-after-free in safe code, producing DOM-level type confusion. Hence generational handles (§4.1).

Auditing the revision itself surfaced seven more, listed in §14. The two worth remembering: the rename-safety design would have silently broken security updates for existing installs, and the test-only CA and WebDriver surfaces introduced in v2 must be scanned for in release artifacts rather than merely flagged off.

The lesson to carry forward: **audit the plan again after any structural change to it.** Each revision introduced defects the previous audit could not have found.
