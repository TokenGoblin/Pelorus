# px-sandbox

Process sandboxing; the only crate permitted unsafe. Implemented in phase 2.

Full specification: `docs/build-spec.md`. Root working agreement:
`/CLAUDE.md` — it applies here too; this file only adds what is local.

## Local invariants

- The sole exception to forbid(unsafe_code). This crate carries
  `#![deny(unsafe_op_in_unsafe_fn)]` instead.
- **This is the audited unsafe core, not only the sandbox** (ADR 008). Every
  unsafe OS operation in the project lives here, whether or not it is about
  sandboxing, and other crates get safe wrappers. The crate's name understates
  it; that is the price of the single-exception rule, and it is cheaper than a
  second exception the gate has to know about.
- The Phase 2 deliverable is sandboxed **spawn**, not handle passing (ADR 008,
  correcting ADR 005). px-broker is forbid(unsafe_code) and cannot apply a
  policy from std::process::Command: Windows needs CreateProcessAsUserW with a
  PROC_THREAD_ATTRIBUTE_LIST, Linux needs pre_exec. In-band handle passing
  waits for its first real consumer — Phase 8 or Phase 21.
- Job object and cgroup **membership** is taken at spawn, not retrofitted at
  Phase 17 (spec audit 001, finding 9). A job object is created *at* process
  creation; adding it later means writing the same function twice. Phase 17
  keeps the limits.
- The sandbox is a ladder with a floor (ADR 007). Apply every rung available,
  record which applied, and refuse below the floor. Never degrade silently, and
  never name a sysctl you did not check — the restriction is spelled three
  different ways across distributions.
- A capability probe that wrongly reports success is far worse than one that
  wrongly reports failure: the first launches an unsandboxed process believing
  it is sandboxed. Probes fail closed; inconclusive counts as unavailable.
- Every unsafe block carries a `// SAFETY:` comment justified against the
  OS documentation, not against what happened to work.
- Fail closed. If a policy cannot be applied, the process does not launch
  (invariant 8). Never a silent fallback to unsandboxed.
- ASAN and TSAN builds of this crate run in CI (§4.5).
- Adversarial review required (§10).
