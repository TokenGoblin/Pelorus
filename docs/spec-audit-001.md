# Spec audit 001 — the plan against what Phases 0–1 built

*2026-09-09. Requested by the spec itself.*

`docs/build-spec.md` Appendix B ends:

> The lesson to carry forward: **audit the plan again after any structural
> change to it.** Each revision introduced defects the previous audit could not
> have found.

Six structural changes have been made since the spec was written: the transport
is inherited pipes rather than a socket (ADR 005); the client/server direction
was inverted so the content process asks and the broker decides; `px-sandbox`
is becoming the audited unsafe core (ADR 008); ADR 007 rejects §14.5's binary
framing; frame visibility was separated from hosting; and `Response::Denied`
lost its reason.

This is the audit of the plan against them. Thirteen findings. What follows is
the record; the actionable ones are also in `docs/backlog.md`, and the two that
change Phase 2 are folded into that phase's plan.

---

## 1. The protocol has no broker→content direction — HIGHEST

`serve_once` is strictly `next_request → dispatch_guarded → reply`. Every
`Response` variant is an *answer*; `Request` is content→broker only. The broker
can physically push a frame but has no vocabulary for a **command**, and with
no correlation id the peer could not distinguish an unsolicited command from
the answer to its last request.

The spec needs broker-initiated messages in at least four places — §7.2's
`navigate`, `click`, `type`, `scroll`; Phase 14's `window.opener` and popups;
Phase 13's navigation. But the first phase that needs it is **Phase 4**:
something has to tell the content process which document to parse.

This is Appendix B item 1 one level down. Phase 1 built a boundary, and built
it request/response-only, single-in-flight and uncorrelated. **The spec is not
wrong here — it is silent.** §9 Phase 1's "broker request/response shape" never
says which side initiates, and no phase names bidirectionality, multiplexing,
concurrent in-flight requests, or backpressure as a deliverable.

**Action:** an ADR in Phase 2 settling the protocol shape, before Phase 3
writes against the current one. It must cover correlation ids,
broker-initiated messages, concurrency, and how a payload larger than
`MAX_MESSAGE_BYTES` is carried.

Related and unowned: §3's diagram routes every arrow through the broker, so
page bytes (Phase 3), display lists (Phase 8) and raster output all relay
through a per-peer transport that is one thread pair and a four-deep queue.
Nobody has costed that.

## 2. §7's MCP model requires handle passing — HIGH

§7.3 requires both "Local transport only… Unix socket (`0600`, user runtime
dir), or Windows named pipe with an explicit DACL" **and** "`px-mcp` is
sandboxed like a content process. No filesystem".

A socket in the runtime directory is a filesystem object; a named pipe with a
DACL is an OS handle. Either the broker owns the listener and hands the
accepted connection over — the exact `SCM_RIGHTS`/`DuplicateHandle` operation
ADR 005 deferred — or the second requirement is false.

So handle passing is a **hard Phase 21 requirement**, the first in the plan.
Phase 21's gate also says "no endpoint exists (verified by socket/pipe
enumeration)", and with inherited pipes every child already has anonymous
pipes, so that enumeration does not distinguish the MCP endpoint from routine
IPC.

## 3. ADR 005 deferred the wrong thing — HIGH, and it changes Phase 2

Phase 2's gate is "content process launches under a policy on both OSes".
Neither platform can do that from `std::process::Command`: Windows needs
`CreateProcessAsUserW` with a `PROC_THREAD_ATTRIBUTE_LIST`, Linux needs
`CommandExt::pre_exec`, which is `unsafe`. `px-broker` is
`#![forbid(unsafe_code)]` and calls `Command::new` directly.

**`ContentProcess::spawn` must move behind `px-sandbox`.** The broker keeps the
supervisor — the threads, the deadline, the restart policy — and gets a
sandboxed child handed back to it.

Meanwhile in-band handle passing has no consumer before Phase 8 or Phase 21, so
by ADR 005's own argument — "unused unsafe is unreviewable unsafe" —
implementing it in Phase 2 is premature. ADR 008's scope is right; its
deliverable list is wrong. **Phase 2 implements sandboxed spawn. Handle passing
moves to its first real consumer.**

## 4. §14.5 is factually wrong, and §11 and §13 carry the error forward — HIGH

The binary framing, and "the sysctl" in the singular when there are three
spellings across distributions. ADR 007 replaces it with a ladder and a floor.
Three passages must change together or the next reader takes the stale one:
§14.5 itself, §11's row, and §13 item 7.

## 5. Gate items that are now vacuous or measure the wrong thing — HIGH

Phase 1's "broker rejects any message asserting its own identity" is
unobservable as a test: there is no such message to reject, because the type
cannot express one. What carries the property is two source checks — one of
which was **close to inverted** until an adversarial session found it. The gate
should say "no wire type can name its sender, proven by a negative control".

"Killing the content process is recovered from cleanly" measures the easy case.
Killing closes pipes. A peer that *stalls* was untested, and stalling is
cheaper for an attacker than crashing.

Downstream, the same shape: **Phase 14** should test that a content process
cannot enumerate frames it was not granted, and cannot distinguish "gone" from
"not yours" — a same-origin-class leak needing no memory corruption, which by
§12's own logic outranks the escape its gate does test. Also, `FrameTree` is
flat, so "nested cross-origin frames to depth 5" is new work, not an extension.

And a standing risk §11 does not name: **the gate checks are the least-reviewed
code in the project.** Phase 1 found seven defects in the checks themselves.

## 6. §2.1, §2.2 and §8 are contradicted by shipped decisions — MEDIUM-HIGH

§2.1's "both set from one workspace variable" is impossible; Cargo requires a
literal `[[bin]] name`. §2.2's allowlist is missing `CLAUDE.md`. §8 instructs a
maintainer to commit the archives that ADR 002 says must not be published —
**and §8's replay therefore cannot run in this repository at all.** ADR 002's
"decide where the compat replay runs" is unassigned, and Phase 23's gate has no
venue.

## 7. Nobody builds the audit log — MEDIUM-HIGH

§3 and §7.3 both assign it to the broker. What exists is a 256-entry in-memory
ring buffer: not append-only, not durable, not user-readable, and **a hostile
channel evicts every prior record with 256 denials** — which is exactly what an
attacker generates. A defensible Phase 1 skeleton; the problem is that no phase
owns replacing it, and Phase 21's gate ("every tool call denied *and logged*")
inherits it. The consent-prompt machinery §3 assigns to the broker is likewise
unowned.

## 8. §4.4 bounds the content process; the broker is unbounded — MEDIUM-HIGH

Per-peer queues are 8 MiB, times N processes at one-per-site, plus a leaked
thread and pipe per hostile restart. §4.4 needs a broker clause.

## 9. Ordering: two things move into Phase 2, one moves later — MEDIUM

**Into Phase 2:** process-tree teardown — job object membership on Windows,
cgroup membership on Linux. A job object is created *at* `CreateProcess`, so
"retrofit at Phase 17" means writing the same function twice. Phase 17 keeps
the *limits*; Phase 2 takes *membership*. Also §14.4's release-artifact scan,
already pulled forward because Phase 2 introduces the first test-only
capability.

**Later:** in-band handle passing, to Phase 8 or 21.

**New:** the IPC protocol ADR, at Phase 2.

Everything else in §9's ordering still holds.

## 10. §4.3's untested-configuration shape recurs — MEDIUM

Five more properties asserted about configurations nothing tests:
`overflow-checks = true` in `[profile.release]` (tests run under `test`, where
it is on anyway — delete the line and everything stays green); `env_clear()`,
with no test asserting the child's environment is empty, and it is currently
the *only* enforcement of invariant 1; `CLAUDE.md`'s "CI enforces by symbol
audit", which it does not; invariant 4's "no telemetry endpoint compiled into
the binary", checked by grepping source rather than an artifact; and
`_QUEUE_BYTES_CEILING`, a constant documenting a bound nothing asserts.

Two retired: reproducibility path-remapping and the supply-chain gate, both
vacuous at Phase 0 and real now.

## 11. The threat model has four uncovered surfaces — MEDIUM

ADR 001 says it should be rewritten, not amended, if the architecture changes
shape. It has.

1. **The build machine and the dependency author.** Eight of thirteen
   dependencies are audited by nobody in our chain, and a compromised
   `serde_derive` executes at build time. §11 has **no supply-chain row at all**.
2. **A compromised content process attacking the broker.** Adversary 1's
   controls are all about containing the page; the broker's IPC surface is not
   named as attack surface — and that is where all four Phase 1 adversarial
   findings landed.
3. **Denial of service as a goal.** Not an objective of any adversary. The
   three wedges and the 262,144× amplification were availability attacks on the
   one process that may not die.
4. **The broker as an information oracle.** The `DenyReason` leak defeated site
   isolation through the *answers*, with no memory corruption and no CSP
   bypass. This deserves a standing rule: **no `Response` variant may vary with
   state the caller was not granted.**

One improvement worth recording: inherited pipes have no filesystem endpoint,
so adversary 7 has strictly less surface than the socket §3 implies.

## 12. Decisions taken without being recorded — MEDIUM

The IPC direction, visibility-versus-hosting, `Denied` carrying no reason, and
the supervision constants (`MAX_RESTARTS`, `QUEUE_DEPTH`, `AUDIT_CAPACITY`,
`DEADLINE`) all exist only as code comments and gate-report prose. Each is an
architectural decision of ADR weight.

**More urgent than §13 implies:** the licence half of open decision 9 — a
public repository with no `LICENSE` is all-rights-reserved, which defeats the
stated reason for being public — and `SECURITY.md`, since reports can arrive
now. Both are filed at Phase 20.

## 13. Smaller staleness — LOW

§3 lists handle passing under `px-ipc`; it is not there and, per ADR 008, never
will be. §9 Phase 1's description claims handle passing was delivered. §4.5's
loom requirement has no lock-free code to apply to. §4.1 and §14.3 are written
about `NodeId`, but `FrameId` needed the same treatment in a crate they never
mention — generalise to *any handle crossing a process boundary*. §4.3 should
add what the code learned: the broker must survive a peer that stalls, not only
one that dies.

---

## Sections checked and still accurate

Invariants 2, 3, 5, 6, 7 and 10 are untouched. Invariant 9 is strengthened
rather than merely intact — the identity claim is unrepresentable, not
rejected. §4.1's and §4.2's substance, §5, §6, §10, §12 and Appendix A need no
change. §14.1, §14.2, §14.3, §14.6 and §14.7 stand.
