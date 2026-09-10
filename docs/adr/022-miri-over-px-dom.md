# 022 — A third nightly consumer: Miri over `px-dom`

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 4
- **Invariants touched:** 7 (protected, by the same construction as ADRs 006
  and 011).
- **Amends:** ADR 006, by the mechanism ADR 011 established.

## Context

build-spec §4.5 names it directly:

> Miri on `px-dom`, `px-ipc`, `px-store` unit tests (pure-Rust paths only —
> Miri cannot run the FFI ones, and that limitation is documented rather than
> papered over).

It has never run. `crates/px-dom/CLAUDE.md` claimed since Phase 0 that it did,
which was corrected earlier this phase to a "Not true yet" heading rather than
quietly deleted.

Twice it was deferred for a reason that was correct at the time and is not any
more:

- Phase 0: "each covers a crate that is currently an empty skeleton. Adding
  them now would mean six green jobs that inspect nothing."
- Phase 2, for `px-ipc`: "`px-ipc` has no `unsafe` and no FFI, so what Miri
  would add today is UB detection in `std` calls — real but not urgent."

**`px-dom` is a different case, and became one during this phase.** It is
`#![forbid(unsafe_code)]`, so Miri finds nothing in its own code — but ADR 017
brought in a closure whose unsafe its tests now *execute*: `parking_lot` 539
tokens, `tendril` 129, `smallvec` 75, `string_cache` 19. `ci/unsafe-audit.sh`
counts them. Until now, nothing ran them under anything.

The obstacle was ADR 006, which pinned stable for the repository and fenced a
single pinned nightly to `fuzz/`. ADR 011 already answered whether a second
consumer was drift or the same kind of use, and set the mechanism.

## Decision

**Miri runs over selected `px-dom` test suites, on the nightly ADR 006 already
pins — the same one, by exact name — invoked as
`cargo +nightly-2026-09-01 miri` from CI.**

Exactly ADR 011's construction, and for the same reason: the root toolchain
file is untouched, so `ci/gate-fuzz-smoke.sh`'s assertion that nothing outside
`fuzz/` resolves to nightly keeps holding, and invariant 7 is unaffected
because nothing Miri builds is shipped.

### Which suites, and why not all of them

| suite | Miri time | included |
|---|---:|---|
| `parse` (minus three) | 8.0 s | **yes** — the only suite that drives html5ever, tendril and string_cache |
| `ranges` | 7.2 s | yes |
| `handles` | 6.9 s | yes |
| `snapshots` | 6.4 s | yes |
| `order` | 3.4 s | yes |
| `opaque` | 182 s | no |
| `depth`, `layout`, `harness`, `html5lib`, `mutation` | minutes to hours | no |

Miri interprets rather than executes, at roughly a thousandfold slowdown, so
anything sized for a fuzz harness or a conformance corpus is out of reach. The
three `parse` tests excluded are the ones that build 100,000- and
1,000,000-node documents; `opaque` churns four thousand handles through a
`HashSet`.

**What that costs is worth stating rather than implying.** The deep-nesting,
generation-exhaustion and conformance paths are *not* under Miri, and those are
where this crate's own logic is most intricate. What is under Miri is the
dependency unsafe — which is the part nothing else checks at all, and the
reason §4.5 names this crate.

## What it found immediately

`tendril` does **integer-to-pointer casts**:

```
warning: integer-to-pointer cast
  --> tendril-0.5.1/src/tendril.rs:1079:9
   |
   |  (self.ptr.get().get() & !1) as *mut Header<A>
   |
   = help: ... which means that Miri might miss pointer bugs in this program
```

It packs a tag bit into a pointer and casts back. That is a legitimate and
common technique, and the consequence is specific: **Miri's provenance tracking
is weakened for the crate that holds every string in the DOM.** A green Miri
run over `px-dom` is a weaker statement about `tendril` than about anything
else in the closure.

`MIRIFLAGS=-Zmiri-strict-provenance` would turn that warning into an error and
`tendril` would fail on the first parse — which is not a defect in `tendril`,
so the flag is not set. Recorded here because "Miri is green" would otherwise
be read as covering `tendril` to the same standard as the rest, and it does
not.

## Alternatives rejected

**Keep deferring until `px-ipc` and `px-store` are ready too.** §4.5 names
three crates and this covers one. Waiting for all three means the dependency
unsafe added in Phase 4 goes unexercised through Phase 5 and beyond, and the
Phase 2 note's own reasoning — that Miri over a crate with no unsafe and no
dependencies with unsafe adds little — is exactly why `px-dom` should not wait
for them.

**A separate, newer nightly for Miri.** Rejected on ADR 006's original
argument, which ADR 011 restated: a second pinned nightly is a second thing to
keep current, and the drift ADR 006 exists to prevent is precisely a
proliferation of "just this one tool needs a different toolchain".

**Run every suite.** The conformance suite alone would be hours per CI run.
A check nobody waits for is a check that gets disabled.

## Consequences

**`px-dom`'s `CLAUDE.md` can say Miri runs, and now means it.** The "Not true
yet" section is replaced by a statement of what is and is not covered.

**Two of §4.5's three crates still have no Miri job.** `px-ipc` and `px-store`
remain in the backlog, and this ADR does not cover them.

**The excluded suites are a standing gap, not a resolved one.** If Miri gets
materially faster, or if a cheaper subset of the deep-nesting paths can be
written, the table above should shrink.

## Verification

Wrong if Miri over these suites turns out to inspect nothing — if, for example,
the dependency unsafe is all behind `cfg` paths the tests do not reach. Checked
directly: the `tendril` finding above is Miri reporting on a dependency's
pointer handling during a parse, which is the coverage this ADR is for.

Wrong if the CI cost turns out to be more than the table says. The numbers are
local, on one machine, and CI runners are slower; if the job becomes the long
pole it should lose suites rather than be disabled.

It is **not** falsified by Miri finding no bugs. It is a check, not a
prediction.
