# 045 — Phase 14 out-of-process iframes: the broker routes, the partition key is the process key

- **Status:** proposed. No code written. Waits for the user on the decisions at the
  end; units that do not depend on them (the plan's units 0 to 2) can start.
- **Date:** 2026-11-04
- **Phase:** 14
- **Invariants touched:** 1 (no ambient authority: no process-to-process channel),
  2 (partitioning: the process key equals the storage key), 8 (fail closed at every
  cap), 9 (authority from the channel: `event.origin` is the broker's, never the
  sender's).
- **Read with:** `docs/phase-14-plan.md` (units, gate mapping, hardware limits).
  What exists was read from `crates/px-broker`, `px-ipc`, `px-sandbox`, `px-gpu`,
  `px-websec`, `px-net`; what Phase 14 adds is marked *new*.

## Context

Build-spec section 9, Phase 14: "cross-origin iframes render in their own
process, with `window.opener` relationships and popups handled across the
boundary. Without this, 'one process per site' is a claim rather than a property."
Gate: a cross-origin iframe is in a different process; a compromised-frame
simulation cannot reach the parent's memory or cookies; nested cross-origin frames
work to depth 5. ADR 001 names it the primary control against a malicious ad, and
`docs/backlog.md` (the string interner, ADR 017) records that until this phase two
sites can share a process and a timing channel.

**What exists today**, and what it does not yet do:

- One `ContentProcess` per `px_sandbox::spawn`, with a `ChannelId` that is the
  inherited stdin/stdout pipe pair. The broker owns both ends; `Request` has **no
  field naming its sender** (`px-ipc/src/message.rs`; `cargo xtask ipc` fails if one
  appears). A restart gets a **new** `ChannelId` and zero authority;
  `MAX_RESTARTS` is 8; every wait has a deadline; queues are 4 deep.
- `FrameTree` is **flat**: a slot is `{generation, owner: Option<ChannelId>,
  retired}`. No parent, no children, no origin, no navigation. `ChannelCaps` has two
  sets, `hosts` and `visible`; `grant_visibility` is how a `FrameId` resolves
  remotely, and the only question a content process can ask is `FrameHost`, which
  answers `Local`, `Remote`, or a bare `Denied` (the reason is audit-only, because
  a distinguishing reply was a cross-site enumeration oracle).
- No handle passing (ADR 005, ADR 008). The only channel between a content process
  and anything else is its pipe to the broker. There is no content-to-content path
  and no shared memory, and this ADR keeps it that way.
- The partition key is `px_net::PartitionKey { top_level_site: String, origin }`,
  built only through `PartitionKey::new(psl, top_level_host, origin)`, which returns
  `None` for an IP-literal or bare-suffix top-level host. `px-websec` has `Origin`
  (with opaque origins), `Site`, `same_site`, `sandboxed_origin`,
  `embedding_allowed` (frame-ancestors + X-Frame-Options), `parse_sandbox_attribute`
  and `needs_group_switch` (COOP). They are pure functions and nothing calls them.
- `px-broker` links `px-net` only under its `testing` feature, deliberately: `px-net`
  pulls `rustls` and `ring`.
- Pixels: content builds a display list; the broker's `GpuSession` sends it to the
  GPU process; frames return as strips of at most 256 KiB, reassembled and
  arithmetic-checked by the receiver. The rasterizer draws on a **fresh white
  canvas** (`Rasterizer::rasterize`), so it has no transparency.
- Linux content profile today: `no_new_privs` + a seccomp **deny-list** of 17
  primitives that includes `ptrace`, `process_vm_readv`, `process_vm_writev`
  (`px-sandbox/src/linux.rs`). `open`/`openat` are *not* denied for content (only
  the GPU's stage two denies them). Landlock and namespaces are Phase 17.

## Decision

### 1. One process per (browsing context group, partition key)

**`ProcessKey` is `PartitionKey`**: `(eTLD+1 of the top-level document, origin of
the frame)`, the same value the storage layer uses, as the brief asks. Frames with
equal keys *in one browsing context group* share a process; anything else gets its
own. Consequences that are the point:

- A same-site, cross-origin iframe (`a.example.com` inside `www.example.com`) gets
  its own process. Chromium shares a process per site because `document.domain`
  makes same-site frames script each other; `document.domain` is a non-goal here, so
  origin granularity is available.
- **The same third party embedded under two top-level sites gets two processes**
  (its key includes the top-level site). That is the cost of using the storage key:
  strictly more isolation than a per-origin process (no cross-embedder interner
  timing channel, no shared heap between partitions), at roughly
  `tabs x (1 + distinct embedded origins)` processes.
- **Two tabs on the same site get two processes**, because the group is part of the
  key. A crash, a memory cap and a hung script are then per tab, which is what
  section 4.4's "1 GB per tab" meant.
- The broker computes the key. A content process cannot name its own
  (invariant 9). The broker learns a document's origin from the navigation it
  performed through `px-net`, never from a message. Opaque origins (`data:`,
  `sandbox` without `allow-same-origin`, via `px_websec::sandboxed_origin`) key as
  `(top-level site, a fresh opaque id)`: one process each, counted against the cap.
- **Where the key type lives is a design problem, not a detail**: `px-broker` must
  not link `px-net` (and therefore `rustls`). `px-websec` already avoids that edge
  on purpose. Unit 1 either moves `PartitionKey`/PSL lookup behind a trait the
  broker takes (as `px-websec::SiteResolver` already does) or defines the broker's
  key from `px-websec::Origin` and a resolver, and a test asserts `cargo tree -p
  px-broker` contains no `rustls` (the pattern ADR 040 used for `boa`).
  A top-level host with no registrable domain yields no key and the load is
  refused, as `PartitionKey::new` already demands. **That refuses `http://192.168.1.1/`
  and `http://localhost/`**; Phase 13's rule applies and this is a compat cost to
  surface, not to work around.

### 2. The broker is the only router

There is no process-to-process channel: no shared memory, no pipe, no handle
passed, no socket. Every cross-process effect is a typed request on a content
channel, decided by the broker, delivered as a typed event on another. *New*
vocabulary, all postcard, all size-capped, **none carrying an identity**:

| Direction | Message | Authority check |
|---|---|---|
| content to broker | `CreateChildFrame { parent: FrameId, element_ordinal }` | channel hosts `parent`; depth below `MAX_FRAME_DEPTH`; process cap |
| content to broker | `Navigate { frame, url, kind }` | channel hosts `frame` (or `frame` is a descendant it may navigate, per the HTML rules, decided from the broker's tree) |
| content to broker | `PostMessage { target: FrameId, data, target_origin }` | channel can see `target`; `target_origin` is checked against the *broker's* record of the target's origin |
| content to broker | `OpenWindow { url, features }` | user activation recorded by the broker; popup policy |
| content to broker | `ReportRemoteFrame { frame, rect, clip }` | channel hosts the parent of `frame`; rect clamped |
| broker to content | `DeliverMessage { target, source: FrameId, source_origin: String, data }` | `source_origin` is stamped by the broker from its own table |
| broker to content | `FrameCrashed`, `FrameReplaced`, `SetViewport`, `Input` | broker-initiated |

`target_origin` is a claim about the *recipient* (the spec's own filter), not an
identity claim, and a mismatch drops the message. The recipient's `event.origin`
and `event.source` come from the broker. A compromised sender can lie about
nothing the vocabulary lets it say, and `cargo xtask ipc` keeps it that way.

**Structured clone happens in the sender** and the bytes cross as `data`.
Limits (proposed, to be measured in unit 6): **256 KiB per message** (one IPC
message with margin under the 1 MiB `MAX_MESSAGE_BYTES`; larger payloads use the
bounded chunk pattern `MAX_FONT_CHUNK_BYTES` already set, up to 8 MiB total);
**no transferables in v1** (`MessagePort`, `ArrayBuffer` transfer, `ImageBitmap`
wait; `SharedArrayBuffer` is a non-goal); a per-source rate limit; **a full target
queue drops the message and audit-logs it**, because the broker never blocks on a
peer (its local invariant) and a queue the attacker fills is the attack.

### 3. `window.opener`, popups, `parent`, `top`

The browser tracks the relationship; a content process holds only **remote-frame
proxies**. A proxy exposes the cross-origin-accessible surface (`postMessage`,
`location` set, `closed`, `length`, `top`, `parent`, `opener`, `frames[i]`, `focus`,
`blur`, `close`) and every other access throws `SecurityError` **in the caller's
own process, from the broker's earlier `Remote` answer**, with no round trip.
`length`, `parent`, `top` are answered structurally from the broker's tree.

- `window.open` is a broker act. The opener link is recorded as an `opener:
  Option<FrameId>` on the new top-level frame. `px_websec::needs_group_switch`
  decides whether the navigation severs it (COOP) and starts a new browsing context
  group; `embedding_allowed` and the `sandbox` flags decide frame loads.
- **Same-origin frames in different partitions are not in one process**, so the
  web's "same-origin means synchronous access" cannot hold for one case: an
  iframe of origin X embedded under site B opens a popup to X (top-level site X).
  Keys `(B, X)` and `(X, X)` differ, so the processes differ, and the popup sees
  its opener only through the cross-origin proxy surface. **Federated login
  popups are unaffected** (the identity provider is cross-origin to the relying
  party and `postMessage` is brokered), which is the case that matters; the
  same-origin-different-partition case is a documented compat break, probably
  rare, and the compat suite will say. Alternative considered below.

### 4. Pixels

Each frame has a broker-held **surface** (RGBA, bounded by the existing canvas
limits, 2^25 pixels), filled by the Phase 8 path unchanged: display list to the GPU
process, strips back, reassembled and checked. *New*: a `RemoteFrame { frame }`
display item in the parent's list marks where a child goes. The compositor, in the
browser process, composites in painting order:

1. The parent's list is rasterized as **segments** split at each `RemoteFrame`, so
   content painted above an iframe (a positioned element over a frame) stays above
   it. Segments after the first need a **transparent canvas**; the Phase 8
   rasterizer only draws on white. That is unit 4's first task.
2. The child's surface is blitted into the parent-reported rectangle, **clipped by
   the broker** to the parent's own surface and the page viewport. The rectangle
   is the parent's claim and is clamped, never trusted; the child is told only its
   own size (`SetViewport`), never the parent's geometry or position.
3. The result is one `Canvas` plus a damage rectangle for the window layer
   (ADR 044), the same object the chrome compositor produces, so Phase 18
   consumes this unit rather than redoing it.

The GPU process is shared across sites by design and sees every site's display
list. Site isolation does not cover it. It is sandboxed and has no filesystem or
network, and its output is bytes the broker validates; recorded here as a residual,
not a fix. Memory: a 1080p surface is 8.3 MB; the broker caps total surface bytes
(proposed 256 MiB) and evicts hidden-tab surfaces first.

### 5. Focus and input

The browser process owns focus: one focused `FrameId` per window. Pointer events
are routed **outermost first**: the broker sends the event to the top-level frame's
process; that process hit-tests its own layout, finds an iframe element, and replies
`ForwardInput { child: FrameId, local_position }`; the broker checks that `child` is
a child of the *sender's* frame, translates nothing it does not know, and forwards
to the child's process. Cost: one round trip per nesting level (at most
`MAX_FRAME_DEPTH`). A compromised parent can misroute input into its own
descendants and nowhere else; a compromised child receives only events the broker
sent it. Keyboard goes to the focused frame; `Tab` leaving a frame is a
`FocusAdvance` exchange mediated by the broker. If the per-event round trip proves
too slow, the optimisation is the broker hit-testing against `RemoteFrame`
rectangles when no parent item covers the point; unit 5 measures first.

### 6. Navigation across a process boundary (process swap)

1. A navigation request reaches the broker, which fetches through `px-net`.
2. On response headers the broker computes the new `ProcessKey` and the COOP
   verdict. Same key and same group: commit in place.
3. Different key: choose or spawn the process for the new key, commit there, **show
   the old document until the new one paints**, then transfer the frame. `FrameId`
   stays stable; `FrameTree` gains an owner transfer (*new*), and the old channel's
   `hosts`/`visible` entries for it are removed, so its next `FrameHost` is `Denied`.
   No generation bump: authority here is set membership, and a bump would force
   every referrer of the frame to be updated.
4. Old process: if it hosts nothing else it is torn down (kill + `release_all`);
   otherwise it keeps its other frames. `beforeunload` runs before step 2;
   `unload` does not run in v1.

### 7. Failure modes and caps

- **One frame crashes**: `release_all` frees its frames; the broker keeps the
  frame slots in a `Crashed` state (*new*; today an owner-less slot is
  indistinguishable from a destroyed one and the parent loses sight of it), draws a
  broker-made placeholder in its rectangle, and sends the parent `FrameCrashed`.
  The parent, its siblings and the chrome are untouched. Recovery is by user action or
  a navigation, never an automatic loop; `MAX_RESTARTS` bounds it per process. If a
  **parent** dies, its descendants' frames are destroyed with it: a child without an
  embedding document has nothing to show.
- **Depth**: `MAX_FRAME_DEPTH` 8 (proposed; the gate needs 5). A self-embedding page
  hits it and the process cap, not the machine.
- **Process cap**: `MAX_CONTENT_PROCESSES` (proposed 64, set by unit 2 from a
  measured idle RSS). Over the cap, in order: discard hidden tabs' processes (state
  restored from session history, Phase 13); else **refuse the new subframe** and
  draw a placeholder. **The broker never shares a process across keys to stay under
  a cap**; that would make the cap the isolation boundary.
- **Memory**: each process carries the per-process budget Phase 17 imposes (1 GiB of
  address space is the unit the brief names; today only the cgroup/job-object
  `memory.max` sketch in `docs/backlog.md` exists, and an address-space limit needs
  `unsafe` in `px-sandbox`). Worst case is `MAX_CONTENT_PROCESSES x 1 GiB` of
  *address space*, not of RAM; the RAM figure needs the idle-RSS measurement.
  Fifty tabs at three embedded origins each is 200 processes, so Phase 23's soak
  depends on discard policy, and this ADR does not pretend otherwise.

### 8. What 'a compromised-frame simulation cannot reach the parent's memory or cookies' means as a test

Specified in the plan (unit 8). The idea: a **testing-only** content request,
scanned for in release artifacts like the GPU's `Probe` (section 14.4), makes the
real `px-content` binary, under the real sandbox profile, perform what an attacker
with native code execution in the child would: attempt cross-process memory reads
of the parent's *known pid and address*, open `/proc/<parent>/mem`, `ptrace`,
`process_vm_readv`, shared-memory opens, and send forged requests naming the
parent's frames and origin; while the parent holds a planted canary in its heap and
in its partition's cookie store. Pass is: every attempt fails with a denial `errno`
(the probe's `denied()` rule, which refuses "failed for some other reason"), no
canary byte is ever returned, every forged request returns the same one-byte
`Denied`, and the child's address space shares no writable mapping with the
parent's. **It will not fully pass on Linux until Phase 17**: the content profile
allows `open`, so reading `/proc/<parent>/mem` depends on the host's Yama
`ptrace_scope` (from memory: 0 on some distributions, 1 on others) rather than on
our sandbox. The syscall half passes today.

## Alternatives rejected

- **Process per origin, shared across embedders** (Chromium-like, fewer processes).
  Cheaper; but two partitions then share a heap and an interner, and a process hosts
  frames of several partitions whose cookies it asks the broker for by frame name.
  The brief asks for the storage key; this is the knob in the decisions below.
- **Process per site (eTLD+1).** Same-site cross-origin frames then share memory, a
  property only `document.domain` justified.
- **Letting content processes talk directly** (pipes, shared memory) for
  `postMessage` or pixels. Gives them a channel the broker cannot see or cap, and
  it needs the handle passing ADR 005 deferred. Zero-copy surfaces are a Phase 18
  optimisation on the broker-to-window side only.
- **Bump `FrameId` generation on process swap.** Forces updating every parent and
  opener reference; the set-membership revocation achieves the security property.
- **Keeping same-origin-different-partition popups synchronous** by sharing the
  process: breaks the key equals the process. Would reopen the partition question
  per feature.
- **Fallback to process sharing at the cap.** Fail open on the one control the phase
  exists for.

## Consequences

- The broker gains a process pool, a frame tree with parents, owner transfer and a
  crashed state, a compositor, and an input router: the largest growth of
  `px-broker` since Phase 1, and the crate that "may not die" gets a larger
  unwinding surface. Every new decoder gets a fuzz target (section 4.5); the
  adversarial-review session (section 0.4) applies to all of it.
- Process count is the cost, and it is unmeasured. Unit 2 produces the numbers
  (spawn-to-first-ping latency, idle RSS per `px-content`) that this ADR lacks.
- Phase 14 sits on three unbuilt phases: 11 (the `postMessage`/`window` JS surface;
  until then the tests drive the broker messages directly), 12 (policy functions are
  there; their callers are not), 13 (navigation, the commit step the swap hangs
  from). The plan says which units wait on which.
- The ADR 017/backlog interner channel closes only for documents in different keys;
  documents of one key in one group still share it, by design.

## What the user must decide

1. **Process key**: the full `PartitionKey` (recommended; most isolation, most
   processes) or origin-only, shared across embedders (about an order of magnitude
   fewer processes on ad-heavy pages, from memory, unmeasured).
2. **The same-origin-different-partition popup break** (section 3): accept and
   document, or find the cheaper compatibility rule.
3. **Process cap behaviour**: refuse-with-placeholder (recommended) or ask the user.
4. **Whether Phase 14's memory-read test may be `#[ignore]` on its `/proc/pid/mem`
   case until Phase 17** (recommended, with the reason in the attribute), or Phase
   14 pulls the Landlock rung forward.

## Verification

Wrong if: spawn latency makes a cross-origin iframe navigation visibly slower than
the product can accept (measured in unit 2; then pre-spawn, not sharing); the
number of processes on the forty compat sites exceeds what a normal machine runs
(then the key is the wrong key, decision 1); a forged message from a hostile child
ever gets anything but a byte-identical `Denied`; the compromised-frame test finds a
shared writable mapping or a readable canary; or input hop latency at depth 5 is
perceptible. Not falsifiable here: Windows behaviour of every sandbox attempt,
Spectre-class leaks (separate address spaces are the mitigation; no test shows
absence), and a real exploit chain.
