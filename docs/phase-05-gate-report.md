# Phase 5 gate report

*Style. Branch `phase/05-style`.*

## The three gate items (build-spec §9)

| Item | Where | Result |
|---|---|---|
| computed-style fixtures match across a defined property set | `crates/px-css/tests/computed.rs` and `tests/properties.rs`, against the 64-property manifest in `tests/properties.toml` | **Pass** |
| WPT `css/css-cascade` subset passes | `crates/px-css/tests/cascade.rs`, 7 tests | **Pass as ported, not as run** — ADR 028, and read that before the tick |
| a documented decision on whether stylo survived contact | `docs/adr/025-stylo-survived-contact.md` | **Pass.** stylo survived contact |

`ci/gate-style.sh` passes on Windows and Linux. **23 of 24 CI jobs are green; the
only red is `compat-list`,** which has been red since Phase 0 and is the user's
forty-site list. The `style` job's `continue-on-error` was set in the phase's
first commit and removed in its last.

## The second item's tick is weaker than it reads, and here is exactly how

**No test in WPT `css/css-cascade` can run in Phase 5.** Every file there is a
reftest carrying `<link rel="match">`, which needs layout, paint and text shaping
(Phases 6, 8, 9), or a testharness.js test asserting through `getComputedStyle`,
which needs JavaScript and DOM bindings (Phases 10, 11). Three files were read in
full to establish that, including `parsing/all-valid.html` — the best candidate
for a runnable subset, since parsing needs no rendering — and it is testharness
too.

So the item is met by **porting** seven named tests: each asserts what its source
asserts, reads it from computed style, and names the file it came from. Five cover
the cascade (`!important` versus inline style, `inherit`/`initial`, `all: unset`
followed by a later declaration, specificity across id/class/type with and without
`!important`, `@layer` ordering including unlayered-wins). Two cover the parser
(`all` accepts exactly the CSS-wide keywords; an invalid declaration is discarded
and its neighbours survive).

What is verified is the cascade. What is **not** verified is WPT's harness, its
edge cases, or the ~145 files not ported. ADR 028 records the decision and the
rule that these are *replaced* when JavaScript lands, not kept alongside the real
tests.

## The property sweep, which is why the first item's tick means something

The manifest defined 64 properties and the fixtures originally tested two. The
gate could not tell the difference: it checked the manifest's size and that tests
existed, which is the exact shape of coverage-by-assertion its own comments warn
about.

`tests/properties.rs` now declares a value for every property, resolves it, and
reads the computed value back. **Eight of sixty-four failed on the first run**, and
the split is the useful part.

| Failure | Cause |
|---|---|
| `border-top-width`, `border-right-width`, `border-bottom-width`, `border-left-width` | **My fixtures were wrong about CSS.** `border-*-width` computes to 0 while `border-*-style` is `none`, which is the initial value. Declaring only a width proves nothing. |
| `grid-template-columns`, `grid-template-rows`, `grid-auto-flow` | `servo_pref = "layout.grid.enabled"` — off by default |
| `writing-mode` | `servo_pref = "layout.writing-mode.enabled"` — off by default |

A declaration for a pref'd-off property **parses without complaint, cascades
nothing, and leaves the initial value.** On a page that reads as a layout bug; in
a test it reads as a pass. Both prefs are now set in `StyleEngine::new`, read from
`properties/longhands.toml` rather than guessed from the symptom.

What the sweep proves is narrow and stated as such: each declaration parses,
cascades, and moves the computed value off its initial. It does **not** check each
value against the spec — 64 hand-written expected strings would be 64 chances to
encode stylo's serialisation as if it were the spec. `computed.rs` does that for
the handful where the computed value is interesting.

## Three requirements found by running something, not by reading a trait

This is the phase's most transferable finding, and it is worth more than the
verdict.

**1. `TElement` must be exactly pointer-sized.** stylo's style-sharing cache keeps
its LRU in a thread-local with the element type erased to `usize` and
`transmute`s it back, guarded by an `assert_eq!` on the sizes. The first style
pass panicked with `left: 10256, right: 9488`. The views were 32 bytes; 24 bytes of
excess across 32 cache entries is that 768. ADR 027, and it forced the borrowed
view to be redesigned from three fields to one pointer.

**2. The thread must be registered via `thread_state::initialize(LAYOUT)`** before
a `StyleContext` is built, or `assertion failed: thread_state::get().contains(ThreadState::LAYOUT)`.

**3. `grid-*` and `writing-mode` are behind `servo_pref` keys** and silently do
nothing when off — found by the sweep above, not by any failure.

None of the three appears in a trait signature, a doc comment, or
`docs/research/stylo-requirements.md`, which was assembled by reading real source.
A trait's real contract includes what its dependencies assert about the types you
hand it, and that is not enumerable by reading the trait.

## ADR 021's tripwire, in both readings

**Literally it did not fire.** ADR 021's falsification condition was Phase 5
having to change the arena's slot layout, `NodeId`'s representation, or the opaque
packing. None changed. `ci/gate-style.sh` pins the blob hashes of
`crates/px-dom/tests/layout.rs` and `tests/opaque.rs` and both match on both
platforms. `NodeId` turned out to be *exactly* the right width, which ADR 018
chose for unrelated reasons.

**As a claim it was wrong.** Its substance was that every layout decision
`StyleNode` depended on had already been banked, so the type would be mechanical
to write. A requirement nobody had identified — finding 1 above — forced a
redesign. A tripwire can only watch risks already enumerated, and this one was
enumerated from a note that had missed this.

## px-dom changed in exactly one way

`slot_count()` became `pub`. No `UnsafeCell`, no atomics, no field on `Node`,
nothing in the structural links. `stylo-requirements.md` §5.3 named "`px-dom`
needing interior mutability in places stylo forced" as a signal to abandon stylo
and said one or two atomics would be a fine price. The price was a visibility
keyword.

Everything stylo needs the DOM to hold — `ElementData`, dirty-descendant bits,
selector flags, interned ids and classes, parsed inline style — lives in a
per-slot side table in `px-css` (ADR 026), with a `Cell` per element in the crate
that wanted it.

## What this phase built

`px-css`: all five stylo traits over `px-dom` — `TNode` (13 required),
`TElement` (35, five of them `unsafe fn`), `TDocument` (4), `TShadowRoot` (2) and
`selectors::Element` (20). A pointer-sized borrowed view whose lifetime still
enforces no-mutation-during-traversal. A `Stylist`, a `Device`, the two embedder
stubs stylo requires, and a `DomTraversal` that resolves computed style for a
parsed document. Inline `style` attributes parsed and cascading.

`forbid(unsafe_code)` is gone from `px-css` and the replacement rule holds
comfortably: **zero `unsafe` blocks, zero `unsafe impl`**, both gate-enforced and
both verified by making the gates fail. The one place a transmute would have been
needed — html5ever's `Atom` to stylo's `GenericAtomIdent` — is covered by stylo's
own safe `cast` over a `repr(transparent)` newtype.

## Gate bugs found by running the gates on a correct tree

Three, all mine, all in checks I had written:

- The ADR status check anchored on `Status:` at the start of a line; this project
  writes `- **Status:** accepted`. It **rejected a correctly-written ADR 025**,
  where the tempting fix looks like weakening the check.
- The unsafe scans' comment filter never matched, because `grep -rn` prefixes
  `file:line:` and the filter anchored on `//`. This crate's own documentation of
  the ADR 024 rule, which necessarily contains the words "unsafe impl", failed the
  gate.
- `gate-unsafe-headers.sh` scraped `src/*.rs` paths out of Cargo.toml *comments*
  and reported `src/dom.rs` as a crate root that had "dropped forbid". That one
  printed an `ok` line — a check that invents a subject can also invent a pass.

All three were invisible to the earlier "add a real violation and watch it fail"
probes, because they are false-positive and false-pass bugs rather than
false-negative ones.

## Carried out of this phase

**All three gate items pass.** `compat-list` is red and is not this phase's.

**Decided during the phase, by the user**

- ADR 023: `stylo` 0.21.0, `rayon` available but unused, and invariant 7 weakened
  to name its build-time generators. Chosen against vendoring the generated
  property files, with the cost understood.
- ADR 024: `px-css` may declare `unsafe fn` and may contain no `unsafe` block or
  `unsafe impl`. Narrower than an exemption.

**Costs now paid and permanent**

- **+108 crates** in `Cargo.lock` (84 → 192). ADR 023 predicted ~45; that was
  stylo's direct-dependency count read as a closure figure.
- **Unsafe lines 18,261 → 21,675** across 36 → 109 crates. ADR 023 predicted
  "roughly doubles"; it is +18.7%.
- **`cargo-vet` exemptions 1 → 77.** 76 crates nobody here has read. Written up in
  `supply-chain/README.md`, because `cargo vet fmt` strips comments from the TOML.
- Invariant 7 weaker; `forbid(unsafe_code)` gone from `px-css`.

**Known gaps, each documented at its definition and in the backlog**

- `:hover`, `:focus`, `:active`, `:checked` never match — `state()` is empty
  because there is no input before Phase 13.
- Presentational attributes synthesise nothing: `<table border="1">` gets no
  border.
- The selector bloom filter is disabled. Correct and slower, deliberately: the
  hashes must match how the engine hashes selector components, that hashing is not
  public, and guessing produces *false negatives* — a rule that silently stops
  applying.
- `ex`, `ch`, `ic` and `cap` resolve against a 16px stub with no font metrics
  behind it, and `monospace` gets 16px where a browser uses 13px. Phase 9.
- Shadow DOM, animations and container queries are `None`/`false` throughout.
  `StyleShadowRoot` is *uninhabited* rather than stubbed, so those bodies are
  `match self.never {}` and cannot panic.

**Not measured, and it should have been**

- **No `stylo` version bump was attempted.** `stylo-requirements.md` §5.3 named
  "a minor bump breaking the impl more than once" as a signal to abandon stylo and
  said *"Measure it; do not estimate it."* The phase pinned 0.21.0 and never
  bumped, so the most load-bearing number in ADR 025's decision is absent. In the
  backlog, due before Phase 20.
