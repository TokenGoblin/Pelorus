# 009 — The IPC protocol shape

- **Status:** PROPOSED — this one is **not** taken on the standing overnight
  authorisation. See "Why this is not decided here".
- **Date:** 2026-09-09
- **Phase:** 2 (decided), first needed at Phase 4
- **Invariants touched:** 9 (must survive whatever is chosen)

## Why this is not decided here

ADRs 007 and 008 were taken unattended on the recommended option, because both
had a recommendation in the spec or a forced answer in the code. This one has
neither. It is a protocol design with several defensible shapes, different
costs, and consequences reaching Phase 21. The overnight authorisation covers
proceeding on a recommendation; it does not cover inventing one for a decision
the spec never framed.

So: alternatives, a recommendation, and a stop.

## Context

`docs/spec-audit-001.md` finding 1. What Phase 1 built is strictly
content→broker request/response, one message in flight, no correlation id:

```rust
pub fn serve_once(broker, process, timeout) -> Result<Decision, ServeError> {
    let request = process.next_request(timeout)?;   // content -> broker
    let decision = broker.dispatch_guarded(channel, request);
    process.reply(&decision.response)?;             // broker -> content, an ANSWER
}
```

Every `Response` variant is an answer. `Request` only travels one way. The
broker can physically write a frame at any time, but it has no message that
*means* a command, and with no correlation id a peer could not tell an
unsolicited command from the reply to its last request.

The spec needs broker-initiated messages in at least four places — §7.2's
`navigate`, `click`, `type`, `scroll`; Phase 13's navigation and history; Phase
14's `window.opener` and popups; Phase 15's media control. But the first phase
that needs one is **Phase 4**: something has to tell the content process which
document to parse.

**The spec is not wrong about this. It is silent.** §9 Phase 1 says "broker
request/response shape" and never says which side initiates; no phase names
bidirectionality, concurrency, or backpressure as a deliverable. That silence
is why Phase 1 built a shape that reads correct in isolation and does not
compose.

This is Appendix B item 1 one level down — the same failure at a smaller scale.
Converting a single-in-flight, one-directional protocol into a bidirectional
concurrent one, after Phases 3 through 5 have written against it, is a rewrite
of every call site rather than a change to the transport.

## Alternatives

### A. Leave it. Content polls for work.

The broker answers a `Request::NextCommand` when it has one. No new shape.

Costs: a wakeup per poll or a latency floor per navigation, and it inverts
control — the process being *told* what to do has to ask. It also makes the
deadline machinery meaningless, since a polling peer is never silent. Cheapest
today, and it is the option that quietly becomes permanent.

### B. Symmetric request/response with correlation ids

Both sides may initiate. Every message carries a `RequestId`; a response
references the id it answers; a third kind, a notification, expects no reply.
Bounded outstanding requests per direction.

Costs: a wire-format change, an id allocator on each side, and a
timeout/cleanup policy for outstanding requests. It is the minimum that
supports Phase 4, 13, 14 and §7.2 without a second change later.

### C. Two independent channels, one per direction

Each stays request/response, single-in-flight. No correlation ids: a reply is
always to the only outstanding request on that channel.

Costs: two transports per process to supervise, doubling the thread pair and
the deadline logic — and it does not solve concurrency, so a slow `navigate`
blocks a fast `FrameHost`. Simpler than B in the type system and worse in
operation.

### D. A full multiplexed stream protocol

Chromium's Mojo shape: interfaces, message pipes, associated interfaces,
streaming.

Costs: it is a subsystem, not a decision. §12's honest calibration applies —
this is a multi-month project for a solo maintainer, and Phase 2 is not the
place.

## Recommendation

**B**, with three specifics, and stated as a recommendation rather than a
ruling:

1. **`RequestId` is scoped per channel and per direction**, and is never
   authority. Invariant 9 must survive this change: an id identifies a
   *conversation*, and the broker still learns *who* from the channel. A
   response arriving with an id the peer never issued is a protocol error and
   poisons the channel, exactly as a bad tag does today.

2. **A bounded number of outstanding requests per direction**, refused rather
   than queued past the limit. The existing `QUEUE_DEPTH = 4` bound exists
   because a peer that stops draining must not choose the broker's memory
   usage; concurrency must not reintroduce that.

3. **Payloads over `MAX_MESSAGE_BYTES` are chunked**, with a bounded
   reassembly budget per channel, and shared memory is deferred to Phase 8
   where GPU surfaces need it. That is also where in-band handle passing gets
   its first real consumer (ADR 008), so the two arrive together rather than
   one waiting on the other.

## Consequences if B is taken

The wire format changes, so this should land **before** Phase 3 writes against
the current shape — which is the argument for deciding it in Phase 2 even
though nothing needs it until Phase 4.

`serve_once` stops being the whole protocol. It becomes one arm of a loop that
also drains outbound commands and matches inbound responses to outstanding
requests. Every hazard the Phase 1 adversarial passes found in `serve_once` —
the reply that could not be delivered, the revoked channel, the peer that
stalls — has to be re-reasoned against a state machine with more states.

`docs/backlog.md`'s "IPC has no request/response correlation id" is closed by
this, and the entry should point here.

## Consequences if A is taken

Say so explicitly in the spec, because it constrains Phase 21: §7.2's `click`,
`type` and `scroll` become poll-driven, and an agent's input injection inherits
the poll latency. That may be perfectly acceptable. It should not be discovered.

## Verification

Falsified if Phase 4 can be built without a broker-initiated message. I do not
believe it can — the content process has no way to learn which document to
parse — but that is the observation that would settle it.
