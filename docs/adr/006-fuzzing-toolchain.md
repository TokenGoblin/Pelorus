# 006 — A pinned nightly, fenced to `fuzz/`, and a 24h campaign off the PR path

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 1
- **Invariants touched:** 7 (protected, see Consequences)

## Context

build-spec §4.5 requires `cargo-fuzz` targets for every parser and every IPC
deserializer, and Phase 1's gate requires the IPC deserializers to be "fuzz
clean for 24h".

`cargo-fuzz` builds with `-Z sanitizer=address` and links libFuzzer. Both are
nightly-only. `rust-toolchain.toml` pins stable 1.98, deliberately, because
invariant 7 makes the toolchain part of the source.

Separately: a 24-hour job cannot run on every push. A gate that takes a day to
report is a gate that gets bypassed within a fortnight.

## Decision

**Two toolchains, one of which cannot build anything that ships.** A specific
nightly is pinned in `fuzz/rust-toolchain.toml`, scoped to that directory by
rustup's directory-based override. Nothing outside `fuzz/` sees it, and CI
asserts that the release build resolves to the stable pin.

**Two fuzzing cadences.**

| | Runs | Duration | Blocks |
|---|---|---|---|
| Smoke | Every push and PR | 60s per target, against the committed corpus | Yes |
| Campaign | Scheduled, and on demand | 24h total, sharded | No — reports |

Phase 1's gate item is satisfied by **one completed 24h campaign, recorded in
the gate report with its run URL**, not by every push doing the impossible.

A crash found by the campaign is a defect, filed and fixed, and the corpus entry
that found it is committed so the smoke job catches its regression in 60
seconds.

## Alternatives rejected

**Property tests on stable** (`proptest`, `quickcheck`). Honest, cheap, and not
fuzzing: no coverage feedback, no sanitizers, no corpus that grows toward the
interesting inputs. It would satisfy the word "fuzz" in the gate and none of its
purpose. Rejecting §4.5 would need an ADR arguing against the spec.

**Nightly everywhere.** One toolchain, no fencing, no directory override to
explain. Rejected because it silently un-pins the project: nightly changes
weekly, `overflow-checks` and codegen change with it, and invariant 7's
"same source + same toolchain" becomes a moving target. The reproducibility gate
would still pass, on a toolchain nobody chose.

**No fuzzing until later.** Rejected. Phase 1's whole reason for existing early
is that the IPC boundary must be right before anything is written against it.
Deserializers that have never been fuzzed are the least trustworthy code in the
project.

## Consequences

**A nightly toolchain now exists in the repository, and people reach for
nightlies.** The fencing is the entire mitigation: a `rust-toolchain.toml` inside
`fuzz/`, a CI assertion that release artifacts come from the stable pin, and this
ADR to point at when someone proposes a nightly-only feature "since we already
have one". The nightly is pinned to an exact date, not `nightly`, so it does not
drift either.

**Reproducibility is unaffected, and it matters that this is by construction.**
Fuzz binaries are never released, never hashed by the reproducibility gate, and
never built by the release profile. Invariant 7 constrains what ships; `fuzz/`
does not ship.

**A GitHub-hosted job is killed at six hours, so the campaign cannot be one
run.** This was found when the campaign was first dispatched for real, and it
would have produced a gate nobody could ever satisfy: a weekly job cancelled at
the six-hour mark, forever. The campaign is a matrix instead — targets x shards
— giving 24 hours of fuzzing in about four hours of wall clock, each job well
inside the limit. The shards are not redundant work: libFuzzer seeds from the
clock and its own corpus, so three shards of one target explore different paths
from the same committed starting corpus.

**The 24h number is a floor that will need revisiting.** Twenty-four hours of
libFuzzer against a small message enum will exhaust the interesting space quickly
and then report clean forever, which is a false sense of coverage. When the
message set grows, the campaign should be judged on coverage and corpus growth
rather than on wall-clock time. Noted for the phase that first finds the campaign
boring.

**The corpus is committed**, so the smoke job is meaningful from the first run
and a fixed crash cannot silently regress.

## Verification

Falsified if the fencing leaks — if any shipping artifact is ever built by the
nightly. CI asserts this directly rather than trusting the directory override.
