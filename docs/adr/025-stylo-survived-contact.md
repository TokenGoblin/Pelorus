# 025 — stylo survived contact

- **Status:** accepted
- **Date:** 2026-09-12
- **Phase:** 5
- **Invariants touched:** 7, already amended by ADR 023. This ADR records the
  outcome rather than making a new change.

## Context

§9 Phase 5's third gate item is *"a documented decision on whether stylo survived
contact or is being replaced."* This is that decision.

It is the only gate item in the whole plan that asks for a judgement rather than a
measurement, which makes it the easiest to satisfy without doing anything — an ADR
that recounts the phase and stops short of a verdict passes a check that only
looks for the file. `ci/gate-style.sh` therefore requires this document to be in a
decided state and to answer in §9's own words, and this section is not the answer.

`stylo-requirements.md` §5.3 asked Phase 4 to write down the decision *rule* in
advance so that Phase 5 would not have to exercise judgement at the end. Phase 4
did not, which is a real miss — but the note itself lists six signals, and they
are specific enough to be used as the rule they were meant to become. Each is
answered below with evidence rather than impression.

## Decision

**stylo survived contact. It is not being replaced.**

`px-css` implements all five traits over `px-dom`, the cascade resolves computed
style for a parsed document, and every one of the 64 properties in the defined
set cascades. Two of three gate items are green on both operating systems, and
the third is this document.

The decision is not close. Of the six signals §5.3 named as reasons to abandon
stylo, **four are clearly not present, one passes, and one was never measured** —
and the unmeasured one is a maintenance projection rather than a design failure.

## §5.3's signals, answered

**1. "Any required `unsafe` block in `px-css`." — Not present.**

Zero `unsafe` blocks and zero `unsafe impl`. Both are enforced by
`ci/gate-unsafe-headers.sh` and `ci/gate-style.sh`, and both gates were verified
to fail on a deliberately added `unsafe impl Send` before being trusted. Five
methods are declared `unsafe fn` because `TElement` declares them so — that is
ADR 024, and the body of an `unsafe fn` needs no `unsafe` block.

The one place a transmute would have been needed is `&Atom<S>` to
`&GenericAtomIdent<S>`, and stylo ships `GenericAtomIdent::cast` as a *safe*
public function over a `repr(transparent)` newtype. So the unsafe stayed in the
crate that reviewed it. Had `cast` not existed this signal would have fired, and
the fallback — making `px-dom` store stylo's types — would have inverted §3's
layering.

**2. "`px-dom` needing interior mutability in places stylo forced." — Not
present, and by a wide margin.**

`px-dom` changed in exactly one way across the whole phase: `slot_count()` became
`pub`. No `UnsafeCell`, no atomics, no new field on `Node`, nothing in the
structural links. The note said one or two atomics would be a fine price; the
price was a visibility keyword.

Everything stylo needs the DOM to *hold* — `ElementData`, dirty-descendant bits,
selector flags, interned ids and classes, parsed inline style — lives in a
per-slot side table in `px-css` (ADR 026). The interior mutability is a `Cell`
per element in the crate that wanted it.

**3. "Unable to satisfy `TElement` without stubs that break WPT." — Passes, with
the qualification that WPT cannot be run.**

Counted rather than estimated: **14 of the 75 trait methods answer a constant**
(`add_element_unique_hashes`, `as_shadow_root`, `containing_shadow`,
`containing_shadow_host`, `exports_any_part`, `has_animations`, `has_part_attr`,
`is_html_document`, `is_part`, `is_pseudo_element`, `may_have_animations`,
`parent_node_is_shadow_root`, `shadow_root`, `skip_item_display_fixup`), and a
handful more have empty bodies. Two of the fourteen are not stubs at all:
`is_html_document` is `true` because there is no XML path, and
`skip_item_display_fixup` is `false` because nothing here synthesises
pseudo-elements.

The rest cluster into five areas, and each is a phase rather than a problem:
shadow DOM (Phase 14), animations (no timeline before Phase 13), presentational
attributes, interaction state (no input before Phase 13), and container queries
(no layout before Phase 6). One — `add_element_unique_hashes` — is a deliberate
performance deferral documented at the method.

Against the seven ported cascade tests, the stubs lose nothing. **That claim is
bounded and the bound matters:** ADR 028 records that no WPT test in
`css/css-cascade` can run in Phase 5, because every one needs either rendering or
JavaScript. So the honest form of this signal's answer is *the stubs lose nothing
in what can currently be tested*, and the real check arrives with Phase 11.

**4. "A `stylo` minor bump breaking the impl more than once during Phase 5." —
Never measured. This is the one open item.**

`stylo` is pinned at 0.21.0 and was never bumped, so the projection the note
wanted measured has no data behind it. The note was explicit: *"Measure it; do
not estimate it."* This phase estimated it by not testing it.

What is known is the cadence — 24 published versions in about 28 months, with
`style/dom.rs` gaining a supertrait as recently as three months before this
phase — and that ADR 023 already commits every bump to a `TElement` review rather
than a `Cargo.lock` edit. The measurement is cheap and should be taken before
Phase 20: bump to the next published version, count what breaks, and record it.
Filed in `docs/backlog.md`.

**5. "The build-reproducibility gate cannot be made green." — Passes.**

The `reproducible` job is green on both `ubuntu-latest` and `windows-latest` with
`PYTHON3` pinned to a venv built from `build-python/requirements.txt`.
`ci/build-and-hash.sh` refuses to build without the pin rather than falling back
to whatever is on `PATH`, so a green here means the pinned generator produced
byte-identical binaries across two directories, two `HOME`s and two `CARGO_HOME`s.

Invariant 7 is nonetheless *weaker* than it was — ADR 023 amended it to name its
build-time generators, and the release documentation has to say the weaker thing.
That was the user's decision, taken against the vendoring alternative, with the
cost understood. It is a recorded cost, not a failed signal.

**6. "Not a signal: integration being slow or unpleasant."** Noted, and it was
neither. 75 trait methods across five traits took about a day.

## §5.4's middle path, costed rather than dismissed

The note named a third option the gate's binary phrasing hides: **take
`selectors` and `cssparser`, write the cascade.** It deserves a real answer
because it is a good idea that is now clearly the wrong one.

What it would have saved is real: `rayon`, `to_shmem`, `servo_arc`, the Python
build step and therefore invariant 7 intact, most of the 108 crates, most of the
76 `cargo-vet` exemptions, and `forbid(unsafe_code)` in `px-css`. That is a
substantial list and it is the strongest argument against the decision above.

What it would have cost is larger, and Phase 5 now has numbers for it rather than
intuition. The four hundred-odd properties' computed-value conversions are the
bulk of `style/` and would all have had to be written; the property sweep in
`tests/properties.rs` exercises 64 of them and every one worked on the first try
once two prefs were set. Cascade *bookkeeping* is not the hard part, as the note
says — but `@layer` ordering, `all: unset` interacting with a later declaration in
the same block, `revert` against origins, and `!important` reversing specificity
all worked without a line of cascade code here, and each is a place where a
hand-written cascade is subtly wrong for a year before anybody notices.

The middle path also keeps the part of the work this phase found hardest —
`selectors::Element` is 27 items of the 75 — while giving up the part that turned
out to be free.

**Rejected, and it would have been the right choice if signal 1 or 2 had fired.**
Those were the ones that would have meant the DOM and stylo's assumptions had not
reconciled. They did.

## ADR 021's tripwire, reported as it asked to be

ADR 021 deferred the borrowed style-view type and named its own falsification:
*"Wrong if Phase 5 has to change the arena's slot layout, `NodeId`'s
representation, or the opaque packing in order to write `StyleNode`."* It asked
for that to be said plainly rather than absorbed as ordinary difficulty.

**Read literally, the tripwire did not fire.** None of those three changed.
`ci/gate-style.sh` pins the blob hashes of `crates/px-dom/tests/layout.rs` and
`tests/opaque.rs` and both still match on both platforms. `NodeId` is in fact
*exactly* the right width, which ADR 018 chose for unrelated reasons.

**Read as the claim it was making, it was wrong.** ADR 021's substance was that
every layout decision `StyleNode` depended on had already been banked, so writing
the type would be mechanical. It was not. A requirement nobody had identified —
that `TElement` be exactly pointer-sized, enforced by an `assert_eq!` in stylo's
style-sharing cache — forced the view to be redesigned from three fields to one
pointer. That is ADR 027.

The lesson is not "ADR 021 was fine" and not "ADR 021 was wrong about `NodeId`".
It is that **a tripwire can only watch the risks already enumerated**, and this
one was enumerated from a research note that had missed this. Three of the
phase's requirements were found by running something and reading a panic, not by
reading a trait:

1. `TElement` must be pointer-sized — ADR 027.
2. The thread must be registered via `thread_state::initialize(LAYOUT)`.
3. `grid-*` and `writing-mode` are behind `servo_pref` keys and silently do
   nothing when off — found by the property sweep, not by a failure.

None is in a trait signature, a doc comment, or the research note. That is the
shape of the risk stylo carries, and it is worth more than the verdict above: the
integration is sound, and its contract is partly undocumented and partly
enforced at runtime.

## Consequences

**Phase 6 builds on computed values that are tested, but narrowly.** 64
properties cascade; the specific computed value is asserted for a handful. Unit
resolution against real font metrics is untested because there are no font
metrics until Phase 9 — `ex`, `ch`, `ic` and `cap` resolve against a 16px stub.

**Every stylo bump is a `TElement` review**, per ADR 023, and the cost of that is
unmeasured — signal 4.

**The costs ADR 023 recorded are now paid and permanent**: +108 crates, 76
unaudited exemptions, invariant 7 weakened, `forbid(unsafe_code)` gone from
`px-css`. Phase 20 ships against all of them.

**Three things in this phase are correct-for-now and will be wrong later**, each
documented at its definition and in the backlog: `:hover` and friends never match
because there is no input, presentational attributes synthesise nothing, and the
selector bloom filter is disabled.

## Verification

Wrong if signal 4 turns out badly — if the first `stylo` bump breaks the impl
substantially, the maintenance tax the note worried about is real and this
decision was taken without its most load-bearing number. That is measurable
before Phase 20 and is in the backlog.

Wrong if Phase 11 runs the real WPT `css-cascade` suite and the ported
assertions turn out to have been testing something else. ADR 028 says to replace
them rather than keep them for that reason.

Wrong in the other direction — stylo being a better bet than this ADR claims — if
the stubs in the five deferred areas turn out to be cheap to fill when their
phases arrive, in which case the 14 constant answers were never a cost at all.
