# Phase 1 gate report

*Process and IPC skeleton. Branch `phase/01-process-ipc`.*

The phase build-spec §9 calls "the most important reordering in v2": everything
after this is written against a boundary that already exists.

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| IPC deserializers fuzz clean for 24h | `fuzz-campaign` workflow | **Outstanding** — see below |
| Broker rejects any message asserting its own identity | `hostile_identity_*` ×4, plus two source checks | **Pass** |
| Killing the content process is recovered from cleanly | `crash_restart_*` ×4, real processes, both OSes | **Pass** |
| Message size limits enforced, tested with a hostile length prefix | `size_limit_*` ×5 | **Pass** |

28 tests, six of which cross a real process boundary. All CI jobs green on
Ubuntu and Windows except `compat-list`, which is Phase 0's outstanding
deliverable.

## Identity: made impossible, then tested anyway

The gate says the broker must *reject* a message asserting its own identity. The
design goes further: such a message cannot be expressed.

```rust
fn dispatch(&mut self, channel: ChannelId, request: Request) -> Response
```

The channel is a separate argument, supplied by the caller from the connection
it read the bytes off. `Request` has no field naming its sender, and `ChannelId`
does not derive `Serialize`, so it cannot reach the wire even by accident.

Naming a **resource** is still allowed, and is how the capability model works:
`Request::FrameHost` carries the `FrameId` it asks about, and the broker checks
whether the channel it arrived on owns that frame. "I am tab 7" is
unrepresentable. "Tell me about frame 7" is a question the broker is entitled to
refuse — and does, with `Denied { NotYourFrame }`, telling the asker nothing
about who does own it.

Two source checks in `ci/gate-ipc.sh` guard the property against future code,
because tests cannot prove that a field which does not exist yet will never
exist. Both were verified to fire on a synthetic violation, not merely to pass.

## What is outstanding

**The 24-hour campaign.** ADR 006 splits fuzzing into a 60-second smoke on every
push and a scheduled campaign, with the gate satisfied by one recorded campaign
run. The smoke job passes in CI. The campaign workflow is validated but has only
been run at 120 seconds per target:
[34297716092](https://github.com/TokenGoblin/Pelorus/actions/runs/34297716092)
— 7,195,763 executions of `frame_response`, coverage 167, no crashes.

Until a full campaign completes, **this phase's gate is not met.**

**The adversarial review.** §10 requires `px-ipc` to get "a dedicated session
whose only job is attacking the previous session's output". That cannot be done
by the session that wrote the code — the value is entirely in the fresh context.
It needs a new session pointed at this branch. Not done.

## What the phase found

**`workflow_dispatch` resolves against the default branch only**, so the
campaign could not be started from the phase branch: the gate required a
campaign, the campaign required the workflow on `main`, and the branch should not
merge until its gate passes. Broken by putting only the workflow file on `main`;
it still runs against whatever ref it is dispatched with.

**The supply-chain gate stopped being vacuous, loudly.** ADR 003 predicted this;
what it did not predict was the size. `cargo vet` reported 13 unvetted
dependencies and an estimated audit backlog of **107,581 lines** — on the first
dependency the project ever added. §5 names Mozilla's and Google's audit sets;
those two alone did not cover it. Closing the gap took three more imported sets
and eight crates trusted by publisher, both recorded in ADR 003 rather than left
in a config diff. The honest summary is there: **eight of thirteen dependencies
are not audited by anyone in our chain**; they are trusted because a specific
person published them and Mozilla vouches for that person, with an expiry of
2027-09-09 so the position has to be re-taken rather than inherited.

**Two tests that looked fine and tested nothing.** The first version of the
`catch_unwind` test panicked in an unrelated closure and would have passed with
the `catch_unwind` deleted. The replacement drives a real panic through
`dispatch_guarded` via a `cfg(test)` flag, and was checked by removing the
`catch_unwind` and confirming the test fails.

The second was subtler: suppressing the deliberate panic's output with
`take_hook`/`set_hook` makes libtest fail the test, because the harness tracks
panics through its own hook. The suppression is gone and the noise is accepted.

## What a passing check does not mean

**The unsafe baseline is now 135 lines, and none of it has been read by us.**
`syn` is 108 of that. The gate detects *movement*, which is what it is for; it
does not mean anyone has audited those 135 lines.

**`panic = "abort"` is not what the tests run under.** `px-content` ships from
`content-release`, which aborts; its tests run under the `test` profile, which
unwinds. A code path that would abort in production can pass a test. ADR 004
records this. The compensating control is that `px-content` denies `unwrap`,
`expect`, `panic!` and indexing outright, so a panic is a lint failure before it
is an abort.

**The transport is inherited pipes, not the final one.** ADR 005 defers handle
passing to Phase 2. stdin/stdout gives no credential passing, no datagram
boundaries, and no route to sending a handle. What it does give — and what the
gate actually needed — is a broker that knows who sent a message because it
holds that process's pipe.

**The frame tree is flat.** `FrameId` resolves through the broker and can name a
frame owned by another channel, which is the shape Phase 14 needs. Parent/child
structure is not there, because nothing navigates yet.

## Verdict

Three of four gate items pass on both operating systems. The phase is **not
closeable** until a 24-hour campaign is recorded and `px-ipc` has had its
adversarial review — the second of which is, by §10's design, not something this
session can do.
