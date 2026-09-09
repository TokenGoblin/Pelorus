# 004 — Two build profiles, and what the second one will cost

- **Status:** accepted
- **Date:** 2026-09-08
- **Phase:** 1
- **Invariants touched:** 7 (see Consequences)

## Context

build-spec §4.3 wants `panic = "abort"` for content processes and unwinding for
the broker. §14.2 records why that is not a one-line change: **Cargo sets the
panic strategy per profile, not per crate**, so a single `cargo build` cannot
produce both. It asks for an ADR in Phase 1 "to confirm the build-time cost is
tolerable, since this shapes CI duration for the whole project".

The reasoning behind the requirement is worth restating, because the cost is
about to be argued and the benefit should be in the same document. A content
process that dies is contained: the broker treats a dead child as routine and
restarts it. A content process that *unwinds* runs destructors across a
half-mutated DOM, in a process that an attacker may already partially control,
and then keeps running. Abort is not a performance choice.

## Decision

Two profiles. `release` unwinds and builds everything; `content-release`
inherits it and sets `panic = "abort"`, and is used for `px-content` alone:

```
cargo build --workspace --locked --release
cargo build --locked --profile content-release -p px-content
```

CI's build gate runs both, on both operating systems.

The reproducibility gate hashes **both** artifacts. `px-browser` comes from
`release` and `px-content` from `content-release`, which is also what a real
release will ship, so hashing anything else would be verifying a build nobody
runs.

## Alternatives rejected

**One profile, everything aborts.** Simplest, and wrong at the broker: §4.3
requires the broker to survive a panic in IPC dispatch via `catch_unwind`, which
does nothing under `panic = "abort"`. The one process that may not die would
become the one process that dies most eagerly.

**One profile, everything unwinds.** Accepts the half-mutated-DOM case in the
process most likely to be attacked. Rejected on the same grounds §4.3 gives.

**`-C panic=abort` via `RUSTFLAGS` for one invocation.** Works, and is worse:
`RUSTFLAGS` is not recorded in the profile, so the setting lives in whatever
invoked the build. A shipping property should be in the manifest where the
reproducibility gate and a reader can both see it.

## The measurement §14.2 asked for

Measured today, on this workspace, cold (`cargo clean` before each), Windows,
`lto = "fat"`, `codegen-units = 1`:

| Build | Cold wall clock |
|---|---|
| `--workspace --release` | 1.96 s |
| `--profile content-release -p px-content` | 1.60 s |
| Both, in sequence, as CI runs them | 3.94 s |

**These numbers mean nothing yet and should not be quoted as if they do.** The
workspace has twenty-one empty crates and, as of this phase, three dependencies.
The 1.60 s is almost entirely process startup and linking.

What the measurement does establish is the *shape* of the cost, which is the part
that will not change: the second invocation recompiles every dependency
`px-content` transitively needs, because a different panic strategy is a
different compilation. It does not recompile the workspace.

So the projection that matters is not "×2 build time". It is "**the cost equals
`px-content`'s dependency closure, compiled twice**". Today that closure is
`px-ipc` plus serde and postcard. The number to watch is what enters it later:

- If `px-content` ends up depending on `stylo`, `boa_engine` and `wgpu` — which
  the architecture implies it will, since it is the process that renders — the
  duplicated closure is most of the heaviest crates in the tree, and CI build
  time roughly doubles.
- If the content process is kept thin, driving engine crates that live behind
  IPC, the duplication stays small.

**That is a design lever, and this ADR is the place it becomes visible.** The
build-time cost of `panic = "abort"` is a direct function of how fat the content
process is. Revisit this ADR at Phase 5, when `stylo` lands, with the same
measurement re-run.

## Consequences

**CI time grows with `px-content`'s dependency closure, not with the workspace.**
Cheap now, potentially the largest single line in CI by Phase 8. Mitigations
available when it hurts, in order of preference: keep the content process thin;
cache the `content-release` target directory separately; build it only on the
default branch and on release tags rather than every push. None are needed yet
and none should be adopted pre-emptively.

**Two artifacts must both be reproducible.** The reproducibility gate now hashes
`px-browser` and `px-content` from their respective profiles, so a divergence in
either fails.

**`panic = "abort"` changes what tests can observe.** `#[should_panic]` tests and
`catch_unwind` do not work under abort. `px-content`'s unit tests run under the
normal `test` profile, which unwinds, so a test can pass on a code path that
would abort in production. That gap is real and worth naming: the behaviour under
test is not the behaviour that ships. The compensating control is that
`px-content` denies `unwrap`, `expect`, `panic!` and indexing outright (§4.3), so
a panic reaching production is already a lint failure before it is an abort.

## Verification

Falsified if the duplicated build becomes the dominant cost in CI before there is
a reason for `px-content` to be heavy. Re-measure at Phase 5 and record the new
numbers here rather than in a new ADR.
