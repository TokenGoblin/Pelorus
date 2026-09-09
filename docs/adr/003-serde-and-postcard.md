# 003 — serde and postcard for IPC

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 1
- **Invariants touched:** none

## Context

build-spec §3 names `postcard` as an approved dependency and the working
agreement requires IPC to be "postcard over typed enums with size limits". What
§3 does not say is that postcard is a *serde format*: it implements
`serde::Serializer` and `serde::Deserializer` and has no independent derive or
reflection of its own.

So approving postcard approves serde, and the closure is materially bigger than
the one-line entry in §3 suggests. This ADR exists because "no new crate
dependency without an ADR" would otherwise be satisfied by a dependency nobody
consciously chose.

## Decision

Add `serde` (with `derive`) and `postcard` (with `alloc`) to `px-ipc`, and to no
other crate. Every other crate that needs to send a message depends on `px-ipc`.

## Alternatives rejected

**Hand-rolled encoding against postcard's flavor API.** Possible — postcard can
be driven without derive — but every message type would need a hand-written
encoder and decoder kept in sync with the type by discipline alone. For a wire
format that is the trust boundary between a privileged broker and a hostile
content process, hand-maintained parsing is the wrong place to save a
dependency. Rejected.

**A different codec with no serde dependency** (bincode has the same
dependency; a bespoke format has none). Rejected: postcard is settled in §3, and
Appendix A's rule is that settled decisions are not re-proposed. Replacing it
would be an ADR arguing against the spec, not a dependency ADR.

## The dependency, in the terms §5 requires

| | |
|---|---|
| **Crates** | `serde`, `serde_derive`, `postcard` |
| **Transitive** | `proc-macro2`, `quote`, `unicode-ident`, `syn`, `cobs` |
| **What they do** | serde: the serialisation trait vocabulary. serde_derive: proc macro generating impls. postcard: a compact, no-std-friendly binary format with COBS framing available. |
| **Why not write it** | A binary codec is writable in a week. Keeping it correct against every enum variant, every schema change, and an adversary who controls the bytes is not a week. postcard is small, has an explicit wire-format specification, and is designed for embedded targets where mis-parsing is expensive. |
| **Maintainer count** | serde: two principal maintainers, very high ecosystem exposure. postcard: primarily one maintainer, low but real bus factor. |
| **Audit status** | See "What the first run actually decided" below. Not what was assumed. |
| **Unsafe-line count** | 135, recorded in `ci/unsafe-baseline.json`. `syn` is 108 of it. |

## What the first run actually decided

§5 says to import "Mozilla's and Google's audit sets". Those two, alone, did not
cover this closure: `cargo vet` reported **13 unvetted dependencies** and an
estimated audit backlog of 107,581 lines. That is the number §5's reasoning
predicts — auditing a browser tree unaided is a project of its own — arriving on
the very first dependency.

Two things closed the gap, and both go beyond what §5 names, so both are
recorded here rather than left in a config diff:

**Three more audit sets imported:** `bytecode-alliance`, `isrg`, and
`embark-studios`. This took 13 unvetted down to 8. The choices are not arbitrary:
Bytecode Alliance audits are Wasmtime's, and ISRG's are from the organisation
that funds Rustls — which this project depends on from Phase 3. Widening the
import list widens who we are trusting to have read the code, and that is the
actual cost.

**Eight crates trusted by publisher.** The remaining 8 are all published by
David Tolnay, whom Mozilla already trusts for exactly this. `cargo vet trust`
records a per-crate entry in `supply-chain/audits.toml` against his crates.io
user id, with a start date and an **expiry of 2027-09-09**.

Per crate, deliberately, rather than `cargo vet trust --all dtolnay`: the blanket
form would auto-trust every crate he publishes in future, including ones this
project has never evaluated. Eight named entries that expire is a bounded
statement; "everything by this person, forever" is not.

**What that means honestly:** eight of our thirteen dependencies are not audited
by anyone in our chain. They are trusted because a specific person published
them and Mozilla vouches for that person. That is a reasonable position and it is
not the same as an audit, and the expiry date exists so it has to be re-taken
rather than inherited.

## Consequences

**Phase 0's supply-chain gate stops being vacuous.** `cargo vet` has something to
check, `cargo deny` has licences to evaluate against a list that has so far
matched nothing, and the unsafe baseline becomes a number that can move. Expect
the licence allowlist in `deny.toml` to need pruning against reality.

**proc-macro crates are build-time code execution.** `serde_derive` runs during
every build of `px-ipc`. That is true of any derive macro and is not specific to
serde, but it belongs in a threat model conversation rather than being invisible:
a compromised release of `serde_derive` executes on the build machine, not in the
browser. `Cargo.lock` is committed and CI builds `--locked`, which means an
upstream release cannot reach us without appearing in a diff.

**postcard's bus factor is the weakest link here**, more than serde's. The
mitigation is that the wire format is specified and small: if postcard were
abandoned, replacing it is a bounded piece of work against a written spec, not a
rewrite of the IPC layer.

## Verification

Falsified if `cargo vet` cannot be satisfied for this closure without writing
audits we are not qualified to write, or if the unsafe-line delta is larger than
a codec should account for. Both are visible in the same commit that adds them.
