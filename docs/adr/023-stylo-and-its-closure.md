# 023 — `stylo`, its 45-crate closure, and the Python in its build

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 5
- **Invariants touched:** 7 (reproducible builds) directly and unavoidably.
  Bears on 1 (`env_clear`) and on §14.5's seccomp floor through `num_cpus`;
  bears on `panic = "abort"` through `rayon`. See Consequences.

## Context

§3 lists `stylo` in "take from the ecosystem" and Appendix A closes the
argument about writing our own. §9 Phase 5 says `stylo` integration by name and
calls it the highest-risk phase. So the *headline* decision is settled and this
ADR does not re-propose it.

It exists because the standing rule is **no new crate dependency without an
ADR — propose, do not add and continue**, and because measuring what `stylo`
actually drags in turned up three things §3 could not have known when it
approved the name. All three are from `docs/research/stylo-requirements.md`,
which fetched real source at `servo/stylo` HEAD `e81a3d9` rather than recalling
it.

**One: the build shells out to Python 3 and Mako.** `style/build.rs` reads
`PYTHON3` from the environment, falls back to `python3` / `python.exe`, and
panics if neither is found. It then runs `properties/build.py` through Mako to
generate the property definitions. This collides head-on with invariant 7 and
with Phase 0's shipped gate — *"hashes reproducible across two machines with
different paths and usernames"* — which has held since the workspace was empty.
From Phase 5 onward the release binary's contents depend on an interpreter that
is in neither `Cargo.lock` nor `rust-toolchain.toml`.

**Two: the closure is about 45 crates**, including `rayon` and `rayon-core`,
`parking_lot`, `atomic_refcell`, `servo_arc`, `to_shmem`, `icu_segmenter`,
`cssparser`, `string_cache`, `thin-vec`, `uluru`, `euclid`, `app_units`,
`encoding_rs`, `num_cpus`, `serde` and `url`. §3 approves `stylo`. It does not
obviously approve `rayon` inside a content process built `panic = "abort"`, nor
`num_cpus` reading `/proc` and calling `sched_getaffinity` inside a seccomp
sandbox whose floor ADR 007 defines.

**Three: the unsafe baseline jumps by a large one-time amount.**
`ci/unsafe-baseline.json` currently tracks 4 unsafe lines for `html5ever` and
676 for `libc`. The stylo closure will add hundreds across `servo_arc`,
`to_shmem`, `atomic_refcell` and `thin-vec`, all of which exist precisely to do
things the borrow checker cannot express.

## Decision

**Decided 2026-09-10.** The reproducibility trade was put to the user
explicitly, against the vendoring alternative below, and the pinned-interpreter
option was chosen with the weaker invariant understood as its cost.

1. **Take `stylo` 0.21.0** with `rayon`'s parallel traversal *available but
   unused*: `traverse_dom` takes `pool: Option<&rayon::ThreadPool>` and `None`
   runs fully sequential. Phase 5 passes `None`. This is the largest
   de-risking lever available and it is free — it defers most of stylo's
   unchecked thread-safety contract to a later phase that can opt in
   deliberately, and it keeps `rayon` from spawning threads inside a
   `panic = "abort"` content process before anybody has thought about what a
   panicking style thread does there.

2. **Invariant 7 weakens, and the honest statement of it changes** from
   "reproducible across two machines" to **"reproducible across two machines
   given the same pinned Python 3 and Mako"**. `PYTHON3` gets pinned to an
   exact interpreter version in CI and recorded in the build manifest, and
   `ci/gate-reproducible.sh` asserts the pin is present rather than silently
   inheriting whatever is on PATH. The alternative — vendoring the generated
   property files — is discussed below and rejected, but it is the one worth
   arguing about.

3. **`num_cpus` is not called.** With `pool: None` there is no reason to ask
   how many CPUs there are, and a `sched_getaffinity` under a seccomp policy
   that does not allow it is a crash at style time rather than a degraded
   result. The symbol audit CI already performs gets a rule for it.

The unsafe baseline is regenerated and committed **in the same commit as this
ADR's acceptance**, with the diff visible, rather than being bumped by whatever
commit first trips the gate.

## Alternatives rejected

**Vendor stylo's generated property files and drop the Python build step.**
This is the option that would preserve invariant 7 intact, and it was put to the
user alongside the decision above rather than rejected here unilaterally. It is
the right choice if currency matters less than the invariant. It is neither
small nor stable: the
generated code is large, it is regenerated from `properties.py` on every stylo
version bump, and vendoring it means either re-running the generator by hand at
each of the 24-releases-in-28-months cadence or pinning stylo forever at 0.21.0.
Worse, a vendored generated file that has drifted from the `.py` it came from
fails in a way that looks like a stylo bug. **Not chosen**, with the weaker
invariant accepted as the price — and it was made now rather than at Phase 20,
which is the part that mattered.

**Fork stylo to remove the Mako step.** Same maintenance cost as vendoring,
plus owning a fork of the highest-churn dependency in the project. Rejected.

**Write the cascade instead.** `stylo-requirements.md` §5 costs this honestly
and it is not cheap: the cascade, specificity, inheritance, computed-value
conversion, custom properties, `@media`/`@supports`, invalidation and the
selector matching engine. §9 puts stylo in Phase 5 and Appendix A settles it.
Rejected here, but §5.3's signals for when stylo is the wrong bet are real and
ADR 025 is where that verdict gets recorded at the end of the phase.

**Take `rayon`'s parallel traversal now.** It is what stylo is for and what
Servo ships. Rejected for Phase 5 because §2 of the research note lists the
thread-safety invariants stylo *assumes and does not check* — `ElementData` has
no runtime check in release builds, `SharedStyleContext` must be `Sync` and
nothing verifies it, and subtree ownership is an unwritten contract. Taking all
of that in the same phase that first implements `TElement` means a data race
and a trait bug are indistinguishable. Sequential first.

## Consequences

**Invariant 7 is weaker after this than before it, and no wording makes that
untrue.** The reproducibility gate has held since Phase 0 and this is the first
thing to breach it. What is preserved is a pinned, declared dependency on a
named interpreter version; what is lost is the property that only
`Cargo.lock` and `rust-toolchain.toml` determine the output. Phase 20 ships
against the weaker claim and the release documentation has to say the weaker
thing.

**The unsafe-line count stops being a number anybody reads.** It roughly
doubles in one commit. The baseline remains useful as a *ratchet* — it catches
the next increase — but "the project has N unsafe lines" stops being a
meaningful sentence about code anybody here reviewed.

**`px-css` cannot keep `#![forbid(unsafe_code)]`.** That is a separate decision
with its own narrower answer; see ADR 024. It is listed here because the two
arrive together and accepting this one without that one leaves Phase 5
unbuildable.

**Version churn is now a standing cost.** 24 published versions in ~28 months,
with breaking trait changes in most, and `style/dom.rs` gained a supertrait as
recently as 2026-06-30. Every stylo bump is a `TElement` review, not a
`Cargo.lock` edit.

**Sequential style resolution is slower than the reference implementation, on
purpose.** If style time becomes the thing that makes a page feel slow, the fix
is a later phase that turns the pool on deliberately, with the thread-safety
contract reviewed rather than assumed.

## Verification

Wrong on the reproducibility trade if two CI machines with the same pinned
Python produce different hashes anyway — which would mean the Python step is
not the only unpinned input and the weaker claim is still too strong. Cheap to
detect: the reproducible job already builds twice.

Wrong on `pool: None` if sequential style resolution turns out to be
unacceptable on the first real page, in which case the deferral bought nothing
and the thread-safety work moves into Phase 5 after all. Also cheap to detect,
and worth recording either way.

Wrong on the whole shape if `TElement` cannot be implemented over the arena at
all — that is ADR 021's tripwire and ADR 025's subject, not this one's.

---

## Dependency record (build-spec §3, §5)

- **Crate and version:** `stylo` 0.21.0 (latest published, 2026-09). Research
  read HEAD `e81a3d9`, which is ahead of the published crate; the trait shapes
  quoted may differ and are a snapshot, not a contract.
- **What it does:** CSS parsing, the cascade, specificity, computed values,
  custom properties, `@media`/`@supports`, selector matching and style
  invalidation — over a DOM the consumer supplies through `TElement`/`TNode`.
- **Why not write it:** `stylo-requirements.md` §5.1 costs it. Appendix A
  settles it.
- **Maintainer count:** Servo project; not a single-maintainer crate.
- **Audit status:** to be recorded from `cargo vet` output at acceptance. Mozilla
  audit set covers much of the closure; `to_shmem` and `servo_arc` need
  checking specifically.
- **Unsafe-line count:** unknown until the closure is resolved; expected to
  roughly double `ci/unsafe-baseline.json`. Committed with this ADR, not after.
- **Transitive dependencies added:** ~45. Named individually above; `rayon`,
  `rayon-core`, `num_cpus`, `atomic_refcell`, `to_shmem` and `servo_arc` are
  the ones that need a decision rather than a note.
