# Phase 14 plan: out-of-process iframes

Design: ADR 045 (proposed; nothing starts until the user decides, except units 0 to 2).
This file is units, gate mapping and what only hardware proves.

Gate (build-spec section 9): a cross-origin iframe is in a different process; a
compromised-frame simulation cannot reach the parent's memory or cookies; nested
cross-origin frames work to depth 5.

## Starting state (read, not assumed)
- Built: `px-broker` (`Broker`, flat `FrameTree`, `ChannelCaps { hosts, visible }`,
  `ContentProcess::spawn/restart`), `px-ipc` (`Request`/`Response` with no sender
  field, script and GPU vocabularies, strips), `px-sandbox` (spawn, profile, the
  `testing`-only `probe`), `px-gpu` (`GpuSession`), `px-websec` policy functions
  (`needs_group_switch`, `embedding_allowed`, `sandboxed_origin`), `px-net`
  `PartitionKey`.
- **Not built**: any navigation, any document load in `px-content`, `TabId`,
  parent/child frames, `postMessage`, a compositor, input routing. Phases 11, 12
  (callers) and 13 are not closed. Units 3, 5, 6 below say what they need.
- Content's Linux profile is `no_new_privs` + a 17-syscall deny-list; `open` is
  allowed (Landlock and namespaces are Phase 17).

## Gate items to units

| Gate item | Units | Proven by |
|---|---|---|
| Cross-origin iframe in a different process | 1, 2, 3 | `cargo xtask frames` test `oopif_distinct_process` |
| Compromised frame cannot reach parent memory or cookies | 8 (needs 1, 2) | `oopif_compromised_frame_*`; memory half completes at Phase 17 |
| Nested cross-origin frames to depth 5 | 1, 2, 4, 5 | `oopif_depth_five_*` |
| (supporting) `window.opener`, popups, `postMessage` | 6 | `oopif_post_message_*`, `oopif_opener_*` |
| (supporting) one frame crashing | 7 | `oopif_child_crash_isolated` |

`cargo xtask frames` (new gate in `px-xtask`, std only, same shape as the `sandbox`
gate's name/test/file table) runs these and fails on a skip under
`PX_REQUIRE_SANDBOX`, because a silent skip looks like a pass.

## Units

**Unit 0: gate tests, failing.** `crates/px-broker/tests/oopif_*.rs` and the `frames`
xtask gate, all red, driving a testing-only commit of a document into a frame (a
stub for Phase 13, scanned for in release). One commit.

**Unit 1: frame tree and process key** (`px-broker`, `px-ipc`, `px-websec`).
`FrameTree` gains `parent`, `depth`, committed `Origin`, `state { Live, Crashed }`
and owner transfer; `ProcessKey` and the browsing-context-group id; resolve ADR 045
section 1's crate-edge problem so `cargo tree -p px-broker` has no `rustls` (test).
New vocabulary (`CreateChildFrame`, `Navigate`, `FrameReplaced`, `FrameCrashed`,
`SetViewport`): size caps, no sender field (`cargo xtask ipc` stays green), a fuzz
target per decoder. Tests: a forged `FrameId` for every slot in a range returns a
byte-identical `Denied`; a stale id after owner transfer is `Denied`; depth 9 is
refused; an IP-literal top-level host yields no key. Risk, medium (the crate edge).

**Unit 2: process pool** (`px-broker`). `(group, ProcessKey) -> ContentProcess`;
spawn on demand; `MAX_CONTENT_PROCESSES`; refuse-with-placeholder at the cap; a
hidden-tab discard hook; per-key restart accounting. **Measures**: spawn-to-first-ping
latency and idle RSS of `px-content`, which set the cap. Tests: same key and group
share a process; same site but different origin, same origin under two top-level
sites, and two groups each do not; over the cap fails closed and shares nothing.
Risk, low-medium.

**Unit 3: navigation and process swap** (`px-broker`; needs Phase 13's commit).
Header-time key computation with `px-websec` COOP and frame-ancestors verdicts; swap
with the old document shown until first paint; owner transfer; old process torn down
only if empty. Tests: same-key navigation spawns nothing; cross-key spawns, and the
old channel then gets `Denied`; a COOP switch severs the opener. Risk, medium. **If
Phase 13 is not closed, this builds against unit 0's stub and the report says so.**

**Unit 4: surfaces and compositor** (`px-paint`, `px-gpu`, `px-broker`). A
`RemoteFrame` display item; the rasterizer gains a transparent canvas; the parent
rasterizes as segments split at each `RemoteFrame`; per-frame surfaces with a byte
cap; the compositor blits child surfaces into broker-clamped, broker-clipped
rectangles in painting order and produces one `Canvas` plus a damage rectangle.
Headless, PNG baselines at tolerance 0 (ADR 033 (b)). **This is the compositor
Phase 18 presents**, built once. Tests: a 1x1 sentinel from a depth-5 child lands at
the expected pixel; a hostile `ReportRemoteFrame` (huge, negative, off-viewport) is
clamped and cannot paint outside the page region; content above an iframe stays
above. Risk, **high** (new rasterizer behaviour; display-list vocabulary changes).

**Unit 5: focus and input routing** (`px-broker`, `px-ipc`). Focus owner, outermost
first pointer routing with `ForwardInput`, `FocusAdvance`. Headless: synthetic
events in, typed events out. Tests: an event for a point inside a depth-5 child
reaches exactly that child's process after at most 5 hops; a parent naming a
non-descendant as `child` is `Denied` and audit-logged; a child never receives an
event outside its own rectangle. **Measures** per-hop latency. Risk, medium.

**Unit 6: `postMessage`, `window.opener`, popups** (`px-broker`, `px-ipc`; the JS
surface is Phase 11). Broker-stamped `source_origin`; `target_origin` filter;
256 KiB limit, chunked to 8 MiB; drop-and-log on a full target queue; rate limit;
`OpenWindow` with the opener link and `needs_group_switch`; remote-frame proxy rules.
Tests: a message claiming another origin cannot be expressed (`cargo xtask ipc`); a
wrong `target_origin` is dropped; a flood leaves the broker responsive and the audit
log bounded; a popup keeps `opener` across a same-group navigation and loses it on a
COOP switch; the same-origin-different-partition case sees only the proxy surface
(ADR 045 decision 2). Risk, medium.

**Unit 7: crash isolation** (`px-broker`). `Crashed` state, placeholder, `FrameCrashed`,
descendant teardown. Tests: kill a child's pid and the parent still answers, siblings
still composite, ids stay stable; kill a parent and its descendants go; a crash loop
stops at `MAX_RESTARTS`. Risk, low.

**Unit 8: compromised-frame simulation** (`px-ipc` testing vocabulary,
`px-content` testing handler, `px-sandbox` probe ops, a broker test). A
`testing`-only request in the content vocabulary, scanned for in release artifacts,
that makes the real `px-content` under the real profile attempt, with the parent's
pid and a heap address supplied out of band (the attacker is assumed to know them):
`process_vm_readv`, `ptrace`, `open("/proc/<pid>/mem")` + `pread`, `shm_open` of a
guessed name, `open` of the parent's profile directory; on Windows `OpenProcess`
with `PROCESS_VM_READ`. The parent holds a canary pattern in its heap and a canary
cookie in its partition's store (a fixture). Then the child sends forged requests:
every `FrameId` in a range, a `PostMessage` to the parent, every storage and fetch
request in the vocabulary. Pass: each attempt is a denial (`errno` 1 or 13, or the
process killed by the filter; "no such file" does not count, as `probe::Attempt::
denied`); no response byte contains the canary; every forged request returns the
one-byte `Denied`; `/proc/<child>/maps` shares no writable `s` mapping with the
parent. The child's report is evidence about behaviour, never authority (ADR 033
(e)2); pids and `applied` rungs come from `SandboxedChild`. Risk, **high for the
memory half**, which depends on Phase 17: the syscalls pass today, the
`/proc/<pid>/mem` case depends on the host's Yama setting and would be
`#[ignore = "needs Phase 17 Landlock/namespaces"]` if the user agrees (ADR 045, 4).

**Unit 9: depth-5 integration and the gate report.** Chain of 6 distinct origins
(top-level plus 5 nested): six distinct pids held by the broker (not the child's
word), six distinct partition keys, composite correct, input reaches the leaf,
killing a middle frame blanks only its subtree. Report whose first paragraph says
what was and was not proven on each OS, as Phases 8 and 9 did.

## Parallelism

0, then 1. Then {2, 4, 7} are independent sessions (pool, `px-paint`/compositor,
crash). 3 needs 1 and 2; 5 needs 1 and 4; 6 and 8 need 1 (8 also 2); 9 last. Unit 4
is the risky integration point and one session. `px-ipc` and `px-broker` get the
adversarial review (section 0.4) after units 1 and 6.

## What can only be proven where

| Here (Linux, no display, no GPU, no Windows) | Only on real hardware |
|---|---|
| All broker, pool, tree, routing and message tests, headless, deterministic | **Windows**: spawn latency, process count limits, every sandbox attempt in unit 8 (the restricted token, not AppContainer, until Phase 17) |
| Process distinctness by pid and `/proc` | Memory reads under the host's actual Yama and namespace settings across distributions |
| Compositor against PNG baselines, tolerance 0 | Cross-OS byte identity of the composite (the CI Windows job has never run it) |
| Syscall-level denials of unit 8 | **Idle RSS and spawn latency at realistic scale** (200 processes) on a user's machine; Phase 23's 50-tab soak |
| Hop latency of input routing in a loop | Input feel with a real window, IME focus across frames (Phase 18/9) |
| | Spectre-class leakage: separate address spaces are the mitigation, no test shows absence |

## Honest risk

- Phase 14 may close with the memory half of gate item 2 open if Phase 17 has not
  landed; the report says so rather than ticking it.
- Process count is the cost of the storage key; unit 2's numbers inform ADR 045 decision 1.
