# Overnight log

Running unattended. Newest entry last. This is a work journal, not a gate
report — `docs/phase-NN-gate-report.md` is where conclusions live.

## Scope authorised

The user chose to continue past the point where I recommended stopping, and
then said they trust my recommendations. I am reading that as: proceed with the
wide scope, and take the **recommended** option at each decision the spec calls
theirs. Two such decisions arise tonight and both are recorded as ADRs for the
user to confirm or overturn:

- **§14.5, the Linux sandbox fallback** — refuse to run, naming the exact
  sysctl, with a `--no-sandbox` flag behind interactive confirmation and a
  permanent warning banner. Never a silent fallback. (ADR 007)
- **Where unsafe OS operations live** — `px-sandbox`, whose remit widens from
  "sandboxing" to "the audited unsafe core". ADR 005 deferred this to Phase 2.
  (ADR 008)

Guardrails I am keeping regardless:

- **Phase 2 does not merge.** It stays on `phase/02-sandbox` for review.
- `px-sandbox` gets an adversarial review session once it has real code, per
  build-spec §10 and its own `CLAUDE.md`. Tonight's evidence for why: two
  reviews of Phase 1 produced 38 findings, and the second found that the
  product binary always exited failure.
- No dependency outside build-spec §3 without stopping to ask.
- Every unsafe block gets a `// SAFETY:` comment citing the OS documentation it
  rests on, not the fact that it happened to work.

---

## 03:4x UTC — waiting on the fuzz campaign

Campaign [34303798308](https://github.com/TokenGoblin/Pelorus/actions/runs/34303798308)
still running: six shards, four hours each, ~06:35 UTC. It is the last open
item on Phase 1's gate, so the merge waits for it.

Everything else is green on both operating systems. `compat-list` is red and
expected to be — the forty-site list is the user's to write.

Read `docs/build-spec.md` §9 Phase 2 and §14.5 to ground the ADRs. Phase 2's
gate is three things:

1. a content process launches under a policy on both OSes;
2. with the sandbox forced unavailable, the browser **refuses** to launch
   content processes and says why;
3. the Linux capability ladder is documented and exercised.

The second is the interesting one to build first, because it is the one that
fails closed and the one a user will actually hit.

## 04:20 UTC — ADRs drafted; campaign 1h45m in

Campaign still running (started 02:35, six shards of four hours, so ~06:35).
Nothing to do on Phase 1 until it lands, so I drafted the two Phase 2 ADRs into
the scratchpad rather than committing Phase 2 content onto the Phase 1 branch.
They go in as the first commit on `phase/02-sandbox`.

**ADR 007 departs from §14.5, and the user should look at this specifically.**
§14.5 frames the Linux question as binary — SUID helper, or refuse to run when
the sandbox is unavailable. On Linux the sandbox is not one mechanism but four
(`no_new_privs`, seccomp-bpf, Landlock, user namespaces), and **only the last
is commonly restricted**. Treating its absence as "no sandbox" discards three
mechanisms that are still available and still real, and it refuses in exactly
the scenario §14.5 worries about — which is what drives a user to
`--no-sandbox`.

So ADR 007 takes §14.5's recommendation (refuse to run, never silently degrade)
and adds a **floor**: the ladder is applied rung by rung, and refusal happens
below a defined minimum rather than on the loss of any single rung. Floor on
Linux is `no_new_privs` + seccomp-bpf; on Windows a restricted token + job
object.

A second thing §14.5 could not have known: **there is no single sysctl to
name.** The restriction is `kernel.unprivileged_userns_clone` on Debian
derivatives, `kernel.apparmor_restrict_unprivileged_userns` on recent Ubuntu,
and `user.max_user_namespaces` on RHEL-family kernels. §14.5's recommendation
is to name it precisely, and naming the wrong one sends a user to edit a
setting that does not exist on their machine. The message has to be derived
from the condition actually detected.

The honest cost is in the ADR: on a kernel with user namespaces restricted,
this browser runs with a weaker sandbox than Chromium would, because we will
not ship a setuid binary. That is the trade.

ADR 008 answers what ADR 005 deferred — `px-sandbox` owns every unsafe OS
operation, not only sandboxing ones — and records the argument for the `px-os`
split that was rejected, because the naming argument for it is good and will
come back at Phase 17.

Next: waiting on the campaign. A monitor is armed on it, so the merge starts
the moment it reports.

## 04:55 UTC — Phase 2 gate drafted, and it pulls §14.4 forward

Campaign 2h20m in, ~1h40m to go. Drafted `ci/gate-sandbox.sh` into the
scratchpad. Two things came out of writing it that change Phase 2's scope.

**Phase 2 introduces the project's first test-only capability, so §14.4's
release-artifact scan has to arrive now rather than at Phase 8.** The gate item
"with the sandbox forced unavailable, the browser refuses" needs a way to force
unavailability, and that override must not exist in a shipping binary. §14.4 is
explicit that a feature flag being off is not evidence — absence of the symbol
from the artifact is. `docs/backlog.md` files that scanner as a prerequisite for
Phases 8 and 12; Phase 2 gets there first, and the backlog entry needs updating
to say so.

The override is `PX_TEST_FORCE_SANDBOX_UNAVAILABLE`, behind
`#[cfg(feature = "testing")]`, and the gate greps the release binaries for the
symbol.

**The gate runs the browser.** Phase 1's most embarrassing finding was that
`px-browser` exited FAILURE on a clean run while every check stayed green,
because nothing executed it. The refusal check is exactly the shape that would
repeat: it is easy to test `Sandbox::probe()` in a unit test and never confirm
that the *product* refuses to start. So the Phase 2 gate runs the binary with
the override set and asserts both that it exits non-zero and that the message
names the missing mechanism — §14.5 requires the message to be specific, and a
refusal that says nothing is what sends a user to `--no-sandbox` blind.

The gate also asserts ADR 007's ladder is documented rung by rung. A rung
nobody wrote down is a rung nobody can tell you that you lost.

Still waiting on the campaign before any of this can be committed — it is Phase
2 work and Phase 1 has not merged.

## 05:25 UTC — quiet hold

Campaign ~2h50m in, ~1h10m left. Re-ran `ci/gate-all.sh` against the merge
candidate: green except the expected `compat-list`. Phase 2 gate draft parses.

Two research agents were started alongside this loop, both read-only so they
cannot collide with it: one auditing `docs/build-spec.md` against what Phases
0-1 actually built (Appendix B asks for exactly this after any structural
change, and six have been made), and one researching what stylo requires of a
DOM so that Phase 4 builds against those requirements rather than being
redesigned by Phase 5 — §9 calls that the highest-risk phase and §11's only
mitigation is that discovery happens early.

Nothing else to do until the campaign reports.

## 05:35 UTC — spec audit landed, and it corrects tonight's plan

`docs/spec-audit-001.md`. Thirteen findings. Appendix B asked for this after
any structural change and six have been made; it was worth asking.

**One finding changes Phase 2 before a line of it was written.** ADR 005
deferred handle passing to Phase 2 as the thing `px-sandbox` would own. That is
the wrong operation. Phase 2's gate is "content process launches under a policy
on both OSes", and neither platform can do that from `std::process::Command` —
Windows needs `CreateProcessAsUserW` with a `PROC_THREAD_ATTRIBUTE_LIST`, Linux
needs `CommandExt::pre_exec`, which is unsafe. Verified: `px-broker` opens with
`#![forbid(unsafe_code)]` at line 1 and calls `Command::new` at line 574.

So **`ContentProcess::spawn` moves behind `px-sandbox`**, the broker keeps the
supervisor, and in-band handle passing waits for its first real consumer —
Phase 8 or Phase 21. ADR 008's draft is corrected. Without this the phase would
have been implemented against a spawn path that cannot apply a policy.

**The largest finding is one nobody can act on tonight.** The protocol has no
broker→content direction: every `Response` variant is an answer, `Request` is
content→broker only, and there is no correlation id. The spec needs
broker-initiated messages for §7.2's `navigate`/`click`/`type`, Phase 14's
popups, Phase 13's navigation — but the first phase that needs it is **Phase
4**, because something has to tell the content process which document to parse.
The spec is not wrong here; it is silent. §9 Phase 1 says "broker
request/response shape" and never says which side initiates.

That wants an ADR in Phase 2 before Phase 3 writes against the current shape.
It is a protocol decision with real alternatives, so I am drafting it, not
deciding it — it goes to the user alongside 007.

Also folded into Phase 2: process-tree teardown moves up from Phase 17, because
a job object is created *at* `CreateProcess` and retrofitting it means writing
the same function twice.

Four new backlog entries: spec amendments owed, nobody owns the broker's audit
log or consent prompts, where the compat replay runs given ADR 002, and no test
asserts `env_clear()` actually emptied the environment — currently the only
enforcement of invariant 1.

Campaign still running at 3h. Longer than the 4h shards implied; watching it.

## 05:40 UTC — stylo research landed; two committed rules will have to bend

`docs/research/stylo-requirements.md`, 869 lines, fetched from real source
(`servo/stylo` at a named commit) rather than recalled. §9 calls Phase 5 the
highest-risk phase and §11's only mitigation is that discovery happens early.
This moves the discovery earlier still — before Phase 4 writes the DOM that
Phase 5 would have redesigned.

**Two findings contradict rules this project has already committed to.**

`px-css` cannot carry `#![forbid(unsafe_code)]`. `TElement` declares six
`unsafe fn` methods, and implementing an unsafe method is exactly what `forbid`
rejects — verified by compiling a probe against the pinned toolchain, not
inferred. `/CLAUDE.md`'s first hard rule and Phase 0's gate check 4 both assert
the opposite. The proposed replacement is *narrower* than a waiver: `unsafe fn`
declarations allowed, zero `unsafe` blocks, zero `unsafe impl`, enforced as its
own rule rather than as an exemption.

stylo's `build.rs` shells out to Python 3 and Mako, which breaks invariant 7
from Phase 5 onward. The reproducibility gate has held since the workspace was
empty; it will not survive this without a decision.

**The generational-handle collision resolves, and better than expected.**
§4.1's mandatory `Option`-returning accessors genuinely cannot implement
`TNode` — dozens of its methods are infallible. But §4.1 governs px-dom's arena
API, not every type, and stylo's own `where Self: 'a` bounds are it asking for a
lifetime-parameterised handle. A borrowed view resolved once through a fallible
generation check preserves §4.1's actual safety property, adds no infallible
index API, and **statically enforces the no-mutation-during-traversal invariant
that stylo assumes and Servo enforces only by comment**. It makes `Send`/`Sync`
true rather than asserted, which is stronger than the reference implementation.

Two things Phase 4 must decide that nobody had listed: a chunked arena rather
than a flat `Vec`, because stable slot addresses are what make that view cheap;
and biasing the `NodeId` packing, because `OpaqueElement` is `NonNull` and index
0 / generation 0 packs to zero.

And a warning worth repeating: **do not model node flags on Servo.** It gets
`set_dirty_descendants` wrong today, with a live FIXME admitting a non-atomic
`Cell` read-modify-write from parallel style threads.

The largest de-risking lever is free: `traverse_dom` takes
`pool: Option<&rayon::ThreadPool>`, and `None` runs fully sequential. Phase 5
can ship stylo with no parallelism at all, which defers most of the unchecked
thread-safety contract.

Four backlog entries filed. The note's own top recommendation — a one-day spike
implementing a stub `TElement` over a toy chunked arena before the tree builder
is written — belongs in Phase 4's decomposition.

Campaign still running at 3h05m.

## 06:05 UTC — ADR 009 drafted, deliberately undecided

Campaign 3h30m in; shards are four hours, so ~06:35.

Drafted ADR 009, the IPC protocol shape, into the scratchpad. **This one is not
taken on the standing overnight authorisation, and that is the point of the
entry.** ADRs 007 and 008 were: 007 had a recommendation in §14.5, 008 had a
forced answer in the code — `px-broker` is `forbid(unsafe_code)` and cannot
spawn under a policy, so the operation moves or the gate fails. Neither
required inventing a position.

009 does. It is a protocol design with four defensible shapes and consequences
reaching Phase 21, and the spec never framed the question at all. The overnight
authorisation covers proceeding on a recommendation; it does not cover
manufacturing one. So the draft lays out the alternatives, recommends, and
stops.

The recommendation is symmetric request/response with correlation ids, with
three specifics: the id scopes a conversation and is **never** authority —
invariant 9 has to survive the change, and the broker still learns who from the
channel; a bounded number of outstanding requests per direction, because
concurrency must not undo the reason `QUEUE_DEPTH` exists; and oversized
payloads chunked, with shared memory deferred to Phase 8, where in-band handle
passing gets its first real consumer anyway.

The reason it wants deciding in Phase 2 despite nothing needing it until Phase
4: it is a wire-format change, and Phase 3 writes against whatever shape exists.
