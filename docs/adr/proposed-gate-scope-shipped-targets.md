# NNN — The supply-chain gates audit the targets we ship, not every target the lockfile mentions

- **Status:** proposed
- **Date:** 2026-11-04
- **Phase:** unassigned (CI configuration; no phase gate depends on it)
- **Invariants touched:** none. Invariant 7 (reproducible builds) is untouched;
  this changes what two gates count, not what is built.

*Numbering: this file is deliberately `proposed-<slug>.md`, with no number.
ADR numbers are sequential and permanent, and branches collide on them: `main`
stops at 028 while `origin/phase/06-layout` already holds 029 and 030. Whoever
merges this assigns the next free number and renames the file. Evidence:
`docs/research/dependency-audit.md`.*

## Context

`Cargo.lock` is target-independent: it records every crate that any target
could need. Two gates read it that way.

* `deny.toml` sets no `[graph] targets`, so `cargo deny` evaluates every
  platform. That is why the `windows-sys` entry in `bans.skip` exists (ADR 016):
  `ring` 0.17.14 pulls `windows-sys` 0.52 under
  `cfg(all(all(target_arch = "aarch64", target_endian = "little"), target_os = "windows"))`
  (`ring-0.17.14/Cargo.toml:256`).
* `ci/unsafe-audit.sh` runs `cargo vendor`, which vendors the whole lockfile, and
  the baseline in `ci/unsafe-baseline.json` is the sum.

The project builds two targets, `x86_64-unknown-linux-gnu` and
`x86_64-pc-windows-msvc`. There is no `aarch64` or `arm64` in `.github/`,
`packaging/`, `ci/`, `docs/` or the README. Measured with
`cargo tree --workspace --locked --target <t> --edges normal,build,dev`:

| | Crates |
|---|---|
| Built on any target | 169 |
| Built on x86_64 Linux | 153 |
| Built on x86_64 Windows (plus `winapi-util` when the Windows machine is the build host) | 154 |
| **Built on neither** | **13** |
| In the lockfile, built nowhere (`embedded-io` 0.4.0 and 0.6.1, postcard's disabled optional features) | 2 |

The 13: `windows-sys` 0.52.0 (3,411 tokens), `windows-targets` 0.52.6 and eight
`windows_*` per-architecture crates (0), `redox_syscall` 0.5.18 (178), `wasi`
0.11.1 (62), `hermit-abi` 0.5.3 (4). **3,655 of the baseline's 21,675 unsafe
tokens (16.9%) belong to crates no shipped binary links**, plus 2 for the
lock-only `embedded-io`.

Two things follow that look like a finding and are not one: this is not a
reduction in what an attacker can reach, and the dependency tree the product
ships is unchanged. It is a correction of what the gates report.

## Decision

1. Add `[graph] targets = ["x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"]`
   to `deny.toml` and delete the `{ name = "windows-sys" }` entry (and its comment)
   from `bans.skip`. The `syn` skip stays; it is a real duplicate on both targets.
2. Make `ci/unsafe-audit.sh` count only crates that `cargo metadata --filter-platform`
   returns for either target (with build-dependencies resolved for the host, as
   `cargo tree` does), rather than the whole vendor directory. Regenerate
   `ci/unsafe-baseline.json` once, in the same commit, with the delta in the message.
3. Record that adding a target (aarch64 Windows, macOS) requires adding it to both
   places in the same change, with its own baseline delta.

## Alternatives rejected

**Leave it.** The cost is small and constant, and the gates are honest in the
direction that fails safe (over-count). Rejected because the over-count is not
neutral: ADR 010 had to explain a 98.8x jump at length, and 3,411 tokens of the
current total (`windows-sys` 0.52) belong to a crate that is never compiled for a
shipped target. A number that has to
be explained away each time is a number people stop reading. The brief for this
project is a small audited surface; a measure that cannot tell shipped from
unshipped cannot be used to tell whether it is small.

**`[graph] exclude` the individual crates.** Brittle; the exclusion list rots when
`ring` moves. Targets is the property, the crate names are consequences.

**Replace or patch `ring`.** Out of scope. `rustls` is settled and ADR 016 chose
`ring` after measurement.

## Consequences

* The audited surface drops by 13 crates and 3,655 unsafe tokens. The
  `windows-sys` skip, which carried an uncomfortable comment about two audit
  surfaces, disappears. The one remaining skip is a build-time-only duplicate.
* `cargo vet` still reads the whole lockfile. Its exemptions are unchanged. The
  13 crates keep whatever audit entries they have; nothing here deletes an audit.
* **Unpleasant cost:** a platform nobody builds is no longer checked. If
  aarch64 Windows appears in a packaging file without being added here, the first
  build on it is also the first time `cargo deny` sees `windows-sys` 0.52. That is
  the failure this ADR accepts, and item 3 of the decision exists to make it a
  review checklist item rather than a surprise.
* Changing the filter changes the number the Phase 0 gate compares against. The
  commit that does it must regenerate the baseline and state the old and new
  totals, so that "the baseline went down" is never silent.
* Do not describe the result as a smaller dependency tree in any user-facing or
  release text. It is not one.

## Verification

Wrong if, after the change, `cargo deny` or the unsafe count passes on a
dependency that a shipped binary links, or fails to report one that it does.
Test it by temporarily adding a Linux-only crate to a workspace member in a
scratch branch: the baseline must move and the gate must fail. Wrong also if a
target is added to `packaging/` without the gates noticing; a one-line check in
`ci/gate-supply-chain.sh` that greps `packaging/` for target triples missing
from `deny.toml` would make that mechanical.

---

## For a dependency ADR, additionally (build-spec section 3, section 5)

Not a dependency ADR: no crate is added, removed or changed. The numbers that
would otherwise go here:

- **Crate and version:** none.
- **What it does:** n/a.
- **Why not write it:** n/a.
- **Maintainer count:** n/a.
- **Audit status:** unchanged; `cargo vet` is not target-filtered.
- **Unsafe-line count:** `ci/unsafe-baseline.json` falls from 21,675 to about
  18,018 (minus 3,655 off-target tokens), a change in what is counted, not in what
  is shipped. The exact total is set by regenerating.
- **Transitive dependencies added:** 0.
