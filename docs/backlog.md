# Backlog

Out-of-phase defects land here, not in the current phase. Phases that absorb
every defect they encounter never close.

Format: one entry per defect.

```
## <short title>
- **Found in:** phase NN, <crate or file>
- **Belongs to:** phase NN (or: unassigned)
- **What:** <the defect, concretely>
- **Why deferred:** <why it is not this phase's work>
```

---

## Release-artifact scan for test-only capabilities

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** phase 8 (local CA anchor) and phase 12 (testdriver shim), and
  must exist before either ships
- **What:** §14.4 requires CI to scan built release binaries for the test CA's
  fingerprint and the WebDriver/automation entry symbols, and fail if either is
  present. Symbol absence in the artifact, not a feature flag being off — flags
  get enabled by accident. No such job exists.
- **Why deferred:** neither capability exists yet, so the scan would assert the
  absence of something that was never there and would pass for the wrong
  reason. It must land in the same phase as the capability it polices, not
  after it.

## Symbol audit for network and filesystem confinement

- **Found in:** phase 0, `CLAUDE.md`
- **Belongs to:** phase 3 (network side), phase 13 (filesystem side)
- **What:** the working agreement states "No network calls outside px-net and
  px-update. No filesystem access outside px-broker and px-store. CI enforces
  by symbol audit." CI does not. The rule is currently a convention.
- **Why deferred:** every crate is empty, so the audit has nothing to inspect
  and no baseline to hold. It needs to arrive with the first crate that could
  violate it, which is px-net in phase 3.

## Miri, ASAN, TSAN, loom and fuzz jobs

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** phase 1 (fuzz + loom, px-ipc), phase 2 (ASAN/TSAN,
  px-sandbox), phase 4 (Miri, px-dom)
- **What:** §4.5 requires Miri on px-dom, px-ipc and px-store unit tests, ASAN
  and TSAN builds of px-sandbox, loom for lock-free code in px-ipc, and
  cargo-fuzz targets for every parser and every IPC deserializer. None exist.
- **Why deferred:** each covers a crate that is currently an empty skeleton.
  Adding them now would mean six green jobs that inspect nothing, which is
  worse than no job — a passing check nobody has read is a check people stop
  reading.

## Windows half of the reproducibility gate does not vary the username

- **Found in:** phase 0, `.github/workflows/gate.yml`
- **Belongs to:** unassigned; revisit if a username ever reaches a binary
- **What:** gate check 2 asks for hashes reproducible across machines with
  different paths *and usernames*. The Linux job creates a second user and
  builds as them. On a hosted Windows runner a second interactive account is
  not practical, so the Windows job varies path, `HOME` and `CARGO_HOME` only.
- **Why deferred:** the residual risk is narrow — a username reaches a binary
  through a path, and paths are remapped — but that is an argument rather than
  a test, and it is recorded as one in `docs/phase-00-gate-report.md`.

## GitHub runner kernel features for the sandbox suite

- **Found in:** phase 0, while writing the CI matrix
- **Belongs to:** phase 2, confirmed by phase 17
- **What:** the sandbox gates need Landlock, seccomp-bpf and unprivileged user
  namespaces on the Linux runner. Hosted runner kernels may not offer all
  three, and §14.5 already flags that several distributions restrict
  unprivileged user namespaces by default.
- **Why deferred:** phase 2's ADR on the Linux fallback ladder is where this
  gets decided. Worth knowing before that ADR is written that CI itself may be
  one of the environments where the sandbox is unavailable.

## deny.toml licence allowlist is speculative

- **Found in:** phase 0, `deny.toml`
- **Belongs to:** phase 3
- **What:** the allowlist permits what §3's named dependencies are expected to
  carry. It has never matched a real crate, and cargo-deny warns about every
  unused allowance.
- **Why deferred:** correcting it against reality requires having the
  dependencies. Prune it when phase 3 lands the first ones.

## IPC has no request/response correlation id

- **Found in:** phase 1, `crates/px-ipc/src/message.rs`
- **Belongs to:** phase 2, with the socket transport
- **What:** `Request` and `Response` carry no id, so if a reply is ever lost or
  reordered, response N answers request N-k and **neither side can detect it**.
  `serve_once` closes the channel on any reply failure, which makes the state
  unreachable today rather than merely unlikely — but the protocol has no way
  to notice if a future caller is less strict.
- **Why deferred:** it is a wire-format change, and the transport is being
  replaced in phase 2 anyway (ADR 005). Doing both at once is one migration
  instead of two.

## Spec amendments owed after audit 001

- **Found in:** phase 1, `docs/spec-audit-001.md`
- **Belongs to:** whoever next edits the spec; several block later phases
- **What:** thirteen findings. The spec is factually wrong in §14.5 (the Linux
  sandbox is four mechanisms, not one, and "the sysctl" has three spellings),
  §2.1 (Cargo cannot template a `[[bin]]` name), §2.2 (allowlist), §8 (tells a
  maintainer to commit archives ADR 002 forbids), and §3/§9 Phase 1 (handle
  passing, never delivered and moving to px-sandbox). §11 has no supply-chain
  row and no row for the gate checks being the least-reviewed code. §13 is
  missing four decisions that are now open.
- **Why deferred:** amending the spec mid-phase, unattended, is how a
  specification stops being a shared reference. These are recorded and should
  be applied deliberately, together, by someone who can weigh them.

## Nobody owns the broker's audit log or the consent prompts

- **Found in:** phase 1, audit 001 finding 7
- **Belongs to:** unassigned — needs a phase
- **What:** §3 and §7.3 both assign the audit log to the broker. What exists is
  a 256-entry in-memory ring buffer that a hostile channel can flush with 256
  denials, which is exactly what an attacker generates. Not append-only, not
  durable, not user-readable. Phase 21's gate ("every tool call denied *and*
  logged") inherits it. §3 also assigns consent prompts to the broker and no
  phase builds them.
- **Why deferred:** it is a phase-assignment question, not a defect to fix in
  place. The ring buffer is an honest Phase 1 skeleton; what is missing is
  anyone owning its replacement.

## Where the compat replay runs

- **Found in:** phase 1, audit 001 finding 6
- **Belongs to:** before phase 23, which has no venue without it
- **What:** ADR 002 keeps the traffic archives out of this public repository.
  §8's replay therefore cannot run here at all, and Phase 23's gate ("compat
  suite green on all forty sites") has nowhere to execute.
- **Why deferred:** it depends on the site list, which is the user's to write.

## No test asserts the content process's environment is empty

- **Found in:** phase 1, audit 001 finding 10
- **Belongs to:** phase 2
- **What:** `env_clear()` appears exactly once — at the call site. Nothing
  asserts the child sees an empty environment, and it is currently the *only*
  enforcement of invariant 1's "no ambient authority" before the sandbox
  exists.
- **Why deferred:** phase 2 is building the spawn path and will own this.

## px-css cannot be forbid(unsafe_code) — a hard rule will have to bend

- **Found in:** phase 1, `docs/research/stylo-requirements.md`
- **Belongs to:** an ADR before phase 5, and an amendment to `/CLAUDE.md`
- **What:** stylo's `TElement` trait declares six `unsafe fn` methods, so any
  crate implementing it must write `unsafe fn` — which `#![forbid(unsafe_code)]`
  rejects outright. Verified by compiling a probe against the pinned toolchain:
  `error: implementation of an 'unsafe' method`. `/CLAUDE.md`'s first hard rule
  and Phase 0's gate check 4 both say every crate but `px-sandbox` carries
  `forbid(unsafe_code)`.
- **Why deferred:** it is a rule change, and the useful version is *narrower*
  than a waiver. The research note proposes: `unsafe fn` declarations permitted
  in `px-css`, but zero `unsafe` blocks and zero `unsafe impl` — greppable, and
  enforceable by `ci/gate-unsafe-headers.sh` as a distinct rule rather than an
  exemption. Decide it before Phase 5, not during.

## stylo's build script breaks the reproducible-build gate

- **Found in:** phase 1, `docs/research/stylo-requirements.md`
- **Belongs to:** an ADR before phase 5
- **What:** stylo's `build.rs` shells out to Python 3 and Mako. Invariant 7 says
  same source plus same toolchain gives the same binary hash; a build that
  depends on an external interpreter and a template library does not, and Phase
  0's reproducibility gate has held since the workspace was empty.
- **Why deferred:** the options — pin and vendor the generator, commit its
  output, or accept a documented exception to invariant 7 — are a decision, and
  invariant 7 is load-bearing enough that it should not be amended by whoever
  happens to hit this first.

## Phase 4's gate omits the mutation-side snapshot path stylo needs

- **Found in:** phase 1, `docs/research/stylo-requirements.md`
- **Belongs to:** phase 4
- **What:** stylo's invalidation needs prior-state snapshots recorded on the
  mutation path (`ServoElementSnapshot`-shaped, keyed by an opaque node id).
  That is a `px-dom` feature, it is absent from Phase 4's gate, and retrofitting
  it means touching every attribute setter twice.
- **Why deferred:** phase 4 owns it; recorded now so the gate can be written
  with it rather than amended after.

## Style fixtures must run with debug assertions on

- **Found in:** phase 1, `docs/research/stylo-requirements.md`
- **Belongs to:** phase 5
- **What:** stylo's `ElementDataWrapper` is an `UnsafeCell` whose aliasing check
  is `#[cfg(debug_assertions)]` only. In a release build, aliasing is silent
  undefined behaviour rather than a panic — and `parallel.rs`'s module doc
  claiming "we'll generally panic if something goes wrong" is stale relative to
  `data.rs` at the same commit.
- **Why deferred:** phase 5 owns the fixtures; the constraint needs to be in
  their CI job when it is written.

## Miri is not run on px-ipc

- **Found in:** phase 1, `.github/workflows/gate.yml`
- **Belongs to:** phase 2
- **What:** build-spec §4.5 names Miri on `px-dom`, `px-ipc` and `px-store`
  unit tests. `px-ipc` now exists and has 15 of them; there is no Miri job.
- **Why deferred:** Miri needs a nightly toolchain, and ADR 006 fences nightly
  to `fuzz/` with a CI assertion that nothing else resolves to it. Adding a
  second nightly consumer means amending that ADR, which is a decision rather
  than a chore. `px-ipc` has no `unsafe` and no FFI, so what Miri would add
  today is UB detection in `std` calls — real but not urgent.

## check_not_serializable cannot see an aliased derive macro

- **Found in:** phase 1, `ci/check_not_serializable.py`
- **Belongs to:** unassigned
- **What:** `use serde::Serialize as Ser; #[derive(Ser)]` passes the textual
  check.
- **Why deferred:** the structural check beside it — `px-broker` has no `serde`
  dependency, so it cannot name the trait under any alias — is what actually
  holds the property. Defeating the textual check requires a deliberate hand
  *and* adding the dependency the other check rejects. Recorded so nobody
  mistakes the textual check for the guarantee.

## Orphaned pipes leak a worker thread per restart

- **Found in:** phase 1, `crates/px-broker/src/lib.rs`
- **Belongs to:** phase 17
- **What:** if a content process orphans its stdout to a grandchild and exits,
  killing the child does not close the pipe, so the reader thread stays blocked
  forever. Measured at exactly one leaked thread per restart. `MAX_RESTARTS`
  now bounds it at 8 per process.
- **Why deferred:** the real fix is process-tree teardown — Windows job objects
  and Linux cgroups — which is phase 17's work. The cap turns an unbounded leak
  an attacker drives into a bounded one.

## Destroying a frame leaves its handle in every capability set

- **Found in:** phase 1, `crates/px-broker/src/lib.rs`
- **Belongs to:** the phase that adds `Request::DestroyFrame`
- **What:** `FrameTree::destroy` frees the slot but touches neither the
  owner's `ChannelCaps::hosts` nor any viewer's `ChannelCaps::visible`. Today
  that is unreachable — `destroy` has no caller outside tests and `dispatch`
  handles only `Ping`, `Echo` and `FrameHost` — but `destroy`'s own doc
  comment anticipates `Request::DestroyFrame` arriving, and the obvious
  wrapper for it inherits the problem.

  There is no capability confusion. `can_see` and `holds_frame` both consult
  `FrameTree::owner` first, so a destroyed frame fails closed however stale
  the sets are, and slot reuse bumps the generation. The defect is unbounded
  growth, and it has a second half that is easy to miss: `close_channel`
  builds its `live` set by unioning every channel's `hosts`, so stale `hosts`
  entries keep the matching stale `visible` entries alive through the very
  purge that was added to stop those sets growing without bound.

  Verified rather than reasoned about: a temporary in-crate test destroyed a
  granted frame, closed an unrelated channel, and asserted both halves —
  `hosts` retained the frame and the viewer's `visible` entry survived the
  purge. Both passed. The test was reverted; it belongs to the phase that
  makes the path reachable.
- **Why deferred:** phase discipline. It is not a phase 1 gate item, no wire
  message reaches it, and inventing `Broker::destroy_frame` now to fix a
  defect nothing can trigger is exactly the scope creep the backlog exists to
  absorb. Recorded so the wrapper is written with the capability sets in mind
  rather than discovered leaking later.

## No ASAN or TSAN build of px-sandbox in CI — CLOSED

- **Found in:** phase 2, `crates/px-sandbox/`
- **Closed by:** ADR 011 and `ci/gate-sanitizers.sh`, phase 2.
- **What it was:** the crate's CLAUDE.md stated as a local invariant that ASAN
  and TSAN builds run in CI, and none existed. Phase 2 had just given this
  crate the project's entire `unsafe` surface, which made the gap worse than
  the wording admitted.
- **How it closed:** ADR 011 amended ADR 006's nightly fence to admit exactly
  one further consumer — the same pinned nightly, named explicitly so the
  directory override and its CI assertion are untouched, and building only
  test binaries that never ship. ASAN runs on both platforms, TSAN on Linux
  only. The sanitizer is proved able to fail before a clean run is trusted.

## A content process exiting with code 259 reads as still running on Windows

- **Found in:** phase 2, `crates/px-sandbox/src/windows.rs`
- **Belongs to:** unassigned
- **What:** `GetExitCodeProcess` reports `STILL_ACTIVE` (259) for a running
  process, so `Child::try_wait` cannot distinguish that from a process that
  genuinely exited with code 259. `is_alive` would report a dead content
  process as live, and the supervisor would keep serving a channel whose peer
  is gone until the deadline reaps it.
- **Why deferred:** the ambiguity is in the Win32 API rather than in this code,
  and the fix is to wait on the process handle — `WaitForSingleObject` with a
  zero timeout — and use `GetExitCodeProcess` only to retrieve the code once
  the handle is known signalled. That is a small change, but it belongs with
  the Phase 17 work that revisits process teardown rather than being slipped in
  after the gate passed. The exposure today is bounded: `px-content` exits 0 or
  is killed, `next_request`'s deadline catches a silent peer regardless, and
  nothing chooses 259.

## The aarch64 seccomp syscall numbers have never been executed

- **Found in:** phase 2, `crates/px-sandbox/src/linux.rs`
- **Belongs to:** the phase that adds an aarch64 CI runner
- **What:** `DENIED_SYSCALLS` carries a second table for `aarch64`, and CI runs
  `x86_64` only. The x86_64 table is exercised on every Linux gate run; the
  aarch64 one is compiled at most. A wrong number there denies a syscall a
  renderer needs — a crash — or, worse, fails to deny one of the escape
  primitives while the code still reads as denying it.
- **Why deferred:** it needs an aarch64 runner, which is infrastructure rather
  than code. The structural tests (`sandbox_policy_*` in `linux.rs`) check the
  filter's *shape* on whichever architecture they run on, so a malformed
  program is caught; what they cannot check is whether 117 is really `ptrace`
  on this ABI. Recorded so the table is treated as unverified rather than as
  tested-by-association with the x86_64 one.

## The unsafe baseline does not count C or assembly

- **Found in:** phase 3, `ci/unsafe-audit.sh`
- **Belongs to:** unassigned, but it matters from now on
- **What:** the audit counts `unsafe` tokens in Rust sources. ADR 016 brought
  in `ring`, which is 120,164 lines of assembly and 5,413 lines of C. The
  audit reports it as 230 tokens. That is not a rounding error, it is the
  metric failing to see the majority of what was accepted — and it will be
  wrong in the same direction for every future dependency with a native core,
  which for a browser means the GPU stack, the font shaper and the image
  decoders.
- **Why deferred:** the fix is a decision rather than a line of code. Counting
  C and assembly lines alongside Rust `unsafe` tokens puts two
  non-comparable numbers in one file, and a combined total would be
  meaningless. The honest options are a second baseline for native code, or a
  per-crate note recording what the count omits. Both change the shape of
  `ci/unsafe-baseline.json` and what the supply-chain gate compares, so it
  wants doing deliberately rather than in the middle of a phase that needed a
  TLS stack. Recorded now because the number is already misleading, and the
  ADR that made it so says as much.

## px-net is a library, not a process, and making it one needs ADR 009 first

- **Found in:** phase 3, `crates/px-net/`
- **Belongs to:** the phase that settles ADR 009, or Phase 4 — whichever comes
  first
- **What:** §9 Phase 3 asks for "`px-net` as its own process". It is a library.
  Everything else the phase names is built — rustls, HTTP/1.1, partitioned
  pools, the PSL — but the process boundary is not, and the gate does not
  check for it, so a passing gate does not mean the phase is complete.
- **Why deferred, specifically:** the broker→px-net vocabulary has to name a
  destination and a partition for each fetch, and `ci/gate-ipc.sh` forbids the
  field names `origin`, `partition`, `partition_key`, `channel` and friends
  anywhere under `crates/px-ipc/src/`. That rule is right for the
  content→broker direction it was written for — a content process must never
  describe its own authority — and the broker→px-net direction is a different
  relationship, because the broker *is* the authority and px-net is downstream
  of it.

  Renaming the fields to slip past the regex would be gaming a check this
  project put there deliberately, so the honest options are: a reasoned
  amendment to the gate that scopes the ban by direction; or a design where
  px-net learns the partition from *which channel* the request arrived on,
  which is invariant 9 in its strongest form but costs a channel per
  partition.

  Both are protocol-shape decisions, and ADR 009 — the IPC protocol shape — is
  marked PROPOSED with an explicit note that it was **not** taken on the
  standing authorisation because it is a design with several defensible shapes
  and consequences reaching Phase 21. Deciding this would be deciding that,
  quietly, from underneath.

## The fuzz campaign uses one -max_len for targets with different boundaries — CLOSED

- **Found in:** phase 3, `ci/gate-fuzz-smoke.sh` and the campaign workflow
- **Closed by:** `ci/gate-fuzz-smoke.sh`'s `max_len_for`, phase 3, in the
  commit after the one that recorded the campaign.
- **What it was:** `-max_len` was 1,100,000 for every target. That number was chosen in
  Phase 1 to straddle `px-ipc`'s `MAX_MESSAGE_BYTES` of 1,048,576, and for the
  IPC targets it is exactly right. The HTTP targets have different limits —
  `MAX_BODY_BYTES` at 32 MiB and `MAX_CHUNK_BYTES` at 8 MiB — and the campaign
  never approached either, so those boundaries are unfuzzed while the gate item
  reads as passed.
- **How it closed:** a per-target value. The HTTP targets get 8,500,000,
  just past `MAX_CHUNK_BYTES`; everything else keeps 1,100,000. Deliberately
  not 32 MiB, for the reason below.
- **Why it was deferred one commit:** raising the global `-max_len` to 32 MiB would make every
  target spend its budget generating enormous inputs instead of exploring
  structure, which would make the *IPC* coverage worse to improve the HTTP
  coverage. The fix is a per-target `-max_len`, which means the campaign matrix
  and the smoke script both learn that targets differ — a small change to two
  files and a slightly less uniform gate. Recorded rather than done because the
  Phase 3 gate report already states plainly what was and was not covered, and
  changing the fuzzing harness while recording a campaign result would mean the
  recorded numbers came from a configuration that no longer exists.
