# 047 — WebAssembly in v1: none, revisited after the Phase 23 compat run

- **Status:** proposed. Awaiting the user. No code, no dependency, no manifest change.
- **Date:** 2026-11-05
- **Phase:** 10 (build-spec section 9 Phase 10: "WASM ADR due"); the decision itself is
  reopened at Phase 23
- **Invariants touched:** 6 (no JIT: a wasm engine that compiled to native code would reopen
  it; an interpreter would not), 1 and 8 (a new untrusted-binary parser in the content
  process). Appendix A ("Boa, interpreter only") is not re-argued.

## Context

Phase 10 owes a WebAssembly decision, and ADR 040 already contains the recommendation in
one paragraph ("WASM recommendation") without a standalone record. This ADR is that record,
so the decision can be accepted, rejected or reopened on its own.

What is true now, checked rather than assumed:

- **Boa 0.22.0 has no `WebAssembly` global.** ADR 040 counted zero occurrences in its
  source. The host asserts it: `crates/px-script/tests/globals.rs`
  (`script_absent_*`) tests `typeof WebAssembly === "undefined"` beside `Intl`, `Temporal`
  and `SharedArrayBuffer`, so the absence is deliberate and cannot regress by accident.
- **test262 says nothing about it.** The vendored subset contains no WebAssembly test
  (`tests/test262/INDEX` has no entry; the language's own test262 does not cover the JS
  API), so the 98.5% on the vendored subset is silent on this question.
- **No wasm interpreter crate was probed.** The research note lists options; none was built,
  sized or counted, so this ADR gives no number for one and will not invent it.
- **No measurement says how many of the target sites need it.** The forty-site list
  (`tests/compat`) is empty (docs/backlog.md), and the Phase 23 compat run is the only
  instrument that can say which sites break because `WebAssembly` is undefined.

## Decision

**No WebAssembly in v1.** `typeof WebAssembly` is `"undefined"`, on purpose, and the
preferences page documents it beside the no-JIT statement. The decision is **revisited
after the Phase 23 compat run**, using that run's data: if sites in the forty fail for no
other reason than the missing global, a follow-up ADR measures a specific interpreter
crate (size, new crates, unsafe tokens, speed on a fixed module, a fuzz target) and
proposes it.

## Reasons

1. A wasm module is an untrusted binary parsed and validated in the content process: a new
   parser surface, a new fuzz target, a JS API (`Module`, `Instance`, `Memory`, `Table`,
   streaming compile) and its own memory cap, all inside the process the project already
   treats as hostile.
2. The dependency directive is "as few as possible", and the only candidates are
   interpreters (invariant 6 forbids native code generation), which are slow by design;
   a feature that is present but one to two orders of magnitude slower than a JIT may serve
   the sites that need it worst.
3. Libraries that use wasm for speed commonly feature-detect and fall back to JS. Which of
   the forty do not is a fact to measure, not to assume from the build spec's "much of the
   web assumes it".
4. Doing nothing is reversible and costs nothing now; adding it is neither.

## Alternatives rejected

- **Add a wasm interpreter crate now.** No measurement exists to justify the attack
  surface or the dependency; see reasons 1 and 2.
- **Expose a stub `WebAssembly` object whose methods reject.** Feature detection
  (`typeof WebAssembly !== "undefined"`) would then succeed and send pages down a path that
  cannot work, which breaks pages that would otherwise have fallen back.
- **Defer without recording.** Rejected: the phase plan says an ADR is due, and an
  unrecorded absence reads as an oversight.

## Consequences

- Pages that require wasm fail, visibly, at first use (`WebAssembly is not defined`).
  Some of the forty will.
- No wasm code is fuzzed, reviewed or sandboxed in v1.
- The decision has a named trigger (the Phase 23 compat run) and a named owner (the user).

## Verification

The decision is wrong if the Phase 23 run shows more than a handful of the forty sites
unusable for this reason alone, or if one of them is a site the user has said must work.
Before that run no observation can falsify it; that is why it is deferred rather than
closed.
