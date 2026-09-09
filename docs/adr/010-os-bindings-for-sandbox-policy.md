# 010 — Take `libc` and `windows-sys` for sandbox policy application, scoped to `px-sandbox`

- **Status:** accepted
- **Date:** 2026-09-09
- **Phase:** 02
- **Invariants touched:** none. Invariant 1 (no ambient authority) is what this
  enables; nothing here changes what the product promises.

## Context

Phase 2's gate needs a content process to launch *under a policy* on both
operating systems. `px-sandbox` already detects the capability ladder, defines
the floor and produces the refusal (ADR 007); all of it is file reads under
`/proc` and `/sys`, and it needs no dependency. Applying a policy is different.
It needs `prctl(PR_SET_NO_NEW_PRIVS)` and `seccomp` on Linux, and
`CreateRestrictedToken` with `CreateProcessAsUserW` and a
`PROC_THREAD_ATTRIBUTE_LIST` on Windows.

Neither `libc` nor `windows-sys` appears in build-spec §3's list, and §3's rule
is to propose rather than add. The alternative is hand-declaring the externs,
which adds no dependency and suits an audited unsafe core.

What was true when this was decided, including what was not known:

- The machine this was decided on runs Windows. The Linux path cannot be
  executed locally at all; it is validated only through CI.
- An earlier note in `docs/overnight-log.md` recommended these crates while
  warning that `windows-sys` historically pulled `windows-targets` and a set of
  per-architecture crates shipping **prebuilt binary import libraries**. That
  is no longer true and the recommendation was re-measured rather than
  inherited: at 0.61, `windows-sys` depends only on `windows-link` 0.2.1, which
  is 39 lines, `no_std`, has zero dependencies, contains no `unsafe`, and ships
  no binaries. It emits `#[link(kind = "raw-dylib")]` declarations, so no
  import library exists to trust.
- What is *not* known: whether the policies these will apply are correct. This
  ADR buys correct function signatures. It buys nothing about whether the
  sandbox is any good, which is Phase 17's problem and the sandbox test suite's.

## Decision

`px-sandbox` takes `libc` on Linux and `windows-sys` on Windows, both
target-scoped so neither appears in the other platform's build, and both
confined to `px-sandbox` — the one crate build-spec §4 permits `unsafe` in at
all. These are declarations of the platform's own entry points, not libraries
acting on our behalf, which is a different kind of dependency from the ones §3
was written to control. §3 says to write the sandbox ourselves; that means the
*policy*, and the policy is still ours. Getting a `STARTUPINFOEXW` field offset
right is not a design decision we want to own.

## Alternatives rejected

**Hand-declare every extern.** Adds no dependency and was genuinely tempting on
Linux, where the ABI is a handful of stable structs. Rejected on Windows: it
means hand-writing `STARTUPINFOEXW` and `PROC_THREAD_ATTRIBUTE_LIST`, where one
wrong field offset produces a process that *looks* sandboxed and is not. ADR
007 names that as the error that cannot be recovered from — a sandbox that
fails open is worse than no sandbox, because everything downstream is written
trusting it. Hand-rolling is also exactly the work these crates exist to do,
and doing it by hand on a machine that cannot execute one of the two targets is
the worst available combination.

**Split: hand-roll Linux, `windows-sys` on Windows.** Spends the dependency only
where the ABI is dangerous to write by hand, and `libc` is by far the cheaper of
the two by every measure below. Rejected because it puts two different FFI
idioms in one small crate, and because the Linux half would be the half nobody
can run locally — the reviewer's attention would be lowest exactly where the
hand-written code is.

**`cargo vet certify` instead of `trust`.** Rejected as dishonest: `cargo vet`
puts the audit backlog at 468,409 lines. Certifying them would be a claim to
have read them.

**A blanket `cargo vet` exemption.** Rejected because it silences the gate
without saying anything. Publisher trust at least records a specific claim that
can be argued with.

## Consequences

**The dependency-tree unsafe count rises from 135 to 13,341 — a factor of 98.8.**
This is the real cost and it should be stated first. `ci/unsafe-baseline.json`
moves accordingly. The count is a deliberately over-counting text scan across
whole vendored crates regardless of enabled features, so it is not a measure of
what compiles, but it is the number the gate reads and it is now dominated
entirely by these two crates.

What those tokens are, counted rather than assumed:

| Crate | `unsafe` tokens | Shape |
|---|---|---|
| `windows-sys` 0.61.2 | 12,530 | 3,605 `unsafe extern` declarations; 8,925 occurrences of `unsafe { core::mem::zeroed() }` in generated `Default` impls. Those two shapes account for **100%** of them — there is no third kind and no hand-written unsafe logic. |
| `libc` 0.2.189 | 676 | 364 `unsafe fn`, 198 `unsafe extern`, 96 `unsafe {}`, 10 `unsafe impl`, 2 `unsafe trait`. Proportionally more genuine logic than `windows-sys`, and a much smaller absolute number. |
| `windows-link` 0.2.1 | 0 | 39 lines of macro definition. |

The 8,925 `core::mem::zeroed()` calls are the one item here worth being uneasy
about. Zeroing is valid for the `#[repr(C)]` POD structs they are generated for
and invalid for anything with a niche or a reference; they are generated from
Windows metadata, not written, so the risk is a metadata bug rather than a
typo. We do not call `Default::default()` on these types, which makes them
unreachable code for us, but they are still counted and still compiled.

**A future `windows-sys` bump can move the count without any change of ours,**
and the gate will fail closed on it. That is the gate working, but it means a
dependency bump now carries an ADR-shaped question rather than being a chore.

**Supply chain.** Three crates added: `libc` 0.2.189, `windows-sys` 0.61.2,
`windows-link` 0.2.1. All resolved through `cargo vet trust` on the publishers
— `rust-lang-owner` for `libc`, `kennykerr` (Microsoft) for the two Windows
crates — which ISRG, Mozilla and the Bytecode Alliance each already trust
independently. This is the mechanism the workspace already uses for dtolnay's
crates, not a new posture. Publisher trust is broader than version audit: it
covers future releases by the same publisher until the recorded end date, and
that is a real weakening compared to auditing a pinned version.

**`px-sandbox` keeps `#![deny(unsafe_op_in_unsafe_fn)]`** and the requirement
that every `unsafe` block we write carries a `// SAFETY:` comment reviewed
against the OS documentation. Taking these crates removes the ABI-transcription
risk; it removes none of the calling-convention risk, which is ours.

## Verification

This decision is wrong if any of the following is observed:

- A sandbox escape or a fail-open launch is traced to a signature or struct
  layout supplied by either crate. That is the specific failure hand-declaring
  was supposed to avoid, and it would mean the trade went the wrong way.
- `cargo vet` trust in either publisher has to be revoked, or an advisory lands
  against either crate that publisher trust would not have caught.
- The Windows policy turns out to need APIs `windows-sys` does not generate, so
  the hand-written externs appear anyway and we are carrying the dependency for
  a subset of the work.

It is *not* falsified by the unsafe count being large. That was known and
measured before accepting, and is recorded above precisely so that nobody
re-litigates it later as a discovery.

---

## Dependency record (build-spec §3, §5)

- **Crate and version:** `libc` 0.2.189 (Linux targets only); `windows-sys`
  0.61.2 (Windows targets only); `windows-link` 0.2.1 (transitive, via
  `windows-sys`).
- **What it does:** declares the platform's own syscalls, structs and
  constants. `windows-link` provides the single `link!` macro that emits
  `raw-dylib` extern blocks.
- **Why not write it:** we would be writing the same declarations by hand, and
  a wrong field offset in `STARTUPINFOEXW` or `PROC_THREAD_ATTRIBUTE_LIST`
  yields a process that looks sandboxed and is not — ADR 007's unrecoverable
  error. These are generated from the OS vendor's own metadata.
- **Maintainer count:** `libc` — the rust-lang libc team, published under the
  `rust-lang-owner` crates.io account. `windows-sys` and `windows-link` —
  Microsoft, published by `kennykerr`.
- **Audit status:** no version-pinned audit. Accepted via `cargo vet trust` on
  both publishers, each already trusted by ISRG, Mozilla and the Bytecode
  Alliance. `cargo vet --locked` passes: 16 fully audited.
- **Unsafe-line count:** 12,530 (`windows-sys`) + 676 (`libc`) + 0
  (`windows-link`). `ci/unsafe-baseline.json` rises from 135 to 13,341.
- **Transitive dependencies added:** one — `windows-link` 0.2.1, which itself
  has none. `libc`'s only listed dependency, `rustc-std-workspace-core`, is
  optional and used solely when building as part of `std`; it is not enabled
  here.
