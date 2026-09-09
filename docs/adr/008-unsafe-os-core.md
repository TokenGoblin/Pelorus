# 008 — px-sandbox is the audited unsafe core

- **Status:** accepted, pending the user's confirmation — see ADR 007's note
- **Date:** 2026-09-09
- **Phase:** 2
- **Invariants touched:** the `forbid(unsafe_code)` hard rule, made specific

## Context

ADR 005 deferred this. It observed that `px-ipc` is required by §3 to do handle
passing, that handle passing needs `SCM_RIGHTS` on Unix and `DuplicateHandle`
on Windows, and that neither has a safe wrapper in `std` — while the working
agreement forbids `unsafe` everywhere but `px-sandbox`. Phase 1 avoided the
collision by not needing a handle passed. Phase 2 cannot: applying a sandbox
policy *is* unsafe FFI.

So the question ADR 005 postponed has to be answered now: when a crate needs an
unsafe OS call and is not `px-sandbox`, where does that call live?

## Decision

**`px-sandbox` owns every unsafe OS operation in the project**, not only the
ones that are about sandboxing. Its remit widens from "process sandboxing" to
"the audited unsafe core, which is also where sandboxing lives".

Other crates get safe wrappers from it.

**The first of those is not handle passing — it is spawning.** A spec audit
(`docs/spec-audit-001.md`, finding 3) found that ADR 005 deferred the wrong
operation. Phase 2's gate is "content process launches under a policy on both
OSes", and neither platform can do that from `std::process::Command`: Windows
needs `CreateProcessAsUserW` with a `PROC_THREAD_ATTRIBUTE_LIST`, Linux needs
`CommandExt::pre_exec`, which is `unsafe`. `px-broker` opens with
`#![forbid(unsafe_code)]` and calls `Command::new` directly, so
`ContentProcess::spawn` moves behind `px-sandbox`. The broker keeps the
supervisor — the worker threads, the deadline, the restart policy — and is
handed a sandboxed child.

In-band handle passing has no consumer before Phase 8 (GPU surfaces) or Phase
21 (§7.3's MCP endpoint, which the same audit shows *requires* it). By ADR
005's own argument — unused unsafe is unreviewable unsafe — it waits for one.

## Alternatives rejected

**A new `px-os` crate**, with `px-sandbox` layered on top. This is the better
*name*: the unsafe core would be called what it is, and "sandbox" would mean
sandbox. Rejected for one reason, which is worth being explicit about because
the naming argument is genuinely good: the rule that matters is **one crate
with unsafe**, and every split creates a second place a reviewer must think
about. `ci/gate-unsafe-headers.sh` hardcodes a single exception; two crates
means two exceptions, and the gate stops saying "unsafe lives here" and starts
saying "unsafe lives in these".

Revisit if `px-sandbox` grows to the point where the sandbox policy and the OS
primitives are genuinely two jobs. That is a real possibility by Phase 17, and
splitting then is cheap because the callers already only see safe wrappers.

**Unsafe in each crate that needs it, with local review.** This is what the
`forbid(unsafe_code)` rule exists to prevent. Rejected without qualification.

**A dependency that wraps the syscalls.** These are the operations the entire
process model rests on. Rejected.

## Consequences

**The crate's name now understates it**, and a reader who greps for where
`DuplicateHandle` lives will not guess `px-sandbox`. Mitigated by saying so in
`crates/px-sandbox/CLAUDE.md` and in the crate documentation, which is weaker
than a good name and is the price of the single-exception rule.

**`px-sandbox` becomes the crate with the highest review cost in the project.**
Everything in it is unsafe FFI against two operating systems' documentation,
and §10 already requires it to get an adversarial session. That session's scope
now includes operations that have nothing to do with sandboxing.

**`// SAFETY:` comments must cite the OS documentation**, not the observed
behaviour. "This works" is what a comment says the day before the kernel
changes; "SCM_RIGHTS transfers a duplicated descriptor, per unix(7)" is a claim
someone can check.

**ASAN and TSAN builds of this crate (§4.5) now cover more than the sandbox.**
That is an argument for the decision rather than against it: the audited-unsafe
surface has one CI treatment, not two.

## Verification

Falsified if `px-sandbox` ends up containing two clearly separable bodies of
code with no shared invariants — at which point `px-os` is the right split and
the gate grows a second, deliberate exception.
