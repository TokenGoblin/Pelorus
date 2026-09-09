# Phase 2 gate report

*Sandbox skeleton. Branch `phase/02-sandbox`.*

Permissive but real policies on both platforms, the capability-detection ladder,
and fail-closed behaviour. Tightening is Phase 17's; what this phase buys is
that no code after it is written against ambient authority.

## The three gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| Content process launches under a policy on both OSes | `sandbox_policy_*` ×8, plus the real-process suite | **Pass** |
| With the sandbox forced unavailable, the browser refuses and says why | `sandbox_refuses_*` ×3, plus `px-browser` run as a binary | **Pass** |
| Linux fallback ladder documented and exercised (§14.5) | `sandbox_ladder_*` ×3, ADR 007 | **Pass** |

`ci/gate-sandbox.sh` passes on `ubuntu-latest` and `windows-latest`. 67 tests.

## What was built

**The policy is applied at creation, not afterwards.** This is the phase's one
real design decision, and the first implementation got it wrong.

The first attempt spawned normally and then confined: a job object the parent
attached after the fact, a token the child restricted for itself. Both
`crates/px-sandbox/CLAUDE.md` and ADR 008 rule that out, and they are right. A
child confined after creation runs unconfined for the window between
`CreateProcess` and whenever the policy lands — process and runtime
initialisation, loader work, anything a `DllMain` does. That window is
attacker-reachable and buys nothing. ADR 008 had already decided that
`ContentProcess::spawn` moves behind `px-sandbox`; the correction was to do
what the ADR said.

So `px-broker` keeps the supervisor — worker threads, deadline, restart policy
— and is handed a child that was *created* confined.

**Windows.** `CreateProcessAsUserW` with a token from
`CreateRestrictedToken(DISABLE_MAX_PRIVILEGE)`. The job object is attached by
`PROC_THREAD_ATTRIBUTE_JOB_LIST` at creation rather than retrofitted, which
`docs/spec-audit-001.md` finding 9 asked for and which means Phase 17 adds
limits to an object that already exists. Handle inheritance is restricted by
`PROC_THREAD_ATTRIBUTE_HANDLE_LIST` to exactly the pipe ends the child is meant
to have — that one is invariant 1, because `bInheritHandles = TRUE` without an
explicit list hands the child every inheritable handle in the broker,
including other content processes' pipes.

**Linux.** `no_new_privs`, then a seccomp filter, both in `pre_exec`, so the
policy is in place before the image runs. The BPF program is built before the
fork deliberately: the closure runs in a forked child of a multi-threaded
broker, where allocating can deadlock on an allocator lock another thread held
at the moment of the fork.

**Neither side takes the other's word.** Both platforms read state back rather
than trusting a setter's return value, and `applied()` reports only what the OS
confirmed. Nothing asks the child whether it confined itself — that would be
authority taken from a message, which invariant 9 forbids. On Linux a failure
inside `pre_exec` fails the spawn, so an unconfined content process is never
returned to the broker.

## How the claims were checked

Three of these are worth stating because a passing test is not evidence that a
test can fail.

**The job object was verified from the parent.** `IsProcessInJob`, asked by the
broker about the child it just created, is the one vantage point a content
process cannot influence. `spawn` returning `Ok` is a claim; this is the OS
answering it.

**That check was confirmed to fire.** Skipping the `AssignProcessToJobObject`
call while leaving everything else intact makes the test fail with "the job
object did not take: the process is not in it". The synthetic violation was
reverted.

**Both directions of the refusal are asserted.** `px-browser` exits 1 and names
the missing mechanism with the sandbox forced unavailable, and exits 0 on a
normal run. A refusal test passes trivially against a browser that refuses
everything, which is Phase 1's lesson about a product binary that failed on a
clean run behind a green gate.

**And the Linux path was proved non-vacuous.** The policy tests return early on
a machine below the floor — correct behaviour, but a silent skip is
indistinguishable from a pass, and that is this project's most expensive
recurring bug. The first green Linux run was not in fact vacuous, by an
indirect argument: `ContentProcess::spawn` now refuses below the floor, so the
seven crash-restart and size-limit tests that spawn real content processes
could not have passed unless `pre_exec` ran, both `prctl` calls succeeded, and
the filter installed without breaking the child. They passed.

Relying on another file's side effect is not a control, so the skip was made
explicit: the gate exports `PX_REQUIRE_SANDBOX=1`, under which a machine that
cannot clear the floor fails the suite and names the missing rungs. Verified to
fire the same way the job-object check was.

## What is weaker than it sounds

Recorded here rather than only at the mechanism, because a sandbox that is
trusted for more than it delivers is worse than one nobody trusts.

**The Linux filter is a deny-list.** A real sandbox is an allow-list. This one
denies seventeen syscalls that are escape or escalation primitives — `ptrace`,
`process_vm_readv`/`writev`, the `kexec` and module calls, mount and namespace
manipulation, `perf_event_open`, `bpf`, `userfaultfd` — and allows everything
else. It pins the architecture first, because syscall numbers are meaningless
without that and a filter matching the wrong numbers reads as protection.
Phase 17 owes an allow-list.

**The Windows token is not as restricted as its name.**
`DISABLE_MAX_PRIVILEGE` strips privileges, which is real and irreversible, but
this is not a low-integrity token, not an AppContainer, and carries no
restricted SIDs. A content process confined this way can still read the user's
files.

**The job object carries no limits.** It exists, membership is taken at
creation, and it is verified — but the memory cap and kill-on-close are Phase
17's.

**`px-content` still does nothing dangerous**, so the filter has not been
exercised by a real renderer. It will first meet one in Phase 4.

## What is outstanding

One item from `crates/px-sandbox/CLAUDE.md` that this phase does not close, and one it does.

**ASAN and TSAN builds of this crate do not run in CI.** The local invariant
requires them and §4.5 asks for them. They need `-Z sanitizer` and therefore
nightly, and ADR 006 fences nightly to `fuzz/` with a CI assertion that nothing
else resolves to it — so wiring them means amending that ADR, which is a
decision rather than a chore. Same shape as the Miri item already in
`docs/backlog.md`. **This crate now contains the project's only `unsafe`, which
makes it the crate where sanitizers matter most**, so this should not sit long.

**The adversarial review §10 requires for `px-sandbox` is done.** It found one
real defect and produced two tests for properties that are invisible until they
are violated.

*Fixed.* A BPF jump offset is a `u8`, and the filter builder saturated with
`unwrap_or(u8::MAX)` past 255 denied syscalls — a silently wrong filter, where
the jump lands on another instruction, the syscall is allowed, and the source
still reads as denying it. Now a compile-time assertion stops the build, and
the fallback is 0 rather than a plausible wrong number. Verified to fire.

*Tested.* The parent must keep no copy of the child's end of a pipe, or the
pipe never reports EOF when the child dies and the reader blocks forever —
Phase 1's wedge, which this phase could now cause by accident. And handles must
not accumulate: the broker spawns a content process per site and replaces one
on every crash, so a handle leaked per spawn is a denial of service any page
that can kill a renderer can reach. Forty spawns, zero growth.

*Recorded.* `GetExitCodeProcess` cannot distinguish a running process from one
that exited with code 259, so `is_alive` can report a dead content process as
live; and the aarch64 syscall table has never been executed, because CI is
x86_64 only. Both are in `docs/backlog.md`.

Neither is a gate item. The sanitizer gap is listed because the phase closing
is not the same thing as the crate being finished.

## Verdict

**All three gate items pass in CI on both operating systems.**

The one CI job still red is `compat-list`, Phase 0's outstanding deliverable,
which is no part of this gate and is red on `main` as well.

Said plainly: what this phase establishes is that a content process cannot be
launched without a policy, on either platform, and that failing to apply one
stops the launch rather than degrading it. What it does not establish is that
the policy is any good. The mechanisms above are the weakest ones that are
still real, chosen so that Phase 17 tightens something already load-bearing
rather than introducing a boundary nothing was written against.
