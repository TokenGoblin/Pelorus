# Phase 1 gate report

*Process and IPC skeleton. Branch `phase/01-process-ipc`.*

The phase build-spec §9 calls "the most important reordering in v2": everything
after this is written against a boundary that already exists.

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| IPC deserializers fuzz clean for 24h | `fuzz-campaign` workflow | **Pass** — second campaign, see below |
| Broker rejects any message asserting its own identity | `hostile_identity_*` ×5, plus two source checks | **Pass** |
| Killing the content process is recovered from cleanly | `crash_restart_*` ×5, real processes, both OSes | **Pass** |
| Message size limits enforced, tested with a hostile length prefix | `size_limit_*` ×5 | **Pass** |

41 tests, seven of which cross a real process boundary. All CI jobs green on
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

## The fuzzing gate item

It took two campaigns. The first was clean and did not test what it was
supposed to test; the second did, and is the one this gate closes on.

### Campaign 2 — the one that counts

Run [34320581865](https://github.com/TokenGoblin/Pelorus/actions/runs/34320581865):
six shards, `-max_total_time=14400` each, **no crashes and no artifacts
written**.

| Target | Shard | Runs | cov | ft | corpus |
|---|---|---|---|---|---|
| `frame_request` | 1 | 6,159,406,185 | 170 | 208 | 94 / 1,377 b |
| `frame_request` | 2 | 7,187,748,436 | 170 | 208 | 101 / 1,279 b |
| `frame_response` | 1 | 8,551,169,682 | 152 | 190 | 85 / 1,259 b |
| `channel_stream` | 1 | 3,087,919,159 | 178 | 653 | 264 / 1,020 KiB |
| `channel_stream` | 2 | 2,596,913,344 | 178 | 668 | 254 / 1,229 KiB |
| `broker_sequence` | 1 | 2,973,383 | 512 | 3,346 | 1,025 / 270 KiB |

**The flag applied this time, and it was verified rather than assumed.** Three
independent checks, because the failure being guarded against is an edit that
reports success without doing anything:

- `-max_len=1100000` appears on the `Running` line of all six shards.
- The `-max_len is not provided` warning appears **zero** times. It appeared on
  every shard of campaign 1.
- libFuzzer's own `lim:` field climbs to `1100000` and stays there for 1,360
  log lines. This is the load-bearing one: the flag being on the command line
  proves it was passed, but `lim:` proves libFuzzer acted on it.

`-max_len` is 1,100,000 against a `MAX_MESSAGE_BYTES` of 1,048,576, so the
limit is straddled deliberately: inputs were generated on both sides of the
boundary, and the over-limit rejection path in `recv` was reachable rather than
merely present.

**The larger inputs changed the result, which is the proof they mattered.**
`channel_stream`'s corpora are now 1,020 KiB and 1,229 KiB — sizes structurally
unreachable under a 4 KiB cap — and `broker_sequence` went from 343 cov / 2,104
ft to **512 cov / 3,346 ft**, a ~49% coverage gain against a live `Broker`.
Campaign 1's clean result was not merely under-scoped; it was measurably
blind to the state this one reached.

The honest statement of this gate item is now the item as written: **24 CPU-hours
of fuzzing across the IPC deserializers, with inputs spanning the message-size
boundary, found no crash.**

### Campaign 1 — kept because the failure is the lesson

**It ran, was clean, and did not test what it was supposed to test.**

Run [34303798308](https://github.com/TokenGoblin/Pelorus/actions/runs/34303798308):
six shards, 14,401 seconds each — a genuine 24 CPU-hours — roughly 32 billion
executions, **no crashes**.

| Target | Shard | Runs | cov | ft | corpus |
|---|---|---|---|---|---|
| `frame_request` | 1 | 7,166,320,831 | 164 | 202 | 87 |
| `frame_request` | 2 | 7,354,072,260 | 170 | 209 | 97 |
| `frame_response` | 1 | 8,789,936,877 | 152 | 191 | 85 |
| `channel_stream` | 1 | 4,110,542,034 | 176 | 702 | 262 / 25 KiB |
| `channel_stream` | 2 | 4,916,238,807 | 176 | 617 | 222 / 13.7 KiB |
| `broker_sequence` | 1 | 17,778,828 | 343 | 2,104 | 636 / 122 KiB |

And then the logs say, on every shard:

```
INFO: -max_len is not provided; libFuzzer will not generate inputs
      larger than 4096 bytes
```

**The 4 KiB cap was still in force.** The `-max_len` fix an adversarial review
called for — because the large-payload path is the only code in `recv` that
allocates, and `MAX_MESSAGE_BYTES` is the boundary this gate names — was
committed as a message and never applied to the file. Two separate edits to
`ci/gate-fuzz-smoke.sh` were silent no-ops, and the commit describing them
asserted work that had not happened. `grep max_len ci/gate-fuzz-smoke.sh`
returned nothing.

So the honest statement of this gate item is: **24 hours of fuzzing found no
crash in inputs up to 4096 bytes.** That is not nothing — `channel_stream` and
`broker_sequence` are new targets with real state space, and `broker_sequence`
reached 343 coverage points and 2,104 features driving a live `Broker` — but it
is not the item as written, and the size boundary remains unfuzzed.

The script was fixed and the fix verified by asserting the string is present in
the file after writing, rather than trusting the edit to have happened.
Campaign 2 above is that corrected script, and it closes the item.

The pattern is worth naming because it is the third instance tonight: a change
that reports success without doing anything is indistinguishable from one that
worked. It defeated two gate checks earlier and has now defeated the fix for a
third.

**The adversarial review.** Done, and it is the reason most of this report
exists. §10 asks for "a dedicated session whose only job is attacking the
previous session's output"; it found the wedges, the amplification, the oracle,
and the fact that `dispatch` had never run on the IPC path.

### The second adversarial pass

The one this report asked for before the phase closed, against the code the
first review caused: the supervisor, the direction inversion and the tagged
codec, none of which an adversary had seen. It attacked the codec's framing,
the worker threads and lifecycle, and the capability model.

**It found one defect, and it is latent rather than live.** `FrameTree::destroy`
frees a slot without removing the handle from the owner's `hosts` set or from
any viewer's `visible` set — and `close_channel`'s purge computes its `live`
set by unioning every channel's `hosts`, so stale `hosts` entries preserve the
matching stale `visible` entries through the very purge written to bound those
sets. Confirmed with a temporary in-crate test asserting both halves, then
reverted. It is unreachable today: `destroy` has no caller outside tests and
`dispatch` handles only `Ping`, `Echo` and `FrameHost`. It is in the backlog
against the phase that adds `Request::DestroyFrame`, which `destroy`'s own doc
comment already anticipates.

**What it tried and did not break.** Recorded because a review that reports
only findings does not say how hard it looked:

- *Reflecting the broker's bytes back.* Per-direction tags make an echoing
  child produce `Malformed`.
- *A length prefix that is never honoured.* Chunked reads mean a declared
  megabyte costs 8 KiB until the bytes actually arrive.
- *A valid prefix with junk appended.* `take_from_bytes` plus a non-empty
  remainder check keeps the frame-to-message mapping injective.
- *Unbounded broker memory.* Both queues are `sync_channel` at `QUEUE_DEPTH`,
  `reply` uses `try_send` and reports `NotDraining` rather than absorbing.
- *Sweeping the handle space.* `DenyReason` never reaches the wire;
  `Decision::deny` always emits a bare `Response::Denied`, pinned by a test
  asserting it encodes to one byte.
- *Forging a handle.* `FrameId::new(u32::MAX, u32::MAX)` misses the slot
  vector and resolves to `None`; every accessor returns `Option` and there is
  no infallible variant.
- *Reviving authority across a restart.* `restart` spawns before it closes,
  mints a new `ChannelId`, and `dispatch` denies an unknown channel before it
  looks at the request at all.
- *Half-open and stale states.* `send` refuses oversized frames before writing
  a byte; any error that could misalign the stream poisons the channel
  permanently and poisoning is never cleared.

The leaked reader thread on an orphaned pipe was re-examined and left alone: it
is documented at `abandon`, bounded by `MAX_RESTARTS`, and already in the
backlog against Phase 17's process-tree teardown.

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

## What the two reviews found

Phase 1 was reviewed twice: an ordinary code review, and the adversarial
session §10 requires for `px-ipc`. Twenty-four findings between them, all real.
The second review found things the first could not, which is the argument for
§10 in one sentence.

### The adversarial session, in order of what it cost

**The broker could be hung forever from an empty input.** There was no timeout
anywhere in the IPC layer. Three wedges, each confirmed against real spawned
children: a child that never reads its stdin blocks the broker's `write_all` on
a full pipe; one that writes half a frame blocks its `read_exact`; and one that
spawns a grandchild holding the same stdout, then exits, blocks the read
forever **while `try_wait` reports the child dead**. None is a panic, so
`catch_unwind` does nothing about any of them.

Why the gate could not see it is worth stating exactly: every crash-restart
test *killed* the child, and killing closes the pipes. A content process that
stalls instead of dying was untested — and stalling is strictly cheaper for an
attacker than crashing.

**`dispatch` was dead code.** Every call site was inside `#[cfg(test)]`. There
was no broker read loop at all, so the channel-keyed capability table,
invariant 9's machinery and §4.3's `catch_unwind` had never seen a byte from a
hostile process. The cause was a direction error: the broker was the client and
the content process the server, which leaves the capability table nothing to
key on. Inverted, and `serve_once` is now the path a real process drives.

**Four bytes bought a megabyte.** `resize(length)` before `read_exact` meant a
peer that declared 1 MiB and sent nothing cost a committed, zeroed megabyte —
262,144x amplification, entirely inside the size limit. §4.4's letter was
satisfied while its spirit was not.

**A `cat` defeated the only end-to-end proof.** `Request` and `Response` were
indistinguishable on the wire, so a content process that reflected the broker's
own bytes verbatim, decoding nothing, satisfied `px-browser`'s assertion that
the boundary worked.

**`DenyReason` was a cross-site oracle.** `NoSuchFrame` and `NotYourFrame` were
distinct *on the wire*, so a hostile channel could sweep the handle space and
read off — exactly, with no false positives — which slots hold live frames
belonging to other sites, and how fast they churn as tabs open and close. In a
browser whose thesis is site isolation.

### Seven defects in the gate checks themselves

Across both reviews. This is the part worth reading twice, because the checks
were consistently the weakest code in the phase:

- The identity-field regex was close to **inverted**: it anchored the name
  after leading whitespace, so `pub sender: ChannelId` did not match — and a
  field must be `pub` to be readable from another crate. It caught exactly the
  declarations that cannot be used and missed every one that can.
- `BROKER_SRC` was assigned and never used. A check that was meant to exist did
  not.
- The `ChannelId` check required `derive(` and `Serialize` on the same line,
  which a multi-line derive defeats — and multi-line is what rustfmt emits, in
  a project that runs `cargo fmt --check`.
- Deleting `crates/px-content/tests/crash_restart.rs` entirely — the only tests
  in the phase that cross a process boundary — left both gate items green,
  matching similarly-named in-memory tests elsewhere.
- `px-ipc` was put under §4.3's panic lints and left out of the waiver scan.
- `ci/gate-fuzz-smoke.sh` aborted under `set -e` before reaching its own
  diagnostics.
- Two loops over `git ls-files` passed having inspected zero files when their
  pathspec matched nothing, and the header check looked at exactly two paths
  per crate — never at an explicit `[[bin]]` target, which `px-broker` has.

**And the fuzzing could not have found any of the above.** No `-max_len` was
set, so libFuzzer capped inputs at 4096 bytes and the entire large-payload path
— the only code in `recv` that allocates, and the `MAX_MESSAGE_BYTES` boundary
the gate specifically names — was unreachable. Both targets called
`decode_frame`, which reads one frame and discards the rest, so framing desync
could not be expressed at all. Half the budget went to the direction whose own
documentation says it matters less. Two targets were added: `channel_stream`
(multi-frame, asserting every non-EOF error poisons the channel) and
`broker_sequence` (invariant 9 as a fuzzable property — no channel is ever told
it owns a frame it was not issued).

The pattern across all of it: **a check that cannot fail is indistinguishable
from a check that passes.** Every gate check in this phase now has a recorded
negative control — the thing it detects was introduced deliberately and the
check was watched to fire.

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

**All four gate items pass in CI on both operating systems.** The phase closes.

The two conditions this report set for closing are both met. The campaign ran
against the current code with `-max_len` verified applied three independent
ways, and the coverage it gained over the capped run is the evidence it tested
something the first campaign could not reach. The second adversarial pass ran
against the rewritten supervisor, direction inversion and tagged codec; it
found one latent defect, which is recorded in the backlog against the phase
that makes it reachable.

The one CI job still red is `compat-list`, which is Phase 0's outstanding
deliverable and no part of this phase's gate. It is red on `main` as well.

Two things are worth saying plainly rather than leaving implied. The gate that
reports these greens is itself seven repairs old — it is more trustworthy than
it was, which is a statement about how little it was worth before, not a claim
that it is finished. And the fuzzing item is closed on 24 CPU-hours against
four targets, not on a proof: the honest claim is that no crash was found in
inputs spanning the size boundary, which is weaker than "there is none" and is
the strongest thing fuzzing ever says.
