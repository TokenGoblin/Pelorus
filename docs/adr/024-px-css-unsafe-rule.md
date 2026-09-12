# 024 — `px-css` declares `unsafe fn` and contains no `unsafe` block

- **Status:** accepted
- **Date:** 2026-09-10
- **Phase:** 5
- **Invariants touched:** none in build-spec §1. This amends `/CLAUDE.md`'s
  first hard rule and Phase 0's gate check 4 — a working-agreement change
  rather than an invariant change, which is why it was put to the user rather
  than taken on the standing authorisation.

## Context

`/CLAUDE.md`, first hard rule: *"Every crate begins with
`#![forbid(unsafe_code)]`. The sole exception is `px-sandbox`."* Phase 0's gate
asserts it, and `crates/px-css/src/lib.rs` line 1 carries it today.

stylo's `TElement` declares `unsafe fn` methods that an implementor must write
as `unsafe fn`, because the signature is part of the trait. And
`#![forbid(unsafe_code)]` rejects *implementing* an unsafe method, not only
writing an unsafe block:

```
error: implementation of an `unsafe` method
 --> probe.rs:4:16
4 | impl T for S { unsafe fn f(&self) { ... } }
note: the lint level is defined here
1 | #![forbid(unsafe_code)]
```

`stylo-requirements.md` §3.6 obtained that by compiling a probe against the
pinned toolchain rather than inferring it. **Phase 5 cannot be written under the
rule as it stands.** This is not a surprise to absorb quietly during the phase;
it is a working-agreement amendment, and the person who wrote the rule should
be the one to change it.

**Counted in the published crate rather than taken from the research note.**
The note said six `unsafe fn`, reading `servo/stylo` at HEAD. In `stylo 0.21.0`
— the version ADR 023 takes — `TElement` declares **eight**, of which **five are
required**: `set_handled_snapshot`, `set_dirty_descendants`,
`unset_dirty_descendants`, `ensure_data` and `clear_data`. The other three
(`set_animation_only_dirty_descendants`, its `unset_` partner, and
`clear_descendant_bits`) ship default bodies, so an implementor may leave them
alone. Five rather than six does not change the decision — one required
`unsafe fn` is enough to make `forbid` impossible — but a number stated in an
ADR should be the number in the crate being depended on.

## Decision

**Decided 2026-09-10**, against the blanket-exemption and shim-crate
alternatives below. Replace `forbid(unsafe_code)` in `px-css` with a rule that
is *narrower* than an exemption:

> `px-css` may declare `unsafe fn` where a `stylo` trait signature requires it.
> It contains **zero** `unsafe` blocks and **zero** `unsafe impl`.

The reason this is narrower rather than weaker is in the probe: the *body* of an
`unsafe fn` needs no `unsafe` block. Declaring `unsafe fn f(&self)` is a
statement about who may call it, not permission to do anything unchecked inside
it. And with the borrowed-view design in `stylo-requirements.md` §3.2 there is
no `unsafe impl Send`/`Sync` either, because `Send` and `Sync` are *derived*
legitimately rather than asserted — which is a stronger position than the
reference implementation, where Servo asserts them.

So what `px-css` gives up is a lint that cannot coexist with the trait. What it
keeps — and what `forbid` was actually protecting — is that no line in the crate
does anything the compiler is not checking.

The rule is greppable, so `ci/gate-unsafe-headers.sh` enforces it as a named
per-crate rule rather than as a hole in a blanket one. `ci/gate-style.sh`
already checks both halves and passes today, while `px-css` is still an empty
skeleton under `forbid` — deliberately, so the guarantee does not lapse during
the commit that removes `forbid`, which is the one window where it would.

## Alternatives rejected

**Keep `forbid(unsafe_code)` and put the trait impls in `px-sandbox`.**
`px-sandbox` is the audited unsafe core (ADR 008) and this would technically
satisfy every existing rule. Rejected, and firmly: it would move thousands of
lines of cascade and DOM-traversal glue into the one crate whose entire value is
that it is small enough to review against OS documentation line by line. The
rule exists to keep unsafe reviewable; satisfying its letter by making the
audited core unreviewable inverts it.

**Blanket-exempt `px-css`: "px-css may use unsafe".** The obvious amendment,
and the bad outcome — it gives up `forbid` and buys nothing checkable back. An
`unsafe` block could then appear in the cascade with no gate objecting.
Rejected.

**Wrap stylo's traits in a thin `unsafe`-carrying shim crate and keep `px-css`
under `forbid`.** Honest, and it localises the amendment to a crate whose whole
job is the boundary. Rejected on the grounds that it is a new crate — which
needs its own ADR — to hold a property that a per-crate lint rule already
expresses, and the shim's methods would be `unsafe fn` for exactly the same
reason, so the amendment would follow it rather than being avoided.

**`#[allow(unsafe_code)]` on the impl blocks, keeping the crate-level
`forbid`.** Does not work: `forbid` cannot be overridden by an inner `allow`,
which is the difference between `forbid` and `deny` and the reason the hard rule
chose `forbid`. Worth stating because it is the first thing anybody tries.

## Consequences

**`/CLAUDE.md`'s first hard rule gains a second exception, and the file has to
say so.** The rule currently reads "the sole exception is `px-sandbox`". After
this it names two exceptions with different shapes: `px-sandbox` may contain
reviewed `unsafe` blocks; `px-css` may declare `unsafe fn` and may not contain
`unsafe` blocks at all. Two exceptions with distinct rules is more to hold in
your head than one blanket rule, and that is the cost.

**Phase 0's gate check 4 changes from a uniform assertion to a table.** A gate
that checks "every crate has `forbid`" is one line; a gate that checks three
different per-crate rules is a table that can itself be wrong. Mitigated by the
rules being greppable and by `gate-style.sh` checking `px-css` independently.

**A future crate will want the same exception and the argument will be weaker.**
The precedent set here is "a trait signature we do not control requires it",
which is narrow. It should be quoted back at anything broader.

**If stylo is replaced (ADR 025), this amendment becomes unnecessary and should
be reverted rather than left standing.** A hand-written cascade needs no
`unsafe fn`. Noting it here because an exemption that outlives its reason is how
a rule erodes.

## Verification

Wrong if `px-css` ends up containing an `unsafe` block after all — that would
mean the claim that an `unsafe fn` body needs no `unsafe` block was false in
some case the probe did not cover, and the amendment as written is unbuildable.
`ci/gate-style.sh` fails in that case, by construction, which is the point of
having written the check before the code.

Wrong in the other direction if `TElement` turns out to be implementable without
declaring any `unsafe fn` — for instance if the six methods acquire safe default
bodies in a later stylo release. Then `forbid` should come back. Cheap to check
at every stylo bump, and the bump already requires a `TElement` review.
