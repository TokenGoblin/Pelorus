# 011 — A second nightly consumer: sanitizers over `px-sandbox`

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 2 (closing an obligation the phase left open)
- **Invariants touched:** 7 (protected, by the same construction as ADR 006)
- **Amends:** ADR 006, which fenced nightly to `fuzz/` and said to point at
  itself when someone proposes a second nightly-only feature. This is that
  proposal, and it argues the case rather than assuming the exception.

## Context

`crates/px-sandbox/CLAUDE.md` states as a local invariant that ASAN and TSAN
builds of the crate run in CI, and build-spec §4.5 asks for them. **Neither
existed.** The invariant was written as though it already held, which is the
shape of claim this project has been caught by repeatedly, and Phase 2's gate
report recorded it as outstanding rather than letting it stand.

What changed in Phase 2 is that the claim now matters. `px-sandbox` went from a
crate with no `unsafe` to the crate holding *all* of it: two operating systems'
process-creation paths, an opaque variable-length `PROC_THREAD_ATTRIBUTE_LIST`
the OS sizes and we allocate, hand-assembled BPF, and handle lifetimes managed
by a hand-written RAII type on paths with several early returns. That is the
code sanitizers are for.

The obstacle is ADR 006. Sanitizers need `-Z sanitizer`, which is nightly-only,
and ADR 006 pinned stable 1.98 for the whole repository, fenced a single pinned
nightly to `fuzz/` by a directory override, and made CI assert the fence. Its
Consequences section anticipated exactly this request:

> this ADR to point at when someone proposes a nightly-only feature "since we
> already have one"

So the question is not "may we use nightly" but "is this the same kind of use,
or the drift ADR 006 exists to prevent".

## Decision

**Sanitizer jobs may use the nightly ADR 006 already pins — the same one, by
exact name — invoked as `cargo +nightly-2026-09-01` from CI.**

Three properties make this the same kind of use rather than an erosion:

1. **It is the same nightly, not a second one.** One nightly version exists in
   this project. A sanitizer job that pinned its own would double the surface
   ADR 006 was written to bound, so it does not get to.
2. **The fence is untouched, and still asserted.** ADR 006's mechanism is a
   directory-scoped `rust-toolchain.toml` plus a CI check that the repository
   root resolves to stable and `fuzz/` resolves to nightly. Invoking a
   toolchain by explicit `+name` changes neither resolution, so
   `ci/gate-fuzz-smoke.sh`'s assertion keeps passing *unmodified* and keeps
   meaning what it meant. Nothing about the fence is weakened; a caller simply
   names the toolchain instead of inheriting it.
3. **Nothing built here ships.** Sanitizer builds are test binaries, built to a
   separate target directory, never hashed by the reproducibility gate and
   never produced by the release profile. Invariant 7 constrains what ships,
   and this does not ship — the identical argument ADR 006 made for `fuzz/`.

**Scope: `px-sandbox` and `px-broker`, not the workspace.** ASAN on both
platforms; TSAN on Linux only, because TSAN has no Windows support. `px-broker`
is included for TSAN specifically: it is `forbid(unsafe_code)`, but it is where
the worker threads and the bounded queues live, and it is the process that may
not die.

**The job is required, not advisory.** A sanitizer job that is allowed to fail
is a sanitizer job nobody reads.

## Alternatives rejected

**Leave it in the backlog.** Defensible for a crate with no `unsafe`; not for
the one holding all of it. The concrete cost of waiting is that the Windows
attribute-list buffer — sized by the OS, allocated by us, written by the OS —
has no automated check that it is big enough, and getting that wrong is a heap
overflow in the code that creates every content process.

**Drop the invariant instead and amend the crate's CLAUDE.md.** Honest, and
genuinely on the table: it would replace an untrue statement with a true one at
no cost. Rejected because the invariant is *right* — this is the crate where
sanitizers pay for themselves — and the fix for "the document claims something
untrue" should be to make it true when the claim is worth keeping.

**Run sanitizers under a `rust-toolchain.toml` in a `sanitizers/` directory**,
mirroring `fuzz/`. Rejected: the point is to instrument `crates/px-sandbox`,
which lives where it lives. A directory override cannot reach it without
either moving the crate or building from a directory that does not contain it,
and `+toolchain` expresses the same thing without the pretence.

**Stable-only alternatives — Miri, `cargo careful`.** Miri cannot execute FFI
into `CreateProcessAsUserW` or `prctl` at all, which is the entire body of code
in question. It remains worth having for the safe arena code (already in
`docs/backlog.md`) and is not a substitute here.

## Consequences

**The nightly fence now has two consumers, and the argument for the third is
weaker than the argument for this one was.** That is the real cost, and it is
worth stating plainly: each exception makes the next easier to argue. The
mitigation is that this ADR is narrow on purpose — same pinned version, named
explicitly, non-shipping, and scoped to two crates — so a future proposal has
to meet those terms or amend them in the open.

**ASAN on Windows needs a runtime DLL that is not on `PATH` by default.**
`clang_rt.asan_dynamic-x86_64.dll` lives in the MSVC toolchain directory, and
without it the test binary dies with `STATUS_DLL_NOT_FOUND` before running a
single test — a failure that looks like a broken build rather than a missing
environment variable. `ci/gate-sanitizers.sh` locates it through `vswhere` and
says so when it cannot.

**TSAN reports on uninstrumented `std` unless `std` is rebuilt.** The job
passes `-Z build-std` for TSAN, which needs the `rust-src` component — already
present in the pinned nightly, for `fuzz/`.

**CI gets slower.** Sanitized builds do not share artifacts with the ordinary
ones, so this is a second full build of the affected crates on each platform.

## Verification

This decision is wrong if the sanitizer job is green while unable to detect
anything — the failure mode that makes a check worse than no check, because it
converts an absence of evidence into apparent assurance.

So the arming is checked rather than assumed, and was before this ADR was
accepted: a deliberate heap overflow was injected into `px-sandbox` and the
Windows ASAN build reported it as
`ERROR: AddressSanitizer: heap-buffer-overflow`, naming the file and line. The
probe was reverted. `ci/gate-sanitizers.sh --self-check` reproduces that on
demand rather than leaving it as a claim in this document.

It is also wrong if the fence leaks — if any shipping artifact is ever built by
the nightly. That assertion already exists in `ci/gate-fuzz-smoke.sh` and is
deliberately left unchanged by this ADR, so it continues to test the same
property.
