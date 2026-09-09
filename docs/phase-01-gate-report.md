# Phase 1 gate report

*Process and IPC skeleton. Branch `phase/01-process-ipc`.*

The phase build-spec §9 calls "the most important reordering in v2": everything
after this is written against a boundary that already exists.

## The four gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| IPC deserializers fuzz clean for 24h | `fuzz-campaign` workflow | **Outstanding** — see below |
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

## What is outstanding

**The 24-hour campaign.** ADR 006 splits fuzzing into a 60-second smoke on
every push and a scheduled campaign, with the gate satisfied by one recorded
campaign run. The smoke job passes in CI. The campaign is
[34303798308](https://github.com/TokenGoblin/Pelorus/actions/runs/34303798308)
— six shards, four hours each, against the rewritten codec.

Two earlier attempts are kept here rather than quietly superseded, because both
say something about the gate. The first,
[34298965894](https://github.com/TokenGoblin/Pelorus/actions/runs/34298965894),
failed in **twelve seconds** on all six shards: the `FUZZ_TARGET` membership
test used a space-delimited `case` match, and `cargo fuzz list` is
newline-separated, so no target name is ever surrounded by spaces and none
could ever match. The gate item this phase most depends on could not have gone
green — and the run was dispatched and not checked, because a job that fails in
twelve seconds looks exactly like a job that has just started. The second was
cancelled deliberately: it was fuzzing a codec the adversarial review was about
to change.

Until a campaign completes against the current code, **this phase's gate is not
met.**

**The adversarial review.** Done, and it is the reason most of this report
exists. §10 asks for "a dedicated session whose only job is attacking the
previous session's output"; it found the wedges, the amplification, the oracle,
and the fact that `dispatch` had never run on the IPC path. A second adversarial
pass against the rewritten code is warranted before this phase closes — the
supervisor and the direction inversion are new code that no adversary has seen.

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

Three of four gate items pass in CI on both operating systems. The phase is
**not closeable** until a campaign completes against the current code.

Two things about that verdict are worth saying plainly. The three passing items
pass against code that was substantially rewritten after review — the
supervisor, the direction inversion and the tagged codec are new, so a second
adversarial pass is warranted before this closes. And the gate that reports
those three greens is itself seven repairs old: it is more trustworthy than it
was this morning, and that is a statement about how much it was worth before,
not a claim that it is finished.
