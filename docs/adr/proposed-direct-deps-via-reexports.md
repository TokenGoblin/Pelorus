# NNN — Name types through the crate that owns them instead of declaring a second direct dependency

- **Status:** proposed
- **Date:** 2026-11-04
- **Phase:** 6 for the `px-layout`/`px-broker` part; the rest is out of phase and
  belongs in `docs/backlog.md`
- **Invariants touched:** none.

*Numbering: `proposed-<slug>.md`, no number, because numbers collide across
branches (`main` ends at 028, `origin/phase/06-layout` already has 029 and 030).
Number it at merge. Evidence: `docs/research/dependency-audit.md`.*

## Context

Several direct dependencies exist only so a crate can spell a type that a
dependency it already has re-exports. They add **no crate to the tree**, which
the existing `Cargo.toml` comments say honestly. What they add is a version pin
that must be kept in lockstep with someone else's, where drift is a type error
(`px-css` says so itself) or, worse, a second copy in the lockfile (the `toml`
lesson: `toml = "0.9"` once resolved a second copy, +3 crates, +55 unsafe lines).

Measured on `origin/main` (`bd47ba9`):

| Declaration | Crate | What it names | Where else the same type is reachable | Sites |
|---|---|---|---|---|
| `rustls-pki-types = "1"` | `px-net` | **nothing**: zero occurrences of `rustls_pki_types::` | `rustls::pki_types` (already how all 5 sites spell it) | 0 |
| `webpki-roots = "1"` under `[dev-dependencies]` | `px-net` | `TLS_SERVER_ROOTS` in `tests/anchors.rs` | the normal dependency of the same name | 1 |
| `app_units = "0.7.8"` | `px-css` | `Au` in `query_container_size` | `style::values::computed::Au` (`pub use app_units::Au`, stylo `values/computed/mod.rs:137`) | 1 |
| `web_atoms = "0.2.6"` | `px-css` | `LocalName`, `Namespace`, `LocalNameStaticSet`, `NamespaceStaticSet` | `html5ever::{LocalName, Namespace}` (markup5ever re-exports `web_atoms`) and `style::{LocalName, Namespace, Prefix}`, which are the `GenericAtomIdent<...StaticSet>` aliases | 8 in 2 files |

Marginal crates removed from the lockfile by deleting all four: **0**. Distinct
direct dependencies: 17 to 14 (declaration lines 19 to 15).

Phase 6 is about to repeat the pattern. On `origin/phase/06-layout`,
`px-layout` declares `app_units = "0.7.8"` and `stylo`, and `px-broker` adds
optional `px-net`, `px-dom`, `px-css`, `px-layout`, `html5ever`, `url`,
`app_units` and `stylo` for the test-only `px-fetch` diagnostic. All are in the
closure already (the branch's own comment says so), so this is about pin
discipline, not tree size.

`px-layout/src/geom.rs` on that branch already contains the right argument for
using `app_units::Au` and not writing one: it is stylo's own type (`Au` is
referenced from 21 stylo source files), a replacement saves 0 crates and would
need a conversion layer at every font-metrics and container-query boundary, and
the real hazard is the public tuple field `Au(pub i32)`, which the branch bans
by a CI scan of the tuple constructor. This ADR agrees with that and adds one
step: `px-layout` and `px-broker` should not each declare `app_units`.

## Decision

1. `px-net`: delete the `rustls-pki-types` declaration and the dev-dependency
   `webpki-roots` line. Cargo.lock does not change.
2. `px-css`: delete `app_units` and `web_atoms`. Use `style::values::computed::Au`,
   `style::LocalName`/`style::Namespace` and `html5ever::LocalName`/`Namespace`.
3. `px-css` re-exports exactly the types downstream crates need to name from
   its boundary (`pub use style::values::computed::Au;` and `pub use url::Url`
   if `px-broker` needs one), so `px-layout` and `px-broker` get `Au` from
   `px_css::Au` and declare neither `app_units` nor `url`. `px-layout` may still
   declare `stylo` if it must name `ComputedValues`; that is a different question
   from `Au`.
4. Add to `/CLAUDE.md`'s dependency rule one sentence: before adding a direct
   dependency to name a type, check whether a crate you already depend on
   re-exports it.

Optional and separate: `px-dom` could `pub use html5ever::{QualName, LocalName,
Namespace}` so that `px-css` can drop its own `html5ever` declaration too (29 src
+ 3 test sites to retarget); `px-dom` is already the one crate that owns the
html5ever version. Not part of this decision because it moves more lines than it
removes and `px-css` documents a reason for the explicit pin.

## Alternatives rejected

**Leave them.** Cheap, and the existing comments justify each pin. Rejected for
`rustls-pki-types` outright (nothing uses it, so the justification is vacuous) and
for the `px-css` pair because the re-exports are in the crate the code already
imports from; the comment's own claim ("a version drift here is a type error") is
the cost, not the safeguard.

**Write our own `Au`.** Your estimate of ~130 lines (150 to 200 with tests) is
right, and it saves 0 crates: `stylo`, `euclid` and `servo_arc` keep `app_units`
and `serde` in the tree whatever we do. It creates a second `Au` type, with a
conversion at the stylo boundary, to avoid a dependency we are paying for anyway.
Rejected.

**Wrap `Au` in a newtype with private field.** The mechanism that would remove
the `Au(pub i32)` hazard properly. Valuable, orthogonal, and Phase 6's decision;
it still wraps the same `app_units::Au`.

## Consequences

* 3 fewer distinct direct dependencies, 4 fewer declaration lines, and no
  place for `px-css`'s `app_units`/`web_atoms` to diverge from `stylo`'s.
* **Not verified by building.** Stylo's build runs Python and Mako, which the
  audit environment lacks. The re-exports were checked by reading their source,
  not by compiling the substitution. A wrong substitution fails to type-check,
  so the failure mode is loud, but the implementer should expect to find one or
  two spots where the alias differs (`GenericAtomIdent<...>` versus the bare
  `Atom` type: the `px-css` comment at `element.rs:173` is about exactly that
  distinction).
* `px-css` becomes a mild chokepoint for types downstream wants to name. That is
  intentional, and it is the crate that decides when `stylo` is bumped.
* Out of phase: items 1 and 2 touch Phase 3 and Phase 5 crates, so they go to
  `docs/backlog.md` and land in a maintenance commit, not a phase branch.

## Verification

Wrong if `Cargo.lock` changes in the commit that does it (it must not: 0 crates
move), or if removal makes `cargo deny`/`cargo vet` complain (no entry should be
needed or removed). Wrong in spirit if a later stylo bump moves `Au` out of
`style::values::computed`; that is the cost of leaning on a re-export and it is
caught at the bump, which ADR 023 already makes a `TElement` review.

---

## For a dependency ADR, additionally (build-spec section 3, section 5)

Nothing is added. Removal record:

- **Crate and version:** `rustls-pki-types` 1.15.1 (declaration only; remains via
  `rustls`), `app_units` 0.7.8 (declaration only in `px-css`; remains via
  `stylo`), `web_atoms` 0.2.6 (declaration only; remains via `html5ever` and
  `stylo`), `webpki-roots` 1.0.9 (duplicate dev line only).
- **What it does:** n/a.
- **Why not write it:** n/a.
- **Maintainer count:** n/a.
- **Audit status:** unchanged.
- **Unsafe-line count:** unchanged. `ci/unsafe-baseline.json` does not move.
- **Transitive dependencies added:** 0. Removed: 0.
